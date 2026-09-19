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
];

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
        "update_character_card_data" => ok(state
            .services
            .character_service
            .update_character_card_data(&arg::<String>(&args, "name")?, arg(&args, "dto")?)
            .await?),
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
        "create_character_with_avatar" => ok(state
            .services
            .character_service
            .create_with_avatar(arg(&args, "dto")?)
            .await?),
        "import_character" => ok(state
            .services
            .character_service
            .import_character(arg(&args, "dto")?)
            .await?),
        "replace_character" => ok(state
            .services
            .character_service
            .replace_character(arg(&args, "dto")?)
            .await?),
        "update_avatar" => ok(state
            .services
            .character_service
            .update_avatar(arg(&args, "dto")?)
            .await?),
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
        "upload_background_from_path" => ok(state
            .services
            .background_service
            .upload_background_from_path(
                &arg::<String>(&args, "filename")?,
                &arg::<String>(&args, "filePath")?,
            )
            .await?),
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
        "get_world_infos_batch" => ok(state
            .services
            .world_info_service
            .get_world_infos_batch(arg(&args, "names")?)
            .await?),
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

        other => Err(ServerError::NotFound(format!(
            "Command `{other}` is not available in server mode"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
