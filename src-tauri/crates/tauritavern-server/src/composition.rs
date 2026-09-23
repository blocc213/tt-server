//! Server composition root.
//!
//! Mirrors the Tauri host's service graph for the subset the browser server
//! needs. Native-only subsystems (LAN Sync, data archive, dev observability,
//! notifications) are intentionally absent rather than stubbed: a server build
//! should fail to expose them, not pretend they work.

use std::path::Path;
use std::sync::Arc;

use tokio::sync::Semaphore;

use tt_adapter_extension::FileExtensionRepository;
use tt_adapter_http::{HttpClientPool, HttpExternalImportDownloader};
use tt_adapter_media::{
    FileAvatarRepository, FileBackgroundRepository, FileImageMetadataRepository,
    FilesystemHostResourceStore,
};
use tt_adapter_provider_http::HttpChatCompletionRepository;
use tt_adapter_storage_core::{
    DataDirectory, FileChatRepository, FileContentRepository, FileExtensionStoreRepository,
    FileGroupRepository, FileLlmConnectionRepository, FilePresetRepository,
    FilePromptCacheRepository, FileQuickReplyRepository, FileSecretRepository,
    FileSettingsRepository, FileThemeRepository,
    chat_directory_identity::new_shared_chat_alias_store_for_user_dir,
};
use tt_adapter_storage_userdata::{
    FileAgentProfileRepository, FileAgentRepository, FileCharacterRepository, FileSkillRepository,
    FileWorldInfoRepository,
};
use tt_adapter_tokenization::MiktikTokenizerRepository;
use tt_application::services::agent_model_gateway::ChatCompletionAgentModelGateway;
use tt_application::services::agent_profile_diagnostic_service::AgentProfileDiagnosticService;
use tt_application::services::agent_profile_service::AgentProfileService;
use tt_application::services::agent_run_history_service::AgentRunHistoryService;
use tt_application::services::agent_run_retention_automation_service::AgentRunRetentionAutomationService;
use tt_application::services::agent_runtime_service::AgentRuntimeService;
use tt_application::services::agent_workspace_lifecycle_service::{
    AgentRunActivity, AgentWorkspaceLifecycleService,
};
use tt_application::services::avatar_service::AvatarService;
use tt_application::services::background_service::BackgroundService;
use tt_application::services::character_service::CharacterService;
use tt_application::services::chat_completion_service::ChatCompletionService;
use tt_application::services::chat_history_coordinator::ChatHistoryCoordinator;
use tt_application::services::chat_payload_commit_service::ChatPayloadCommitService;
use tt_application::services::chat_service::ChatService;
use tt_application::services::content_service::ContentService;
use tt_application::services::extension_service::ExtensionService;
use tt_application::services::extension_store_service::ExtensionStoreService;
use tt_application::services::group_service::GroupService;
use tt_application::services::host_resource_service::HostResourceService;
use tt_application::services::image_metadata_service::ImageMetadataService;
use tt_application::services::llm_connection_service::LlmConnectionService;
use tt_application::services::native_regex_service::NativeRegexService;
use tt_application::services::preset_service::PresetService;
use tt_application::services::prompt_assembly_service::PromptAssemblyService;
use tt_application::services::quick_reply_service::QuickReplyService;
use tt_application::services::secret_service::SecretService;
use tt_application::services::settings_service::{RequestProxyRuntime, SettingsService};
use tt_application::services::skill_service::SkillService;
use tt_application::services::theme_service::ThemeService;
use tt_application::services::tokenization_service::TokenizationService;
use tt_application::services::world_info_service::WorldInfoService;
use tt_domain::errors::DomainError;
use tt_domain::ios_policy::{
    IosPolicyActivationReport, IosPolicyScope, resolve_ios_policy_activation_report,
};
use tt_domain::models::settings::TauriTavernSettings;

use crate::resources::DirectoryResourceStore;

