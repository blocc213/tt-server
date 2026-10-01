//! HTTP routing and the authentication gate.

use axum::body::Body;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use tower_http::decompression::RequestDecompressionLayer;

use crate::auth::{SESSION_COOKIE, session_token_from_cookies};
use crate::error::ServerError;
use crate::state::SharedState;

/// Upper bound for a single request body.
///
/// Sized for the largest thing the browser legitimately sends in one request:
/// a character card PNG staged through `/rpc-raw/stage_upload_chunk`, or a
/// chat JSONL chunk. Large enough to stop rejecting real content, small enough
/// that an unauthenticated body cannot exhaust memory before the session gate
/// runs.
const MAX_REQUEST_BODY_BYTES: usize = 64 * 1024 * 1024;

/// Upper bound for `/rpc/upload_user_file` only.
///
/// The payload is a whole file base64-encoded in JSON (~4/3 inflation), and
/// LittleWhiteBox vector backups of long chats exceed 64 MiB once encoded.
/// The route sits behind the session gate, so only authenticated clients can
/// send a body this large; every other route keeps `MAX_REQUEST_BODY_BYTES`.
const MAX_USER_FILE_UPLOAD_BYTES: usize = 128 * 1024 * 1024;

pub fn router(state: SharedState) -> Router {
    let protected = Router::new()
        .route("/csrf-token", get(csrf_token))
        .route("/api/bootstrap", post(bootstrap))
        .route("/api/chats/get", post(crate::chat::get_chat))
        .route("/api/chats/save", post(crate::chat::save_chat))
        .route("/api/users/backup", post(crate::archive::user_backup))
        .route(
            "/api/tauritavern/data-migration/export/download",
            get(crate::archive::export_download),
        )
        .route(
            "/api/tauritavern/group-chats/get",
            post(crate::chat::get_group_chat),
        )
        .route(
            "/api/tauritavern/group-chats/save",
            post(crate::chat::save_group_chat),
        )
        .route(
            "/api/backends/chat-completions/status",
            post(crate::generate::status),
        )
        .route(
            "/api/backends/chat-completions/generate",
            post(crate::generate::generate),
        )
        .route(
            "/api/backends/chat-completions/cancel",
            post(crate::generate::cancel_stream),
        )
        .route(
            "/api/backends/chat-completions/cancel-generation",
            post(crate::generate::cancel_generation),
        )
        .route(
            "/api/backends/chat-completions/resume",
            post(crate::generate::resume),
        )
        // Static segments win over `{command}`, so this route alone gets the
        // larger ceiling while every other command keeps the global one.
        .route(
            "/rpc/upload_user_file",
            post(upload_user_file).layer(DefaultBodyLimit::max(MAX_USER_FILE_UPLOAD_BYTES)),
        )
        .route("/rpc/{command}", post(rpc))
        .route("/rpc-raw/stage_upload_chunk", post(stage_upload_chunk))
        .route("/api/logout", post(logout))
        .fallback(crate::resource::serve)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            require_session,
        ));

    Router::new()
        .route("/api/login", post(login))
        .route("/api/auth/status", get(auth_status))
        .merge(protected)
        // axum defaults to 2 MiB, which the Tauri host never had: IPC carried
        // whole character cards and chat payloads with no transport ceiling.
        // Character card PNGs routinely exceed 2 MiB, and the frontend emits
        // chat JSONL in 4 MiB chunks (src/scripts/tauri/chat/jsonl.js).
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BODY_BYTES))
        // Upstream enables gzip request bodies through the bootstrap settings;
        // chat and settings saves must accept the same Content-Encoding.
        .layer(RequestDecompressionLayer::new().gzip(true))
        .with_state(state)
}

#[derive(Debug, Deserialize)]
struct LoginRequest {
    password: String,
}

async fn auth_status(State(state): State<SharedState>) -> Json<Value> {
    Json(json!({ "requiresPassword": !state.auth.is_open() }))
}

