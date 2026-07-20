use std::path::{Path, PathBuf};

use crate::identity::Slug;

/// A non-fatal problem with a specific file.  Used by both lint and the
/// sync protocol to report issues without aborting.
#[derive(Debug, Clone)]
pub struct FileError {
    pub path: PathBuf,
    /// Repo slug, if operating multi-repo.  `None` in single-repo mode.
    pub repo: Option<Slug>,
    pub message: String,
}

impl FileError {
    /// Create a FileError with no repo context (single-repo mode).
    pub fn new(path: &Path, message: impl Into<String>) -> Self {
        Self {
            path: path.to_path_buf(),
            repo: None,
            message: message.into(),
        }
    }
}
