use crate::chat_jsonl::{parse_header_integrity, read_header_record_async};
use crate::commit_stage::{CommitSession, CommitStage};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::file_system::persist_file;
use async_trait::async_trait;
use sha2::{Digest, Sha256};
use tokio::fs;
use tt_contracts::byte_commit::CommitBegin;
use tt_domain::errors::DomainError;
use tt_ports::repositories::chat_commit_repository::{
    ChatCommitOperation, ChatCommitRepository, ChatCommitTarget, ChatSwipeSource,
    ColdSwipeCommitSource, ChatPayloadPrecondition, ChatPayloadVersion, CommittedChat,
};

use super::integrity::verify_integrity_match;
use super::{ContentSignature, FileChatRepository};

// Keep digest semantics deterministic in unit tests; production only hashes on hardware backends.
#[cfg(test)]
fn new_content_hasher() -> Option<Sha256> {
    Some(Sha256::new())
}

#[cfg(all(not(test), target_arch = "aarch64", not(target_os = "windows")))]
fn new_content_hasher() -> Option<Sha256> {
    std::arch::is_aarch64_feature_detected!("sha2").then(Sha256::new)
}

#[cfg(all(not(test), any(target_arch = "x86", target_arch = "x86_64")))]
fn new_content_hasher() -> Option<Sha256> {
    (std::arch::is_x86_feature_detected!("sha")
        && std::arch::is_x86_feature_detected!("sse2")
        && std::arch::is_x86_feature_detected!("ssse3")
        && std::arch::is_x86_feature_detected!("sse4.1"))
    .then(Sha256::new)
}

#[cfg(all(
    not(test),
    not(any(
        all(target_arch = "aarch64", not(target_os = "windows")),
        target_arch = "x86",
        target_arch = "x86_64"
    ))
))]
fn new_content_hasher() -> Option<Sha256> {
    None
}

pub(super) struct CommitMetadata {
    target: ChatCommitTarget,
    target_path: PathBuf,
    operation: ChatCommitOperation,
    content_hasher: Option<(u64, Sha256)>,
    precondition: ChatPayloadPrecondition,
}

impl FileChatRepository {
    pub async fn cleanup_orphaned_chat_commit_staging(&self) {
        self.chat_commit_sessions.cleanup_orphans().await;
    }

    pub(super) async fn resolve_chat_commit_target(
        &self,
        target: &ChatCommitTarget,
    ) -> Result<PathBuf, DomainError> {
        match target {
            ChatCommitTarget::Character {
                character_id,
                file_name,
            } => {
                self.resolve_character_chat_path(character_id, file_name)
                    .await
            }
            ChatCommitTarget::Group { chat_id } => self.get_group_chat_path(chat_id),
        }
    }

    /// Drops cached reads of a chat whose file was just replaced.
    pub(super) async fn invalidate_chat_caches(
        &self,
        target: &ChatCommitTarget,
        path: &Path,
    ) -> Result<(), DomainError> {
        if let ChatCommitTarget::Character {
            character_id,
            file_name,
        } = target
        {
            let cache_key = self.get_cache_key(character_id, file_name)?;
            self.memory_cache.lock().await.remove(&cache_key);
        }
        self.remove_summary_cache_for_path(path).await;
        Ok(())
    }

