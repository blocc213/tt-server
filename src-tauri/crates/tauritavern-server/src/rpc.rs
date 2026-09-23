//! Command dispatch over HTTP.
//!
//! Every command the browser may reach is listed here explicitly. Native-only
//! commands (iOS pickers, share sheets, barcode scanning, LAN Sync pairing,
//! archive delivery to a local Downloads folder, window/tray control) are absent
//! by construction rather than filtered at runtime: an unknown name is a 404,
//! and the frontend surfaces that as an unavailable feature.
//!
//! Handlers mirror `crates/tauritavern/src/presentation/commands/*` one-to-one.
//! Keep argument names in camelCase: the frontend's invoke transport already
//! normalizes to that form (`src/tauri/main/services/invokes/invoke-service.js`).

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use serde::Deserialize;

use std::sync::Arc;

use serde_json::{Value, json};

use tt_application::dto::agent_dto;
use tt_application::dto::llm_connection_dto::{LlmConnectionIdDto, SaveLlmConnectionDto};
use tt_application::services::agent_workspace_lifecycle_service::AgentChatWorkspaceTarget;
use tt_ports::repositories::agent_workspace_lifecycle_repository::AgentPersistentStatePruneRequest;

use crate::error::ServerError;
use crate::state::AppState;

type Handled = Result<Value, ServerError>;

#[derive(Debug, Deserialize)]
struct StageUploadBeginDto {
    kind: String,
    preferred_extension: String,
    size: u64,
}

fn arg<T: serde::de::DeserializeOwned>(args: &Value, key: &str) -> Result<T, ServerError> {
    let value = args
        .get(key)
        .ok_or_else(|| ServerError::BadRequest(format!("Missing argument `{key}`")))?;
    serde_json::from_value(value.clone())
        .map_err(|error| ServerError::BadRequest(format!("Invalid argument `{key}`: {error}")))
}

fn opt_arg<T: serde::de::DeserializeOwned>(
    args: &Value,
    key: &str,
) -> Result<Option<T>, ServerError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => serde_json::from_value(value.clone())
            .map(Some)
            .map_err(|error| ServerError::BadRequest(format!("Invalid argument `{key}`: {error}"))),
    }
}

fn ok<T: serde::Serialize>(value: T) -> Handled {
    serde_json::to_value(value)
        .map_err(|error| ServerError::Internal(format!("Failed to serialize response: {error}")))
}

/// Every command name the browser may invoke.
const EXPOSED_COMMANDS: &[&str] = &[
    "is_ready",
    "wait_for_backend_ready",
    "backend_error_bridge_ready",
    "get_version",
    "get_client_version",
    "get_bootstrap_snapshot",
    "get_tauritavern_settings",
    "get_all_characters",
    "get_character",
    "get_character_chats_by_id",
    "get_character_chats",
    "update_character",
    "update_character_card_data",
    "merge_character_card_data",
    "bulk_merge_character_card_data",
    "delete_character",
    "rename_character",
    "duplicate_character",
    "stage_upload_begin",
    "stage_upload_finish",
    "stage_upload_discard",
    "create_character",
    "create_character_with_avatar",
    "import_character",
    "replace_character",
    "update_avatar",
    "export_character_content",
    "count_openai_tokens",
    "count_openai_tokens_batch",
    "count_openai_token_prefixes",
    "encode_openai_tokens",
    "decode_openai_tokens",
    "build_openai_logit_bias",
    "apply_native_regex_batch",
    "get_all_backgrounds",
    "get_all_background_metadata",
    "delete_background",
    "rename_background",
    "upload_background_from_path",
    "get_background_folders",
    "create_image_metadata_folder",
    "update_image_metadata_folder",
    "delete_image_metadata_folder",
    "set_image_metadata_folder_thumbnails",
    "assign_images_to_metadata_folder",
    "unassign_images_from_metadata_folder",
    "get_extensions",
    "install_extension",
    "update_extension",
    "delete_extension",
    "get_extension_version",
    "get_extension_branches",
    "switch_extension_branch",
    "move_extension",
    "download_external_import_url",
    "get_extension_store_json",
    "try_get_extension_store_json",
    "set_extension_store_json",
    "update_extension_store_json",
    "rename_extension_store_key",
    "delete_extension_store_json",
    "list_extension_store_keys",
    "list_extension_store_tables",
    "delete_extension_store_table",
    "get_extension_store_blob",
    "set_extension_store_blob",
    "delete_extension_store_blob",
    "list_extension_store_blob_keys",
    "devlog_append_frontend_logs",
    "get_sillytavern_settings",
    "save_user_settings",
    "save_user_settings_patch",
    "chat_history_generation_started",
    "chat_history_generation_finished",
    "create_settings_snapshot",
    "get_settings_snapshots",
    "load_settings_snapshot",
    "restore_settings_snapshot",
    "get_character_chat_metadata",
    "list_chat_summaries",
    "list_recent_chat_summaries",
    "search_chats",
    "rename_chat",
    "delete_chat",
    "get_world_infos_batch",
    "get_avatars",
    "get_all_groups",
    "save_world_info",
    "save_preset",
    "save_theme",
    "delete_world_info",
    "delete_theme",
    "delete_preset",
    "read_secret_state",
    "read_secret_settings",
    "write_secret",
    "find_secret",
    "view_secrets",
    "delete_secret",
    "rotate_secret",
    "rename_secret",
    "update_tauritavern_settings",
    "save_quick_reply_set",
    "delete_quick_reply_set",
    "list_llm_connections",
    "load_llm_connection",
    "save_llm_connection",
    "delete_llm_connection",
    "start_agent_run",
    "prepare_agent_prompt_assembly",
    "build_agent_current_model_connection_snapshot",
    "apply_agent_current_model_connection_snapshot",
    "list_agent_profiles",
    "list_agent_tools",
    "resolve_agent_system_prompt",
    "load_agent_profile",
    "diagnose_agent_profile",
    "save_agent_profile",
    "delete_agent_profile",
    "repair_agent_profile_file",
    "retarget_agent_profile_preset_refs",
    "cancel_agent_run",
    "submit_agent_run_guidance",
    "list_agent_runs",
    "plan_agent_run_prune",
    "apply_agent_run_prune",
    "read_agent_run_events",
    "read_agent_workspace_file",
    "read_agent_model_turn",
    "read_agent_prompt_assembly_request",
    "resolve_agent_chat_commit",
    "resolve_agent_prompt_assembly",
    "resolve_agent_persistent_state_metadata_update",
    "prune_agent_chat_persistent_states",
    "download_skill_import_url",
    "list_skills",
    "list_skill_files",
    "preview_skill_import",
    "install_skill_import",
    "read_skill_file",
    "write_skill_file",
    "export_skill",
    "delete_skill",
    "move_skill",
    "retarget_skill_scope",
];

