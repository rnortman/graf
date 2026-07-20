use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

pub mod clock;
pub mod config;
pub mod db;
pub mod errors;
pub mod frontmatter;
pub mod git;
pub mod identity;
pub mod lint;
pub mod manifest;
pub mod migrate;
pub mod todo;

/// Expand `~` in a path string to the home directory.
pub fn expand_tilde(path_str: &str) -> Result<PathBuf> {
    if let Some(rest) = path_str.strip_prefix("~/") {
        let home = std::env::var("HOME").context("HOME environment variable not set")?;
        Ok(PathBuf::from(home).join(rest))
    } else if path_str == "~" {
        let home = std::env::var("HOME").context("HOME environment variable not set")?;
        Ok(PathBuf::from(home))
    } else {
        Ok(PathBuf::from(path_str))
    }
}

/// Check if a path has a `.md` extension.
pub fn is_md(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext == "md")
}

/// Files that should be skipped during repo traversal.
pub fn should_skip_file(rel_path: &Path) -> bool {
    rel_path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n == "CLAUDE.md")
}

/// Walk all non-dotfile, non-CLAUDE.md files in the repo recursively.
/// Returns paths relative to `repo_dir`.
pub fn collect_repo_files(repo_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in walkdir::WalkDir::new(repo_dir)
        .into_iter()
        .filter_entry(|e| {
            e.depth() == 0 || e.file_name().to_str().is_none_or(|s| !s.starts_with('.'))
        })
    {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry.path().strip_prefix(repo_dir).unwrap_or(entry.path());
        if should_skip_file(rel) {
            continue;
        }
        files.push(rel.to_path_buf());
    }
    Ok(files)
}
