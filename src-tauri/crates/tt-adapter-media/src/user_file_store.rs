use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use tokio::fs;
use tt_domain::errors::DomainError;
use tt_ports::user_file::UserFileStore;

pub struct FilesystemUserFileStore {
    root: PathBuf,
}

impl FilesystemUserFileStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    async fn root(&self) -> Result<PathBuf, DomainError> {
        fs::create_dir_all(&self.root).await.map_err(|error| {
            DomainError::InternalError(format!("Failed to ensure files directory: {error}"))
        })?;
        fs::canonicalize(&self.root).await.map_err(|error| {
            DomainError::InternalError(format!("Failed to resolve files directory: {error}"))
        })
    }

    async fn target(&self, relative: &Path) -> Result<PathBuf, DomainError> {
        let root = self.root().await?;
        let target = root.join(relative);
        // Check existing components, including symlinks, before touching user-controlled paths.
        let parent = target
            .parent()
            .ok_or_else(|| DomainError::InvalidData("Invalid path".into()))?;
        let parent = fs::canonicalize(parent)
            .await
            .map_err(|error| match error.kind() {
                ErrorKind::NotFound => DomainError::NotFound("File not found".into()),
                _ => {
                    DomainError::InternalError(format!("Failed to resolve file directory: {error}"))
                }
            })?;
        if !parent.starts_with(&root) {
            return Err(DomainError::InvalidData("Invalid path".into()));
        }
        let target = parent.join(
            relative
                .file_name()
                .ok_or_else(|| DomainError::InvalidData("Invalid path".into()))?,
        );
        match fs::symlink_metadata(&target).await {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                let resolved = fs::canonicalize(&target)
                    .await
                    .map_err(|_| DomainError::InvalidData("Invalid path".into()))?;
                if !resolved.starts_with(&root) {
                    return Err(DomainError::InvalidData("Invalid path".into()));
                }
            }
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => {
                return Err(DomainError::InternalError(format!(
                    "Failed to stat file: {error}"
                )));
            }
        }
        Ok(target)
    }
}

#[async_trait]
impl UserFileStore for FilesystemUserFileStore {
    async fn write_file(&self, name: &str, bytes: Vec<u8>) -> Result<(), DomainError> {
        let target = self.target(Path::new(name)).await?;
        fs::write(target, bytes)
            .await
            .map_err(|error| DomainError::InternalError(format!("Failed to save file: {error}")))
    }

    async fn delete_file(&self, relative_path: &Path) -> Result<(), DomainError> {
        let target = self.target(relative_path).await?;
        let metadata = fs::metadata(&target)
            .await
            .map_err(|error| match error.kind() {
                ErrorKind::NotFound => DomainError::NotFound("File not found".into()),
                _ => DomainError::InternalError(format!("Failed to stat file: {error}")),
            })?;
        if !metadata.is_file() {
            return Err(DomainError::NotFound("File not found".into()));
        }
        fs::remove_file(target)
            .await
            .map_err(|error| match error.kind() {
                ErrorKind::NotFound => DomainError::NotFound("File not found".into()),
                _ => DomainError::InternalError(format!("Failed to delete file: {error}")),
            })
    }

    async fn is_file(&self, relative_path: &Path) -> Result<bool, DomainError> {
        let target = match self.target(relative_path).await {
            Ok(path) => path,
            Err(DomainError::NotFound(_) | DomainError::InvalidData(_)) => return Ok(false),
            Err(error) => return Err(error),
        };
        match fs::metadata(target).await {
            Ok(metadata) => Ok(metadata.is_file()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
            Err(error) => Err(DomainError::InternalError(format!(
                "Failed to stat file: {error}"
            ))),
        }
    }
}
