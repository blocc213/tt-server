use std::path::Path;

use async_trait::async_trait;
use tt_domain::errors::DomainError;

#[async_trait]
pub trait UserFileStore: Send + Sync {
    async fn write_file(&self, name: &str, bytes: Vec<u8>) -> Result<(), DomainError>;
    async fn delete_file(&self, relative_path: &Path) -> Result<(), DomainError>;
    async fn is_file(&self, relative_path: &Path) -> Result<bool, DomainError>;
}
