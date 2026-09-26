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
    Json(request): Json<GenerateRequest>,
) -> Result<Response, ServerError> {
    let payload = request.payload;

    if request.background {
        let id = request
            .stream_id
            .ok_or_else(|| ServerError::BadRequest("Background generation needs a stream id".into()))?;
        validate_stream_id(&id)?;
        let stream = wants_stream(&payload);
        let (job, created) = crate::background::get_or_insert(&id, stream);
        if created {
            let dto = serde_json::from_value(payload)?;
            spawn_background(state.services.chat_completion_service.clone(), id, job.clone(), dto);
        }
        return Ok(background_response(&job, 0).await);
    }

    if !wants_stream(&payload) {
        // Registered even without streaming so Stop can abort a slow
        // non-streaming call instead of leaving it running upstream.
        let request_id = request
            .stream_id
            .clone()
            .unwrap_or_else(|| format!("request-{}", uuid_like()));
        validate_stream_id(&request_id)?;

        let service = state.services.chat_completion_service.clone();
        let dto = serde_json::from_value(payload)?;
        let cancel = service.register_generation(&request_id).await;
        let completion = service.generate_with_cancel(dto, cancel).await;
        service.complete_generation(&request_id).await;

        return Ok(Json(completion?).into_response());
    }

    let stream_id = request
        .stream_id
        .unwrap_or_else(|| format!("stream-{}", uuid_like()));
    validate_stream_id(&stream_id)?;

    let service = state.services.chat_completion_service.clone();
    let dto = serde_json::from_value(payload)?;
    let cancel = service.register_stream(&stream_id).await;

    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel::<String>();
    let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel::<Result<Event, Infallible>>();

    // Provider stream -> SSE frames. Ends on provider completion, provider
    // error, or cancellation.
    let generation_service = service.clone();
    let generation_stream_id = stream_id.clone();
    tokio::spawn(async move {
        let mut chunks = UnboundedReceiverStream::new(receiver);
        let forward = {
            let event_tx = event_tx.clone();
            tokio::spawn(async move {
                use futures_util::StreamExt as _;
                while let Some(chunk) = chunks.next().await {
                    if chunk.is_empty() {
                        continue;
                    }
                    // Sending fails once the browser is gone; stop pumping.
                    if event_tx.send(Ok(Event::default().data(chunk))).is_err() {
                        break;
                    }
                }
            })
        };

        let outcome = generation_service
            .generate_stream(dto, sender, cancel)
            .await;
        let _ = forward.await;

        match outcome {
            Ok(()) => {
                let _ = event_tx.send(Ok(Event::default().data("[DONE]")));
            }
            Err(error) => {
                let message = error.to_string();
                tracing::warn!(stream_id = %generation_stream_id, %message, "Chat completion stream ended with an error");
                let payload = json!({ "error": { "message": message } });
                let _ = event_tx.send(Ok(Event::default().data(payload.to_string())));
                let _ = event_tx.send(Ok(Event::default().data("[DONE]")));
            }
        }

        generation_service
            .complete_stream(&generation_stream_id)
            .await;
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
    Json(request): Json<CancelRequest>,
) -> Result<Json<Value>, ServerError> {
    validate_stream_id(&request.stream_id)?;
    let cancelled = state
        .services
        .chat_completion_service
        .cancel_stream(&request.stream_id)
        .await;

    tracing::info!(stream_id = %request.stream_id, cancelled, "Chat completion stream cancel requested");
    Ok(Json(json!({ "ok": true, "cancelled": cancelled })))
}

pub async fn cancel_generation(
    State(state): State<SharedState>,
    Json(request): Json<CancelRequest>,
) -> Result<Json<Value>, ServerError> {
    validate_stream_id(&request.stream_id)?;
    let cancelled = state
        .services
        .chat_completion_service
        .cancel_generation(&request.stream_id)
        .await;

    Ok(Json(json!({ "ok": true, "cancelled": cancelled })))
}

/// Re-attaches to a background generation after the browser lost its socket.
pub async fn resume(Json(request): Json<ResumeRequest>) -> Result<Response, ServerError> {
    validate_stream_id(&request.stream_id)?;
    let job = crate::background::get(&request.stream_id).ok_or_else(|| {
        ServerError::NotFound("Background generation expired or the server restarted".into())
    })?;
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

/// Runs one provider call detached from any HTTP connection. Cancellation
/// comes only from the existing cancel endpoints (the Stop button).
fn spawn_background(
    service: Arc<ChatCompletionService>,
    id: String,
    job: Arc<crate::background::Job>,
    dto: tt_application::dto::chat_completion_dto::ChatCompletionGenerateRequestDto,
) {
    tokio::spawn(async move {
        if job.stream {
            let cancel = service.register_stream(&id).await;
            let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<String>();
            let forward = {
                let job = job.clone();
                tokio::spawn(async move {
                    while let Some(chunk) = receiver.recv().await {
                        if !chunk.is_empty() {
                            job.push(chunk);
                        }
                    }
                })
            };
            let outcome = service.generate_stream(dto, sender, cancel).await;
            let _ = forward.await;
            if let Err(error) = outcome {
                tracing::warn!(stream_id = %id, %error, "Background chat completion stream failed");
                job.push(json!({ "error": { "message": error.to_string() } }).to_string());
            }
            job.push("[DONE]".into());
            service.complete_stream(&id).await;
        } else {
            let cancel = service.register_generation(&id).await;
            let body = match service.generate_with_cancel(dto, cancel).await {
                Ok(value) => value,
                Err(error) => json!({ "error": { "message": error.to_string() } }),
            };
            job.push(body.to_string());
            service.complete_generation(&id).await;
        }
        job.finish();
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
            if service.cancel_stream(&stream_id).await {
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
    use rand::RngCore;
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
