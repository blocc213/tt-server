//! Chat completion: streaming, non-streaming, and cancellation.
//!
//! Upstream SillyTavern proxies the provider's SSE stream straight to the
//! browser and aborts the upstream request when the client socket closes
//! (`src/endpoints/backends/chat-completions.js:224-228`, `src/util.js:743-746`).
//! This host does the same, with one addition: an explicit cancel endpoint, so
//! pressing Stop cancels immediately instead of waiting for the socket teardown
//! the browser performs lazily.

use std::convert::Infallible;
use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures_util::Stream;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_stream::wrappers::UnboundedReceiverStream;

use tt_application::services::chat_completion_service::ChatCompletionService;

use crate::error::ServerError;
use crate::state::SharedState;

#[derive(Debug, Deserialize)]
pub struct GenerateRequest {
    /// Identifies the stream so Stop can cancel it.
    #[serde(default)]
    pub stream_id: Option<String>,
    /// User opted in: keep generating after the browser disconnects.
    #[serde(default)]
    pub background: bool,
    #[serde(flatten)]
    pub payload: Value,
}

#[derive(Debug, Deserialize)]
pub struct ResumeRequest {
    pub stream_id: String,
    /// Index of the first buffered event the client has not seen.
    #[serde(default)]
    pub from: usize,
}

#[derive(Debug, Deserialize)]
pub struct CancelRequest {
    pub stream_id: String,
}

fn wants_stream(payload: &Value) -> bool {
    payload
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Rejects ids that could collide or grow unbounded. Mirrors the host command's
/// validation (`chat_completion_commands.rs:166-183`).
fn validate_stream_id(stream_id: &str) -> Result<(), ServerError> {
    let trimmed = stream_id.trim();
    if trimmed.is_empty() || trimmed.len() > 128 {
        return Err(ServerError::BadRequest("Invalid stream id length".into()));
    }

    if !trimmed
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        return Err(ServerError::BadRequest(
            "Invalid stream id characters".into(),
        ));
    }

    Ok(())
}

pub async fn status(
    State(state): State<SharedState>,
    Json(payload): Json<Value>,
) -> Result<Json<Value>, ServerError> {
    let dto = serde_json::from_value(payload)?;
    crate::rpc_services::ensure_endpoint(&state,state.services.chat_completion_service.resolve_status_user_endpoint(&dto)?).await?;
    Ok(Json(
        state
            .services
            .chat_completion_service
            .get_status(dto)
            .await?,
    ))
}

pub async fn generate(
    State(state): State<SharedState>,
    axum::Extension(owner): axum::Extension<crate::browser_resources::RequestOwner>,
    Json(mut request): Json<GenerateRequest>,
) -> Result<Response, ServerError> {
    if let Some(id)=request.stream_id.as_mut() { validate_stream_id(id)?; *id=scoped_stream_id(&owner.0,id); }
    let payload = &request.payload;
    log_failed_request(generate_inner(state, &request).await, Some(payload))
}

fn log_failed_request<T, E>(outcome: Result<T, E>, payload: Option<&Value>) -> Result<T, E> {
    if outcome.is_err()
        && tracing::enabled!(target: "tauritavern_server::llm", tracing::Level::INFO)
        && let Some(payload) = payload
    {
        tracing::info!(target: "tauritavern_server::llm",
            "Chat Completion failed browser request:\n{}", crate::llm_logging::pretty(payload, None));
    }
    outcome
}

