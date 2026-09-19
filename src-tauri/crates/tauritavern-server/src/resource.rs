//! Browser-visible resources and frontend files.
//!
//! `HostResourceService` already owns the SillyTavern URL contract for avatars,
//! backgrounds, thumbnails, user files/CSS, and third-party extension assets.
//! The server adapts its `http::Response<Vec<u8>>` to Axum and then falls back to
//! the checked-in frontend tree. A missing resource is a true 404 -- never
//! `index.html` masquerading as an image or script.

use std::path::{Component, Path, PathBuf};

use axum::body::Body;
use axum::extract::{Request, State};
use axum::response::{IntoResponse, Response};
use http::{StatusCode, Uri};
use tt_application::services::host_resource_service::HostResourceDeliveryCapabilities;

use crate::state::SharedState;

const HTTP_DELIVERY: HostResourceDeliveryCapabilities =
    HostResourceDeliveryCapabilities::new(true, false);

pub async fn serve(State(state): State<SharedState>, request: Request) -> Response {
    let path = request.uri().path();
    if has_forbidden_component(path) {
        return StatusCode::NOT_FOUND.into_response();
    }

    let host_request = to_host_request(&request);
    if let Some(response) = state
        .services
        .host_resource_service
        .try_serve(&host_request, HTTP_DELIVERY)
    {
        return vec_response(response);
    }

    serve_frontend_file(&state.frontend_dir, request.uri()).await
}

fn to_host_request(request: &Request) -> http::Request<Vec<u8>> {
    let mut builder = http::Request::builder()
        .method(request.method().clone())
        .uri(request.uri().clone());
    if let Some(headers) = builder.headers_mut() {
        headers.extend(request.headers().clone());
    }
    builder.body(Vec::new()).unwrap_or_default()
}

fn vec_response(response: http::Response<Vec<u8>>) -> Response {
    let (parts, body) = response.into_parts();
    Response::from_parts(parts, Body::from(body))
}

async fn serve_frontend_file(root: &Path, uri: &Uri) -> Response {
    let relative = uri.path().trim_start_matches('/');
    let relative = if relative.is_empty() {
        PathBuf::from("index.html")
    } else {
        PathBuf::from(relative)
    };

    if relative
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return StatusCode::NOT_FOUND.into_response();
    }

    let path = root.join(&relative);
    let metadata = match tokio::fs::metadata(&path).await {
        Ok(metadata) if metadata.is_file() => metadata,
        _ => return StatusCode::NOT_FOUND.into_response(),
    };

    let body = match tokio::fs::read(&path).await {
        Ok(body) => body,
        Err(error) => {
            tracing::error!(path = %path.display(), %error, "Failed to read frontend asset");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let content_type = mime_guess::from_path(&path).first_or_octet_stream();
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        http::header::CONTENT_TYPE,
        http::HeaderValue::from_str(content_type.as_ref())
            .unwrap_or_else(|_| http::HeaderValue::from_static("application/octet-stream")),
    );
    response.headers_mut().insert(
        http::header::CONTENT_LENGTH,
        http::HeaderValue::from_str(&metadata.len().to_string())
            .unwrap_or_else(|_| http::HeaderValue::from_static("0")),
    );
    response
}

/// True when any path segment names a Git metadata directory.
///
/// Compares *decoded* segments. `client_asset_paths::parse_third_party_asset_request_path`
/// percent-decodes every segment before touching the filesystem, so matching the
/// raw text here would let `%2Egit` and `.%67it` walk straight past the filter and
/// serve `.git/config` -- which on a migrated SillyTavern clone can carry a
/// `https://user:token@host/...` remote URL.
///
/// `.git` stays a legal path component for the Tauri host (see
/// `docs/CurrentState/ThirdPartyExtensions.md`): there the request never leaves
/// the device. Over HTTP it is an information leak, so the server host departs
/// from that contract on purpose.
fn has_forbidden_component(path: &str) -> bool {
    path.split('/').any(|part| {
        percent_encoding::percent_decode_str(part)
            .decode_utf8()
            .map(|decoded| decoded.eq_ignore_ascii_case(".git"))
            .unwrap_or(true)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_metadata_is_never_browser_visible() {
        assert!(has_forbidden_component(
            "/scripts/extensions/third-party/example/.git/config"
        ));
        assert!(has_forbidden_component(
            "/scripts/extensions/third-party/example/.GIT/HEAD"
        ));
        assert!(!has_forbidden_component(
            "/scripts/extensions/third-party/example/index.js"
        ));

        // The resource layer percent-decodes every segment before it reaches the
        // filesystem, so a raw-text filter is bypassable. These encodings all
        // decode to `.git` and served real repository bytes before the fix.
        for encoded in [
            "/scripts/extensions/third-party/example/%2Egit/config",
            "/scripts/extensions/third-party/example/.%67it/config",
            "/scripts/extensions/third-party/example/%2E%67%69%74/HEAD",
        ] {
            assert!(
                has_forbidden_component(encoded),
                "percent-encoded Git metadata must stay blocked: {encoded}"
            );
        }

        // Decoding must not over-block: `.gitignore` is an ordinary file a
        // third-party extension may legitimately ship and reference.
        assert!(!has_forbidden_component(
            "/scripts/extensions/third-party/example/.gitignore"
        ));
    }
}
