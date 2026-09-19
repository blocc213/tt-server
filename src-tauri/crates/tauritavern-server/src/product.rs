pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const USER_AGENT: &str = concat!("TauriTavern/", env!("CARGO_PKG_VERSION"));

/// Upstream SillyTavern release this frontend tracks.
///
/// Must match `src/compat-version.js`; extensions compare against it.
pub const SILLYTAVERN_COMPAT_VERSION: &str = "1.18.0";
