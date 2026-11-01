use crate::byte_reader::ByteReader;
use async_trait::async_trait;
use std::path::Path;
use std::sync::Arc;
use tt_contracts::byte_commit::CommitBegin;
use tt_domain::errors::DomainError;

/// An opened chat file retained by the current page, independent of later path replacements.
#[async_trait]
pub trait ChatSwipeSource: Send + Sync {
    fn projection(self: Arc<Self>, source_id: u32) -> Box<dyn ByteReader>;
    async fn record(self: Arc<Self>, record: usize) -> Result<Box<dyn ByteReader>, DomainError>;
    /// Adapter-internal expansion of a staged payload; application services do not call this
    /// operation or handle its file paths.
    async fn restore_payload(
        self: Arc<Self>,
        source_id: u32,
        input: &Path,
        output: &Path,
        hash: bool,
    ) -> Result<RestoredChatPayload, DomainError>;
}

#[derive(Clone)]
pub struct ColdSwipeCommitSource {
    pub id: u32,
    pub source: Arc<dyn ChatSwipeSource>,
}

/// What the staged JSON changes. Transport and publication share one lifecycle.
pub enum ChatCommitOperation {
    /// Replace the complete JSONL, optionally restoring unloaded swipe content.
    Payload {
        force: bool,
        cold_source: Option<ColdSwipeCommitSource>,
    },
    /// Replace `chat_metadata` on an existing file, retaining header fields and body bytes.
    Metadata,
    /// Set one namespace on the latest disk metadata; a staged JSON null deletes it.
    MetadataExtension { namespace: String },
}

pub struct RestoredChatPayload {
    pub size: u64,
    pub sha256: Option<[u8; 32]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ChatCommitTarget {
    Character {
        character_id: String,
        file_name: String,
    },
    Group {
        chat_id: String,
    },
}

/// Revision of the stored JSONL bytes, independent of its integrity header.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChatPayloadVersion {
    pub byte_len: u64,
    pub sha256_hex: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ChatPayloadPrecondition {
    IntegritySlug,
    MustNotExist,
    MatchesVersion(ChatPayloadVersion),
    Overwrite,
}

#[derive(Debug)]
pub struct CommittedChat {
    pub target: ChatCommitTarget,
    pub version: ChatPayloadVersion,
}

/// Target-addressed chat file reads and atomic payload or metadata commits.
#[async_trait]
pub trait ChatCommitRepository: Send + Sync {
    async fn open_payload_json(
        &self,
        target: ChatCommitTarget,
    ) -> Result<Box<dyn ByteReader>, DomainError>;

    async fn open_swipe_source(
        &self,
        target: ChatCommitTarget,
    ) -> Result<Arc<dyn ChatSwipeSource>, DomainError>;

    async fn open_payload_json_versioned(&self, target: ChatCommitTarget) -> Result<(Box<dyn ByteReader>, ChatPayloadVersion), DomainError>;
    async fn open_swipe_source_versioned(&self, target: ChatCommitTarget) -> Result<(Arc<dyn ChatSwipeSource>, ChatPayloadVersion), DomainError>;
    async fn begin(
        &self,
        target: ChatCommitTarget,
        operation: ChatCommitOperation,
    ) -> Result<CommitBegin, DomainError> {
        self.begin_with_precondition(target, operation, ChatPayloadPrecondition::IntegritySlug).await
    }

    async fn begin_with_precondition(
        &self,
        target: ChatCommitTarget,
        operation: ChatCommitOperation,
        precondition: ChatPayloadPrecondition,
    ) -> Result<CommitBegin, DomainError>;

    async fn read_version(
        &self,
        target: &ChatCommitTarget,
    ) -> Result<Option<ChatPayloadVersion>, DomainError>;

    async fn append(&self, session_id: &str, offset: u64, bytes: &[u8])
    -> Result<u64, DomainError>;

    async fn finish(
        &self,
        session_id: &str,
        expected_size: u64,
    ) -> Result<CommittedChat, DomainError>;

    /// Aborting an absent or already-consumed session is a successful no-op.
    async fn abort(&self, session_id: &str) -> Result<(), DomainError>;
}
