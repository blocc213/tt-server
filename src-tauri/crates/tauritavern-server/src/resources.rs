//! Packaged default content, resolved relative to the server binary.
//!
//! The Tauri host gets these from its bundle; the server resolves them from a
//! `--resources-dir` (defaulting to a `resources/` sibling of the executable, or
//! the repository `default/` tree during development). Missing resources are a
//! hard error: seeding default content with a partial tree would silently
//! produce a broken data root.

use std::path::{Path, PathBuf};

use tt_domain::errors::DomainError;
use tt_ports::bundled_resource::BundledResourceStore;

pub struct DirectoryResourceStore {
    root: PathBuf,
}

impl DirectoryResourceStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Resolves the resource root from an explicit override or the executable location.
    ///
    /// Layout matches `tauri.conf.json > bundle.resources`: `default/` and
    /// `frontend-templates/` live directly under the returned directory.
    pub fn resolve_root(explicit: Option<PathBuf>) -> Result<PathBuf, DomainError> {
        if let Some(root) = explicit {
            if !root.is_dir() {
                return Err(DomainError::NotFound(format!(
                    "Resource directory not found: {}",
                    root.display()
                )));
            }
            return Ok(root);
        }

        let exe = std::env::current_exe().map_err(|error| {
            DomainError::InternalError(format!("Failed to resolve executable path: {error}"))
        })?;
        let exe_dir = exe.parent().ok_or_else(|| {
            DomainError::InternalError("Executable has no parent directory".to_string())
        })?;

        let candidates = [
            exe_dir.join("resources"),
            // cargo run from the workspace: crates/../.. is the repository root.
            exe_dir.join("../../../resources"),
        ];
        for candidate in candidates {
            if candidate.join("default").is_dir() {
                return Ok(candidate);
            }
        }

        Err(DomainError::NotFound(format!(
            "Could not locate packaged resources near {}. Pass --resources-dir.",
            exe_dir.display()
        )))
    }

    fn resolve(&self, relative_path: &str) -> Result<PathBuf, DomainError> {
        let normalized = relative_path.trim().replace('\\', "/");
        let normalized = normalized.trim_start_matches('/');
        if normalized.is_empty() || normalized.split('/').any(|part| part == "..") {
            return Err(DomainError::InvalidData(format!(
                "Invalid resource path: {relative_path}"
            )));
        }

        Ok(self.root.join(normalized))
    }
}

impl BundledResourceStore for DirectoryResourceStore {
    fn read_bytes(&self, relative_path: &str) -> Result<Vec<u8>, DomainError> {
        let path = self.resolve(relative_path)?;
        std::fs::read(&path).map_err(|error| map_read_error(relative_path, &path, error))
    }

    fn list_default_content_files_under(&self, prefix: &str) -> Vec<String> {
        let normalized_prefix = prefix.trim_matches('/').replace('\\', "/");
        let Ok(base) = self.resolve(&format!("default/content/{normalized_prefix}")) else {
            return Vec::new();
        };

        let mut entries = Vec::new();
        collect_files(&base, &normalized_prefix, &mut entries);
        entries.sort();
        entries
    }
}

fn collect_files(dir: &Path, prefix: &str, out: &mut Vec<String>) {
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in read_dir.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let relative = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };

        match entry.file_type() {
            Ok(file_type) if file_type.is_dir() => {
                collect_files(&entry.path(), &relative, out);
            }
            Ok(file_type) if file_type.is_file() => out.push(relative),
            _ => {}
        }
    }
}

fn map_read_error(relative_path: &str, path: &Path, error: std::io::Error) -> DomainError {
    if error.kind() == std::io::ErrorKind::NotFound {
        DomainError::NotFound(format!(
            "Resource not found: {relative_path} (looked in {})",
            path.display()
        ))
    } else {
        DomainError::InternalError(format!(
            "Failed to read resource '{relative_path}': {error}"
        ))
    }
}
