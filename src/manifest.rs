//! Repo config (`.graf/config.toml`) and application manifest (`~/.config/graf/manifest.toml`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::identity::{GlobalId, Slug};

// ---- Repo Config (.graf/config.toml) ----

/// Repo-level configuration stored in `.graf/config.toml`.
///
/// Declares the repo's global identity, default slug, sharing domain,
/// and external repos/domains referenced by its documents.
#[derive(Debug, Serialize, Deserialize)]
pub struct RepoConfig {
    /// Global identifier for this repo (e.g., `example.com/life`).
    pub id: GlobalId,
    /// Suggested slug for use when adding to an application manifest.
    pub default_slug: Slug,
    /// Sharing domain this repo belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<GlobalId>,
    /// External repos and domains referenced by documents in this repo.
    #[serde(default, skip_serializing_if = "RepoRefs::is_empty")]
    pub refs: RepoRefs,
}

/// Cross-repo and cross-domain references declared in a repo config.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct RepoRefs {
    /// External repos referenced by slug in this repo's documents.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repo: Vec<RefEntry>,
    /// External domains referenced by slug in this repo's documents.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub domain: Vec<RefEntry>,
}

impl RepoRefs {
    pub fn is_empty(&self) -> bool {
        self.repo.is_empty() && self.domain.is_empty()
    }
}

/// A slug-to-global-ID mapping for an external repo or domain.
#[derive(Debug, Serialize, Deserialize)]
pub struct RefEntry {
    pub slug: Slug,
    pub id: GlobalId,
}

impl RepoConfig {
    /// Path to the config file within a repo directory.
    pub fn path_in_repo(repo_dir: &Path) -> PathBuf {
        repo_dir.join(".graf").join("config.toml")
    }

    /// Load from a repo directory. Returns `None` if no config file exists.
    pub fn load(repo_dir: &Path) -> Result<Option<Self>> {
        let path = Self::path_in_repo(repo_dir);
        if !path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let config: Self =
            toml::from_str(&content).with_context(|| format!("parsing {}", path.display()))?;
        Ok(Some(config))
    }

    /// Save to the repo directory. Creates `.graf/` if it doesn't exist.
    pub fn save(&self, repo_dir: &Path) -> Result<()> {
        let path = Self::path_in_repo(repo_dir);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let content = toml::to_string_pretty(self).context("serializing repo config")?;
        std::fs::write(&path, content).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }
}

// ---- Application Manifest (~/.config/graf/manifest.toml) ----

/// Application-level manifest mapping slugs to repos and domains.
///
/// Located at `~/.config/graf/manifest.toml` (overridable via `GRAF_MANIFEST`).
/// Maps local slugs to filesystem paths and global IDs.
#[derive(Debug, Serialize, Deserialize)]
pub struct AppManifest {
    /// Repos known to this graf installation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repo: Vec<ManifestRepo>,
    /// Sharing domains known to this graf installation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub domain: Vec<ManifestDomain>,
}

/// A repo entry in the application manifest.
#[derive(Debug, Serialize, Deserialize)]
pub struct ManifestRepo {
    pub slug: Slug,
    /// Filesystem path (may contain `~` for home directory).
    pub path: String,
    pub id: GlobalId,
}

/// A sharing domain entry in the application manifest.
#[derive(Debug, Serialize, Deserialize)]
pub struct ManifestDomain {
    pub slug: Slug,
    pub id: GlobalId,
}

impl AppManifest {
    /// Determine the manifest file path.
    ///
    /// Uses `GRAF_MANIFEST` env var if set, otherwise `~/.config/graf/manifest.toml`.
    pub fn default_path() -> Result<PathBuf> {
        if let Ok(p) = std::env::var("GRAF_MANIFEST") {
            return Ok(PathBuf::from(p));
        }
        let config_dir = match std::env::var("XDG_CONFIG_HOME") {
            Ok(dir) => PathBuf::from(dir),
            Err(_) => {
                let home = std::env::var("HOME").context("HOME environment variable not set")?;
                PathBuf::from(home).join(".config")
            }
        };
        Ok(config_dir.join("graf/manifest.toml"))
    }