/// Rejects any filesystem path the browser did not get from upload staging.
///
/// Import/avatar/background commands take a server path because the Tauri host
/// hands them native file-picker paths. Over HTTP the only legitimate source is
/// `stage_upload_finish`; anything else would let a client import, copy or
/// delete an arbitrary server file (`/etc/passwd`, `default-user/secrets.json`).
fn staged(state: &AppState, path: &str) -> Result<(), ServerError> {
    state.upload_staging.validate_path(path).map(|_| ())
}

fn staged_field(state: &AppState, dto: &Value, key: &str) -> Result<(), ServerError> {
    match dto.get(key) {
        None | Some(Value::Null) => Ok(()),
        Some(Value::String(path)) => staged(state, path),
        Some(_) => Err(ServerError::BadRequest(format!("Invalid `{key}`"))),
    }
}

/// Skill imports from the browser must carry their bytes (`inlineFiles`,
/// `archiveBase64`) or a staged archive; `directory` would copy any server
/// directory into a readable skill.
fn staged_skill_input(
    state: &AppState,
    input: &tt_domain::models::skill::SkillImportInput,
) -> Result<(), ServerError> {
    use tt_domain::models::skill::SkillImportInput;
    match input {
        SkillImportInput::InlineFiles { .. } | SkillImportInput::ArchiveBase64 { .. } => Ok(()),
        SkillImportInput::ArchiveFile { path, .. } => staged(state, path),
        SkillImportInput::Directory { .. } => Err(ServerError::BadRequest(
            "Directory Skill imports are not available in server mode".into(),
        )),
    }
}

/// Dispatches one command. `None` means "not exposed by the server host".
pub async fn dispatch(state: &Arc<AppState>, command: &str, args: Value) -> Option<Handled> {
    if !is_exposed(command) {
        return None;
    }

    Some(run(state, command, args).await)
}

/// Names reachable over HTTP. Kept next to the handlers so adding a handler
/// without listing it here simply leaves it unreachable, never half-exposed.
fn is_exposed(command: &str) -> bool {
    EXPOSED_COMMANDS.contains(&command)
}

