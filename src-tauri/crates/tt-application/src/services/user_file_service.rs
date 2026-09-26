use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use serde::Serialize;
use url::Url;

use tt_domain::errors::DomainError;
use tt_ports::user_file::UserFileStore;

const UNSAFE_EXTENSIONS: &[&str] = &[
    ".php",
    ".exe",
    ".com",
    ".dll",
    ".pif",
    ".application",
    ".gadget",
    ".msi",
    ".jar",
    ".cmd",
    ".bat",
    ".reg",
    ".sh",
    ".py",
    ".js",
    ".jse",
    ".jsp",
    ".pdf",
    ".html",
    ".htm",
    ".hta",
    ".vb",
    ".vbs",
    ".vbe",
    ".cpl",
    ".msc",
    ".scr",
    ".sql",
    ".iso",
    ".img",
    ".dmg",
    ".ps1",
    ".ps1xml",
    ".ps2",
    ".ps2xml",
    ".psc1",
    ".psc2",
    ".msh",
    ".msh1",
    ".msh2",
    ".mshxml",
    ".msh1xml",
    ".msh2xml",
    ".scf",
    ".lnk",
    ".inf",
    ".doc",
    ".docm",
    ".docx",
    ".dot",
    ".dotm",
    ".dotx",
    ".xls",
    ".xlsm",
    ".xlsx",
    ".xlt",
    ".xltm",
    ".xltx",
    ".xlam",
    ".ppt",
    ".pptm",
    ".pptx",
    ".pot",
    ".potm",
    ".potx",
    ".ppam",
    ".ppsx",
    ".ppsm",
    ".pps",
    ".sldx",
    ".sldm",
    ".ws",
];

#[derive(Debug, Serialize)]
pub struct UserFileUploadResult {
    pub path: String,
}

pub struct UserFileService {
    store: Arc<dyn UserFileStore>,
}

impl UserFileService {
    pub fn new(store: Arc<dyn UserFileStore>) -> Self {
        Self { store }
    }

    pub async fn upload_user_file(
        &self,
        name: &str,
        data_base64: &str,
    ) -> Result<UserFileUploadResult, DomainError> {
        let name = validate_upload_name(name)?;
        let bytes = BASE64_STANDARD
            .decode(data_base64.as_bytes())
            .map_err(|error| {
                DomainError::InvalidData(format!("No upload data specified: {error}"))
            })?;
        self.store.write_file(name, bytes).await?;
        Ok(UserFileUploadResult {
            path: format!("/user/files/{name}"),
        })
    }

    pub async fn delete_user_file(&self, path: &str) -> Result<(), DomainError> {
        let relative = normalize_user_file_reference(path)?;
        self.store.delete_file(&relative).await
    }

    pub async fn verify_user_files(
        &self,
        urls: Vec<String>,
    ) -> Result<HashMap<String, bool>, DomainError> {
        let mut result = HashMap::with_capacity(urls.len());
        for original_url in urls {
            let Ok(relative) = normalize_user_file_reference(&original_url) else {
                continue;
            };
            result.insert(original_url, self.store.is_file(&relative).await?);
        }
        Ok(result)
    }
}

fn normalize_relative_path(raw: &str) -> Result<PathBuf, DomainError> {
    let normalized = raw.replace('\\', "/");
    let normalized = normalized.trim().trim_start_matches('/');
    if normalized.is_empty() {
        return Err(DomainError::InvalidData("File path cannot be empty".into()));
    }

    let mut path = PathBuf::new();
    for component in Path::new(normalized).components() {
        match component {
            Component::Normal(segment) => path.push(segment),
            _ => return Err(DomainError::InvalidData("Invalid file path".into())),
        }
    }
    if path.as_os_str().is_empty() {
        return Err(DomainError::InvalidData("File path cannot be empty".into()));
    }
    Ok(path)
}

fn validate_upload_name(raw: &str) -> Result<&str, DomainError> {
    let name = raw.trim();
    if name.is_empty() {
        return Err(DomainError::InvalidData("No upload name specified".into()));
    }
    if name.starts_with('.') {
        return Err(DomainError::InvalidData(
            "Filename cannot start with '.'".into(),
        ));
    }
    if name.contains('/') || name.contains('\\') {
        return Err(DomainError::InvalidData(
            "Illegal character in filename".into(),
        ));
    }
    if !name
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' || ch == '.')
    {
        return Err(DomainError::InvalidData(
            "Illegal character in filename; only alphanumeric, '_', '-' are accepted.".into(),
        ));
    }
    let extension = Path::new(name)
        .extension()
        .map(|ext| format!(".{}", ext.to_string_lossy().to_lowercase()))
        .unwrap_or_default();
    if UNSAFE_EXTENSIONS.contains(&extension.as_str()) {
        return Err(DomainError::InvalidData("Forbidden file extension.".into()));
    }
    Ok(name)
}

fn normalize_user_file_reference(raw: &str) -> Result<PathBuf, DomainError> {
    let value = raw.trim();
    if value.is_empty() {
        return Err(DomainError::InvalidData("No path specified".into()));
    }
    let value = match Url::parse(value) {
        Ok(parsed) => parsed.path().to_owned(),
        Err(_) => value.to_owned(),
    };
    let normalized = value.replace('\\', "/");
    let relative = normalized
        .trim_start_matches('/')
        .strip_prefix("user/files/")
        .ok_or_else(|| DomainError::InvalidData("Invalid path".into()))?;
    // URL parsers can normalize literal dot segments; decoded segments must not
    // acquire path separators or traversal after validation.
    let decoded = percent_encoding::percent_decode_str(relative)
        .decode_utf8()
        .map_err(|_| DomainError::InvalidData("Invalid file path".into()))?;
    if decoded.starts_with('/') || decoded.contains('\\') {
        return Err(DomainError::InvalidData("Invalid file path".into()));
    }
    normalize_relative_path(&decoded)
}

#[cfg(test)]
mod tests {
    use super::{normalize_relative_path, normalize_user_file_reference, validate_upload_name};

    #[test]
    fn validate_upload_name_accepts_safe_filename() {
        assert_eq!(
            validate_upload_name("LittleWhiteBox_CommonSettings.json").unwrap(),
            "LittleWhiteBox_CommonSettings.json"
        );
    }

    #[test]
    fn validate_upload_name_rejects_unsafe_extension() {
        assert!(validate_upload_name("payload.js").is_err());
    }

    #[test]
    fn normalize_relative_path_rejects_parent_segments() {
        assert!(normalize_relative_path("../secret.txt").is_err());
    }

    #[test]
    fn normalize_user_file_reference_extracts_relative_part() {
        let path = normalize_user_file_reference("user/files/test.json").unwrap();
        assert_eq!(path.to_string_lossy(), "test.json");
    }

    #[test]
    fn encoded_traversal_cannot_escape_files_root() {
        assert!(normalize_user_file_reference("/user/files/%2e%2e/secrets.json").is_err());
        assert!(normalize_user_file_reference("/user/files/%2fetc/passwd").is_err());
    }
}
