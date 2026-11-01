use std::path::PathBuf;
use std::sync::Arc;

use crate::auth::Auth;
use crate::composition::ServerServices;
use crate::upload::SharedUploadStaging;

pub struct AppState {
    pub services: ServerServices,
    pub auth: Auth,
    pub frontend_dir: PathBuf,
    /// Opaque per-process token echoed by `/csrf-token`.
    ///
    /// The frontend requires the endpoint to exist and to return a non-empty
    /// string before it will continue booting (`src/script.js:1072-1080`).
    /// Same-origin request authority already comes from the session cookie.
    pub csrf_token: String,
    pub upload_staging: SharedUploadStaging,
    pub browser_resources: crate::browser_resources::BrowserResources,
    pub templates: std::sync::OnceLock<tt_application::services::bundled_template_service::BundledTemplateService>,
}

impl AppState {
    pub fn templates(&self) -> &tt_application::services::bundled_template_service::BundledTemplateService {
        self.templates.get_or_init(|| tt_application::services::bundled_template_service::BundledTemplateService::new(
            Arc::new(crate::resources::FrontendTemplateStore(self.frontend_dir.clone())),
        ))
    }
}

pub type SharedState = Arc<AppState>;
