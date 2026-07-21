use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

const DEFAULT_TODO_LIMIT: usize = 100;

/// Resolve graf's config directory: `$XDG_CONFIG_HOME/graf`, falling back
/// to `$HOME/.config/graf`. Callers append their own filename.
pub fn graf_config_dir() -> Result<PathBuf> {
    graf_config_dir_from(
        std::env::var("XDG_CONFIG_HOME").ok(),
        std::env::var("HOME").ok(),
    )
}

/// Pure core of [`graf_config_dir`]: `None` means the variable is unset.
/// Separated so unit tests need no env mutation.
fn graf_config_dir_from(xdg_config_home: Option<String>, home: Option<String>) -> Result<PathBuf> {
    let root = match xdg_config_home {
        // TODO(xdg-empty-unset): an empty XDG_CONFIG_HOME is used verbatim; the
        // XDG spec says empty means unset (fall back to $HOME/.config).
        Some(dir) => PathBuf::from(dir),
        None => {
            let home = home.context("HOME environment variable not set")?;
            PathBuf::from(home).join(".config")
        }
    };
    Ok(root.join("graf"))
}

/// Application-level config, loaded from `~/.config/graf/config.toml`.
///
/// Unlike `AppManifest` (identity/location registry), this holds user
/// preferences and behavioral defaults.  Always returns a value — uses
/// built-in defaults when no file exists.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub todo: TodoConfig,
}

/// Todo-related config.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct TodoConfig {
    /// Default limit for todo queries when no explicit `--limit` is given.
    /// `Some(n)` caps results at `n`.  `Some(0)` means no limit.
    /// Defaults to `Some(100)` when absent from config.
    pub default_limit: Option<usize>,
}

impl Default for TodoConfig {
    fn default() -> Self {
        Self {
            default_limit: Some(DEFAULT_TODO_LIMIT),
        }
    }
}

impl TodoConfig {
    /// Resolve the effective limit: `Some(0)` is treated as "no limit" (None).
    pub fn effective_limit(&self) -> Option<usize> {
        match self.default_limit {
            Some(0) => None,
            other => other,
        }
    }
}

impl AppConfig {
    /// Determine the config file path.
    ///
    /// Uses `GRAF_CONFIG` env var if set, otherwise `~/.config/graf/config.toml`.
    pub fn default_path() -> Result<PathBuf> {
        if let Ok(p) = std::env::var("GRAF_CONFIG") {
            return Ok(PathBuf::from(p));
        }
        let config_dir = graf_config_dir()?;
        Ok(config_dir.join("config.toml"))
    }

    /// Load from the default path.  Returns defaults if the file doesn't exist.
    pub fn load_default() -> Result<Self> {
        let path = Self::default_path()?;
        Self::load_from(&path)
    }

    /// Load from a specific path.  Returns defaults if the file doesn't exist.
    pub fn load_from(path: &Path) -> Result<Self> {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        let config: Self =
            toml::from_str(&content).with_context(|| format!("parsing {}", path.display()))?;
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_limit_100() {
        let config = AppConfig::default();
        assert_eq!(config.todo.default_limit, Some(100));
        assert_eq!(config.todo.effective_limit(), Some(100));
    }

    #[test]
    fn zero_limit_means_no_limit() {
        let config: AppConfig = toml::from_str("[todo]\ndefault_limit = 0").unwrap();
        assert_eq!(config.todo.default_limit, Some(0));
        assert_eq!(config.todo.effective_limit(), None);
    }

    #[test]
    fn missing_file_returns_defaults() {
        let config = AppConfig::load_from(Path::new("/nonexistent/config.toml")).unwrap();
        assert_eq!(config.todo.default_limit, Some(100));
    }

    #[test]
    fn empty_file_returns_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "").unwrap();
        let config = AppConfig::load_from(&path).unwrap();
        assert_eq!(config.todo.default_limit, Some(100));
    }

    #[test]
    fn custom_limit() {
        let config: AppConfig = toml::from_str("[todo]\ndefault_limit = 50").unwrap();
        assert_eq!(config.todo.default_limit, Some(50));
        assert_eq!(config.todo.effective_limit(), Some(50));
    }

    #[test]
    fn absent_limit_uses_default() {
        let config: AppConfig = toml::from_str("[todo]\n").unwrap();
        assert_eq!(config.todo.default_limit, Some(100));
    }

    #[test]
    fn config_dir_uses_xdg_when_set() {
        let dir = graf_config_dir_from(Some("/xdg/dir".to_string()), None).unwrap();
        assert_eq!(dir, PathBuf::from("/xdg/dir/graf"));
    }

    #[test]
    fn config_dir_falls_back_to_home_dot_config() {
        let dir = graf_config_dir_from(None, Some("/home/u".to_string())).unwrap();
        assert_eq!(dir, PathBuf::from("/home/u/.config/graf"));
    }

    #[test]
    fn config_dir_errors_without_home() {
        let err = graf_config_dir_from(None, None).unwrap_err();
        assert!(format!("{err:#}").contains("HOME"));
    }

    #[test]
    fn config_dir_honors_empty_xdg_verbatim() {
        let dir = graf_config_dir_from(Some(String::new()), Some("/home/u".to_string())).unwrap();
        assert_eq!(dir, PathBuf::from("graf"));
    }
}