async fn run(state: &Arc<AppState>, command: &str, args: Value) -> Handled {
    match command {
        // ---- readiness / bootstrap -------------------------------------------------
        "is_ready" => ok(true),
        "wait_for_backend_ready" => ok(Value::Null),
        // The Tauri host mirrors error-level tracing into a webview event stream.
        // The server reports failures in each HTTP response instead, so there is
        // never a queued backlog to drain.
        "backend_error_bridge_ready" => ok(Vec::<String>::new()),
        "get_version" => ok(crate::product::VERSION),
        "get_client_version" => ok(json!({
            "agent": format!(
                "SillyTavern:{}:TauriTavern",
                crate::product::SILLYTAVERN_COMPAT_VERSION
            ),
            "pkgVersion": crate::product::SILLYTAVERN_COMPAT_VERSION,
            "tauriVersion": crate::product::VERSION,
            "gitRevision": Value::Null,
            "gitBranch": Value::Null,
            "defaultUpdateChannel": "stable",
        })),
        "get_bootstrap_snapshot" => bootstrap_snapshot(state).await,
        "get_tauritavern_settings" => ok(state
            .services
            .settings_service
            .get_tauritavern_settings()
            .await?),

        // ---- characters ------------------------------------------------------------
        "get_all_characters" => ok(state
            .services
            .character_service
            .get_all_characters(arg(&args, "shallow").unwrap_or(false))
            .await?),
        "get_character" => ok(state
            .services
            .character_service
            .get_character(&arg::<String>(&args, "name")?)
            .await?),
        "get_character_chats_by_id" => ok(state
            .services
            .character_service
            .get_character_chats(arg(&args, "dto")?)
            .await?),
        "get_character_chats" => ok(state
            .services
            .chat_service
            .get_character_chats(&arg::<String>(&args, "characterName")?)
            .await?),
        "update_character" => ok(state
            .services
            .character_service
            .update_character(&arg::<String>(&args, "name")?, arg(&args, "dto")?)
            .await?),
        "update_character_card_data" => {
            let dto: Value = arg(&args, "dto")?;
            staged_field(state, &dto, "avatar_path")?;
            ok(state
                .services
                .character_service
                .update_character_card_data(&arg::<String>(&args, "name")?, serde_json::from_value(dto)?)
                .await?)
        }
        "merge_character_card_data" => ok(state
            .services
            .character_service
            .merge_character_card_data(&arg::<String>(&args, "name")?, arg(&args, "dto")?)
            .await?),
        "bulk_merge_character_card_data" => ok(state
            .services
            .character_service
            .bulk_merge_character_card_data(arg(&args, "dto")?)
            .await?),
        "delete_character" => ok(state
            .services
            .character_service
            .delete_character(arg(&args, "dto")?)
            .await?),
        "rename_character" => ok(state
            .services
            .character_service
            .rename_character(arg(&args, "dto")?)
            .await?),
        "duplicate_character" => ok(state
            .services
            .character_service
            .duplicate_character(arg(&args, "dto")?)
            .await?),

        // ---- upload staging ---------------------------------------------------------
        "stage_upload_begin" => {
            let dto: StageUploadBeginDto = arg(&args, "dto")?;
            ok(state
                .upload_staging
                .begin(&dto.kind, &dto.preferred_extension, dto.size)
                .await?)
        }
        "stage_upload_finish" => ok(json!({
            "file_path": state
                .upload_staging
                .finish(
                    &arg::<String>(&args, "filePath")?,
                    arg(&args, "expectedSize")?,
                )
                .await?,
        })),
        "stage_upload_discard" => ok(state
            .upload_staging
            .discard(&arg::<String>(&args, "filePath")?)
            .await?),

        // ---- character files --------------------------------------------------------
        "create_character" => ok(state
            .services
            .character_service
            .create_character(arg(&args, "dto")?)
            .await?),
        "create_character_with_avatar" => {
            let dto: Value = arg(&args, "dto")?;
            staged_field(state, &dto, "avatar_path")?;
            ok(state
                .services
                .character_service
                .create_with_avatar(serde_json::from_value(dto)?)
                .await?)
        }
        "import_character" => {
            let dto: Value = arg(&args, "dto")?;
            staged_field(state, &dto, "file_path")?;
            ok(state
                .services
                .character_service
                .import_character(serde_json::from_value(dto)?)
                .await?)
        }
        "replace_character" => {
            let dto: Value = arg(&args, "dto")?;
            staged_field(state, &dto, "file_path")?;
            ok(state
                .services
                .character_service
                .replace_character(serde_json::from_value(dto)?)
                .await?)
        }
        "update_avatar" => {
            let dto: Value = arg(&args, "dto")?;
            staged_field(state, &dto, "avatar_path")?;
            ok(state
                .services
                .character_service
                .update_avatar(serde_json::from_value(dto)?)
                .await?)
        }
        "export_character_content" => ok(state
            .services
            .character_service
            .export_character_content(arg(&args, "dto")?)
            .await?),

        // ---- prompt/token processing -------------------------------------------------
        "count_openai_tokens" => ok(state
            .services
            .tokenization_service
            .count_openai_tokens(arg(&args, "dto")?)
            .await?),
        "count_openai_tokens_batch" => ok(state
            .services
            .tokenization_service
            .count_openai_tokens_batch(arg(&args, "dto")?)
            .await?),
        "count_openai_token_prefixes" => ok(state
            .services
            .tokenization_service
            .count_openai_token_prefixes(arg(&args, "dto")?)
            .await?),
        "encode_openai_tokens" => ok(state
            .services
            .tokenization_service
            .encode_openai_tokens(arg(&args, "dto")?)
            .await?),
        "decode_openai_tokens" => ok(state
            .services
            .tokenization_service
            .decode_openai_tokens(arg(&args, "dto")?)
            .await?),
        "build_openai_logit_bias" => ok(state
            .services
            .tokenization_service
            .build_openai_logit_bias(arg(&args, "dto")?)
            .await?),
        "apply_native_regex_batch" => ok(state
            .services
            .native_regex_service
            .apply_batch(arg(&args, "dto")?)
            .await?),

        // ---- browser appearance ------------------------------------------------------
        "get_all_backgrounds" => ok(state
            .services
            .image_metadata_service
            .get_background_list_entries()
            .await?),
        "get_all_background_metadata" => ok(state
            .services
            .image_metadata_service
            .get_all_background_metadata(opt_arg::<String>(&args, "prefix")?.as_deref())
            .await?),
        "delete_background" => {
            let dto: tt_application::dto::background_dto::DeleteBackgroundDto = arg(&args, "dto")?;
            ok(state
                .services
                .background_service
                .delete_background(&dto.bg)
                .await?)
        }
        "rename_background" => {
            let dto: tt_application::dto::background_dto::RenameBackgroundDto = arg(&args, "dto")?;
            ok(state
                .services
                .background_service
                .rename_background(&dto.old_bg, &dto.new_bg)
                .await?)
        }
        "upload_background_from_path" => {
            let file_path = arg::<String>(&args, "filePath")?;
            staged(state, &file_path)?;
            ok(state
                .services
                .background_service
                .upload_background_from_path(&arg::<String>(&args, "filename")?, &file_path)
                .await?)
        }
        "get_background_folders" => ok(state
            .services
            .image_metadata_service
            .get_background_folders()
            .await?),
        "create_image_metadata_folder" => {
            let dto: tt_application::dto::image_metadata_dto::CreateImageMetadataFolderDto =
                arg(&args, "dto")?;
            ok(state
                .services
                .image_metadata_service
                .create_folder(dto)
                .await?)
        }
        "update_image_metadata_folder" => {
            let dto: tt_application::dto::image_metadata_dto::UpdateImageMetadataFolderDto =
                arg(&args, "dto")?;
            ok(state
                .services
                .image_metadata_service
                .update_folder(dto)
                .await?)
        }
        "delete_image_metadata_folder" => {
            let dto: tt_application::dto::image_metadata_dto::DeleteImageMetadataFolderDto =
                arg(&args, "dto")?;
            ok(state
                .services
                .image_metadata_service
                .delete_folder(dto)
                .await?)
        }
        "set_image_metadata_folder_thumbnails" => {
            let dto: tt_application::dto::image_metadata_dto::SetImageMetadataFolderThumbnailsDto =
                arg(&args, "dto")?;
            ok(state
                .services
                .image_metadata_service
                .set_folder_thumbnails(dto)
                .await?)
        }
        "assign_images_to_metadata_folder" => {
            let dto: tt_application::dto::image_metadata_dto::ImageMetadataFolderAssignmentDto =
                arg(&args, "dto")?;
            ok(state
                .services
                .image_metadata_service
                .assign_images_to_folder(dto)
                .await?)
        }
        "unassign_images_from_metadata_folder" => {
            let dto: tt_application::dto::image_metadata_dto::ImageMetadataFolderAssignmentDto =
                arg(&args, "dto")?;
            ok(state
                .services
                .image_metadata_service
                .unassign_images_from_folder(dto)
                .await?)
        }

        // ---- extension discovery / management ---------------------------------------
        // Management mirrors `crates/tauritavern/.../extension_commands.rs`. The
        // iOS capability gate there has no server analogue: the server never runs
        // under the iOS distribution policy (see `composition.rs`), so the
        // unrestricted baseline applies and the calls go straight to the service.
        "get_extensions" => ok(state.services.extension_service.get_extensions().await?),
        "install_extension" => ok(state
            .services
            .extension_service
            .install_extension(
                &arg::<String>(&args, "url")?,
                arg(&args, "global")?,
                opt_arg::<String>(&args, "branch")?,
            )
            .await?),
        "update_extension" => ok(state
            .services
            .extension_service
            .update_extension(
                &arg::<String>(&args, "extensionName")?,
                arg(&args, "global")?,
            )
            .await?),
        "delete_extension" => ok(state
            .services
            .extension_service
            .delete_extension(
                &arg::<String>(&args, "extensionName")?,
                arg(&args, "global")?,
            )
            .await?),
        "get_extension_version" => ok(state
            .services
            .extension_service
            .get_extension_version(
                &arg::<String>(&args, "extensionName")?,
                arg(&args, "global")?,
            )
            .await?),
        "get_extension_branches" => ok(state
            .services
            .extension_service
            .get_extension_branches(
                &arg::<String>(&args, "extensionName")?,
                arg(&args, "global")?,
            )
            .await?),
        "switch_extension_branch" => ok(state
            .services
            .extension_service
            .switch_extension_branch(
                &arg::<String>(&args, "extensionName")?,
                &arg::<String>(&args, "branch")?,
                arg(&args, "global")?,
            )
            .await?),
        "move_extension" => ok(state
            .services
            .extension_service
            .move_extension(
                &arg::<String>(&args, "extensionName")?,
                &arg::<String>(&args, "source")?,
                &arg::<String>(&args, "destination")?,
            )
            .await?),

        // ---- external content import -------------------------------------------------
        // The in-page router whitelists the host before calling this, then turns
        // the PNG bytes back into a Response the upstream import path consumes.
        "download_external_import_url" => ok(state
            .services
            .content_service
            .download_external_import_url(&arg::<String>(&args, "url")?)
            .await?),

        // ---- extension store ---------------------------------------------------------
        // `get_*` fails when a key is absent; `try_get_*` reports absence as
        // `{found: false}`. Extensions rely on that split to tell "never saved"
        // apart from "backend unavailable", so both are exposed verbatim.
        "get_extension_store_json" => ok(state
            .services
            .extension_store_service
            .get_json(
                &arg::<String>(&args, "namespace")?,
                opt_arg::<String>(&args, "table")?.as_deref(),
                &arg::<String>(&args, "key")?,
            )
            .await?),
        "try_get_extension_store_json" => {
            let value = state
                .services
                .extension_store_service
                .try_get_json(
                    &arg::<String>(&args, "namespace")?,
                    opt_arg::<String>(&args, "table")?.as_deref(),
                    &arg::<String>(&args, "key")?,
                )
                .await?;
            // Mirrors ExtensionStoreJsonLookupPayload: `value` is omitted, not
            // null, when the key was never written.
            match value {
                Some(value) => ok(json!({ "found": true, "value": value })),
                None => ok(json!({ "found": false })),
            }
        }
        "set_extension_store_json" => ok(state
            .services
            .extension_store_service
            .set_json(
                &arg::<String>(&args, "namespace")?,
                opt_arg::<String>(&args, "table")?.as_deref(),
                &arg::<String>(&args, "key")?,
                arg::<Value>(&args, "value")?,
            )
            .await?),
        "update_extension_store_json" => ok(state
            .services
            .extension_store_service
            .update_json(
                &arg::<String>(&args, "namespace")?,
                opt_arg::<String>(&args, "table")?.as_deref(),
                &arg::<String>(&args, "key")?,
                arg::<Value>(&args, "value")?,
            )
            .await?),
        "rename_extension_store_key" => ok(state
            .services
            .extension_store_service
            .rename_json_key(
                &arg::<String>(&args, "namespace")?,
                opt_arg::<String>(&args, "table")?.as_deref(),
                &arg::<String>(&args, "key")?,
                &arg::<String>(&args, "newKey")?,
            )
            .await?),
        "delete_extension_store_json" => ok(state
            .services
            .extension_store_service
            .delete_json(
                &arg::<String>(&args, "namespace")?,
                opt_arg::<String>(&args, "table")?.as_deref(),
                &arg::<String>(&args, "key")?,
            )
            .await?),
        "list_extension_store_keys" => ok(state
            .services
            .extension_store_service
            .list_json_keys(
                &arg::<String>(&args, "namespace")?,
                opt_arg::<String>(&args, "table")?.as_deref(),
            )
            .await?),
        "list_extension_store_tables" => ok(state
            .services
            .extension_store_service
            .list_tables(&arg::<String>(&args, "namespace")?)
            .await?),
        "delete_extension_store_table" => ok(state
            .services
            .extension_store_service
            .delete_table(
                &arg::<String>(&args, "namespace")?,
                &arg::<String>(&args, "table")?,
            )
            .await?),
        "get_extension_store_blob" => {
            let key = arg::<String>(&args, "key")?;
            let bytes = state
                .services
                .extension_store_service
                .get_blob(
                    &arg::<String>(&args, "namespace")?,
                    opt_arg::<String>(&args, "table")?.as_deref(),
                    &key,
                )
                .await?;
            // snake_case: the frontend reads `content_base64`/`mime_type`
            // (src/tauri/main/api/extension-store.js getBlob).
            ok(json!({
                "content_base64": BASE64_STANDARD.encode(bytes),
                "mime_type": mime_guess::from_path(&key)
                    .first_or_octet_stream()
                    .essence_str(),
            }))
        }
        "set_extension_store_blob" => {
            let data_base64 = arg::<String>(&args, "dataBase64")?;
            let data_base64 = data_base64.trim();
            if data_base64.is_empty() {
                return Err(ServerError::BadRequest(
                    "No blob data specified".to_string(),
                ));
            }
            let bytes = BASE64_STANDARD
                .decode(data_base64.as_bytes())
                .map_err(|error| ServerError::BadRequest(format!("Invalid base64: {error}")))?;
            ok(state
                .services
                .extension_store_service
                .set_blob(
                    &arg::<String>(&args, "namespace")?,
                    opt_arg::<String>(&args, "table")?.as_deref(),
                    &arg::<String>(&args, "key")?,
                    bytes,
                )
                .await?)
        }
        "delete_extension_store_blob" => ok(state
            .services
            .extension_store_service
            .delete_blob(
                &arg::<String>(&args, "namespace")?,
                opt_arg::<String>(&args, "table")?.as_deref(),
                &arg::<String>(&args, "key")?,
            )
            .await?),
        "list_extension_store_blob_keys" => ok(state
            .services
            .extension_store_service
            .list_blob_keys(
                &arg::<String>(&args, "namespace")?,
                opt_arg::<String>(&args, "table")?.as_deref(),
            )
            .await?),
        "devlog_append_frontend_logs" => {
            let entries: Vec<tt_application::dto::dev_observability_dto::FrontendLogEntryDto> =
                arg(&args, "entries")?;
            for entry in entries {
                let message = match entry.target.as_deref() {
                    Some(target) => format!("[{target}] {}", entry.message),
                    None => entry.message,
                };
                match entry.level.trim().to_ascii_lowercase().as_str() {
                    "debug" => tracing::debug!(target: "frontend", "{message}"),
                    "warn" | "warning" => tracing::warn!(target: "frontend", "{message}"),
                    "error" => tracing::error!(target: "frontend", "{message}"),
                    _ => tracing::info!(target: "frontend", "{message}"),
                }
            }
            ok(Value::Null)
        }

        // ---- settings --------------------------------------------------------------
        "get_sillytavern_settings" => ok(state
            .services
            .settings_service
            .get_sillytavern_settings()
            .await?),
        "save_user_settings" => ok(state
            .services
            .settings_service
            .save_user_settings(arg(&args, "settings")?)
            .await?),
        "save_user_settings_patch" => ok(state
            .services
            .settings_service
            .save_user_settings_patch(arg(&args, "patch")?)
            .await?),

        "chat_history_generation_started" => ok(state
            .services
            .chat_history_coordinator
            .generation_started(arg(&args, "locator")?)
            .await?),
        "chat_history_generation_finished" => ok(state
            .services
            .chat_history_coordinator
            .generation_finished(arg(&args, "locator")?)
            .await?),

        "create_settings_snapshot" => ok(state.services.settings_service.create_snapshot().await?),
        "get_settings_snapshots" => ok(state.services.settings_service.get_snapshots().await?),
        "load_settings_snapshot" => ok(state
            .services
            .settings_service
            .load_snapshot(&arg::<String>(&args, "name")?)
            .await?),
        "restore_settings_snapshot" => ok(state
            .services
            .settings_service
            .restore_snapshot(&arg::<String>(&args, "name")?)
            .await?),

        // ---- chats -----------------------------------------------------------------
        "get_character_chat_metadata" => ok(state
            .services
            .chat_service
            .get_character_chat_metadata(
                &arg::<String>(&args, "characterName")?,
                &arg::<String>(&args, "fileName")?,
            )
            .await?),
        "list_chat_summaries" => ok(state
            .services
            .chat_service
            .list_chat_summaries(
                opt_arg::<String>(&args, "characterFilter")?.as_deref(),
                opt_arg(&args, "includeMetadata")?.unwrap_or(false),
            )
            .await?),
        "list_recent_chat_summaries" => ok(state
            .services
            .chat_service
            .list_recent_chat_summaries(
                opt_arg::<String>(&args, "characterFilter")?.as_deref(),
                opt_arg(&args, "includeMetadata")?.unwrap_or(false),
                opt_arg(&args, "maxEntries")?.unwrap_or(usize::MAX),
                &opt_arg::<Vec<tt_application::dto::chat_dto::PinnedCharacterChatDto>>(
                    &args, "pinned",
                )?
                .unwrap_or_default()
                .into_iter()
                .map(Into::into)
                .collect::<Vec<_>>(),
            )
            .await?),
        "search_chats" => ok(state
            .services
            .chat_service
            .search_chats(
                &arg::<String>(&args, "query")?,
                opt_arg::<String>(&args, "characterFilter")?.as_deref(),
            )
            .await?),
        "rename_chat" => ok(state
            .services
            .chat_service
            .rename_chat(arg(&args, "dto")?)
            .await?),
        "delete_chat" => ok(state
            .services
            .chat_service
            .delete_chat(
                &arg::<String>(&args, "characterName")?,
                &arg::<String>(&args, "fileName")?,
            )
            .await?),

        // ---- world info / presets / themes -----------------------------------------
        "get_world_infos_batch" => {
            use tt_application::dto::world_info_dto::{
                GetWorldInfosBatchDto, GetWorldInfosBatchResponseDto,
            };
            let dto: GetWorldInfosBatchDto = arg(&args, "dto")?;
            let items = state
                .services
                .world_info_service
                .get_world_infos_batch(dto.names)
                .await?;
            ok(GetWorldInfosBatchResponseDto { items })
        }
        "get_avatars" => ok(state.services.avatar_service.get_avatars().await?),
        "get_all_groups" => ok(state
            .services
            .group_service
            .get_all_groups()
            .await?
            .into_iter()
            .map(tt_application::dto::group_dto::GroupDto::from)
            .collect::<Vec<_>>()),
        "save_world_info" => {
            let dto: tt_application::dto::world_info_dto::SaveWorldInfoDto = arg(&args, "dto")?;
            ok(state
                .services
                .world_info_service
                .save_world_info(&dto.name, dto.data)
                .await?)
        }
        "save_preset" => {
            let dto: tt_application::dto::preset_dto::SavePresetDto = arg(&args, "dto")?;
            let preset = state.services.preset_service.create_preset(
                dto.name.clone(),
                &dto.api_id,
                dto.preset,
            )?;
            state.services.preset_service.save_preset(&preset).await?;
            ok(json!({ "name": preset.name }))
        }
        "save_theme" => {
            let dto: tt_application::dto::theme_dto::SaveThemeDto = arg(&args, "dto")?;
            ok(state
                .services
                .theme_service
                .save_theme(&dto.name, dto.data)
                .await?)
        }

        "delete_world_info" => {
            let dto: tt_application::dto::world_info_dto::DeleteWorldInfoDto = arg(&args, "dto")?;
            ok(state
                .services
                .world_info_service
                .delete_world_info(&dto.name)
                .await?)
        }
        "delete_theme" => {
            let dto: tt_application::dto::theme_dto::DeleteThemeDto = arg(&args, "dto")?;
            ok(state.services.theme_service.delete_theme(&dto.name).await?)
        }
        "delete_preset" => {
            let dto: tt_application::dto::preset_dto::DeletePresetDto = arg(&args, "dto")?;
            let preset_type = tt_domain::models::preset::PresetType::from_api_id(&dto.api_id)
                .ok_or_else(|| {
                    ServerError::BadRequest(format!("Unknown API ID: {}", dto.api_id))
                })?;
            ok(state
                .services
                .preset_service
                .delete_preset(&dto.name, &preset_type)
                .await?)
        }

        // ---- secrets ---------------------------------------------------------------
        "read_secret_state" => ok(state.services.secret_service.read_secret_state().await?),
        "read_secret_settings" => ok(state.services.secret_service.read_settings()),
        "write_secret" => {
            let dto: tt_application::dto::secret_dto::WriteSecretDto = arg(&args, "dto")?;
            ok(state
                .services
                .secret_service
                .write_secret(&dto.key, &dto.value, dto.label.as_deref())
                .await?)
        }
        "find_secret" => {
            let dto: tt_application::dto::secret_dto::FindSecretDto = arg(&args, "dto")?;
            ok(state
                .services
                .secret_service
                .find_secret(&dto.key, dto.id.as_deref())
                .await?)
        }
        "view_secrets" => ok(state.services.secret_service.view_secrets().await?),

        "delete_secret" => {
            let dto: tt_application::dto::secret_dto::DeleteSecretDto = arg(&args, "dto")?;
            ok(state
                .services
                .secret_service
                .delete_secret(&dto.key, dto.id.as_deref())
                .await?)
        }
        "rotate_secret" => {
            let dto: tt_application::dto::secret_dto::RotateSecretDto = arg(&args, "dto")?;
            ok(state
                .services
                .secret_service
                .rotate_secret(&dto.key, &dto.id)
                .await?)
        }
        "rename_secret" => {
            let dto: tt_application::dto::secret_dto::RenameSecretDto = arg(&args, "dto")?;
            ok(state
                .services
                .secret_service
                .rename_secret(&dto.key, &dto.id, &dto.label)
                .await?)
        }

        "update_tauritavern_settings" => {
            let dto: tt_application::dto::settings_dto::UpdateTauriTavernSettingsDto =
                arg(&args, "dto")?;
            // Upstream keeps both in config.yaml, not the UI: a browser session
            // must not be able to unlock secret readback or reroute every
            // outbound request through a proxy of its choosing.
            if dto.allow_keys_exposure.is_some() || dto.request_proxy.is_some() {
                return Err(ServerError::Unauthorized(
                    "Key exposure and request proxy are server configuration in server mode"
                        .into(),
                ));
            }
            let retention_changed = dto
                .agent
                .as_ref()
                .is_some_and(|agent| agent.retention.is_some());
            let settings = state
                .services
                .settings_service
                .update_tauritavern_settings(dto)
                .await?;
            state
                .services
                .host_resource_service
                .set_avatar_persona_original_images_enabled(
                    settings.avatar_persona_original_images_enabled,
                );
            if retention_changed {
                state
                    .services
                    .agent_run_retention_automation_service
                    .notify_settings_changed();
            }
            ok(settings)
        }
        "save_quick_reply_set" => ok(state
            .services
            .quick_reply_service
            .save_quick_reply_set(arg(&args, "payload")?)
            .await?),
        "delete_quick_reply_set" => ok(state
            .services
            .quick_reply_service
            .delete_quick_reply_set(arg(&args, "payload")?)
            .await?),

        // ---- LLM connections ---------------------------------------------------------
        "list_llm_connections" => ok(json!({
            "connections": state.services.llm_connection_service.list_connections().await?,
        })),
        "load_llm_connection" => {
            let dto: LlmConnectionIdDto = arg(&args, "dto")?;
            ok(json!({
                "connection": state
                    .services
                    .llm_connection_service
                    .load_connection(&dto.connection_id)
                    .await?,
            }))
        }
        "save_llm_connection" => {
            let dto: SaveLlmConnectionDto = arg(&args, "dto")?;
            ok(state
                .services
                .llm_connection_service
                .save_connection(dto.connection)
                .await?)
        }
        "delete_llm_connection" => {
            let dto: LlmConnectionIdDto = arg(&args, "dto")?;
            ok(state
                .services
                .llm_connection_service
                .delete_connection(&dto.connection_id)
                .await?)
        }

        // ---- agent runtime -------------------------------------------------------------
        // Mirrors crates/tauritavern/src/presentation/commands/agent_commands.rs.
        // The browser drives runs by polling `read_agent_run_events`, so no
        // event channel is needed.
        "start_agent_run" => ok(state
            .services
            .agent_runtime_service
            .start_run(arg(&args, "dto")?)
            .await?),
        "prepare_agent_prompt_assembly" => {
            let dto: agent_dto::AgentPreparePromptAssemblyDto = arg(&args, "dto")?;
            let services = &state.services;
            let profile = services
                .prompt_assembly_service
                .resolve_profile(
                    dto.profile_id.as_deref(),
                    services.agent_runtime_service.tool_catalog(),
                )
                .await?;
            let visible_tools = services
                .agent_runtime_service
                .visible_model_tools(&profile)?;
            ok(services
                .prompt_assembly_service
                .prepare_frontend_prompt_assembly(dto, profile, &visible_tools)
                .await?)
        }
        "build_agent_current_model_connection_snapshot" => {
            let dto: agent_dto::AgentBuildCurrentModelConnectionSnapshotDto = arg(&args, "dto")?;
            ok(agent_dto::AgentBuildCurrentModelConnectionSnapshotResultDto {
                current_model_connection: state
                    .services
                    .prompt_assembly_service
                    .build_current_model_connection_snapshot(
                        &dto.settings,
                        &dto.model,
                        dto.secret_id.as_deref(),
                    )?,
            })
        }
        "apply_agent_current_model_connection_snapshot" => {
            let dto: agent_dto::AgentApplyCurrentModelConnectionSnapshotDto = arg(&args, "dto")?;
            ok(agent_dto::AgentApplyCurrentModelConnectionSnapshotResultDto {
                settings: state
                    .services
                    .prompt_assembly_service
                    .apply_current_model_connection_snapshot(
                        dto.settings,
                        &dto.current_model_connection,
                    )?,
            })
        }
        "list_agent_profiles" => {
            let list = state.services.agent_profile_service.list_profiles().await?;
            ok(agent_dto::AgentListProfilesResultDto {
                profiles: list.profiles,
                issues: list.issues,
            })
        }
        "list_agent_tools" => ok(agent_dto::AgentListToolsResultDto {
            tools: state.services.agent_runtime_service.tool_catalog_items()?,
        }),
        "resolve_agent_system_prompt" => {
            let dto: agent_dto::AgentResolveSystemPromptDto = arg(&args, "dto")?;
            ok(agent_dto::AgentResolveSystemPromptResultDto {
                agent_system_prompt: state
                    .services
                    .agent_runtime_service
                    .resolve_agent_system_prompt(dto.profile_id.as_deref())
                    .await?,
            })
        }
        "load_agent_profile" => {
            let dto: agent_dto::AgentProfileIdDto = arg(&args, "dto")?;
            ok(agent_dto::AgentLoadProfileResultDto {
                profile: state
                    .services
                    .agent_profile_service
                    .load_profile(&dto.profile_id)
                    .await?,
            })
        }
        "diagnose_agent_profile" => {
            let dto: agent_dto::AgentProfileIdDto = arg(&args, "dto")?;
            ok(state
                .services
                .agent_profile_diagnostic_service
                .diagnose_profile(
                    &dto.profile_id,
                    state.services.agent_runtime_service.tool_catalog(),
                )
                .await?)
        }
        "save_agent_profile" => {
            let dto: agent_dto::AgentSaveProfileDto = arg(&args, "dto")?;
            ok(state
                .services
                .agent_profile_service
                .save_profile(
                    dto.profile,
                    state.services.agent_runtime_service.tool_catalog(),
                )
                .await?)
        }
        "delete_agent_profile" => {
            let dto: agent_dto::AgentProfileIdDto = arg(&args, "dto")?;
            ok(state
                .services
                .agent_profile_service
                .delete_profile(&dto.profile_id)
                .await?)
        }
        "repair_agent_profile_file" => {
            let dto: agent_dto::AgentRepairProfileFileDto = arg(&args, "dto")?;
            ok(state
                .services
                .agent_profile_service
                .repair_profile_file(&dto.profile_id, dto.action)
                .await?)
        }
        "retarget_agent_profile_preset_refs" => {
            let dto: agent_dto::AgentRetargetPresetRefsDto = arg(&args, "dto")?;
            let result = state
                .services
                .agent_profile_service
                .retarget_preset_refs(dto.from, dto.to)
                .await?;
            ok(agent_dto::AgentRetargetPresetRefsResultDto {
                updated: result.profile_ids.len(),
                profile_ids: result
                    .profile_ids
                    .iter()
                    .map(|id| id.as_str().to_string())
                    .collect(),
            })
        }
        "cancel_agent_run" => ok(state
            .services
            .agent_runtime_service
            .cancel_run(arg(&args, "dto")?)
            .await?),
        "submit_agent_run_guidance" => ok(state
            .services
            .agent_runtime_service
            .submit_guidance(arg(&args, "dto")?)
            .await?),
        "list_agent_runs" => ok(state
            .services
            .agent_run_history_service
            .list_runs(arg(&args, "dto")?)
            .await?),
        "plan_agent_run_prune" => ok(state
            .services
            .agent_run_history_service
            .plan_run_prune(arg(&args, "dto")?)
            .await?),
        "apply_agent_run_prune" => ok(state
            .services
            .agent_run_history_service
            .apply_run_prune(arg(&args, "dto")?)
            .await?),
        "read_agent_run_events" => ok(state
            .services
            .agent_runtime_service
            .read_events(arg(&args, "dto")?)
            .await?),
        "read_agent_workspace_file" => ok(state
            .services
            .agent_runtime_service
            .read_workspace_file(arg(&args, "dto")?)
            .await?),
        "read_agent_model_turn" => ok(state
            .services
            .agent_runtime_service
            .read_model_turn(arg(&args, "dto")?)
            .await?),
        "read_agent_prompt_assembly_request" => ok(state
            .services
            .agent_runtime_service
            .read_prompt_assembly_request(arg(&args, "dto")?)
            .await?),
        "resolve_agent_chat_commit" => ok(state
            .services
            .agent_runtime_service
            .resolve_chat_commit(arg(&args, "dto")?)
            .await?),
        "resolve_agent_prompt_assembly" => ok(state
            .services
            .agent_runtime_service
            .resolve_prompt_assembly(arg(&args, "dto")?)
            .await?),
        "resolve_agent_persistent_state_metadata_update" => ok(state
            .services
            .agent_runtime_service
            .resolve_persistent_state_metadata_update(arg(&args, "dto")?)
            .await?),
        "prune_agent_chat_persistent_states" => {
            prune_agent_chat_persistent_states(state, arg(&args, "dto")?).await
        }

        // ---- agent skills --------------------------------------------------------------
        "download_skill_import_url" => ok(state
            .services
            .skill_service
            .download_import_url(&arg::<String>(&args, "url")?)
            .await?),
        "list_skills" => ok(state
            .services
            .skill_service
            .list_skills(opt_arg(&args, "scope")?.unwrap_or_default())
            .await?),
        "list_skill_files" => ok(state
            .services
            .skill_service
            .list_skill_files(
                opt_arg(&args, "scope")?.unwrap_or_default(),
                &arg::<String>(&args, "name")?,
            )
            .await?),
        "preview_skill_import" => {
            let input: tt_domain::models::skill::SkillImportInput = arg(&args, "input")?;
            staged_skill_input(state, &input)?;
            ok(state
                .services
                .skill_service
                .preview_import(input, opt_arg(&args, "targetScope")?.unwrap_or_default())
                .await?)
        }
        "install_skill_import" => {
            let request: tt_domain::models::skill::SkillInstallRequest = arg(&args, "request")?;
            staged_skill_input(state, &request.input)?;
            ok(state.services.skill_service.install_import(request).await?)
        }
        "read_skill_file" => {
            use tt_domain::models::skill::DEFAULT_SKILL_READ_FALLBACK_MAX_CHARS as MAX;
            let max_chars = match opt_arg::<usize>(&args, "maxChars")? {
                Some(0) => {
                    return Err(ServerError::BadRequest(
                        "maxChars must be greater than 0".into(),
                    ));
                }
                Some(value) if value > MAX => {
                    return Err(ServerError::BadRequest(format!(
                        "maxChars must be <= {MAX} for api.skill.readFile"
                    )));
                }
                Some(value) => value,
                None => MAX,
            };
            ok(state
                .services
                .skill_service
                .read_skill_file(tt_domain::models::skill::SkillReadRequest {
                    scope: opt_arg(&args, "scope")?.unwrap_or_default(),
                    name: arg(&args, "name")?,
                    path: arg(&args, "path")?,
                    start_line: opt_arg(&args, "startLine")?,
                    line_count: opt_arg(&args, "lineCount")?,
                    start_char: opt_arg(&args, "startChar")?,
                    max_chars: Some(max_chars),
                })
                .await?)
        }
        "write_skill_file" => ok(state
            .services
            .skill_service
            .write_skill_file(tt_domain::models::skill::SkillWriteRequest {
                scope: opt_arg(&args, "scope")?.unwrap_or_default(),
                name: arg(&args, "name")?,
                path: arg(&args, "path")?,
                content: arg(&args, "content")?,
                expected_sha256: opt_arg(&args, "expectedSha256")?,
            })
            .await?),
        "export_skill" => {
            let exported = state
                .services
                .skill_service
                .export_skill(
                    opt_arg(&args, "scope")?.unwrap_or_default(),
                    &arg::<String>(&args, "name")?,
                )
                .await?;
            ok(json!({
                "fileName": exported.file_name,
                "contentBase64": BASE64_STANDARD.encode(exported.bytes),
                "sha256": exported.sha256,
            }))
        }
        "delete_skill" => ok(state
            .services
            .skill_service
            .delete_skill(
                opt_arg(&args, "scope")?.unwrap_or_default(),
                &arg::<String>(&args, "name")?,
            )
            .await?),
        "move_skill" => ok(state
            .services
            .skill_service
            .move_skill(arg(&args, "request")?)
            .await?),
        "retarget_skill_scope" => ok(state
            .services
            .skill_service
            .retarget_scope(arg(&args, "request")?)
            .await?),

        other => Err(ServerError::NotFound(format!(
            "Command `{other}` is not available in server mode"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn world_info_batch_reads_saved_entries_with_browser_dto_contract() {
        let root = std::env::temp_dir().join(format!("tt-world-rpc-{}", rand::random::<u64>()));
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
            auth: crate::auth::Auth::new(None),
            frontend_dir: root.join("frontend"),
            csrf_token: "test".into(),
            upload_staging: Arc::new(crate::upload::UploadStaging::new(&root)),
        });
        let name = "角色世界书";
        let data = json!({"entries": {"7": {
            "uid": 7, "key": ["城门"], "content": "城门在日落时关闭", "disable": false
        }}});
        dispatch(&state, "save_world_info", json!({"dto": {"name": name, "data": data}}))
            .await
            .expect("save command exposed")
            .expect("save world");
        let response = dispatch(
            &state,
            "get_world_infos_batch",
            json!({"dto": {"names": [name]}}),
        )
        .await
        .expect("batch command exposed")
        .expect("read with browser request envelope");
        assert_eq!(response, json!({"items": [{"name": name, "data": data}]}));
        drop(state);
        tokio::fs::remove_dir_all(root).await.expect("remove test data");
    }

    /// Arm names in `run`'s `match`, parsed from this file's own source.
    ///
    /// The dispatcher keeps two hand-maintained lists: `EXPOSED_COMMANDS` gates
    /// reachability, `run` holds the handlers. Nothing in the type system ties
    /// them together, so a name present in one and missing from the other is a
    /// silent 404 (or dead code) that only shows up in the browser.
    fn match_arm_commands() -> Vec<String> {
        let source = include_str!("rpc.rs");
        let body = source
            .split_once("async fn run(")
            .expect("run() present")
            .1;

        // Top-level arms sit at exactly two indent levels inside `run`. Deeper
        // lines belong to nested matches (`devlog_append_frontend_logs` maps log
        // levels) and are not command names.
        const ARM_INDENT: &str = "        ";

        let mut commands = Vec::new();
        for line in body.lines() {
            let Some(rest) = line.strip_prefix(ARM_INDENT) else {
                continue;
            };
            if rest.starts_with(' ') {
                continue;
            }
            let Some(rest) = rest.strip_prefix('"') else {
                continue;
            };
            let Some((name, tail)) = rest.split_once('"') else {
                continue;
            };
            if tail.trim_start().starts_with("=>") {
                commands.push(name.to_string());
            }
        }
        commands
    }

    #[test]
    fn every_exposed_command_has_a_handler() {
        let handled = match_arm_commands();
        let missing: Vec<_> = EXPOSED_COMMANDS
            .iter()
            .filter(|command| !handled.iter().any(|arm| arm == *command))
            .collect();

        assert!(
            missing.is_empty(),
            "exposed but unhandled, so the frontend gets a 404: {missing:?}"
        );
    }

    #[test]
    fn every_handler_is_exposed() {
        let unreachable: Vec<_> = match_arm_commands()
            .into_iter()
            .filter(|command| !EXPOSED_COMMANDS.contains(&command.as_str()))
            .collect();

        assert!(
            unreachable.is_empty(),
            "handled but not exposed, so the arm is dead code: {unreachable:?}"
        );
    }

    #[test]
    fn background_folder_commands_are_reachable() {
        // The browser's first-load path calls get_background_folders inside
        // firstLoadInit; a 404 there aborts every later init step. Folder
        // thumbnail persistence follows in the same frontend function.
        for command in [
            "get_background_folders",
            "set_image_metadata_folder_thumbnails",
        ] {
            assert!(is_exposed(command), "{command} must stay reachable");
        }
    }
}

/// Mirrors `agent_commands::prune_agent_chat_persistent_states`: keeps every
/// state id still referenced by the chat, prunes the rest of the candidates.
async fn prune_agent_chat_persistent_states(
    state: &Arc<AppState>,
    dto: agent_dto::AgentPruneChatPersistentStatesDto,
) -> Handled {
    use tt_domain::models::agent::AgentChatRef;

    let AgentChatRef::Character {
        character_id,
        file_name,
    } = &dto.chat_ref
    else {
        return Err(ServerError::BadRequest(
            "agent.group_persistent_state_prune_unsupported".into(),
        ));
    };
    let candidate_state_ids = dto.candidate_state_ids.ok_or_else(|| {
        ServerError::BadRequest("agent.persistent_state_prune_candidates_required".into())
    })?;

    let payload = state
        .services
        .chat_service
        .get_chat_payload(character_id, file_name)
        .await?;
    let retained_state_ids: std::collections::BTreeSet<String> = payload
        .iter()
        .flat_map(|item| {
            let swipes = item
                .get("swipe_info")
                .and_then(Value::as_array)
                .into_iter()
                .flatten();
            std::iter::once(item).chain(swipes)
        })
        .filter_map(|entry| {
            entry
                .pointer("/extra/tauritavern/agent/persistStateId")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .map(ToString::to_string)
        })
        .collect();

    let prune = state
        .services
        .chat_service
        .prune_agent_persistent_states(
            &AgentChatWorkspaceTarget {
                chat_ref: dto.chat_ref,
                stable_chat_id: dto.stable_chat_id,
            },
            AgentPersistentStatePruneRequest {
                retained_state_ids: retained_state_ids.into_iter().collect(),
                candidate_state_ids,
            },
        )
        .await?;
    ok(agent_dto::AgentPruneChatPersistentStatesResultDto {
        workspace_id: prune.workspace_id,
        removed_state_ids: prune.removed_state_ids,
    })
}

async fn bootstrap_snapshot(state: &Arc<AppState>) -> Handled {
    let services = &state.services;
    let (settings, characters, groups, avatars, secret_state) = tokio::try_join!(
        async { Ok::<_, ServerError>(services.settings_service.get_sillytavern_settings().await?) },
        async { Ok::<_, ServerError>(services.character_service.get_all_characters(true).await?) },
        async { Ok::<_, ServerError>(services.group_service.get_all_groups().await?) },
        async { Ok::<_, ServerError>(services.avatar_service.get_avatars().await?) },
        async { Ok::<_, ServerError>(services.secret_service.read_secret_state().await?) },
    )?;

    ok(json!({
        "ios_policy": services.ios_policy,
        "settings": settings,
        "characters": characters,
        "groups": groups
            .into_iter()
            .map(tt_application::dto::group_dto::GroupDto::from)
            .collect::<Vec<_>>(),
        "avatars": avatars,
        "secret_state": secret_state,
    }))
}
