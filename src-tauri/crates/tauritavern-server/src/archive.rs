//! Data archive host glue: whole-user backup and full data migration.
//!
//! The app host stages archives in an app-private directory and delivers them
//! to the OS Downloads folder or a share sheet. A server owns nothing besides
//! the data root, so staging lives under `_tauritavern/archive-*` (skipped by
//! the shared full-data exporter) and delivery is a browser download: the
//! archive is opened, disposed, and streamed from the open handle. Secret
//! inclusion follows the page route rule: only when key exposure is allowed.

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use axum::Json;
use axum::body::Body;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use http::header;
use serde::Deserialize;
use tt_adapter_storage_core::DataDirectory;
use tt_application::services::character_service::CharacterService;
use tt_application::services::chat_history_coordinator::ChatHistoryCoordinator;
use tt_application::services::chat_service::ChatService;
use tt_application::services::group_chat_service::GroupChatService;
use tt_application::services::group_service::GroupService;
use tt_application::services::secret_service::SecretService;
use tt_application::services::settings_service::SettingsService;
use tt_domain::errors::DomainError;
use tt_ports::data_archive::{
    DataArchiveFileGateway, DataRootInitializer, ExportArchiveExecutionRequest,
    ImportArchiveExecutionRequest, UserBackupArchiveExecutionRequest, UserBackupArchiveTarget,
};
use tt_ports::sync::DataChangeReconciler;

use crate::error::ServerError;
use crate::state::SharedState;

const EXPORT_RETENTION: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Deserialize)]
pub struct UserBackupRequest {
    pub handle: String,
}

pub async fn user_backup(
    State(state): State<SharedState>,
    Json(request): Json<UserBackupRequest>,
) -> Result<Response, ServerError> {
    let include_secrets = state
        .services
        .secret_service
        .read_settings()
        .allow_keys_exposure;
    let service = &state.services.data_archive_service;
    let backup = service
        .export_user_backup(request.handle, include_secrets)
        .await?;

    // One-shot artifact: dispose of it as soon as it is open.
    let file = open_archive(Path::new(&backup.archive_path)).await;
    if let Err(error) = service.cleanup_user_backup(&backup.archive_path) {
        tracing::warn!(%error, "Failed to clean up user backup archive");
    }
    zip_attachment(file?, &backup.file_name).await
}

#[derive(Debug, Deserialize)]
pub struct ExportDownloadQuery {
    pub id: String,
}

/// Browser delivery for a completed full-data export job, in place of the app
/// host's `save_export_data_archive` (copy into Downloads). The artifact is
/// delivered once and then marked disposed.
pub async fn export_download(
    State(state): State<SharedState>,
    Query(query): Query<ExportDownloadQuery>,
) -> Result<Response, ServerError> {
    let service = &state.services.data_archive_service;
    let archive_path = service.completed_export_archive_path(&query.id)?;
    let file_name = service
        .get_status(&query.id)?
        .result
        .and_then(|result| result.file_name)
        .ok_or_else(|| ServerError::Internal("Export archive file name is missing".into()))?;

    let file = open_archive(&archive_path).await?;
    if let Some(error) = service.finalize_export_delivery(&query.id, None)? {
        tracing::warn!(%error, "Failed to clean up delivered export archive");
    }
    zip_attachment(file, &file_name).await
}

async fn open_archive(path: &Path) -> Result<tokio::fs::File, ServerError> {
    tokio::fs::File::open(path)
        .await
        .map_err(|error| ServerError::Internal(format!("Failed to open archive: {error}")))
}

/// Streams an archive whose file the caller already unlinked; the open handle
/// keeps the bytes readable until the body ends.
// ponytail: unlink-while-open is POSIX; on Windows the unlink fails, is logged,
// and the stale-export sweep removes the file after EXPORT_RETENTION.
async fn zip_attachment(file: tokio::fs::File, file_name: &str) -> Result<Response, ServerError> {
    let length = file
        .metadata()
        .await
        .map_err(|error| ServerError::Internal(format!("Failed to stat archive: {error}")))?
        .len();
    Ok((
        [
            (header::CONTENT_TYPE, "application/zip".to_string()),
            (header::CONTENT_LENGTH, length.to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!(
                    "attachment; filename=\"{}\"",
                    attachment_file_name(file_name)
                ),
            ),
        ],
        Body::from_stream(tokio_util::io::ReaderStream::new(file)),
    )
        .into_response())
}

