//! Command-line configuration.
//!
//! Defaults follow upstream SillyTavern's posture (`default/config.yaml`:
//! `listen: false`): bind loopback unless the operator explicitly opts into a
//! wider interface, and refuse to expose a non-loopback socket without a
//! password.

use std::net::IpAddr;
use std::path::PathBuf;

use clap::Parser;

#[derive(Debug, Parser)]
#[command(
    name = "tauritavern-server",
    about = "Serve TauriTavern to browsers on your network"
)]
pub struct Args {
    /// Data root holding `default-user/`, characters, chats, and settings.
    #[arg(long, value_name = "DIR")]
    pub data_dir: PathBuf,

    /// Directory containing the built frontend (the repository `src/` tree).
    #[arg(long, value_name = "DIR")]
    pub frontend_dir: PathBuf,

    /// Packaged resources root containing `default/` and `frontend-templates/`.
    #[arg(long, value_name = "DIR")]
    pub resources_dir: Option<PathBuf>,

    /// Address to bind. Defaults to loopback only.
    #[arg(long, default_value = "127.0.0.1")]
    pub host: IpAddr,

    #[arg(long, default_value_t = 8000)]
    pub port: u16,

    /// Access password. Required whenever `--host` is not a loopback address.
    ///
    /// Falls back to the `TAURITAVERN_PASSWORD` environment variable.
    #[arg(long, value_name = "PASSWORD")]
    pub password: Option<String>,

    /// Allow binding a non-loopback address without a password.
    ///
    /// Only for trusted private networks fronted by another authenticator.
    #[arg(long)]
    pub insecure_no_password: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error(
        "refusing to listen on {0} without --password. \
         Set --password (or TAURITAVERN_PASSWORD), or pass --insecure-no-password \
         if another layer already authenticates this port."
    )]
    MissingPassword(IpAddr),

    #[error("--password must not be empty")]
    EmptyPassword,

    #[error("data directory not found: {0}")]
    MissingDataDir(PathBuf),

    #[error("frontend directory has no index.html: {0}")]
    MissingFrontend(PathBuf),
}

impl Args {
    pub fn validate(&self) -> Result<(), ConfigError> {
        if let Some(password) = &self.password
            && password.trim().is_empty()
        {
            return Err(ConfigError::EmptyPassword);
        }

        if !self.host.is_loopback() && self.password.is_none() && !self.insecure_no_password {
            return Err(ConfigError::MissingPassword(self.host));
        }

        if !self.data_dir.is_dir() {
            return Err(ConfigError::MissingDataDir(self.data_dir.clone()));
        }

        if !self.frontend_dir.join("index.html").is_file() {
            return Err(ConfigError::MissingFrontend(self.frontend_dir.clone()));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(host: &str, password: Option<&str>, insecure: bool) -> Args {
        Args {
            data_dir: PathBuf::from("."),
            frontend_dir: PathBuf::from("."),
            resources_dir: None,
            host: host.parse().expect("host"),
            port: 8000,
            password: password.map(ToString::to_string),
            insecure_no_password: insecure,
        }
    }

    #[test]
    fn public_bind_without_password_is_rejected() {
        let error = args("0.0.0.0", None, false)
            .validate()
            .expect_err("public bind must require a password");
        assert!(matches!(error, ConfigError::MissingPassword(_)));
    }

    #[test]
    fn loopback_bind_without_password_is_allowed() {
        // Reaches the directory checks, which means the password gate passed.
        let error = args("127.0.0.1", None, false).validate().expect_err("cwd");
        assert!(!matches!(error, ConfigError::MissingPassword(_)));
    }

    #[test]
    fn empty_password_is_rejected() {
        let error = args("127.0.0.1", Some("   "), false)
            .validate()
            .expect_err("empty password must be rejected");
        assert!(matches!(error, ConfigError::EmptyPassword));
    }
}
