//! Chat read and save endpoints.
//!
//! Upstream SillyTavern answers `/api/chats/get` with the whole JSONL payload as
//! a JSON array and `/api/chats/save` with the whole array back
//! (`src/endpoints/chats.js`). Third-party extensions read that array directly
//! (`data[0].chat_metadata`), so the read body keeps exactly that shape. The
//! `version` token that lets a save prove it is replacing the revision it
//! loaded travels in the `CHAT_VERSION_HEADER` response header instead; saves
//! send it back in the request body.

use axum::Json;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tt_application::dto::chat_history_dto::{ChatHistoryLocator, CurrentCommitReason};
use tt_ports::repositories::chat_payload_commit_repository::{
    ChatPayloadPrecondition, ChatPayloadVersion,
};

use crate::error::ServerError;
use crate::state::SharedState;

/// Response header carrying the JSON-encoded `VersionToken` of a chat read.
pub const CHAT_VERSION_HEADER: &str = "x-tauritavern-chat-version";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionToken {
    pub byte_len: u64,
    pub sha256: String,
}

impl From<ChatPayloadVersion> for VersionToken {
    fn from(value: ChatPayloadVersion) -> Self {
        Self {
            byte_len: value.byte_len,
            sha256: value.sha256_hex,
        }
    }
}