/// Plain `filename="..."` value: upstream page code splits the header on `=`
/// and strips quotes, so the name must need no encoding at all.
fn attachment_file_name(file_name: &str) -> String {
    file_name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

/// Server counterpart of the app host's `TauriDataArchiveFileGateway`.
pub struct ServerDataArchiveFiles {
    data_root: PathBuf,
    imports_root: PathBuf,
    exports_root: PathBuf,
}

impl ServerDataArchiveFiles {
    pub fn new(data_root: &Path) -> Self {
        let state_root = data_root.join("_tauritavern");
        Self {
            data_root: data_root.to_path_buf(),
            imports_root: state_root.join("archive-imports"),
            exports_root: state_root.join("archive-exports"),
        }
    }

    fn ensure_exports_root(&self, protected_paths: &[PathBuf]) -> Result<(), DomainError> {
        fs::create_dir_all(&self.exports_root).map_err(|error| {
            DomainError::InternalError(format!("Failed to create export directory: {error}"))
        })?;
        cleanup_stale_exports(&self.exports_root, protected_paths);
        Ok(())
    }
}

impl DataArchiveFileGateway for ServerDataArchiveFiles {
    fn prepare_incoming_import_archive_path(&self) -> Result<PathBuf, DomainError> {
        let incoming = self.imports_root.join("incoming");
        fs::create_dir_all(&incoming).map_err(|error| {
            DomainError::InternalError(format!("Failed to create import staging: {error}"))
        })?;
        Ok(incoming.join(format!("tauritavern-import-{}.archive", random_id())))
    }

    fn prepare_import_archive(
        &self,
        archive_path: &Path,
        archive_is_temporary: bool,
        job_id: &str,
    ) -> Result<ImportArchiveExecutionRequest, DomainError> {
        if !archive_path.is_file() {
            return Err(DomainError::InvalidData(format!(
                "Archive file does not exist: {}",
                archive_path.display()
            )));
        }
        let workspace_root = self.imports_root.join(job_id);
        fs::create_dir_all(&workspace_root).map_err(|error| {
            DomainError::InternalError(format!("Failed to create job workspace: {error}"))
        })?;

        // Upload staging and the job workspace share the data root's
        // filesystem, so a rename always suffices.
        let archive_path = if archive_is_temporary {
            let staged = workspace_root.join("import.archive");
            if let Err(error) = fs::rename(archive_path, &staged) {
                cleanup_directory(&workspace_root);
                return Err(DomainError::InternalError(format!(
                    "Failed to move uploaded archive into job workspace: {error}"
                )));
            }
            staged
        } else {
            archive_path.to_path_buf()
        };

        Ok(ImportArchiveExecutionRequest {
            data_root: self.data_root.clone(),
            archive_path,
            workspace_root,
        })
    }

    fn prepare_export_archive(
        &self,
        job_id: &str,
        protected_paths: &[PathBuf],
    ) -> Result<ExportArchiveExecutionRequest, DomainError> {
        self.ensure_exports_root(protected_paths)?;
        Ok(ExportArchiveExecutionRequest {
            data_root: self.data_root.clone(),
            output_path: self.exports_root.join(format!("export-{job_id}.zip")),
            file_name: format!("tauritavern-data-{}.zip", timestamp()),
            include_secrets: false,
        })
    }

    fn prepare_user_backup_archive(
        &self,
        handle: &str,
        include_secrets: bool,
        protected_paths: &[PathBuf],
    ) -> Result<UserBackupArchiveTarget, DomainError> {
        let handle = handle.trim();
        let user_root = user_root(&self.data_root, handle)?;
        self.ensure_exports_root(protected_paths)?;
        let file_name = format!("{handle}-{}.zip", timestamp());
        Ok(UserBackupArchiveTarget {
            request: UserBackupArchiveExecutionRequest {
                user_root,
                output_path: self
                    .exports_root
                    .join(format!(".user-backup-{}-{file_name}", random_id())),
                include_secrets,
            },
            file_name,
        })
    }

    fn cleanup_directory(&self, path: &Path) {
        cleanup_directory(path);
    }

    fn cleanup_export(&self, archive_path: &Path) -> Result<(), DomainError> {
        fs::remove_file(archive_path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                DomainError::NotFound(format!(
                    "Export archive file not found: {}",
                    archive_path.display()
                ))
            } else {
                DomainError::InternalError(format!(
                    "Failed to cleanup export archive {}: {error}",
                    archive_path.display()
                ))
            }
        })
    }

    fn save_export(&self, _archive_path: &Path, _file_name: &str) -> Result<PathBuf, DomainError> {
        Err(DomainError::InvalidData(
            "Server mode delivers exports as browser downloads".into(),
        ))
    }

    fn save_user_backup(
        &self,
        _archive_path: &str,
        _file_name: &str,
    ) -> Result<PathBuf, DomainError> {
        Err(DomainError::InvalidData(
            "Server mode delivers backups as browser downloads".into(),
        ))
    }

    fn cleanup_user_backup(&self, archive_path: &str) -> Result<(), DomainError> {
        let path = Path::new(archive_path);
        if path.parent() != Some(self.exports_root.as_path()) {
            return Err(DomainError::InvalidData(format!(
                "User backup archive path is outside the staging directory: {archive_path}"
            )));
        }
        if let Err(error) = fs::remove_file(path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(%error, path = %path.display(), "Failed to clean up user backup archive");
        }
        Ok(())
    }
}