/// Long-lived services shared by every HTTP request. Only services with a
/// browser-reachable handler are built; the app host's graph is the reference
/// when adding more.
pub struct ServerServices {
    pub character_service: Arc<CharacterService>,
    pub chat_service: Arc<ChatService>,
    pub chat_history_coordinator: Arc<ChatHistoryCoordinator>,
    pub chat_payload_commit_service: Arc<ChatPayloadCommitService>,
    pub settings_service: Arc<SettingsService>,
    pub secret_service: Arc<SecretService>,
    pub skill_service: Arc<SkillService>,
    pub content_service: Arc<ContentService>,
    pub extension_service: Arc<ExtensionService>,
    pub extension_store_service: Arc<ExtensionStoreService>,
    pub avatar_service: Arc<AvatarService>,
    pub group_service: Arc<GroupService>,
    pub background_service: Arc<BackgroundService>,
    pub image_metadata_service: Arc<ImageMetadataService>,
    pub theme_service: Arc<ThemeService>,
    pub preset_service: Arc<PresetService>,
    pub quick_reply_service: Arc<QuickReplyService>,
    pub agent_profile_service: Arc<AgentProfileService>,
    pub agent_profile_diagnostic_service: Arc<AgentProfileDiagnosticService>,
    pub prompt_assembly_service: Arc<PromptAssemblyService>,
    pub agent_run_history_service: Arc<AgentRunHistoryService>,
    pub agent_run_retention_automation_service: Arc<AgentRunRetentionAutomationService>,
    pub agent_runtime_service: Arc<AgentRuntimeService>,
    pub chat_completion_service: Arc<ChatCompletionService>,
    pub llm_connection_service: Arc<LlmConnectionService>,
    pub tokenization_service: Arc<TokenizationService>,
    pub world_info_service: Arc<WorldInfoService>,
    pub native_regex_service: Arc<NativeRegexService>,
    pub host_resource_service: Arc<HostResourceService>,
    pub ios_policy: IosPolicyActivationReport,
}

