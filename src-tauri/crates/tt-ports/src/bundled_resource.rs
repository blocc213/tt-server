use tt_domain::errors::DomainError;

/// Reads packaged default content shipped alongside the binary.
///
/// The Tauri host resolves these through its resource directory; the server host
/// resolves them relative to the executable or an explicit `--resources-dir`.
/// Repositories that seed default content depend on this port instead of a
/// concrete host runtime.
pub trait BundledResourceStore: Send + Sync {
    fn read_bytes(&self, relative_path: &str) -> Result<Vec<u8>, DomainError>;

    fn read_text(&self, relative_path: &str) -> Result<String, DomainError> {
        let bytes = self.read_bytes(relative_path)?;
        String::from_utf8(bytes).map_err(|error| {
            DomainError::InvalidData(format!(
                "Resource '{relative_path}' is not valid UTF-8: {error}"
            ))
        })
    }

    /// Lists packaged `default/content` entries under `prefix`.
    ///
    /// Directory entries in `index.json` expand through this listing, so the
    /// manifest must describe the same files the host can actually read.
    fn list_default_content_files_under(&self, prefix: &str) -> Vec<String>;
}
