//! TauriTavern served over HTTP.
//!
//! Reuses the same Rust core as the desktop/mobile app; only the host shell
//! differs. Native-only capabilities are not exposed here.

mod auth;
mod chat;
mod composition;
mod config;
mod error;
mod generate;
mod http;
mod product;
mod resource;
mod resources;
mod rpc;
mod state;
mod upload;

use std::net::SocketAddr;
use std::sync::Arc;

use clap::Parser;
use rand::RngCore;
use tt_adapter_storage_core::FileSettingsRepository;

use crate::auth::Auth;
use crate::config::Args;
use crate::resources::DirectoryResourceStore;
use crate::state::AppState;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,tauritavern_server=debug".into()),
        )
        .init();

    let mut args = Args::parse();
    if args.password.is_none()
        && let Ok(from_env) = std::env::var("TAURITAVERN_PASSWORD")
        && !from_env.trim().is_empty()
    {
        args.password = Some(from_env);
    }
    args.validate()?;

    // Startup settings come from the same file the app uses, so a data root can
    // move between the app and the server without reconfiguration.
    let settings_repository = FileSettingsRepository::new(args.data_dir.join("default-user"));
    let settings = settings_repository.load_tauritavern_settings_sync()?;
    settings
        .chat_backups
        .validate()
        .map_err(|error| error.message())?;

    let resource_root = DirectoryResourceStore::resolve_root(args.resources_dir.clone())?;
    tracing::info!(resources = %resource_root.display(), "Using packaged resources");

    let services =
        composition::build(&args.data_dir, resource_root, settings, product::USER_AGENT).await?;

    // Seeding default content is part of first-run setup in the app host too;
    // without it a fresh data root has no presets or default character.
    services
        .content_service
        .initialize_default_content("default-user")
        .await?;

    let state = Arc::new(AppState {
        services,
        auth: Auth::new(args.password.clone()),
        frontend_dir: args.frontend_dir.clone(),
        csrf_token: random_token(),
        upload_staging: Arc::new(upload::UploadStaging::new(&args.data_dir)),
    });

    let chat_history = state.services.chat_history_coordinator.clone();
    let chat_history_cancel = tokio_util::sync::CancellationToken::new();
    let chat_history_task = tokio::spawn({
        let cancel = chat_history_cancel.clone();
        async move { chat_history.run(cancel).await }
    });

    let addr = SocketAddr::new(args.host, args.port);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let bound = listener.local_addr()?;

    if state.auth.is_open() {
        tracing::warn!("No password configured: anyone who can reach this port has full access");
    }
    tracing::info!("TauriTavern server listening on http://{bound}");

    axum::serve(listener, http::router(state.clone()))
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    chat_history_cancel.cancel();
    let _ = chat_history_task.await;
    tracing::info!("Server stopped");
    Ok(())
}

fn random_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("Shutdown requested");
}