    /// Load from the default path. Returns `None` if the file doesn't exist.
    pub fn load_default() -> Result<Option<Self>> {
        let path = Self::default_path()?;
        Self::load_from(&path)
    }

    /// Load from a specific path. Returns `None` if the file doesn't exist.
    pub fn load_from(path: &Path) -> Result<Option<Self>> {
        if !path.exists() {
            return Ok(None);
        }
        let content =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let manifest: Self =
            toml::from_str(&content).with_context(|| format!("parsing {}", path.display()))?;
        Ok(Some(manifest))
    }

    /// Save to the default path. Creates parent directories if needed.
    /// Returns the path written to.
    pub fn save_default(&self) -> Result<PathBuf> {
        let path = Self::default_path()?;
        self.save_to(&path)?;
        Ok(path)
    }

    /// Save to a specific path. Creates parent directories if needed.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let content = toml::to_string_pretty(self).context("serializing manifest")?;
        std::fs::write(path, content).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

    /// Expand `~` in a path string to the home directory.
    pub fn expand_path(path_str: &str) -> Result<PathBuf> {
        crate::expand_tilde(path_str)
    }

    /// Look up a repo by slug.
    pub fn find_repo_by_slug(&self, slug: &Slug) -> Option<&ManifestRepo> {
        self.repo.iter().find(|r| r.slug == *slug)
    }

    /// Look up a repo by global ID.
    pub fn find_repo_by_id(&self, id: &GlobalId) -> Option<&ManifestRepo> {
        self.repo.iter().find(|r| r.id == *id)
    }

    /// Look up a domain by slug.
    pub fn find_domain_by_slug(&self, slug: &Slug) -> Option<&ManifestDomain> {
        self.domain.iter().find(|d| d.slug == *slug)
    }

    /// Get all repo global IDs (useful for URI resolution).
    pub fn repo_ids(&self) -> impl Iterator<Item = &GlobalId> {
        self.repo.iter().map(|r| &r.id)
    }

    /// Validate manifest consistency (in-memory checks only).
    ///
    /// Checks: unique slugs, unique IDs.
    /// Does NOT touch the filesystem — use [`validate_repos`] for that.
    /// Does NOT include prefix warnings — use [`warnings`] for those.
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();

        // Duplicate repo slugs
        let mut seen: HashMap<&str, &GlobalId> = HashMap::new();
        for repo in &self.repo {
            if let Some(prev_id) = seen.insert(repo.slug.as_str(), &repo.id) {
                errors.push(format!(
                    "duplicate repo slug {:?} (IDs: {}, {})",
                    repo.slug.as_str(),
                    prev_id,
                    repo.id
                ));
            }
        }

        // Duplicate domain slugs
        let mut seen: HashMap<&str, &GlobalId> = HashMap::new();
        for domain in &self.domain {
            if let Some(prev_id) = seen.insert(domain.slug.as_str(), &domain.id) {
                errors.push(format!(
                    "duplicate domain slug {:?} (IDs: {}, {})",
                    domain.slug.as_str(),
                    prev_id,
                    domain.id
                ));
            }
        }

        // Duplicate repo IDs
        let mut seen: HashMap<&str, &str> = HashMap::new();
        for repo in &self.repo {
            if let Some(prev_slug) = seen.insert(repo.id.as_str(), repo.slug.as_str()) {
                errors.push(format!(
                    "duplicate repo ID {:?} (slugs: {}, {})",
                    repo.id.as_str(),
                    prev_slug,
                    repo.slug.as_str()
                ));
            }
        }

        // Duplicate domain IDs
        let mut seen: HashMap<&str, &str> = HashMap::new();
        for domain in &self.domain {
            if let Some(prev_slug) = seen.insert(domain.id.as_str(), domain.slug.as_str()) {
                errors.push(format!(
                    "duplicate domain ID {:?} (slugs: {}, {})",
                    domain.id.as_str(),
                    prev_slug,
                    domain.slug.as_str()
                ));
            }
        }

