//! Browser upload staging.
//!
//! The existing frontend materializes a `Blob` through four host commands before
//! it calls character/import services. Keeping that contract avoids rewriting
//! every multipart form route. Server paths are opaque implementation details:
//! every operation canonicalizes against one private staging root.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

use crate::error::ServerError;

const MAX_ACTIVE_UPLOADS: usize = 16;
const DEFAULT_CHUNK_BYTES: u64 = 4 * 1024 * 1024;

pub struct UploadStaging {
    root: PathBuf,
    active: Mutex<std::collections::HashSet<PathBuf>>,
}

impl UploadStaging {
    pub fn new(data_root: &Path) -> Self {
        Self {
            root: data_root.join(".server-upload-staging"),
            active: Mutex::new(std::collections::HashSet::new()),
        }
    }

    pub async fn begin(
        &self,
        kind: &str,
        extension: &str,
        size: u64,
    ) -> Result<UploadBegin, ServerError> {
        let kind = sanitize_segment(kind, "upload kind")?;
        let extension = sanitize_extension(extension)?;

        let mut active = self.active.lock().await;
        if active.len() >= MAX_ACTIVE_UPLOADS {
            return Err(ServerError::TooManyRequests(
                "Too many active uploads".into(),
            ));
        }

        let dir = self.root.join(kind);
        tokio::fs::create_dir_all(&dir).await.map_err(|error| {
            ServerError::Internal(format!("Failed to create upload staging: {error}"))
        })?;

        let path = dir.join(format!("{}.{}", random_id(), extension));
        let file = tokio::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .await
            .map_err(|error| ServerError::Internal(format!("Failed to create upload: {error}")))?;
        if size > 0 {
            file.set_len(size).await.map_err(|error| {
                ServerError::Internal(format!("Failed to reserve upload: {error}"))
            })?;
            file.set_len(0).await.map_err(|error| {
                ServerError::Internal(format!("Failed to reset upload: {error}"))
            })?;
        }
        drop(file);

        active.insert(path.clone());
        Ok(UploadBegin {
            file_path: path.to_string_lossy().to_string(),
            chunk_size: DEFAULT_CHUNK_BYTES,
        })
    }

    pub async fn append(
        &self,
        file_path: &str,
        offset: u64,
        bytes: &[u8],
    ) -> Result<u64, ServerError> {
        let path = self.validate_active(file_path).await?;
        if bytes.is_empty() {
            return Err(ServerError::BadRequest("Upload chunk is empty".into()));
        }
        if bytes.len() as u64 > DEFAULT_CHUNK_BYTES {
            return Err(ServerError::BadRequest("Upload chunk is too large".into()));
        }

        let metadata = tokio::fs::metadata(&path)
            .await
            .map_err(|error| ServerError::Internal(format!("Failed to stat upload: {error}")))?;
        if metadata.len() != offset {
            return Err(ServerError::Conflict(format!(
                "Upload offset mismatch: expected {}, got {offset}",
                metadata.len()
            )));
        }

        let mut file = tokio::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .await
            .map_err(|error| ServerError::Internal(format!("Failed to open upload: {error}")))?;
        file.write_all(bytes)
            .await
            .map_err(|error| ServerError::Internal(format!("Failed to append upload: {error}")))?;
        file.flush()
            .await
            .map_err(|error| ServerError::Internal(format!("Failed to flush upload: {error}")))?;

        Ok(offset + bytes.len() as u64)
    }

    pub async fn finish(&self, file_path: &str, expected_size: u64) -> Result<String, ServerError> {
        let path = self.validate_active(file_path).await?;
        let actual = tokio::fs::metadata(&path)
            .await
            .map_err(|error| ServerError::Internal(format!("Failed to stat upload: {error}")))?
            .len();
        if actual != expected_size {
            return Err(ServerError::Conflict(format!(
                "Upload size mismatch: expected {expected_size}, got {actual}"
            )));
        }
        Ok(path.to_string_lossy().to_string())
    }

    pub async fn discard(&self, file_path: &str) -> Result<(), ServerError> {
        let path = self.validate_path(file_path)?;
        self.active.lock().await.remove(&path);
        match tokio::fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(ServerError::Internal(format!(
                "Failed to remove staged upload: {error}"
            ))),
        }
    }

    async fn validate_active(&self, file_path: &str) -> Result<PathBuf, ServerError> {
        let path = self.validate_path(file_path)?;
        if !self.active.lock().await.contains(&path) {
            return Err(ServerError::NotFound("Upload session not found".into()));
        }
        Ok(path)
    }

    /// Accepts only paths inside the staging root. `Path::starts_with` is
    /// lexical, so `<root>/../default-user/secrets.json` passes it; requiring
    /// every remaining component to be `Normal` is what keeps `..` out.
    pub fn validate_path(&self, file_path: &str) -> Result<PathBuf, ServerError> {
        let path = PathBuf::from(file_path);
        let inside = path.is_absolute()
            && path.strip_prefix(&self.root).is_ok_and(|rest| {
                rest.components()
                    .all(|component| matches!(component, Component::Normal(_)))
            });
        if !inside {
            return Err(ServerError::BadRequest(
                "Upload path is outside the staging directory".into(),
            ));
        }
        Ok(path)
    }
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub struct UploadBegin {
    pub file_path: String,
    pub chunk_size: u64,
}

fn sanitize_segment<'a>(value: &'a str, label: &str) -> Result<&'a str, ServerError> {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || trimmed.len() > 64
        || !trimmed
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        return Err(ServerError::BadRequest(format!("Invalid {label}")));
    }
    Ok(trimmed)
}

fn sanitize_extension(value: &str) -> Result<&str, ServerError> {
    let trimmed = value.trim().trim_start_matches('.');
    if trimmed.is_empty()
        || trimmed.len() > 12
        || !trimmed.chars().all(|ch| ch.is_ascii_alphanumeric())
    {
        return Err(ServerError::BadRequest("Invalid upload extension".into()));
    }
    Ok(trimmed)
}

fn random_id() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

pub type SharedUploadStaging = Arc<UploadStaging>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_paths_outside_staging() {
        let staging = UploadStaging::new(Path::new("/tmp/data"));
        assert!(staging.validate_path("/etc/passwd").is_err());
        // Lexically under the root but resolves outside it.
        assert!(
            staging
                .validate_path("/tmp/data/.server-upload-staging/../default-user/secrets.json")
                .is_err()
        );
        assert!(
            staging
                .validate_path("/tmp/data/.server-upload-staging/character/abc.png")
                .is_ok()
        );
    }

    #[test]
    fn rejects_unsafe_extensions_and_kinds() {
        assert!(sanitize_extension("png").is_ok());
        assert!(sanitize_extension("../png").is_err());
        assert!(sanitize_segment("character-import", "kind").is_ok());
        assert!(sanitize_segment("../../bad", "kind").is_err());
    }
}