impl From<VersionToken> for ChatPayloadVersion {
    fn from(value: VersionToken) -> Self {
        Self {
            byte_len: value.byte_len,
            sha256_hex: value.sha256,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct GetChatRequest {
    #[serde(default)]
    pub ch_name: Option<String>,
    #[serde(default)]
    pub avatar_url: Option<String>,
    #[serde(default, alias = "file")]
    pub file_name: Option<String>,
    #[serde(default)]
    pub allow_not_found: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SaveChatRequest {
    #[serde(default)]
    pub ch_name: Option<String>,
    #[serde(default)]
    pub avatar_url: Option<String>,
    #[serde(default, alias = "file")]
    pub file_name: Option<String>,
    pub chat: Vec<Value>,
    /// Set by the frontend only after the user confirms an overwrite.
    #[serde(default)]
    pub force: bool,
    /// Revision this client loaded. Absent for a chat it just created.
    #[serde(default)]
    pub version: Option<VersionToken>,
    /// True when the client believes no file exists yet.
    #[serde(default)]
    pub is_new: bool,
}

#[derive(Debug, Deserialize)]
pub struct GetGroupChatRequest {
    pub id: String,
    #[serde(default)]
    pub allow_not_found: bool,
}

#[derive(Debug, Deserialize)]
pub struct SaveGroupChatRequest {
    pub id: String,
    pub chat: Vec<Value>,
    #[serde(default)]
    pub force: bool,
    #[serde(default)]
    pub version: Option<VersionToken>,
    #[serde(default)]
    pub is_new: bool,
}

/// Resolves the on-disk chat directory key the way the frontend does.
///
/// Chat folders are keyed by the avatar filename stem, falling back to the
/// character name (`src/scripts/tauri/chat/transport.js:26-32`).
fn directory_id(ch_name: Option<&str>, avatar_url: Option<&str>) -> Result<String, ServerError> {
    if let Some(avatar) = avatar_url.map(str::trim).filter(|value| !value.is_empty()) {
        let stem = avatar.rsplit('/').next().unwrap_or(avatar);
        let stem = stem.strip_suffix(".png").unwrap_or(stem);
        if !stem.is_empty() {
            return Ok(stem.to_string());
        }
    }

    ch_name
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| ServerError::BadRequest("Chat request is missing a character".into()))
}

fn strip_jsonl(file_name: Option<&str>) -> Result<String, ServerError> {
    let value = file_name
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ServerError::BadRequest("Chat request is missing a file name".into()))?;

    Ok(value.strip_suffix(".jsonl").unwrap_or(value).to_string())
}

fn locator(character_id: String, file_name: String) -> ChatHistoryLocator {
    ChatHistoryLocator::Character {
        character_id,
        file_name,
    }
}

pub async fn get_chat(
    State(state): State<SharedState>,
    Json(request): Json<GetChatRequest>,
) -> Result<Response, ServerError> {
    let character_id = directory_id(request.ch_name.as_deref(), request.avatar_url.as_deref())?;
    let file_name = strip_jsonl(request.file_name.as_deref())?;

    let payload = state
        .services
        .chat_service
        .get_chat_payload_bytes(&character_id, &file_name)
        .await
        .map_err(ServerError::from);
    payload_response(payload, request.allow_not_found)
}

pub async fn get_group_chat(
    State(state): State<SharedState>,
    Json(request): Json<GetGroupChatRequest>,
) -> Result<Response, ServerError> {
    let chat_id = strip_jsonl(Some(&request.id))?;
    let payload = state
        .services
        .group_chat_service
        .get_group_chat_payload_bytes(&chat_id)
        .await
        .map_err(ServerError::from);
    payload_response(payload, request.allow_not_found)
}

/// Upstream's bare JSON array body, with the revision token of exactly these
/// bytes in `CHAT_VERSION_HEADER`.
fn payload_response(
    payload: Result<Vec<u8>, ServerError>,
    allow_not_found: bool,
) -> Result<Response, ServerError> {
    let payload = match payload {
        Ok(bytes) => bytes,
        Err(ServerError::NotFound(_)) if allow_not_found => {
            return Ok(Json(json!([])).into_response());
        }
        Err(error) => return Err(error),
    };

    let mut entries = Vec::new();
    for line in payload.split(|byte| *byte == b'\n') {
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        entries.push(serde_json::from_slice::<Value>(line).map_err(|error| {
            ServerError::Internal(format!("Chat payload line is not valid JSON: {error}"))
        })?);
    }

    let version = VersionToken {
        byte_len: payload.len() as u64,
        sha256: sha256_hex(&payload),
    };
    let version = serde_json::to_string(&version).map_err(|error| {
        ServerError::Internal(format!("Failed to encode chat version: {error}"))
    })?;

    Ok((
        [(CHAT_VERSION_HEADER, version)],
        Json(Value::Array(entries)),
    )
        .into_response())
}

pub async fn save_chat(
    State(state): State<SharedState>,
    Json(request): Json<SaveChatRequest>,
) -> Result<Response, ServerError> {
    let character_id = directory_id(request.ch_name.as_deref(), request.avatar_url.as_deref())?;
    let file_name = strip_jsonl(request.file_name.as_deref())?;
    let precondition = precondition(request.force, request.version, request.is_new)?;
    commit_payload(
        &state,
        locator(character_id, file_name),
        precondition,
        &request.chat,
    )
    .await
}

pub async fn save_group_chat(
    State(state): State<SharedState>,
    Json(request): Json<SaveGroupChatRequest>,
) -> Result<Response, ServerError> {
    let chat_id = strip_jsonl(Some(&request.id))?;
    let precondition = precondition(request.force, request.version, request.is_new)?;
    commit_payload(
        &state,
        ChatHistoryLocator::Group { chat_id },
        precondition,
        &request.chat,
    )
    .await
}

fn precondition(
    force: bool,
    version: Option<VersionToken>,
    is_new: bool,
) -> Result<ChatPayloadPrecondition, ServerError> {
    match (force, version, is_new) {
        // The user explicitly confirmed an overwrite.
        (true, _, _) => Ok(ChatPayloadPrecondition::Overwrite),
        (false, Some(version), _) => Ok(ChatPayloadPrecondition::MatchesVersion(version.into())),
        (false, None, true) => Ok(ChatPayloadPrecondition::MustNotExist),
        // A save for an existing chat with no version token cannot prove which
        // revision it is replacing. Refusing here is the whole point of the
        // check: accepting it would let a stale tab clobber newer content.
        (false, None, false) => Err(ServerError::Conflict(
            "integrity: this page did not load a chat revision; reload before saving".into(),
        )),
    }
}

async fn commit_payload(
    state: &SharedState,
    locator: ChatHistoryLocator,
    precondition: ChatPayloadPrecondition,
    chat: &[Value],
) -> Result<Response, ServerError> {
    if chat.is_empty() {
        return Err(ServerError::BadRequest("Chat payload is empty".into()));
    }

    let commit = state.services.chat_payload_commit_service.clone();

    let session = commit.begin(locator, precondition).await?;
    let mut bytes = Vec::new();
    for (index, entry) in chat.iter().enumerate() {
        if index > 0 {
            bytes.push(b'\n');
        }
        serde_json::to_writer(&mut bytes, entry)
            .map_err(|error| ServerError::Internal(format!("Failed to encode chat: {error}")))?;
    }

    let result = async {
        let mut offset = 0u64;
        for frame in bytes.chunks(session.max_frame_bytes as usize) {
            offset = commit.append(&session.session_id, offset, frame).await?;
        }
        commit
            .finish(&session.session_id, offset, CurrentCommitReason::Mutation)
            .await
    }
    .await;

    match result {
        Ok((_size, version)) => Ok(Json(json!({
            "ok": true,
            "version": VersionToken::from(version),
        }))
        .into_response()),
        Err(error) => {
            // Leaving a staged file behind would block the next save.
            let _ = commit.abort(&session.session_id).await;

            let server_error = ServerError::from(error);
            if server_error.is_integrity_conflict() {
                // Exactly upstream's shape: the frontend keys its overwrite
                // prompt off `{"error":"integrity"}` with status 400.
                return Ok((
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": "integrity" })),
                )
                    .into_response());
            }
            Err(server_error)
        }
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest: [u8; 32] = hasher.finalize().into();
    let mut out = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_id_prefers_the_avatar_stem() {
        assert_eq!(
            directory_id(Some("Display Name"), Some("alice.png")).expect("id"),
            "alice"
        );
        assert_eq!(
            directory_id(Some("Display Name"), Some("")).expect("id"),
            "Display Name"
        );
    }

    #[test]
    fn file_names_lose_the_jsonl_suffix() {
        assert_eq!(strip_jsonl(Some("chat.jsonl")).expect("name"), "chat");
        assert_eq!(strip_jsonl(Some("chat")).expect("name"), "chat");
        assert!(strip_jsonl(Some("  ")).is_err());
    }
}