async fn generate_inner(
    state: SharedState,
    request: &GenerateRequest,
) -> Result<Response, ServerError> {
    let payload = &request.payload;
    let dto = serde::Deserialize::deserialize(payload)?;
    crate::rpc_services::ensure_endpoint(&state,state.services.chat_completion_service.resolve_generate_user_endpoint(&dto)?).await?;

    if request.background {
        let id = request.stream_id.clone().ok_or_else(|| {
            ServerError::BadRequest("Background generation needs a stream id".into())
        })?;
        validate_stream_id(&id)?;
        let stream = wants_stream(payload);
        let (job, created) = crate::background::get_or_insert(&id, stream);
        tracing::info!(stream_id = %id, stream, created, "Background job accepted");
        if created {
            let dto = serde::Deserialize::deserialize(payload)?;
            spawn_background(
                state.services.chat_completion_service.clone(),
                id,
                job.clone(),
                dto,
                tracing::enabled!(target: "tauritavern_server::llm", tracing::Level::INFO)
                    .then(|| payload.clone()),
            );
        } else {
            tracing::info!(stream_id = %id, "Background job re-attached");
        }
        return Ok(background_response(&job, 0).await);
    }

    if !wants_stream(payload) {
        // Registered even without streaming so Stop can abort a slow
        // non-streaming call instead of leaving it running upstream.
        let request_id = request
            .stream_id
            .clone()
            .unwrap_or_else(|| format!("request-{}", uuid_like()));
        validate_stream_id(&request_id)?;

        let service = state.services.chat_completion_service.clone();
        let dto = serde::Deserialize::deserialize(payload)?;
        let completion = service.generate_request(&request_id, dto).await;

        return Ok(Json(completion?).into_response());
    }

    let stream_id = request
        .stream_id
        .clone()
        .unwrap_or_else(|| format!("stream-{}", uuid_like()));
    validate_stream_id(&stream_id)?;

    let service = state.services.chat_completion_service.clone();
    let dto = serde::Deserialize::deserialize(payload)?;
    let log_payload = tracing::enabled!(target: "tauritavern_server::llm", tracing::Level::INFO)
        .then(|| payload.clone());
    service.start_stream(stream_id.clone(), dto).await?;
    let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel::<Result<Event, Infallible>>();

    // Provider stream -> SSE frames. Ends on provider completion, provider
    // error, or cancellation.
    let generation_service = service.clone();
    let generation_stream_id = stream_id.clone();
    tokio::spawn(async move {
        let outcome = forward_stream(&generation_service, &generation_stream_id, |chunk| {
            event_tx.send(Ok(Event::default().data(chunk))).is_ok()
        }).await;
        if let Err(error) = log_failed_request(outcome, log_payload.as_ref()) {
            let _ = event_tx.send(Ok(Event::default().data(json!({"error":{"message":error.to_string()}}).to_string())));
            let _ = event_tx.send(Ok(Event::default().data("[DONE]")));
        }
        generation_service.close_stream(&generation_stream_id).await;
    });

    // Dropping the response body means the browser is gone: tab closed, network
    // lost, or fetch aborted. Cancel upstream rather than keep paying for tokens
    // nobody will receive.
    let disconnect_guard = StreamDisconnectGuard {
        service: service.clone(),
        stream_id: stream_id.clone(),
    };
    let stream = DisconnectAware {
        inner: UnboundedReceiverStream::new(event_rx),
        _guard: disconnect_guard,
    };

    Ok(Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response())
}

/// Cancels a running stream. Flips the cancel flag the provider read loop
/// checks between chunks, which drops the upstream response and closes the
/// connection to the provider.
pub async fn cancel_stream(
    State(state): State<SharedState>,
    axum::Extension(owner): axum::Extension<crate::browser_resources::RequestOwner>,
    Json(mut request): Json<CancelRequest>,
) -> Result<Json<Value>, ServerError> {
    validate_stream_id(&request.stream_id)?;
    request.stream_id=scoped_stream_id(&owner.0,&request.stream_id);
    let cancelled = state
        .services
        .chat_completion_service
        .close_stream(&request.stream_id)
        .await;

    tracing::info!(stream_id = %request.stream_id, cancelled, "Chat completion stream cancel requested");
    Ok(Json(json!({ "ok": true, "cancelled": cancelled })))
}

pub async fn cancel_generation(
    State(state): State<SharedState>,
    axum::Extension(owner): axum::Extension<crate::browser_resources::RequestOwner>,
    Json(mut request): Json<CancelRequest>,
) -> Result<Json<Value>, ServerError> {
    validate_stream_id(&request.stream_id)?;
    request.stream_id=scoped_stream_id(&owner.0,&request.stream_id);
    let cancelled = state
        .services
        .chat_completion_service
        .cancel_generation(&request.stream_id)
        .await;

    tracing::info!(stream_id = %request.stream_id, cancelled, "Chat completion generation cancel requested");
    Ok(Json(json!({ "ok": true, "cancelled": cancelled })))
}

/// Re-attaches to a background generation after the browser lost its socket.
pub async fn resume(axum::Extension(owner): axum::Extension<crate::browser_resources::RequestOwner>, Json(mut request): Json<ResumeRequest>) -> Result<Response, ServerError> {
    validate_stream_id(&request.stream_id)?;
    request.stream_id=scoped_stream_id(&owner.0,&request.stream_id);
    let job = match crate::background::get(&request.stream_id) {
        Some(job) => {
            tracing::info!(stream_id = %request.stream_id, from = request.from, "Background job resumed");
            job
        }
        None => {
            tracing::info!(stream_id = %request.stream_id, "Background job not found");
            return Err(ServerError::NotFound(
                "Background generation expired or the server restarted".into(),
            ));
        }
    };
    Ok(background_response(&job, request.from).await)
}

async fn background_response(job: &crate::background::Job, from: usize) -> Response {
    if job.stream {
        return Sse::new(job.replay(from))
            .keep_alive(KeepAlive::default())
            .into_response();
    }
    let body = job.result().await;
    ([(http::header::CONTENT_TYPE, "application/json")], body).into_response()
}