async fn login(
    State(state): State<SharedState>,
    Json(request): Json<LoginRequest>,
) -> Result<Response, ServerError> {
    if state.auth.is_open() {
        return Ok(Json(json!({ "ok": true })).into_response());
    }

    let Some(token) = state.auth.login(&request.password) else {
        // Deliberately vague: a single shared password has no user to enumerate.
        return Err(ServerError::Unauthorized("Incorrect password".into()));
    };

    let cookie = format!(
        "{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}",
        30 * 24 * 60 * 60
    );
    let mut response = Json(json!({ "ok": true })).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&cookie)
            .map_err(|error| ServerError::Internal(format!("Invalid session cookie: {error}")))?,
    );
    Ok(response)
}

async fn logout(State(state): State<SharedState>, request: Request) -> Response {
    if let Some(token) = request
        .headers()
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(session_token_from_cookies)
    {
        state.auth.logout(token);
    }

    let mut response = Json(json!({ "ok": true })).into_response();
    if let Ok(cookie) = HeaderValue::from_str(&format!("{SESSION_COOKIE}=; Path=/; Max-Age=0")) {
        response.headers_mut().insert(header::SET_COOKIE, cookie);
    }
    response
}

async fn require_session(
    State(state): State<SharedState>,
    request: Request,
    next: Next,
) -> Response {
    if state.auth.is_open() {
        return next.run(request).await;
    }

    let authorized = request
        .headers()
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(session_token_from_cookies)
        .is_some_and(|token| state.auth.is_valid_session(token));

    if authorized {
        return next.run(request).await;
    }

    // The frontend is a single-page app, so an unauthenticated document request
    // gets the login page rather than a JSON error.
    let wants_document = request
        .headers()
        .get(header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|accept| accept.contains("text/html"));

    if wants_document {
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
            .header(header::CACHE_CONTROL, "no-store")
            .body(Body::from(LOGIN_PAGE))
            .unwrap_or_else(|_| StatusCode::UNAUTHORIZED.into_response());
    }

    (
        StatusCode::UNAUTHORIZED,
        Json(json!({ "error": "Not signed in" })),
    )
        .into_response()
}

async fn csrf_token(State(state): State<SharedState>) -> Json<Value> {
    Json(json!({ "token": state.csrf_token }))
}

async fn bootstrap(State(state): State<SharedState>) -> Result<Json<Value>, ServerError> {
    let value = crate::rpc::dispatch(&state, "get_bootstrap_snapshot", Value::Null)
        .await
        .ok_or_else(|| ServerError::Internal("Bootstrap command is not registered".into()))??;
    Ok(Json(value))
}