    async fn commit_payload_stage(
        &self,
        target_path: &Path,
        stage: CommitStage,
        publish_path: &Path,
        force: bool,
        cold_source: Option<ColdSwipeCommitSource>,
        content_hasher: Option<(u64, Sha256)>,
        precondition: &ChatPayloadPrecondition,
    ) -> Result<ChatPayloadVersion, DomainError> {
        let CommitStage {
            path,
            file,
            accepted_offset,
        } = stage;
        // Only the header is interpreted here; body validation belongs to readers.
        let incoming_integrity = {
            let mut reader = tokio::io::BufReader::new(
                super::windowed_payload_io::open_existing_payload_file(&path).await?,
            );
            let (header, _) = read_header_record_async(&mut reader)
                .await?
                .ok_or_else(|| {
                    DomainError::InvalidData("Chat payload must contain a header".into())
                })?;
            parse_header_integrity(&header)?
        };

        let expands_cold_swipes = cold_source.is_some();
        let (file, completed_path, size, digest) = if let Some(cold) = cold_source {
            drop(file);
            let restored = cold
                .source
                .restore_payload(cold.id, &path, publish_path, content_hasher.is_some())
                .await?;
            let file = fs::OpenOptions::new()
                .write(true)
                .open(publish_path)
                .await
                .map_err(|error| {
                    DomainError::InternalError(format!(
                        "Failed to open chat publication stage {}: {error}",
                        publish_path.display()
                    ))
                })?;
            (file, publish_path, restored.size, restored.sha256)
        } else {
            (file, path.as_path(), accepted_offset, None)
        };
        let content_signature = content_hasher.map(|(epoch, hasher)| {
            let sha256 = if expands_cold_swipes {
                digest.expect("cold restoration computes the requested digest")
            } else {
                hasher.finalize().into()
            };
            (
                epoch,
                ContentSignature {
                    byte_len: size,
                    sha256,
                },
            )
        });

        let _write_guard = self.acquire_payload_mutation_lock(target_path).await;
        check_precondition(target_path, precondition).await?;
        if !force && !matches!(precondition, ChatPayloadPrecondition::Overwrite) {
            let existing_integrity = self.read_chat_integrity_if_exists(target_path).await?;
            verify_integrity_match(existing_integrity.as_deref(), incoming_integrity.as_deref())?;
        }
        persist_file(file, completed_path, target_path).await?;
        if let Some((epoch, signature)) = content_signature {
            self.record_current_content_signature(target_path, epoch, signature)
                .await;
        }
        read_payload_version(target_path).await?.ok_or_else(|| {
            DomainError::InternalError("Committed chat disappeared before versioning".into())
        })
    }
}

#[async_trait]
impl ChatCommitRepository for FileChatRepository {
    async fn open_payload_json(
        &self,
        target: ChatCommitTarget,
    ) -> Result<Box<dyn tt_ports::byte_reader::ByteReader>, DomainError> {
        let path = self.resolve_chat_commit_target(&target).await?;
        super::payload_reader::open_json_array(&path).await
    }

    async fn open_swipe_source(
        &self,
        target: ChatCommitTarget,
    ) -> Result<Arc<dyn ChatSwipeSource>, DomainError> {
        let path = self.resolve_chat_commit_target(&target).await?;
        super::cold_swipes::FileSwipeSource::open(&path).await
    }

    async fn open_payload_json_versioned(&self, target: ChatCommitTarget) -> Result<(Box<dyn tt_ports::byte_reader::ByteReader>, ChatPayloadVersion), DomainError> {
        let path = self.resolve_chat_commit_target(&target).await?;
        let _guard = self.acquire_payload_snapshot_lock(&path).await;
        let source = super::payload_reader::OpenedChatFile::open(&path).await?;
        let version = source.version().await?;
        Ok((super::payload_reader::json_array(source), version))
    }

    async fn open_swipe_source_versioned(&self, target: ChatCommitTarget) -> Result<(Arc<dyn ChatSwipeSource>, ChatPayloadVersion), DomainError> {
        let path = self.resolve_chat_commit_target(&target).await?;
        let _guard = self.acquire_payload_snapshot_lock(&path).await;
        super::cold_swipes::FileSwipeSource::open_versioned(&path).await
    }

    async fn begin_with_precondition(
        &self,
        target: ChatCommitTarget,
        operation: ChatCommitOperation,
        precondition: ChatPayloadPrecondition,
    ) -> Result<CommitBegin, DomainError> {
        let target_path = self.resolve_chat_commit_target(&target).await?;
        if let Some(parent) = target_path.parent() {
            fs::create_dir_all(parent).await.map_err(|error| {
                DomainError::InternalError(format!(
                    "Failed to create chat payload directory {}: {}",
                    parent.display(),
                    error
                ))
            })?;
        }
        let should_hash = self
            .backup_policy
            .try_read()
            .is_ok_and(|policy| policy.automatic_enabled && !policy.history_disabled());
        let content_hasher =
            if should_hash && matches!(operation, ChatCommitOperation::Payload { .. }) {
                match new_content_hasher() {
                    Some(hasher) => Some((self.current_content_signature_epoch().await, hasher)),
                    None => None,
                }
            } else {
                None
            };
        self.chat_commit_sessions
            .begin(CommitMetadata {
                target,
                target_path,
                operation,
                precondition,
                content_hasher,
            })
            .await
    }