async fn forward_stream(
    service: &ChatCompletionService,
    id: &str,
    mut send: impl FnMut(String) -> bool,
) -> Result<(), tt_application::errors::ApplicationError> {
    use tt_application::dto::chat_completion_dto::{ChatCompletionStreamEventDto as EventDto, ChatCompletionStreamStatusDto as Status};
    let mut after = 0;
    loop {
        let read = service.read_stream(id, after).await?;
        let mut terminal = false;
        for event in read.events {
            let (seq, data) = match event {
                EventDto::Chunk {seq,data} => (seq,data),
                EventDto::Done {seq} => {terminal=true;(seq,"[DONE]".into())},
                EventDto::Error {seq,message,details} => {
                    if !send(json!({"error":{"message":message,"details":details}}).to_string()) { return Ok(()); }
                    terminal=true;(seq,"[DONE]".into())
                }
            };
            after=seq;
            if !send(data) { return Ok(()); }
        }
        if terminal { return Ok(()); }
        if read.status != Status::Running { send("[DONE]".into()); return Ok(()); }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}

/// Runs one provider call detached from any HTTP connection. Cancellation
/// comes only from the existing cancel endpoints (the Stop button).
fn spawn_background(
    service: Arc<ChatCompletionService>,
    id: String,
    job: Arc<crate::background::Job>,
    dto: tt_application::dto::chat_completion_dto::ChatCompletionGenerateRequestDto,
    log_payload: Option<Value>,
) {
    tokio::spawn(async move {
        let start = std::time::Instant::now();
        let outcome_str = if job.stream {
            let outcome = async {
                service.start_stream(id.clone(), dto).await?;
                forward_stream(&service, &id, |chunk| { job.push(chunk); true }).await
            }.await;
            let status = match log_failed_request(outcome, log_payload.as_ref()) {
                Ok(()) => "ok",
                Err(error) => {
                    tracing::warn!(stream_id = %id, %error, "Background chat completion stream failed");
                    job.push(json!({"error":{"message":error.to_string()}}).to_string());
                    job.push("[DONE]".into());
                    "error"
                }
            };
            service.close_stream(&id).await;
            status
        } else {
            let (body, status) = match log_failed_request(
                service.generate_request(&id, dto).await,
                log_payload.as_ref(),
            ) {
                Ok(value) => (value, "ok"),
                Err(error) => (
                    json!({ "error": { "message": error.to_string() } }),
                    "error",
                ),
            };
            job.push(body.to_string());
            status
        };
        job.finish();
        let duration_ms = start.elapsed().as_millis();
        let events = job.event_count();
        tracing::info!(
            stream_id = %id,
            events,
            duration_ms,
            outcome = outcome_str,
            "Background job finished"
        );
        tokio::time::sleep(crate::background::RETENTION).await;
        crate::background::remove(&id);
    });
}

/// Cancels the stream when the response body is dropped.
///
/// Mirrors upstream's `request.socket.on('close')` abort
/// (`src/endpoints/backends/chat-completions.js:226`). Cancelling an already
/// finished stream is a no-op, so the normal completion path is unaffected.
struct StreamDisconnectGuard {
    service: Arc<ChatCompletionService>,
    stream_id: String,
}

impl Drop for StreamDisconnectGuard {
    fn drop(&mut self) {
        let service = self.service.clone();
        let stream_id = std::mem::take(&mut self.stream_id);
        tokio::spawn(async move {
            if service.close_stream(&stream_id).await {
                tracing::info!(
                    stream_id = stream_id.as_str(),
                    "Client disconnected; cancelled the upstream chat completion stream"
                );
            }
        });
    }
}

pin_project_lite::pin_project! {
    struct DisconnectAware<S> {
        #[pin]
        inner: S,
        _guard: StreamDisconnectGuard,
    }
}

impl<S: Stream> Stream for DisconnectAware<S> {
    type Item = S::Item;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.project().inner.poll_next(context)
    }
}

fn uuid_like() -> String {
    use rand::Rng;
    let mut bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_ids_are_restricted_to_safe_characters() {
        assert!(validate_stream_id("stream-abc_123").is_ok());
        assert!(validate_stream_id("").is_err());
        assert!(validate_stream_id("../escape").is_err());
        assert!(validate_stream_id(&"x".repeat(129)).is_err());
    }

    #[test]
    fn stream_flag_decides_the_response_shape() {
        assert!(wants_stream(&json!({ "stream": true })));
        assert!(!wants_stream(&json!({ "stream": false })));
        assert!(!wants_stream(&json!({})));
    }
}

fn scoped_stream_id(owner:&str,id:&str)->String {
    use sha2::{Digest,Sha256};
    let session=owner.split_once(':').map_or(owner,|(session,_)|session);
    let mut hash=Sha256::new();hash.update(session.as_bytes());hash.update([0]);hash.update(id.as_bytes());hex::encode(hash.finalize())
}