        errors
    }

    /// Check for non-fatal issues (prefix-extension relationships between repo IDs).
    ///
    /// These are not errors — prefix relationships resolve correctly via longest-match —
    /// but they can cause surprising behavior when adding or removing repos.
    pub fn warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        for (i, a) in self.repo.iter().enumerate() {
            for b in &self.repo[i + 1..] {
                if a.id.has_prefix_relationship(&b.id) {
                    warnings.push(format!(
                        "repo ID prefix relationship: {} and {} — adding or removing either may change URI resolution",
                        a.id, b.id
                    ));
                }
            }
        }
        warnings
    }

    /// Validate the manifest against actual repo configs on disk.
    ///
    /// Checks: paths exist as directories, `.graf/config.toml` exists,
    /// manifest ID matches repo config ID.
    // TODO(cross-repo-ref-validation)
    pub fn validate_repos(&self) -> Vec<String> {
        let mut errors = Vec::new();

        for repo in &self.repo {
            let path = match Self::expand_path(&repo.path) {
                Ok(p) => p,
                Err(e) => {
                    errors.push(format!(
                        "repo {:?}: cannot expand path {:?}: {e}",
                        repo.slug.as_str(),
                        repo.path
                    ));
                    continue;
                }
            };

            if !path.is_dir() {
                errors.push(format!(
                    "repo {:?}: path does not exist or is not a directory: {}",
                    repo.slug.as_str(),
                    path.display()
                ));
                continue;
            }

            match RepoConfig::load(&path) {
                Ok(Some(config)) => {
                    if config.id != repo.id {
                        errors.push(format!(
                            "repo {:?}: manifest ID ({}) does not match repo config ID ({})",
                            repo.slug.as_str(),
                            repo.id,
                            config.id
                        ));
                    }
                }
                Ok(None) => {
                    errors.push(format!(
                        "repo {:?}: no .graf/config.toml found at {}",
                        repo.slug.as_str(),
                        path.display()
                    ));
                }
                Err(e) => {
                    errors.push(format!(
                        "repo {:?}: error reading config: {e}",
                        repo.slug.as_str()
                    ));
                }
            }
        }

        errors
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- RepoConfig ----

    #[test]
    fn repo_config_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let config = RepoConfig {
            id: GlobalId::new("example.com/life").unwrap(),
            default_slug: Slug::new("life").unwrap(),
            domain: Some(GlobalId::new("example.com/household").unwrap()),
            refs: RepoRefs {
                repo: vec![RefEntry {
                    slug: Slug::new("shared").unwrap(),
                    id: GlobalId::new("example.com/shared").unwrap(),
                }],
                domain: vec![RefEntry {
                    slug: Slug::new("household").unwrap(),
                    id: GlobalId::new("example.com/household").unwrap(),
                }],
            },
        };
        config.save(dir.path()).unwrap();

        let loaded = RepoConfig::load(dir.path()).unwrap().unwrap();
        assert_eq!(loaded.id, config.id);
        assert_eq!(loaded.default_slug, config.default_slug);
        assert_eq!(loaded.domain, config.domain);
        assert_eq!(loaded.refs.repo.len(), 1);
        assert_eq!(loaded.refs.repo[0].slug.as_str(), "shared");
        assert_eq!(loaded.refs.domain.len(), 1);
        assert_eq!(loaded.refs.domain[0].slug.as_str(), "household");
    }

    #[test]
    fn repo_config_minimal_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let config = RepoConfig {
            id: GlobalId::new("example.com/life").unwrap(),
            default_slug: Slug::new("life").unwrap(),
            domain: None,
            refs: RepoRefs::default(),
        };
        config.save(dir.path()).unwrap();

        let loaded = RepoConfig::load(dir.path()).unwrap().unwrap();
        assert!(loaded.domain.is_none());
        assert!(loaded.refs.repo.is_empty());
        assert!(loaded.refs.domain.is_empty());
    }

    #[test]
    fn repo_config_load_missing_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(RepoConfig::load(dir.path()).unwrap().is_none());
    }

    #[test]
    fn repo_config_serialization_omits_empty() {
        let config = RepoConfig {
            id: GlobalId::new("example.com/life").unwrap(),
            default_slug: Slug::new("life").unwrap(),
            domain: None,
            refs: RepoRefs::default(),
        };
        let toml_str = toml::to_string_pretty(&config).unwrap();
        assert!(!toml_str.contains("domain"));
        assert!(!toml_str.contains("refs"));
    }

    // ---- AppManifest ----

    #[test]
    fn app_manifest_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("manifest.toml");
        let manifest = AppManifest {
            repo: vec![ManifestRepo {
                slug: Slug::new("life").unwrap(),
                path: "~/grafdata/life".to_string(),
                id: GlobalId::new("example.com/life").unwrap(),
            }],
            domain: vec![ManifestDomain {
                slug: Slug::new("household").unwrap(),
                id: GlobalId::new("example.com/household").unwrap(),
            }],
        };
        manifest.save_to(&path).unwrap();

        let loaded = AppManifest::load_from(&path).unwrap().unwrap();
        assert_eq!(loaded.repo.len(), 1);
        assert_eq!(loaded.repo[0].slug.as_str(), "life");
        assert_eq!(loaded.repo[0].path, "~/grafdata/life");
        assert_eq!(loaded.domain.len(), 1);
        assert_eq!(loaded.domain[0].slug.as_str(), "household");
    }

    #[test]
    fn app_manifest_load_missing_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.toml");
        assert!(AppManifest::load_from(&path).unwrap().is_none());
    }

    #[test]
    fn app_manifest_empty_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("manifest.toml");
        let manifest = AppManifest {
            repo: vec![],
            domain: vec![],
        };
        manifest.save_to(&path).unwrap();

        let loaded = AppManifest::load_from(&path).unwrap().unwrap();
        assert!(loaded.repo.is_empty());
        assert!(loaded.domain.is_empty());
    }

    // ---- Validation ----

    #[test]
    fn validate_duplicate_repo_slug() {
        let manifest = AppManifest {
            repo: vec![
                ManifestRepo {
                    slug: Slug::new("life").unwrap(),
                    path: "/a".to_string(),
                    id: GlobalId::new("example.com/life").unwrap(),
                },
                ManifestRepo {
                    slug: Slug::new("life").unwrap(),
                    path: "/b".to_string(),
                    id: GlobalId::new("example.com/other").unwrap(),
                },
            ],
            domain: vec![],
        };
        let errors = manifest.validate();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("duplicate repo slug"));
    }

    #[test]
    fn validate_duplicate_repo_id() {
        let manifest = AppManifest {
            repo: vec![
                ManifestRepo {
                    slug: Slug::new("life").unwrap(),
                    path: "/a".to_string(),
                    id: GlobalId::new("example.com/life").unwrap(),
                },
                ManifestRepo {
                    slug: Slug::new("other").unwrap(),
                    path: "/b".to_string(),
                    id: GlobalId::new("example.com/life").unwrap(),
                },
            ],
            domain: vec![],
        };
        let errors = manifest.validate();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("duplicate repo ID"));
    }

    #[test]
    fn validate_duplicate_domain_slug() {
        let manifest = AppManifest {
            repo: vec![],
            domain: vec![
                ManifestDomain {
                    slug: Slug::new("personal").unwrap(),
                    id: GlobalId::new("example.com/personal").unwrap(),
                },
                ManifestDomain {
                    slug: Slug::new("personal").unwrap(),
                    id: GlobalId::new("example.com/other").unwrap(),
                },
            ],
        };
        let errors = manifest.validate();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("duplicate domain slug"));
    }

    #[test]
    fn validate_no_error_for_prefix_relationship() {
        // Prefix relationships are warnings, not errors.
        let manifest = AppManifest {
            repo: vec![
                ManifestRepo {
                    slug: Slug::new("eng").unwrap(),
                    path: "/a".to_string(),
                    id: GlobalId::new("acme.com/eng").unwrap(),
                },
                ManifestRepo {
                    slug: Slug::new("frontend").unwrap(),
                    path: "/b".to_string(),
                    id: GlobalId::new("acme.com/eng/frontend").unwrap(),
                },
            ],
            domain: vec![],
        };
        assert!(manifest.validate().is_empty());
    }

    #[test]
    fn warnings_prefix_relationship() {
        let manifest = AppManifest {
            repo: vec![
                ManifestRepo {
                    slug: Slug::new("eng").unwrap(),
                    path: "/a".to_string(),
                    id: GlobalId::new("acme.com/eng").unwrap(),
                },
                ManifestRepo {
                    slug: Slug::new("frontend").unwrap(),
                    path: "/b".to_string(),
                    id: GlobalId::new("acme.com/eng/frontend").unwrap(),
                },
            ],
            domain: vec![],
        };
        let warnings = manifest.warnings();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("prefix relationship"));
    }

    #[test]
    fn warnings_empty_when_no_prefix_relationships() {
        let manifest = AppManifest {
            repo: vec![
                ManifestRepo {
                    slug: Slug::new("life").unwrap(),
                    path: "/a".to_string(),
                    id: GlobalId::new("example.com/life").unwrap(),
                },
                ManifestRepo {
                    slug: Slug::new("work").unwrap(),
                    path: "/b".to_string(),
                    id: GlobalId::new("acme.com/eng").unwrap(),
                },
            ],
            domain: vec![],
        };
        assert!(manifest.warnings().is_empty());
    }

    #[test]
    fn validate_ok() {
        let manifest = AppManifest {
            repo: vec![
                ManifestRepo {
                    slug: Slug::new("life").unwrap(),
                    path: "/a".to_string(),
                    id: GlobalId::new("example.com/life").unwrap(),
                },
                ManifestRepo {
                    slug: Slug::new("work").unwrap(),
                    path: "/b".to_string(),
                    id: GlobalId::new("acme.com/eng").unwrap(),
                },
            ],
            domain: vec![ManifestDomain {
                slug: Slug::new("household").unwrap(),
                id: GlobalId::new("example.com/household").unwrap(),
            }],
        };
        assert!(manifest.validate().is_empty());
    }

    // ---- Path expansion ----

    #[test]
    fn expand_path_absolute_unchanged() {
        let p = AppManifest::expand_path("/absolute/path").unwrap();
        assert_eq!(p, PathBuf::from("/absolute/path"));
    }

    #[test]
    fn expand_path_relative_unchanged() {
        let p = AppManifest::expand_path("relative/path").unwrap();
        assert_eq!(p, PathBuf::from("relative/path"));
    }

    #[test]
    fn expand_path_tilde() {
        // Just verify it doesn't error and produces something with the home dir.
        let p = AppManifest::expand_path("~/foo").unwrap();
        assert!(p.is_absolute());
        assert!(p.ends_with("foo"));
    }

    // ---- Lookups ----

    #[test]
    fn find_repo_by_slug_found() {
        let manifest = AppManifest {
            repo: vec![ManifestRepo {
                slug: Slug::new("life").unwrap(),
                path: "/a".to_string(),
                id: GlobalId::new("example.com/life").unwrap(),
            }],
            domain: vec![],
        };
        let found = manifest.find_repo_by_slug(&Slug::new("life").unwrap());
        assert!(found.is_some());
        assert_eq!(found.unwrap().id.as_str(), "example.com/life");
    }

    #[test]
    fn find_repo_by_slug_not_found() {
        let manifest = AppManifest {
            repo: vec![ManifestRepo {
                slug: Slug::new("life").unwrap(),
                path: "/a".to_string(),
                id: GlobalId::new("example.com/life").unwrap(),
            }],
            domain: vec![],
        };
        assert!(
            manifest
                .find_repo_by_slug(&Slug::new("work").unwrap())
                .is_none()
        );
    }

    #[test]
    fn find_repo_by_id_found() {
        let manifest = AppManifest {
            repo: vec![ManifestRepo {
                slug: Slug::new("life").unwrap(),
                path: "/a".to_string(),
                id: GlobalId::new("example.com/life").unwrap(),
            }],
            domain: vec![],
        };
        let found = manifest.find_repo_by_id(&GlobalId::new("example.com/life").unwrap());
        assert!(found.is_some());
    }

    #[test]
    fn find_repo_by_id_not_found() {
        let manifest = AppManifest {
            repo: vec![ManifestRepo {
                slug: Slug::new("life").unwrap(),
                path: "/a".to_string(),
                id: GlobalId::new("example.com/life").unwrap(),
            }],
            domain: vec![],
        };
        assert!(
            manifest
                .find_repo_by_id(&GlobalId::new("example.com/other").unwrap())
                .is_none()
        );
    }

    // ---- Filesystem validation ----

    #[test]
    fn validate_repos_path_not_found() {
        let manifest = AppManifest {
            repo: vec![ManifestRepo {
                slug: Slug::new("life").unwrap(),
                path: "/nonexistent/path/that/does/not/exist".to_string(),
                id: GlobalId::new("example.com/life").unwrap(),
            }],
            domain: vec![],
        };
        let errors = manifest.validate_repos();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("does not exist"));
    }

    #[test]
    fn validate_repos_no_config() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = AppManifest {
            repo: vec![ManifestRepo {
                slug: Slug::new("life").unwrap(),
                path: dir.path().to_str().unwrap().to_string(),
                id: GlobalId::new("example.com/life").unwrap(),
            }],
            domain: vec![],
        };
        let errors = manifest.validate_repos();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("no .graf/config.toml"));
    }

    #[test]
    fn validate_repos_id_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let config = RepoConfig {
            id: GlobalId::new("example.com/other").unwrap(),
            default_slug: Slug::new("other").unwrap(),
            domain: None,
            refs: RepoRefs::default(),
        };
        config.save(dir.path()).unwrap();

        let manifest = AppManifest {
            repo: vec![ManifestRepo {
                slug: Slug::new("life").unwrap(),
                path: dir.path().to_str().unwrap().to_string(),
                id: GlobalId::new("example.com/life").unwrap(),
            }],
            domain: vec![],
        };
        let errors = manifest.validate_repos();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("does not match"));
    }

    #[test]
    fn validate_repos_ok() {
        let dir = tempfile::tempdir().unwrap();
        let config = RepoConfig {
            id: GlobalId::new("example.com/life").unwrap(),
            default_slug: Slug::new("life").unwrap(),
            domain: None,
            refs: RepoRefs::default(),
        };
        config.save(dir.path()).unwrap();

        let manifest = AppManifest {
            repo: vec![ManifestRepo {
                slug: Slug::new("life").unwrap(),
                path: dir.path().to_str().unwrap().to_string(),
                id: GlobalId::new("example.com/life").unwrap(),
            }],
            domain: vec![],
        };
        assert!(manifest.validate_repos().is_empty());
    }

    #[test]
    fn validate_duplicate_domain_id() {
        let manifest = AppManifest {
            repo: vec![],
            domain: vec![
                ManifestDomain {
                    slug: Slug::new("personal").unwrap(),
                    id: GlobalId::new("example.com/household").unwrap(),
                },
                ManifestDomain {
                    slug: Slug::new("family").unwrap(),
                    id: GlobalId::new("example.com/household").unwrap(),
                },
            ],
        };
        let errors = manifest.validate();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("duplicate domain ID"));
    }

    // ---- Lookups (domains) ----

    #[test]
    fn find_domain_by_slug_found() {
        let manifest = AppManifest {
            repo: vec![],
            domain: vec![ManifestDomain {
                slug: Slug::new("household").unwrap(),
                id: GlobalId::new("example.com/household").unwrap(),
            }],
        };
        let found = manifest.find_domain_by_slug(&Slug::new("household").unwrap());
        assert!(found.is_some());
        assert_eq!(found.unwrap().id.as_str(), "example.com/household");
    }

    #[test]
    fn find_domain_by_slug_not_found() {
        let manifest = AppManifest {
            repo: vec![],
            domain: vec![ManifestDomain {
                slug: Slug::new("household").unwrap(),
                id: GlobalId::new("example.com/household").unwrap(),
            }],
        };
        assert!(
            manifest
                .find_domain_by_slug(&Slug::new("work").unwrap())
                .is_none()
        );
    }

    #[test]
    fn repo_ids_returns_all() {
        let manifest = AppManifest {
            repo: vec![
                ManifestRepo {
                    slug: Slug::new("life").unwrap(),
                    path: "/a".to_string(),
                    id: GlobalId::new("example.com/life").unwrap(),
                },
                ManifestRepo {
                    slug: Slug::new("work").unwrap(),
                    path: "/b".to_string(),
                    id: GlobalId::new("acme.com/eng").unwrap(),
                },
            ],
            domain: vec![],
        };
        let ids: Vec<_> = manifest.repo_ids().collect();
        assert_eq!(ids.len(), 2);
        assert_eq!(ids[0].as_str(), "example.com/life");
        assert_eq!(ids[1].as_str(), "acme.com/eng");
    }

    #[test]
    fn repo_ids_empty() {
        let manifest = AppManifest {
            repo: vec![],
            domain: vec![],
        };
        assert_eq!(manifest.repo_ids().count(), 0);
    }

    // ---- TOML format ----

    #[test]
    fn repo_config_toml_format() {
        // Verify the TOML output matches the design doc format.
        let config = RepoConfig {
            id: GlobalId::new("example.com/life").unwrap(),
            default_slug: Slug::new("life").unwrap(),
            domain: Some(GlobalId::new("example.com/household").unwrap()),
            refs: RepoRefs {
                repo: vec![RefEntry {
                    slug: Slug::new("shared").unwrap(),
                    id: GlobalId::new("example.com/shared").unwrap(),
                }],
                domain: vec![],
            },
        };
        let toml_str = toml::to_string_pretty(&config).unwrap();

        // Should contain top-level fields
        assert!(toml_str.contains(r#"id = "example.com/life""#));
        assert!(toml_str.contains(r#"default_slug = "life""#));
        assert!(toml_str.contains(r#"domain = "example.com/household""#));

        // Should contain refs.repo as array of tables
        assert!(toml_str.contains("[[refs.repo]]"));
        assert!(toml_str.contains(r#"slug = "shared""#));

        // Should not contain empty refs.domain
        assert!(!toml_str.contains("[[refs.domain]]"));
    }

    #[test]
    fn app_manifest_toml_format() {
        let manifest = AppManifest {
            repo: vec![
                ManifestRepo {
                    slug: Slug::new("life").unwrap(),
                    path: "~/grafdata/life".to_string(),
                    id: GlobalId::new("example.com/life").unwrap(),
                },
                ManifestRepo {
                    slug: Slug::new("work").unwrap(),
                    path: "~/grafdata/acme".to_string(),
                    id: GlobalId::new("acme.com/eng").unwrap(),
                },
            ],
            domain: vec![ManifestDomain {
                slug: Slug::new("household").unwrap(),
                id: GlobalId::new("example.com/household").unwrap(),
            }],
        };
        let toml_str = toml::to_string_pretty(&manifest).unwrap();

        assert!(toml_str.contains("[[repo]]"));
        assert!(toml_str.contains(r#"slug = "life""#));
        assert!(toml_str.contains(r#"path = "~/grafdata/life""#));
        assert!(toml_str.contains("[[domain]]"));
        assert!(toml_str.contains(r#"slug = "household""#));
    }
}
