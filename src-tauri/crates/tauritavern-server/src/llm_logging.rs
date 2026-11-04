//! Foreground server logs intentionally include complete prompts and responses.
//! The same redacted exchanges feed the `/llmlog` viewer through `dev_logs`.
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use serde_json::Value;
use tt_application::dto::dev_observability_dto::{
    LlmApiLogPreviewDto, LlmApiLogRawDto, LlmApiRawKindDto,
};
use tt_domain::errors::DomainError;
use tt_ports::repositories::chat_completion_repository::{
    CHAT_COMPLETION_PROVIDER_STATE_FIELD, ChatCompletionApiConfig, ChatCompletionCancelReceiver,
    ChatCompletionRepository, ChatCompletionRepositoryGenerateResponse, ChatCompletionSource,
    ChatCompletionStreamSender, ChatCompletionStreamDelta,
};

use crate::dev_logs::{LlmApiLogEntry, LlmApiLogStore};

pub struct ConsoleChatCompletionRepository {
    inner: Arc<dyn ChatCompletionRepository>,
    logs: Arc<LlmApiLogStore>,
}

impl ConsoleChatCompletionRepository {
    pub fn new(inner: Arc<dyn ChatCompletionRepository>, logs: Arc<LlmApiLogStore>) -> Self {
        Self { inner, logs }
    }
}

/// One exchange as the `/llmlog` viewer shows it.
struct Exchange<'a> {
    source: ChatCompletionSource,
    config: &'a ChatCompletionApiConfig,
    endpoint_path: &'a str,
    payload: &'a Value,
    started: Instant,
    started_at_ms: i64,
    stream: bool,
}

impl<'a> Exchange<'a> {
    fn start(
        source: ChatCompletionSource,
        config: &'a ChatCompletionApiConfig,
        endpoint_path: &'a str,
        payload: &'a Value,
        stream: bool,
    ) -> Self {
        Self {
            source,
            config,
            endpoint_path,
            payload,
            started: Instant::now(),
            started_at_ms: chrono::Utc::now().timestamp_millis(),
            stream,
        }
    }

    /// `response` is the redacted body and its wire kind; `Err` carries the failure.
    fn into_entry(self, response: Result<(String, LlmApiRawKindDto), &DomainError>) -> LlmApiLogEntry {
        let request = pretty(self.payload, Some(self.config));
        let (ok, level, error_message, response_raw, response_raw_kind) = match response {
            Ok((body, kind)) => (true, "INFO", None, body, Some(kind)),
            Err(error) => {
                let level = if matches!(error, DomainError::Cancelled(_)) { "WARN" } else { "ERROR" };
                (false, level, Some(redacted_error(error, self.config)), String::new(), None)
            }
        };
        let preview = LlmApiLogPreviewDto {
            id: 0,
            timestamp_ms: self.started_at_ms,
            level: level.to_string(),
            ok,
            source: self.source.key().to_string(),
            model: self.payload.get("model").and_then(Value::as_str).map(str::to_string),
            endpoint: endpoint(self.config, self.endpoint_path),
            duration_ms: self.started.elapsed().as_millis().min(u128::from(u32::MAX)) as u32,
            stream: self.stream,
            response_readable: error_message.clone().unwrap_or_else(|| response_raw.clone()),
            error_message,
            request_readable: request.clone(),
            response_raw_kind,
        };
        LlmApiLogEntry {
            raw: LlmApiLogRawDto { id: 0, request_raw: request, response_raw, response_raw_kind },
            preview,
        }
    }
}