/// One plain path component naming a real user directory inside the data root.
///
/// `symlink_metadata` rejects a symlinked handle outright, and the canonical
/// containment check catches anything else that resolves elsewhere: either
/// would let the backup ZIP read files from outside the data root.
fn user_root(data_root: &Path, handle: &str) -> Result<PathBuf, DomainError> {
    let mut components = Path::new(handle).components();
    let single =
        matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none();
    if handle.is_empty()
        || handle.contains(['/', '\\'])
        || !single
        || handle.starts_with(['_', '.'])
    {
        return Err(DomainError::InvalidData(format!(
            "Invalid user handle for backup: {handle}"
        )));
    }

    let root = data_root.join(handle);
    let not_found = || DomainError::NotFound(format!("User directory not found: {handle}"));
    if !fs::symlink_metadata(&root).is_ok_and(|metadata| metadata.is_dir()) {
        return Err(not_found());
    }
    let canonical_data_root = fs::canonicalize(data_root).map_err(|error| {
        DomainError::InternalError(format!("Failed to resolve data root: {error}"))
    })?;
    let canonical_root = fs::canonicalize(&root).map_err(|_| not_found())?;
    if canonical_root.parent() != Some(canonical_data_root.as_path()) {
        return Err(not_found());
    }
    Ok(canonical_root)
}

pub struct DataDirectoryInitializer;

#[async_trait]
impl DataRootInitializer for DataDirectoryInitializer {
    async fn initialize_data_root(&self, data_root: &Path) -> Result<(), DomainError> {
        DataDirectory::new(data_root.to_path_buf())
            .initialize()
            .await
    }
}

/// Same cache set the app host's `ServiceCacheReconciler` refreshes after an
/// import rewrites files underneath the running services.
pub struct ServiceCacheReconciler {
    pub character_service: Arc<CharacterService>,
    pub chat_service: Arc<ChatService>,
    pub group_chat_service: Arc<GroupChatService>,
    pub group_service: Arc<GroupService>,
    pub secret_service: Arc<SecretService>,
    pub settings_service: Arc<SettingsService>,
    pub chat_history_coordinator: Arc<ChatHistoryCoordinator>,
}

#[async_trait]
impl DataChangeReconciler for ServiceCacheReconciler {
    async fn reconcile(&self, reason: &str) -> Result<(), DomainError> {
        tracing::info!(
            reason,
            "Refreshing runtime caches after external data change"
        );

        self.chat_history_coordinator.invalidate_all_pending().await;
        self.character_service.clear_cache().await?;
        self.chat_service.clear_cache().await?;
        self.group_chat_service.clear_cache().await?;
        self.group_service.clear_cache().await?;
        self.secret_service.clear_cache().await?;
        self.settings_service.clear_cache().await;
        self.settings_service.reload_chat_backup_settings().await?;
        Ok(())
    }
}

fn cleanup_directory(path: &Path) {
    if let Err(error) = fs::remove_dir_all(path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(%error, path = %path.display(), "Failed to clean up directory");
    }
}

/// Removes archives nobody downloaded within `EXPORT_RETENTION`.
fn cleanup_stale_exports(export_root: &Path, protected_paths: &[PathBuf]) {
    let Ok(entries) = fs::read_dir(export_root) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() || protected_paths.contains(&path) {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age > EXPORT_RETENTION);
        if stale
            && let Err(error) = fs::remove_file(&path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(%error, path = %path.display(), "Failed to remove stale export");
        }
    }
}

fn timestamp() -> String {
    Utc::now().format("%Y%m%d-%H%M%S").to_string()
}

fn random_id() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles_must_be_one_existing_user_directory() {
        let root = std::env::temp_dir().join(format!("tt-backup-{}", random_id()));
        std::fs::create_dir_all(root.join("default-user")).expect("user dir");
        assert!(user_root(&root, "default-user").is_ok());
        for bad in [
            "",
            "..",
            "../etc",
            "a/b",
            "_tauritavern",
            ".server-upload-staging",
            "missing",
        ] {
            assert!(user_root(&root, bad).is_err(), "{bad} must be rejected");
        }

        #[cfg(unix)]
        {
            let outside = std::env::temp_dir().join(format!("tt-backup-outside-{}", random_id()));
            std::fs::create_dir_all(&outside).expect("outside dir");
            std::os::unix::fs::symlink(&outside, root.join("linked-user")).expect("symlink");
            assert!(
                user_root(&root, "linked-user").is_err(),
                "a symlinked user directory must not escape the data root"
            );
            let _ = std::fs::remove_dir_all(outside);
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn attachment_names_need_no_encoding() {
        assert_eq!(
            attachment_file_name("default-user-20261001-120000.zip"),
            "default-user-20261001-120000.zip"
        );
        assert_eq!(attachment_file_name("a\"b;c=d.zip"), "a_b_c_d.zip");
    }
}