    async fn read_version(
        &self,
        target: &ChatCommitTarget,
    ) -> Result<Option<ChatPayloadVersion>, DomainError> {
        let path = self.resolve_chat_commit_target(target).await?;
        let _guard = self.acquire_payload_snapshot_lock(&path).await;
        read_payload_version(&path).await
    }

    async fn append(
        &self,
        session_id: &str,
        offset: u64,
        bytes: &[u8],
    ) -> Result<u64, DomainError> {
        self.chat_commit_sessions
            .append(session_id, offset, bytes, |metadata| {
                if matches!(
                    metadata.operation,
                    ChatCommitOperation::Payload {
                        cold_source: None,
                        ..
                    }
                ) && let Some((_, hasher)) = metadata.content_hasher.as_mut()
                {
                    hasher.update(bytes);
                }
            })
            .await
    }

    async fn finish(
        &self,
        session_id: &str,
        expected_size: u64,
    ) -> Result<CommittedChat, DomainError> {
        self.chat_commit_sessions
            .finish(session_id, expected_size, |session| async move {
                let CommitSession { stage, metadata } = session;
                let CommitMetadata {
                    target,
                    target_path,
                    operation,
                    content_hasher,
                    precondition,
                } = metadata;
                let stage_path = stage.path.clone();
                let publish_path = stage.publish_path();
                let version = match operation {
                    ChatCommitOperation::Payload { force, cold_source } => {
                        self.commit_payload_stage(
                            &target_path,
                            stage,
                            &publish_path,
                            force,
                            cold_source,
                            content_hasher,
                            &precondition,
                        )
                        .await?
                    }
                    ChatCommitOperation::Metadata => {
                        drop(stage.file);
                        self.commit_metadata_stage(&target_path, &stage_path, &publish_path, None, &precondition)
                            .await?
                    }
                    ChatCommitOperation::MetadataExtension { namespace } => {
                        drop(stage.file);
                        self.commit_metadata_stage(
                            &target_path,
                            &stage_path,
                            &publish_path,
                            Some(namespace),
                            &precondition,
                        )
                        .await?
                    }
                };
                self.invalidate_chat_caches(&target, &target_path).await?;
                Ok(CommittedChat { target, version })
            })
            .await
    }

    async fn abort(&self, session_id: &str) -> Result<(), DomainError> {
        self.chat_commit_sessions.abort(session_id).await
    }
}

/// Read actual bytes, not the backup signature cache: external edits must invalidate CAS.
pub(super) fn read_payload_version_blocking(path: &Path) -> Result<Option<ChatPayloadVersion>, DomainError> {
    use std::io::Read;
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(DomainError::InternalError(format!("Failed to open chat revision: {error}"))),
    };
    let mut hasher = Sha256::new();
    let mut byte_len = 0;
    let mut buffer = [0u8; 65536];
    loop {
        let count = file.read(&mut buffer).map_err(|error| DomainError::InternalError(format!("Failed to read chat revision: {error}")))?;
        if count == 0 { break; }
        byte_len += count as u64;
        hasher.update(&buffer[..count]);
    }
    Ok(Some(ChatPayloadVersion { byte_len, sha256_hex: sha256_hex(hasher.finalize().as_slice()) }))
}

async fn read_payload_version(path: &Path) -> Result<Option<ChatPayloadVersion>, DomainError> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || read_payload_version_blocking(&path)).await
        .map_err(|error| DomainError::InternalError(format!("Chat revision task failed: {error}")))?
}

pub(super) async fn check_precondition(path: &Path, precondition: &ChatPayloadPrecondition) -> Result<(), DomainError> {
    let matches = match precondition {
        ChatPayloadPrecondition::IntegritySlug | ChatPayloadPrecondition::Overwrite => true,
        ChatPayloadPrecondition::MustNotExist => read_payload_version(path).await?.is_none(),
        ChatPayloadPrecondition::MatchesVersion(expected) => read_payload_version(path).await?.as_ref() == Some(expected),
    };
    if matches { Ok(()) } else { Err(DomainError::Conflict("Chat changed since it was loaded; reload or explicitly overwrite".into())) }
}

pub(super) fn sha256_hex(digest: &[u8]) -> String {
    use std::fmt::Write;
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut hex, "{byte:02x}").expect("writing to a String cannot fail");
    }
    hex
}