// Config values also redact credentials echoed outside credential-named fields.
fn redact(value: &mut Value, config: Option<&ChatCompletionApiConfig>) {
    match value {
        Value::Object(object) => {
            object.remove(CHAT_COMPLETION_PROVIDER_STATE_FIELD);
            for (key, value) in object {
                if matches!(
                    key.to_ascii_lowercase().replace('-', "_").as_str(),
                    "api_key"
                        | "apikey"
                        | "authorization"
                        | "authorization_header"
                        | "proxy_password"
                        | "password"
                        | "secret"
                        | "access_token"
                        | "x_api_key"
                        | "x_goog_api_key"
                        | "private_key"
                        | "vertexai_service_account_json"
                        | "custom_include_headers"
                ) {
                    *value = Value::String("[REDACTED]".into());
                } else {
                    redact(value, config);
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                redact(value, config);
            }
        }
        Value::String(text) => {
            let Some(config) = config else { return };
            for secret in std::iter::once(config.api_key.as_str())
                .chain(config.authorization_header.as_deref())
                .chain(
                    config
                        .authorization_header
                        .as_deref()
                        .and_then(|header| header.split_once(' ').map(|(_, token)| token)),
                )
                .chain(config.vertexai_service_account_json.as_deref())
                .chain(config.extra_headers.values().map(String::as_str))
                .chain(config.additional_headers.values().map(String::as_str))
            {
                if !secret.is_empty() && text.contains(secret) {
                    *text = text.replace(secret, "[REDACTED]");
                }
            }
        }
        _ => {}
    }
}

pub(crate) fn pretty(value: &Value, config: Option<&ChatCompletionApiConfig>) -> String {
    let mut value = value.clone();
    redact(&mut value, config);
    serde_json::to_string_pretty(&value).expect("JSON value serialization")
}

/// Provider URL without userinfo, query, or fragment: they may carry secrets.
fn endpoint(config: &ChatCompletionApiConfig, endpoint_path: &str) -> String {
    let base = config.base_url.split(['?', '#']).next().unwrap_or_default();
    let base = match base.split_once("://") {
        Some((scheme, rest)) => {
            format!("{scheme}://{}", rest.rsplit('@').next().unwrap_or_default())
        }
        None => "[invalid endpoint]".into(),
    };
    let url = format!("{}{}", base.trim_end_matches('/'),
        endpoint_path.split(['?', '#']).next().unwrap_or_default());
    let mut url = Value::String(url);
    redact(&mut url, Some(config));
    url.as_str().unwrap_or_default().to_string()
}

fn redacted_error(error: &DomainError, config: &ChatCompletionApiConfig) -> String {
    let mut error = Value::String(error.to_string().replace(&config.base_url, "[provider endpoint]"));
    redact(&mut error, Some(config));
    error.as_str().unwrap_or_default().to_string()
}

fn log_request(
    source: ChatCompletionSource,
    config: &ChatCompletionApiConfig,
    endpoint_path: &str,
    payload: &Value,
) {
    if !tracing::enabled!(target: "tauritavern_server::llm", tracing::Level::INFO) {
        return;
    }
    tracing::info!(target: "tauritavern_server::llm", source = source.key(),
        model = %pretty(payload.get("model").unwrap_or(&serde_json::Value::Null), Some(config)),
        "Chat Completion request: {}\n{}", endpoint(config, endpoint_path), pretty(payload, Some(config)));
}

fn log_result<T>(
    result: &Result<T, DomainError>,
    started: Instant,
    config: &ChatCompletionApiConfig,
    chunks: Option<usize>,
) {
    match result {
        Ok(_) => {
            tracing::info!(target: "tauritavern_server::llm", duration_ms = started.elapsed().as_millis() as u64, chunks,
            "Chat Completion completed")
        }
        Err(error) => {
            tracing::error!(target: "tauritavern_server::llm", duration_ms = started.elapsed().as_millis() as u64, chunks,
            "Chat Completion failed: {}", redacted_error(error, config))
        }
    }
}

impl ConsoleChatCompletionRepository {
    fn finish_json(
        &self,
        exchange: Exchange<'_>,
        result: &Result<ChatCompletionRepositoryGenerateResponse, DomainError>,
    ) {
        let response = match result {
            Ok(response) => {
                let body = pretty(&response.body, Some(exchange.config));
                tracing::info!(target: "tauritavern_server::llm", "Chat Completion response:\n{body}");
                Ok((body, LlmApiRawKindDto::Json))
            }
            Err(error) => Err(error),
        };
        log_result(result, exchange.started, exchange.config, None);
        self.logs.record(exchange.into_entry(response));
    }
}

#[async_trait]
impl ChatCompletionRepository for ConsoleChatCompletionRepository {
    async fn list_models(
        &self,
        source: ChatCompletionSource,
        config: &ChatCompletionApiConfig,
    ) -> Result<Value, DomainError> {
        self.inner.list_models(source, config).await
    }

    async fn generate(
        &self,
        source: ChatCompletionSource,
        config: &ChatCompletionApiConfig,
        endpoint_path: &str,
        payload: &Value,
    ) -> Result<ChatCompletionRepositoryGenerateResponse, DomainError> {
        let exchange = Exchange::start(source, config, endpoint_path, payload, false);
        log_request(source, config, endpoint_path, payload);
        let result = self
            .inner
            .generate(source, config, endpoint_path, payload)
            .await;
        self.finish_json(exchange, &result);
        result
    }

    async fn generate_stream(
        &self,
        source: ChatCompletionSource,
        config: &ChatCompletionApiConfig,
        endpoint_path: &str,
        payload: &Value,
        sender: ChatCompletionStreamSender,
        cancel: ChatCompletionCancelReceiver,
    ) -> Result<(), DomainError> {
        let exchange = Exchange::start(source, config, endpoint_path, payload, true);
        log_request(source, config, endpoint_path, payload);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let forward = async move {
            let mut chunks = 0;
            let mut sse = String::new();
            while let Some(chunk) = rx.recv().await {
                chunks += 1;
                sse.push_str(&chunk);
                sse.push('\n');
                let _ = sender.send(chunk);
            }
            (chunks, sse)
        };
        let (result, (chunks, sse)) = tokio::join!(
            self.inner
                .generate_stream(source, config, endpoint_path, payload, tx, cancel),
            forward
        );
        log_result(&result, exchange.started, config, Some(chunks));
        let response = match &result {
            Ok(()) => {
                let mut sse = Value::String(sse);
                redact(&mut sse, Some(config));
                Ok((sse.as_str().unwrap_or_default().to_string(), LlmApiRawKindDto::Sse))
            }
            Err(error) => Err(error),
        };
        self.logs.record(exchange.into_entry(response));
        result
    }

    async fn generate_with_deltas(
        &self,
        source: ChatCompletionSource,
        config: &ChatCompletionApiConfig,
        endpoint_path: &str,
        payload: &Value,
        on_delta: &mut (dyn FnMut(ChatCompletionStreamDelta) + Send),
    ) -> Result<ChatCompletionRepositoryGenerateResponse, DomainError> {
        let exchange = Exchange::start(source, config, endpoint_path, payload, true);
        log_request(source, config, endpoint_path, payload);
        let result = self.inner.generate_with_deltas(source, config, endpoint_path, payload, on_delta).await;
        self.finish_json(exchange, &result);
        result
    }

    async fn close_provider_session(&self, session_id: &str) {
        self.inner.close_provider_session(session_id).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn credentials_are_redacted_without_truncating_prompts() {
        let config = ChatCompletionApiConfig {
            base_url: "http://localhost".into(),
            user_configured_endpoint: false,
            api_key: "secret-key".into(),
            authorization_header: Some("Bearer private-token".into()),
            vertexai_service_account_json: None,
            extra_headers: Default::default(),
            additional_headers: Default::default(),
            anthropic_beta_header_mode: Default::default(),
            aws_bedrock_custom_response_path: None,
            aws_bedrock_custom_stream_path: None,
        };
        let prompt = "hello".repeat(10000);
        let value = serde_json::json!({"messages":[{"content":prompt}], "api_key":"other-secret", "nested":{"Authorization":"Bearer other"}, "echo":"secret-key", "_tauritavern_provider_state":{"secret":"state"}});
        let output = pretty(&value, Some(&config));
        assert!(output.contains(&prompt));
        for secret in ["other-secret", "Bearer other", "secret-key", "state"] {
            assert!(!output.contains(secret));
        }
    }

    #[test]
    fn rejected_browser_request_redacts_credentials_without_truncating_prompt() {
        let prompt = "full browser prompt".repeat(10000);
        let payload = serde_json::json!({
            "chat_completion_source": "custom",
            "custom_api_format": "gemini_generate_content",
            "messages": [{"role": "user", "content": prompt}],
            "proxy_password": "SECRET123",
            "custom_include_headers": "Authorization: Bearer private-header\nx-api-key: private-key",
        });
        let output = pretty(&payload, None);
        assert!(output.contains(&prompt));
        assert!(output.contains("gemini_generate_content"));
        assert!(output.contains("[REDACTED]"));
        for secret in ["SECRET123", "private-header", "private-key"] {
            assert!(!output.contains(secret));
        }
    }
}
