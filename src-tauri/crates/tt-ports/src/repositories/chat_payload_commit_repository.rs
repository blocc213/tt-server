use async_trait::async_trait;
use tt_domain::errors::DomainError;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ChatPayloadTarget {
    Character {
        character_id: String,
        file_name: String,
    },
    Group {
        chat_id: String,
    },
}

/// Identifies one stored revision of a chat payload file.
///
/// Browsers on different devices read and write the same file, so a save has to
/// prove it is replacing the revision it loaded. The token is derived from the
/// stored bytes (length plus digest), which means it needs no extra sidecar
/// state and stays correct if the file is edited outside the app.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChatPayloadVersion {
    pub byte_len: u64,
    pub sha256_hex: String,
}

/// Precondition applied under the payload write lock.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ChatPayloadPrecondition {
    /// Upstream SillyTavern's default: only the `chat_metadata.integrity` slug is
    /// compared, which catches writing one chat over a different chat's file but
    /// does not detect a newer revision of the same chat.
    IntegritySlug,
    /// The target must not exist yet. Used for brand new chats.
    MustNotExist,
    /// The target must currently hold exactly this revision.
    MatchesVersion(ChatPayloadVersion),
    /// Replace whatever is stored, skipping every check.
    ///
    /// Only reachable through an explicit user confirmation.
    Overwrite,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChatPayloadCommitBegin {
    pub session_id: String,
    pub max_frame_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommittedChatPayload {
    pub target: ChatPayloadTarget,
    pub size: u64,
    /// Revision just published, so the caller can save again without re-reading.
    pub version: ChatPayloadVersion,
}

/// Streams a complete chat payload into private, target-volume staging and
/// publishes it atomically when the session is finished.
#[async_trait]
pub trait ChatPayloadCommitRepository: Send + Sync {
    /// Opens a staged write.
    ///
    /// `precondition` is evaluated in `finish`, under the same lock that
    /// publishes the file, so a concurrent save cannot land in between.
    async fn begin(
        &self,
        target: ChatPayloadTarget,
        precondition: ChatPayloadPrecondition,
    ) -> Result<ChatPayloadCommitBegin, DomainError>;

    /// Reads the current revision of a stored payload, if the file exists.
    async fn read_version(
        &self,
        target: &ChatPayloadTarget,
    ) -> Result<Option<ChatPayloadVersion>, DomainError>;

    async fn append(&self, session_id: &str, offset: u64, bytes: &[u8])
    -> Result<u64, DomainError>;

    async fn finish(
        &self,
        session_id: &str,
        expected_size: u64,
    ) -> Result<CommittedChatPayload, DomainError>;

    /// Aborting an absent or already-consumed session is a successful no-op.
    async fn abort(&self, session_id: &str) -> Result<(), DomainError>;
}