async fn stage_upload_chunk(
    State(state): State<SharedState>,
    headers: http::HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, ServerError> {
    let file_path = required_header(&headers, "file-path")?;
    let file_path = percent_encoding::percent_decode_str(&file_path)
        .decode_utf8()
        .map_err(|_| ServerError::BadRequest("Upload file path is not UTF-8".into()))?
        .into_owned();
    let offset = required_header(&headers, "offset")?
        .parse::<u64>()
        .map_err(|_| ServerError::BadRequest("Upload offset is invalid".into()))?;

    let next_offset = state
        .upload_staging
        .append(&file_path, offset, &body)
        .await?;
    Ok(Json(json!(next_offset)))
}

fn required_header(headers: &http::HeaderMap, name: &'static str) -> Result<String, ServerError> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string)
        .ok_or_else(|| ServerError::BadRequest(format!("Missing header `{name}`")))
}

async fn rpc(
    State(state): State<SharedState>,
    axum::extract::Path(command): axum::extract::Path<String>,
    body: Option<Json<Value>>,
) -> Result<Json<Value>, ServerError> {
    let args = body.map(|Json(value)| value).unwrap_or(Value::Null);

    match crate::rpc::dispatch(&state, &command, args).await {
        Some(result) => Ok(Json(result?)),
        None => Err(ServerError::NotFound(format!(
            "Command `{command}` is not available in server mode"
        ))),
    }
}

async fn upload_user_file(
    State(state): State<SharedState>,
    body: Option<Json<Value>>,
) -> Result<Json<Value>, ServerError> {
    rpc(
        State(state),
        axum::extract::Path("upload_user_file".to_string()),
        body,
    )
    .await
}

const LOGIN_PAGE: &str = include_str!("login.html");

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use crate::auth::Auth;
    use crate::state::AppState;

    #[test]
    fn open_auth_accepts_requests_without_a_cookie() {
        assert!(Auth::new(None).is_valid_session(""));
    }

    /// axum's 2 MiB default rejected real content with a bare 413: character
    /// card PNGs and the frontend's 4 MiB chat JSONL chunks both exceed it.
    /// The Tauri host had no transport ceiling at all, so nothing upstream
    /// keeps these payloads small.
    #[test]
    fn body_limit_admits_character_cards_and_chat_chunks() {
        const CHAT_JSONL_CHUNK_BYTES: usize = 4 * 1024 * 1024;

        assert!(
            super::MAX_REQUEST_BODY_BYTES > CHAT_JSONL_CHUNK_BYTES,
            "a single chat JSONL chunk must fit in one request"
        );
        assert!(
            super::MAX_REQUEST_BODY_BYTES >= 64 * 1024 * 1024,
            "character card PNGs reach tens of megabytes"
        );
    }

    /// Sends `len` bytes of JSON-ish body and returns the HTTP status code.
    ///
    /// Raw socket so the test exercises the real router and its per-route
    /// body limits, not just the constants.
    async fn post_status(addr: std::net::SocketAddr, path: &str, len: usize) -> u16 {
        let mut stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
        let head = format!(
            "POST {path} HTTP/1.1\r\nHost: t\r\nContent-Type: application/json\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(head.as_bytes()).await.expect("head");
        // A body that is valid-length but not valid JSON: under the limit the
        // extractor answers 400, over it the body limit answers 413.
        let chunk = vec![b' '; 1024 * 1024];
        let mut sent = 0;
        while sent < len {
            let n = chunk.len().min(len - sent);
            if stream.write_all(&chunk[..n]).await.is_err() {
                break; // server may close early after rejecting
            }
            sent += n;
        }
        let mut response = Vec::new();
        let _ = stream.read_to_end(&mut response).await;
        let text = String::from_utf8_lossy(&response);
        text.split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .unwrap_or(0)
    }

    #[tokio::test]
    async fn only_user_file_upload_gets_the_larger_body_limit() {
        let root = std::env::temp_dir().join(format!("tt-body-limit-{}", rand::random::<u64>()));
        let services = crate::composition::build(
            &root,
            root.join("resources"),
            Default::default(),
            crate::product::USER_AGENT,
        )
        .await
        .expect("build isolated server services");
        let state = Arc::new(AppState {
            services,
            auth: Auth::new(None),
            frontend_dir: root.join("frontend"),
            csrf_token: "test".into(),
            upload_staging: Arc::new(crate::upload::UploadStaging::new(&root)),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let server = tokio::spawn(async move {
            axum::serve(listener, super::router(state))
                .await
                .expect("serve");
        });

        const MIB: usize = 1024 * 1024;
        // The real failing backup was 82,829,791 bytes.
        assert_eq!(
            post_status(addr, "/rpc/upload_user_file", 80 * MIB).await,
            400,
            "80 MiB upload must pass the body limit"
        );
        assert_eq!(
            post_status(addr, "/rpc/upload_user_file", 129 * MIB).await,
            413,
            "upload keeps a ceiling"
        );
        assert_eq!(
            post_status(addr, "/rpc/verify_user_files", 70 * MIB).await,
            413,
            "other commands keep 64 MiB"
        );
        assert_eq!(
            post_status(addr, "/api/login", 70 * MIB).await,
            413,
            "unauthenticated routes keep 64 MiB"
        );

        server.abort();
        let _ = tokio::fs::remove_dir_all(root).await;
    }
}