pub async fn build(
    data_root: &Path,
    resource_root: std::path::PathBuf,
    settings: TauriTavernSettings,
    user_agent: &'static str,
) -> Result<ServerServices, DomainError> {
    let data_directory = DataDirectory::new(data_root.to_path_buf());
    data_directory.initialize().await?;

    let resources = Arc::new(DirectoryResourceStore::new(resource_root));
    let http_client_pool = Arc::new(HttpClientPool::new(user_agent));
    http_client_pool.apply_request_proxy_settings(&settings.request_proxy)?;

    let data_root = data_directory.root().to_path_buf();
    let default_user_dir = data_directory.default_user().to_path_buf();
    let chat_aliases = new_shared_chat_alias_store_for_user_dir(data_directory.default_user());

    let file_chat_repository = Arc::new(FileChatRepository::with_chat_aliases_and_backup_settings(
        data_directory.characters().to_path_buf(),
        data_directory.chats().to_path_buf(),
        data_directory.group_chats().to_path_buf(),
        data_directory.backups().to_path_buf(),
        chat_aliases.clone(),
        settings.chat_backups,
    ));
    if let Err(error) = file_chat_repository
        .cleanup_orphaned_chat_commit_staging()
        .await
    {
        tracing::warn!(%error, "Failed to clean orphaned chat commit staging");
    }

    let character_repository = Arc::new(FileCharacterRepository::with_chat_repository(
        data_directory.characters().to_path_buf(),
        data_directory.chats().to_path_buf(),
        data_directory.default_avatar().to_path_buf(),
        chat_aliases,
        file_chat_repository.clone(),
    ));

    let content_repository = Arc::new(FileContentRepository::new(
        resources.clone(),
        data_root.clone(),
        default_user_dir.clone(),
    ));
    let preset_repository = Arc::new(FilePresetRepository::new(
        resources.clone(),
        default_user_dir.clone(),
        content_repository.clone(),
    ));
    let settings_repository = Arc::new(FileSettingsRepository::new(
        data_directory.settings().to_path_buf(),
    ));
    let secret_repository = Arc::new(FileSecretRepository::new(
        default_user_dir.join("secrets.json"),
    ));
    let world_info_repository = Arc::new(FileWorldInfoRepository::new(
        data_directory.default_user().join("worlds"),
    ));
    let file_agent_repository = Arc::new(FileAgentRepository::new(
        data_root.join("_tauritavern").join("agent-workspaces"),
    ));
    let agent_profile_repository = Arc::new(FileAgentProfileRepository::new(
        data_root.join("_tauritavern").join("agent-profiles"),
    ));
    let llm_connection_repository = Arc::new(FileLlmConnectionRepository::new(
        data_root.join("_tauritavern").join("llm-connections"),
    ));
    let skill_repository = Arc::new(FileSkillRepository::new(
        data_root.join("_tauritavern").join("skills"),
    ));
    let prompt_cache_repository = Arc::new(FilePromptCacheRepository::new(
        data_root.join("_tauritavern").join("prompt-cache"),
    ));

    let chat_completion_repository =
        Arc::new(HttpChatCompletionRepository::new(http_client_pool.clone()));

    // Services. Ordering follows the Tauri host so the two graphs stay comparable.
    let content_service = Arc::new(ContentService::new(
        content_repository.clone(),
        Arc::new(HttpExternalImportDownloader::new(http_client_pool.clone())),
    ));
    let local_mutation_gate = Arc::new(Semaphore::new(1));
    let extension_service = Arc::new(ExtensionService::new(
        Arc::new(FileExtensionRepository::new(
            default_user_dir.join("extensions"),
            data_directory.global_extensions().to_path_buf(),
            data_directory.extension_sources().to_path_buf(),
            http_client_pool.clone(),
        )),
        local_mutation_gate,
    ));
    let extension_store_service = Arc::new(ExtensionStoreService::new(Arc::new(
        FileExtensionStoreRepository::new(data_root.join("_tauritavern").join("extension-store")),
    )));
    let avatar_service = Arc::new(AvatarService::new(Arc::new(FileAvatarRepository::new(
        default_user_dir.join("User Avatars"),
    ))));
    let image_metadata_repository = Arc::new(FileImageMetadataRepository::new(
        default_user_dir.clone(),
        data_directory.default_user().join("backgrounds"),
    ));
    let image_metadata_service =
        Arc::new(ImageMetadataService::new(image_metadata_repository.clone()));
    let background_service = Arc::new(BackgroundService::new(
        Arc::new(FileBackgroundRepository::new(
            data_directory.default_user().join("backgrounds"),
        )),
        image_metadata_repository,
    ));
    let theme_service = Arc::new(ThemeService::new(Arc::new(FileThemeRepository::new(
        default_user_dir.join("themes"),
    ))));
    let preset_service = Arc::new(PresetService::new(preset_repository.clone()));
    let quick_reply_service = Arc::new(QuickReplyService::new(Arc::new(
        FileQuickReplyRepository::new(data_directory.default_user().join("QuickReplies")),
    )));
    let skill_service = Arc::new(SkillService::with_external_import_downloader(
        skill_repository,
        Arc::new(HttpExternalImportDownloader::new(http_client_pool.clone())),
    ));
    let llm_connection_service = Arc::new(LlmConnectionService::new(llm_connection_repository));
    // The server never runs under the iOS distribution policy; take the unrestricted
    // baseline rather than inventing a server-specific capability matrix.
    let ios_policy = resolve_ios_policy_activation_report(IosPolicyScope::Ignored, None)?;
    let chat_completion_service = Arc::new(ChatCompletionService::new(
        chat_completion_repository,
        secret_repository.clone(),
        settings_repository.clone(),
        prompt_cache_repository,
        ios_policy.clone(),
    ));

    let agent_profile_service = Arc::new(AgentProfileService::new(
        agent_profile_repository.clone(),
        agent_profile_repository,
        preset_repository.clone(),
    ));
    let agent_profile_diagnostic_service = Arc::new(AgentProfileDiagnosticService::new(
        agent_profile_service.clone(),
        preset_repository.clone(),
        llm_connection_service.clone(),
    ));
    let prompt_assembly_service = Arc::new(PromptAssemblyService::new(
        agent_profile_service.clone(),
        preset_repository,
        llm_connection_service.clone(),
    ));
    let agent_runtime_service = Arc::new(AgentRuntimeService::new(
        file_agent_repository.clone(),
        file_agent_repository.clone(),
        file_agent_repository.clone(),
        file_agent_repository.clone(),
        file_chat_repository.clone(),
        file_chat_repository.clone(),
        skill_service.clone(),
        Arc::new(ChatCompletionAgentModelGateway::new(
            chat_completion_service.clone(),
        )),
        agent_profile_service.clone(),
        llm_connection_service.clone(),
        prompt_assembly_service.clone(),
    ));
    let agent_run_history_service = Arc::new(AgentRunHistoryService::new(
        file_agent_repository.clone(),
        settings_repository.clone(),
        agent_runtime_service.clone() as Arc<dyn AgentRunActivity>,
    ));
    let agent_run_retention_automation_service = Arc::new(AgentRunRetentionAutomationService::new(
        settings_repository.clone(),
        agent_run_history_service.clone(),
    ));
    let agent_workspace_lifecycle_service = Arc::new(AgentWorkspaceLifecycleService::new(
        file_agent_repository,
        agent_runtime_service.clone() as Arc<dyn AgentRunActivity>,
    ));

    let chat_history_coordinator = Arc::new(ChatHistoryCoordinator::new(
        file_chat_repository.clone(),
        file_chat_repository.clone(),
    ));
    let chat_payload_commit_service = Arc::new(ChatPayloadCommitService::new(
        file_chat_repository.clone(),
        chat_history_coordinator.clone(),
    ));
    let group_service = Arc::new(GroupService::new(
        Arc::new(FileGroupRepository::new(
            data_directory.groups().to_path_buf(),
            data_directory.group_chats().to_path_buf(),
        )),
        agent_workspace_lifecycle_service.clone(),
        chat_history_coordinator.clone(),
    ));
    let character_service = Arc::new(CharacterService::new(
        character_repository,
        file_chat_repository.clone(),
        world_info_repository.clone(),
        agent_workspace_lifecycle_service.clone(),
        chat_history_coordinator.clone(),
    ));
    let chat_service = Arc::new(ChatService::new(
        file_chat_repository.clone(),
        Arc::new(FileCharacterRepository::new(
            data_directory.characters().to_path_buf(),
            data_directory.chats().to_path_buf(),
            data_directory.default_avatar().to_path_buf(),
        )),
        agent_workspace_lifecycle_service,
        chat_history_coordinator.clone(),
    ));
    let secret_service = Arc::new(SecretService::new(
        secret_repository,
        settings.allow_keys_exposure,
    ));
    let request_proxy_runtime: Arc<dyn RequestProxyRuntime> = http_client_pool.clone();
    let settings_service = Arc::new(SettingsService::new(
        settings_repository,
        request_proxy_runtime,
        file_chat_repository,
    ));

    let host_resource_service = Arc::new(HostResourceService::new(
        settings.avatar_persona_original_images_enabled,
        Arc::new(FilesystemHostResourceStore::from_data_root(&data_root)),
    ));

    Ok(ServerServices {
        character_service,
        chat_service,
        chat_history_coordinator,
        chat_payload_commit_service,
        settings_service,
        secret_service,
        skill_service,
        content_service,
        extension_service,
        extension_store_service,
        avatar_service,
        group_service,
        background_service,
        image_metadata_service,
        theme_service,
        preset_service,
        quick_reply_service,
        agent_profile_service,
        agent_profile_diagnostic_service,
        prompt_assembly_service,
        agent_run_history_service,
        agent_run_retention_automation_service,
        agent_runtime_service,
        chat_completion_service,
        llm_connection_service,
        tokenization_service: Arc::new(TokenizationService::new(Arc::new(
            MiktikTokenizerRepository::new(
                data_root.join("_cache").join("tokenizers"),
                http_client_pool,
            ),
        ))),
        world_info_service: Arc::new(WorldInfoService::new(world_info_repository)),
        native_regex_service: Arc::new(NativeRegexService::new()),
        host_resource_service,
        ios_policy,
    })
}
