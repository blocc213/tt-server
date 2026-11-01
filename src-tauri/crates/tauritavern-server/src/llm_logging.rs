//! Foreground server logs intentionally include complete prompts and responses.
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use serde_json::Value;
use tt_domain::errors::DomainError;
use tt_ports::repositories::chat_completion_repository::{
    CHAT_COMPLETION_PROVIDER_STATE_FIELD, ChatCompletionApiConfig, ChatCompletionCancelReceiver,
    ChatCompletionRepository, ChatCompletionRepositoryGenerateResponse, ChatCompletionSource,
    ChatCompletionStreamSender, ChatCompletionStreamDelta,
};

pub struct ConsoleChatCompletionRepository {
    inner: Arc<dyn ChatCompletionRepository>,
}

impl ConsoleChatCompletionRepository {
    pub fn new(inner: Arc<dyn ChatCompletionRepository>) -> Self {
        Self { inner }
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

fn log_request(
    source: ChatCompletionSource,
    config: &ChatCompletionApiConfig,
    endpoint: &str,
    payload: &Value,
) {
    if !tracing::enabled!(target: "tauritavern_server::llm", tracing::Level::INFO) {
        return;
    }
    // URL credentials and query strings may contain secrets; never print them.
    let base = config.base_url.split(['?', '#']).next().unwrap_or_default();
    let base = match base.split_once("://") {
        Some((scheme, rest)) => {
            format!("{scheme}://{}", rest.rsplit('@').next().unwrap_or_default())
        }
        None => "[invalid endpoint]".into(),
    };
    tracing::info!(target: "tauritavern_server::llm", source = source.key(),
        model = %pretty(payload.get("model").unwrap_or(&serde_json::Value::Null), Some(config)),
        "Chat Completion request: {}\n{}", pretty(&serde_json::Value::String(format!("{}{}", base.trim_end_matches('/'),
        endpoint.split(['?', '#']).next().unwrap_or_default())), Some(config)), pretty(payload, Some(config)));
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
            let error = error
                .to_string()
                .replace(&config.base_url, "[provider endpoint]");
            tracing::error!(target: "tauritavern_server::llm", duration_ms = started.elapsed().as_millis() as u64, chunks,
            "Chat Completion failed: {}", pretty(&serde_json::Value::String(error), Some(config)))
        }
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
        let started = Instant::now();
        log_request(source, config, endpoint_path, payload);
        let result = self
            .inner
            .generate(source, config, endpoint_path, payload)
            .await;
        if tracing::enabled!(target: "tauritavern_server::llm", tracing::Level::INFO)
            && let Ok(response) = &result
        {
            tracing::info!(target: "tauritavern_server::llm", "Chat Completion response:\n{}", pretty(&response.body, Some(config)));
        }
        log_result(&result, started, config, None);
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
        let started = Instant::now();
        log_request(source, config, endpoint_path, payload);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let forward = async move {
            let mut chunks = 0;
            while let Some(chunk) = rx.recv().await {
                chunks += 1;
                let _ = sender.send(chunk);
            }
            chunks
        };
        let (result, chunks) = tokio::join!(
            self.inner
                .generate_stream(source, config, endpoint_path, payload, tx, cancel),
            forward
        );
        log_result(&result, started, config, Some(chunks));
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
        let started = Instant::now();
        log_request(source, config, endpoint_path, payload);
        let result = self.inner.generate_with_deltas(source, config, endpoint_path, payload, on_delta).await;
        if tracing::enabled!(target: "tauritavern_server::llm", tracing::Level::INFO)
            && let Ok(response) = &result
        {
            tracing::info!(target: "tauritavern_server::llm", "Chat Completion response:\n{}", pretty(&response.body, Some(config)));
        }
        log_result(&result, started, config, None);
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
