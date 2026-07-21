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

/// Outcome of validating the manifest against repo configs on disk.
///
/// Errors fail `manifest check`; warnings are surfaced but do not.
#[derive(Debug, Default)]
pub struct RepoCheckReport {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

impl AppManifest {
    /// Determine the manifest file path.
    ///
    /// Uses `GRAF_MANIFEST` env var if set, otherwise `~/.config/graf/manifest.toml`.
    pub fn default_path() -> Result<PathBuf> {
        if let Ok(p) = std::env::var("GRAF_MANIFEST") {
            return Ok(PathBuf::from(p));
        }
        let config_dir = crate::config::graf_config_dir()?;
        Ok(config_dir.join("manifest.toml"))
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

    /// Look up a domain by global ID.
    pub fn find_domain_by_id(&self, id: &GlobalId) -> Option<&ManifestDomain> {
        self.domain.iter().find(|d| d.id == *id)
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
        for (slug, prev_id, id) in
            Self::duplicate_pairs(self.repo.iter().map(|r| (r.slug.as_str(), r.id.as_str())))
        {
            errors.push(format!(
                "duplicate repo slug {slug:?} (IDs: {prev_id}, {id})"
            ));
        }

        // Duplicate domain slugs
        for (slug, prev_id, id) in
            Self::duplicate_pairs(self.domain.iter().map(|d| (d.slug.as_str(), d.id.as_str())))
        {
            errors.push(format!(
                "duplicate domain slug {slug:?} (IDs: {prev_id}, {id})"
            ));
        }

        // Duplicate repo IDs
        for (id, prev_slug, slug) in
            Self::duplicate_pairs(self.repo.iter().map(|r| (r.id.as_str(), r.slug.as_str())))
        {
            errors.push(format!(
                "duplicate repo ID {id:?} (slugs: {prev_slug}, {slug})"
            ));
        }

        // Duplicate domain IDs
        for (id, prev_slug, slug) in
            Self::duplicate_pairs(self.domain.iter().map(|d| (d.id.as_str(), d.slug.as_str())))
        {
            errors.push(format!(
                "duplicate domain ID {id:?} (slugs: {prev_slug}, {slug})"
            ));
        }

        errors
    }

    /// Find duplicate keys in a sequence of `(key, value)` pairs.
    ///
    /// Returns `(key, previous_value, value)` for each duplicate occurrence,
    /// in encounter order of the later occurrence. For a key seen 3+ times,
    /// `previous_value` is the immediately preceding occurrence's value, not
    /// the first.
    fn duplicate_pairs<'a>(
        pairs: impl Iterator<Item = (&'a str, &'a str)>,
    ) -> Vec<(&'a str, &'a str, &'a str)> {
        let mut seen: HashMap<&str, &str> = HashMap::new();
        let mut dups = Vec::new();
        for (key, value) in pairs {
            if let Some(prev) = seen.insert(key, value) {
                dups.push((key, prev, value));
            }
        }
        dups
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
    /// manifest ID matches repo config ID, and each repo's `refs` section
    /// resolves against the manifest via `validate_refs`.
    pub fn validate_repos(&self) -> RepoCheckReport {
        let mut report = RepoCheckReport::default();

        for repo in &self.repo {
            let path = match Self::expand_path(&repo.path) {
                Ok(p) => p,
                Err(e) => {
                    report.errors.push(format!(
                        "repo {:?}: cannot expand path {:?}: {e}",
                        repo.slug.as_str(),
                        repo.path
                    ));
                    continue;
                }
            };

            if !path.is_dir() {
                report.errors.push(format!(
                    "repo {:?}: path does not exist or is not a directory: {}",
                    repo.slug.as_str(),
                    path.display()
                ));
                continue;
            }

            match RepoConfig::load(&path) {
                Ok(Some(config)) => {
                    if config.id != repo.id {
                        report.errors.push(format!(
                            "repo {:?}: manifest ID ({}) does not match repo config ID ({})",
                            repo.slug.as_str(),
                            repo.id,
                            config.id
                        ));
                    }
                    let refs_report = self.validate_refs(&repo.slug, &config.refs);
                    report.errors.extend(refs_report.errors);
                    report.warnings.extend(refs_report.warnings);
                }
                Ok(None) => {
                    report.errors.push(format!(
                        "repo {:?}: no .graf/config.toml found at {}",
                        repo.slug.as_str(),
                        path.display()
                    ));
                }
                Err(e) => {
                    report.errors.push(format!(
                        "repo {:?}: error reading config: {e}",
                        repo.slug.as_str()
                    ));
                }
            }
        }

        report
    }

    /// Full `manifest check` report: in-memory manifest validation
    /// (`validate`), on-disk repo/refs validation (`validate_repos`), and
    /// non-fatal `warnings`, merged into one `{errors, warnings}`. Single
    /// source of truth for both the text and JSON check paths.
    pub fn check_report(&self) -> RepoCheckReport {
        let repo_report = self.validate_repos();
        let mut errors = self.validate();
        errors.extend(repo_report.errors);
        let mut warnings = self.warnings();
        warnings.extend(repo_report.warnings);
        RepoCheckReport { errors, warnings }
    }

    /// Validate a repo's `refs` section against this manifest.
    ///
    /// Errors: a ref repo/domain ID that resolves to no manifest entry;
    /// duplicate slug or duplicate ID within `refs.repo` or within
    /// `refs.domain`. Warning: a ref slug equal to an app-manifest slug that
    /// is bound to a *different* global ID (installation-local collision).
    fn validate_refs(&self, repo_slug: &Slug, refs: &RepoRefs) -> RepoCheckReport {
        let mut report = RepoCheckReport::default();
        let rs = repo_slug.as_str();

        // Unknown / duplicate repo refs.
        for entry in &refs.repo {
            if self.find_repo_by_id(&entry.id).is_none() {
                report.errors.push(format!(
                    "repo {rs:?}: ref repo slug {:?} points to unknown ID {}",
                    entry.slug.as_str(),
                    entry.id
                ));
            }
        }
        for (slug, prev_id, id) in
            Self::duplicate_pairs(refs.repo.iter().map(|e| (e.slug.as_str(), e.id.as_str())))
        {
            report.errors.push(format!(
                "repo {rs:?}: duplicate ref repo slug {slug:?} (IDs: {prev_id}, {id})"
            ));
        }
        for (id, prev_slug, slug) in
            Self::duplicate_pairs(refs.repo.iter().map(|e| (e.id.as_str(), e.slug.as_str())))
        {
            report.errors.push(format!(
                "repo {rs:?}: duplicate ref repo ID {id:?} (slugs: {prev_slug}, {slug})"
            ));
        }

        // Unknown / duplicate domain refs.
        for entry in &refs.domain {
            if self.find_domain_by_id(&entry.id).is_none() {
                report.errors.push(format!(
                    "repo {rs:?}: ref domain slug {:?} points to unknown ID {}",
                    entry.slug.as_str(),
                    entry.id
                ));
            }
        }
        for (slug, prev_id, id) in
            Self::duplicate_pairs(refs.domain.iter().map(|e| (e.slug.as_str(), e.id.as_str())))
        {
            report.errors.push(format!(
                "repo {rs:?}: duplicate ref domain slug {slug:?} (IDs: {prev_id}, {id})"
            ));
        }
        for (id, prev_slug, slug) in
            Self::duplicate_pairs(refs.domain.iter().map(|e| (e.id.as_str(), e.slug.as_str())))
        {
            report.errors.push(format!(
                "repo {rs:?}: duplicate ref domain ID {id:?} (slugs: {prev_slug}, {slug})"
            ));
        }

        // Manifest-slug shadowing with a different ID (warning only). Each ref
        // kind is checked only against its own manifest namespace — repos and
        // domains are separate namespaces, so a domain ref reusing a repo slug
        // (or vice versa) is not a shadow.
        for entry in &refs.repo {
            if let Some(mrepo) = self.find_repo_by_slug(&entry.slug)
                && mrepo.id != entry.id
            {
                report.warnings.push(format!(
                    "repo {rs:?}: ref slug {:?} shadows manifest repo slug bound to different ID (manifest: {}, ref: {})",
                    entry.slug.as_str(),
                    mrepo.id,
                    entry.id
                ));
            }
        }
        for entry in &refs.domain {
            if let Some(mdom) = self.find_domain_by_slug(&entry.slug)
                && mdom.id != entry.id
            {
                report.warnings.push(format!(
                    "repo {rs:?}: ref slug {:?} shadows manifest domain slug bound to different ID (manifest: {}, ref: {})",
                    entry.slug.as_str(),
                    mdom.id,
                    entry.id
                ));
            }
        }

        report
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
        let errors = manifest.validate_repos().errors;
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
        let errors = manifest.validate_repos().errors;
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
        let errors = manifest.validate_repos().errors;
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
        let report = manifest.validate_repos();
        assert!(report.errors.is_empty());
        assert!(report.warnings.is_empty());
    }

    // ---- Refs validation ----

    fn ref_entry(slug: &str, id: &str) -> RefEntry {
        RefEntry {
            slug: Slug::new(slug).unwrap(),
            id: GlobalId::new(id).unwrap(),
        }
    }

    fn m_repo(slug: &str, path: &str, id: &str) -> ManifestRepo {
        ManifestRepo {
            slug: Slug::new(slug).unwrap(),
            path: path.to_string(),
            id: GlobalId::new(id).unwrap(),
        }
    }

    fn m_domain(slug: &str, id: &str) -> ManifestDomain {
        ManifestDomain {
            slug: Slug::new(slug).unwrap(),
            id: GlobalId::new(id).unwrap(),
        }
    }

    /// Write a repo config with the given ID and refs into a fresh tempdir.
    fn repo_on_disk(id: &str, refs: RepoRefs) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        RepoConfig {
            id: GlobalId::new(id).unwrap(),
            default_slug: Slug::new("s").unwrap(),
            domain: None,
            refs,
        }
        .save(dir.path())
        .unwrap();
        dir
    }

    #[test]
    fn find_domain_by_id_found() {
        let manifest = AppManifest {
            repo: vec![],
            domain: vec![m_domain("household", "example.com/household")],
        };
        assert!(
            manifest
                .find_domain_by_id(&GlobalId::new("example.com/household").unwrap())
                .is_some()
        );
    }

    #[test]
    fn find_domain_by_id_not_found() {
        let manifest = AppManifest {
            repo: vec![],
            domain: vec![m_domain("household", "example.com/household")],
        };
        assert!(
            manifest
                .find_domain_by_id(&GlobalId::new("example.com/other").unwrap())
                .is_none()
        );
    }

    #[test]
    fn validate_refs_all_valid() {
        let life = repo_on_disk(
            "example.com/life",
            RepoRefs {
                repo: vec![ref_entry("shared", "example.com/shared")],
                domain: vec![ref_entry("house", "example.com/house")],
            },
        );
        let shared = repo_on_disk("example.com/shared", RepoRefs::default());
        let manifest = AppManifest {
            repo: vec![
                m_repo("life", life.path().to_str().unwrap(), "example.com/life"),
                m_repo(
                    "shared",
                    shared.path().to_str().unwrap(),
                    "example.com/shared",
                ),
            ],
            domain: vec![m_domain("house", "example.com/house")],
        };
        let report = manifest.validate_repos();
        assert!(report.errors.is_empty(), "errors: {:?}", report.errors);
        assert!(
            report.warnings.is_empty(),
            "warnings: {:?}",
            report.warnings
        );
    }

    #[test]
    fn validate_refs_unknown_repo_id() {
        let life = repo_on_disk(
            "example.com/life",
            RepoRefs {
                repo: vec![ref_entry("x", "example.com/missing")],
                domain: vec![],
            },
        );
        let manifest = AppManifest {
            repo: vec![m_repo(
                "life",
                life.path().to_str().unwrap(),
                "example.com/life",
            )],
            domain: vec![],
        };
        let errors = manifest.validate_repos().errors;
        assert_eq!(errors.len(), 1, "errors: {errors:?}");
        assert!(errors[0].contains("unknown ID"));
        assert!(errors[0].contains("example.com/missing"));
        assert!(errors[0].contains("\"x\""));
    }

    #[test]
    fn validate_refs_unknown_domain_id() {
        let life = repo_on_disk(
            "example.com/life",
            RepoRefs {
                repo: vec![],
                domain: vec![ref_entry("d", "example.com/missingdom")],
            },
        );
        let manifest = AppManifest {
            repo: vec![m_repo(
                "life",
                life.path().to_str().unwrap(),
                "example.com/life",
            )],
            domain: vec![],
        };
        let errors = manifest.validate_repos().errors;
        assert_eq!(errors.len(), 1, "errors: {errors:?}");
        assert!(errors[0].contains("ref domain slug"));
        assert!(errors[0].contains("example.com/missingdom"));
    }

    #[test]
    fn validate_refs_duplicate_repo_slug() {
        let a = repo_on_disk("example.com/a", RepoRefs::default());
        let b = repo_on_disk("example.com/b", RepoRefs::default());
        let life = repo_on_disk(
            "example.com/life",
            RepoRefs {
                repo: vec![
                    ref_entry("dup", "example.com/a"),
                    ref_entry("dup", "example.com/b"),
                ],
                domain: vec![],
            },
        );
        let manifest = AppManifest {
            repo: vec![
                m_repo("life", life.path().to_str().unwrap(), "example.com/life"),
                m_repo("a", a.path().to_str().unwrap(), "example.com/a"),
                m_repo("b", b.path().to_str().unwrap(), "example.com/b"),
            ],
            domain: vec![],
        };
        let errors = manifest.validate_repos().errors;
        assert_eq!(errors.len(), 1, "errors: {errors:?}");
        assert!(errors[0].contains("duplicate ref repo slug"));
        assert!(errors[0].contains("\"dup\""));
    }

    #[test]
    fn validate_refs_duplicate_domain_slug() {
        let life = repo_on_disk(
            "example.com/life",
            RepoRefs {
                repo: vec![],
                domain: vec![
                    ref_entry("dup", "example.com/d1"),
                    ref_entry("dup", "example.com/d2"),
                ],
            },
        );
        let manifest = AppManifest {
            repo: vec![m_repo(
                "life",
                life.path().to_str().unwrap(),
                "example.com/life",
            )],
            domain: vec![
                m_domain("d1", "example.com/d1"),
                m_domain("d2", "example.com/d2"),
            ],
        };
        let errors = manifest.validate_repos().errors;
        assert_eq!(errors.len(), 1, "errors: {errors:?}");
        assert!(errors[0].contains("duplicate ref domain slug"));
    }

    #[test]
    fn validate_refs_duplicate_repo_id() {
        let a = repo_on_disk("example.com/a", RepoRefs::default());
        let life = repo_on_disk(
            "example.com/life",
            RepoRefs {
                repo: vec![
                    ref_entry("one", "example.com/a"),
                    ref_entry("two", "example.com/a"),
                ],
                domain: vec![],
            },
        );
        let manifest = AppManifest {
            repo: vec![
                m_repo("life", life.path().to_str().unwrap(), "example.com/life"),
                m_repo("a", a.path().to_str().unwrap(), "example.com/a"),
            ],
            domain: vec![],
        };
        let errors = manifest.validate_repos().errors;
        assert_eq!(errors.len(), 1, "errors: {errors:?}");
        assert!(errors[0].contains("duplicate ref repo ID"));
        assert!(errors[0].contains("example.com/a"));
    }

    #[test]
    fn validate_refs_same_slug_repo_and_domain_ok() {
        let shared = repo_on_disk("example.com/shared", RepoRefs::default());
        let life = repo_on_disk(
            "example.com/life",
            RepoRefs {
                repo: vec![ref_entry("both", "example.com/shared")],
                domain: vec![ref_entry("both", "example.com/house")],
            },
        );
        let manifest = AppManifest {
            repo: vec![
                m_repo("life", life.path().to_str().unwrap(), "example.com/life"),
                m_repo(
                    "shared",
                    shared.path().to_str().unwrap(),
                    "example.com/shared",
                ),
            ],
            domain: vec![m_domain("house", "example.com/house")],
        };
        let report = manifest.validate_repos();
        assert!(report.errors.is_empty(), "errors: {:?}", report.errors);
        assert!(
            report.warnings.is_empty(),
            "warnings: {:?}",
            report.warnings
        );
    }

    #[test]
    fn validate_refs_shadow_same_id_clean() {
        // Ref slug equals a manifest repo slug bound to the SAME ID: agreement.
        let shared = repo_on_disk("example.com/shared", RepoRefs::default());
        let life = repo_on_disk(
            "example.com/life",
            RepoRefs {
                repo: vec![ref_entry("shared", "example.com/shared")],
                domain: vec![],
            },
        );
        let manifest = AppManifest {
            repo: vec![
                m_repo("life", life.path().to_str().unwrap(), "example.com/life"),
                m_repo(
                    "shared",
                    shared.path().to_str().unwrap(),
                    "example.com/shared",
                ),
            ],
            domain: vec![],
        };
        let report = manifest.validate_repos();
        assert!(report.errors.is_empty(), "errors: {:?}", report.errors);
        assert!(
            report.warnings.is_empty(),
            "warnings: {:?}",
            report.warnings
        );
    }

    #[test]
    fn validate_refs_shadow_different_id_warns() {
        // Ref slug "shared" resolves by ID to manifest repo "real", but the
        // manifest also has a repo *slug* "shared" bound to a different ID.
        let real = repo_on_disk("example.com/real", RepoRefs::default());
        let other = repo_on_disk("example.com/other", RepoRefs::default());
        let life = repo_on_disk(
            "example.com/life",
            RepoRefs {
                repo: vec![ref_entry("shared", "example.com/real")],
                domain: vec![],
            },
        );
        let manifest = AppManifest {
            repo: vec![
                m_repo("life", life.path().to_str().unwrap(), "example.com/life"),
                m_repo("real", real.path().to_str().unwrap(), "example.com/real"),
                // Manifest repo slug "shared" bound to a different ID than the ref.
                m_repo(
                    "shared",
                    other.path().to_str().unwrap(),
                    "example.com/other",
                ),
            ],
            domain: vec![],
        };
        let report = manifest.validate_repos();
        assert!(report.errors.is_empty(), "errors: {:?}", report.errors);
        assert_eq!(report.warnings.len(), 1, "warnings: {:?}", report.warnings);
        assert!(report.warnings[0].contains("shadows manifest repo slug"));
    }

    #[test]
    fn validate_refs_duplicate_domain_id() {
        let life = repo_on_disk(
            "example.com/life",
            RepoRefs {
                repo: vec![],
                domain: vec![
                    ref_entry("one", "example.com/d"),
                    ref_entry("two", "example.com/d"),
                ],
            },
        );
        let manifest = AppManifest {
            repo: vec![m_repo(
                "life",
                life.path().to_str().unwrap(),
                "example.com/life",
            )],
            domain: vec![m_domain("d", "example.com/d")],
        };
        let errors = manifest.validate_repos().errors;
        assert_eq!(errors.len(), 1, "errors: {errors:?}");
        assert!(errors[0].contains("duplicate ref domain ID"));
        assert!(errors[0].contains("example.com/d"));
    }

    #[test]
    fn validate_refs_shadow_domain_different_id_warns() {
        // Ref domain slug "shared" resolves by ID to manifest domain "real",
        // but the manifest also has a domain *slug* "shared" bound to a
        // different ID.
        let life = repo_on_disk(
            "example.com/life",
            RepoRefs {
                repo: vec![],
                domain: vec![ref_entry("shared", "example.com/real")],
            },
        );
        let manifest = AppManifest {
            repo: vec![m_repo(
                "life",
                life.path().to_str().unwrap(),
                "example.com/life",
            )],
            domain: vec![
                m_domain("real", "example.com/real"),
                m_domain("shared", "example.com/other"),
            ],
        };
        let report = manifest.validate_repos();
        assert!(report.errors.is_empty(), "errors: {:?}", report.errors);
        assert_eq!(report.warnings.len(), 1, "warnings: {:?}", report.warnings);
        assert!(report.warnings[0].contains("shadows manifest domain slug"));
    }

    #[test]
    fn validate_refs_domain_ref_reusing_repo_slug_ok() {
        // A domain ref whose slug equals a manifest *repo* slug is not a shadow:
        // repos and domains are separate namespaces.
        let life = repo_on_disk(
            "example.com/life",
            RepoRefs {
                repo: vec![],
                domain: vec![ref_entry("life", "example.com/house")],
            },
        );
        let manifest = AppManifest {
            repo: vec![m_repo(
                "life",
                life.path().to_str().unwrap(),
                "example.com/life",
            )],
            domain: vec![m_domain("house", "example.com/house")],
        };
        let report = manifest.validate_repos();
        assert!(report.errors.is_empty(), "errors: {:?}", report.errors);
        assert!(
            report.warnings.is_empty(),
            "warnings: {:?}",
            report.warnings
        );
    }

    #[test]
    fn validate_refs_repo_ref_reusing_domain_slug_ok() {
        // A repo ref whose slug equals a manifest *domain* slug is not a shadow.
        let shared = repo_on_disk("example.com/shared", RepoRefs::default());
        let life = repo_on_disk(
            "example.com/life",
            RepoRefs {
                repo: vec![ref_entry("house", "example.com/shared")],
                domain: vec![],
            },
        );
        let manifest = AppManifest {
            repo: vec![
                m_repo("life", life.path().to_str().unwrap(), "example.com/life"),
                m_repo(
                    "shared",
                    shared.path().to_str().unwrap(),
                    "example.com/shared",
                ),
            ],
            domain: vec![m_domain("house", "example.com/house")],
        };
        let report = manifest.validate_repos();
        assert!(report.errors.is_empty(), "errors: {:?}", report.errors);
        assert!(
            report.warnings.is_empty(),
            "warnings: {:?}",
            report.warnings
        );
    }

    #[test]
    fn validate_refs_self_reference_ok() {
        let life = repo_on_disk(
            "example.com/life",
            RepoRefs {
                repo: vec![ref_entry("me", "example.com/life")],
                domain: vec![],
            },
        );
        let manifest = AppManifest {
            repo: vec![m_repo(
                "life",
                life.path().to_str().unwrap(),
                "example.com/life",
            )],
            domain: vec![],
        };
        let report = manifest.validate_repos();
        assert!(report.errors.is_empty(), "errors: {:?}", report.errors);
        assert!(
            report.warnings.is_empty(),
            "warnings: {:?}",
            report.warnings
        );
    }

    #[test]
    fn validate_refs_bad_refs_and_id_mismatch() {
        // Config ID differs from manifest ID AND refs point nowhere: both fire.
        let dir = repo_on_disk(
            "example.com/actual",
            RepoRefs {
                repo: vec![ref_entry("x", "example.com/missing")],
                domain: vec![],
            },
        );
        let manifest = AppManifest {
            repo: vec![m_repo(
                "life",
                dir.path().to_str().unwrap(),
                "example.com/life",
            )],
            domain: vec![],
        };
        let errors = manifest.validate_repos().errors;
        assert_eq!(errors.len(), 2, "errors: {errors:?}");
        assert!(errors.iter().any(|e| e.contains("does not match")));
        assert!(errors.iter().any(|e| e.contains("unknown ID")));
    }

    #[test]
    fn validate_refs_triple_duplicate_repo_slug() {
        // Three ref entries share a slug (distinct IDs): N=3 occurrences yield
        // exactly N-1=2 duplicate-slug reports.
        let a = repo_on_disk("example.com/a", RepoRefs::default());
        let b = repo_on_disk("example.com/b", RepoRefs::default());
        let c = repo_on_disk("example.com/c", RepoRefs::default());
        let life = repo_on_disk(
            "example.com/life",
            RepoRefs {
                repo: vec![
                    ref_entry("dup", "example.com/a"),
                    ref_entry("dup", "example.com/b"),
                    ref_entry("dup", "example.com/c"),
                ],
                domain: vec![],
            },
        );
        let manifest = AppManifest {
            repo: vec![
                m_repo("life", life.path().to_str().unwrap(), "example.com/life"),
                m_repo("a", a.path().to_str().unwrap(), "example.com/a"),
                m_repo("b", b.path().to_str().unwrap(), "example.com/b"),
                m_repo("c", c.path().to_str().unwrap(), "example.com/c"),
            ],
            domain: vec![],
        };
        let errors = manifest.validate_repos().errors;
        let dups: Vec<_> = errors
            .iter()
            .filter(|e| e.contains("duplicate ref repo slug"))
            .collect();
        assert_eq!(dups.len(), 2, "errors: {errors:?}");
    }

    #[test]
    fn check_report_merges_all_streams() {
        // One manifest that simultaneously produces a `validate()` error
        // (duplicate manifest repo slug), a `validate_repos` refs error
        // (unknown ref ID), a repo-level shadow warning, and a `warnings()`
        // prefix-relationship warning — all four streams must reach
        // `check_report`.
        let real = repo_on_disk("example.com/real", RepoRefs::default());
        let other = repo_on_disk("example.com/other", RepoRefs::default());
        let dup1 = repo_on_disk("example.com/dup1", RepoRefs::default());
        let dup2 = repo_on_disk("example.com/dup2", RepoRefs::default());
        let sub = repo_on_disk("example.com/life/sub", RepoRefs::default());
        let life = repo_on_disk(
            "example.com/life",
            RepoRefs {
                repo: vec![
                    // Unknown ID -> validate_repos error.
                    ref_entry("x", "example.com/missing"),
                    // Resolves to "real", but manifest slug "shared" is bound to
                    // a different ID -> shadow warning.
                    ref_entry("shared", "example.com/real"),
                ],
                domain: vec![],
            },
        );
        let manifest = AppManifest {
            repo: vec![
                m_repo("life", life.path().to_str().unwrap(), "example.com/life"),
                m_repo("real", real.path().to_str().unwrap(), "example.com/real"),
                m_repo(
                    "shared",
                    other.path().to_str().unwrap(),
                    "example.com/other",
                ),
                // Duplicate manifest repo slug "dup" -> validate() error.
                m_repo("dup", dup1.path().to_str().unwrap(), "example.com/dup1"),
                m_repo("dup", dup2.path().to_str().unwrap(), "example.com/dup2"),
                // ID "example.com/life/sub" is a /-boundary extension of
                // "example.com/life" -> `warnings()` prefix warning.
                m_repo("sub", sub.path().to_str().unwrap(), "example.com/life/sub"),
            ],
            domain: vec![],
        };
        let report = manifest.check_report();
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.contains("duplicate repo slug")),
            "missing validate() error: {:?}",
            report.errors
        );
        assert!(
            report.errors.iter().any(|e| e.contains("unknown ID")),
            "missing validate_repos error: {:?}",
            report.errors
        );
        assert!(
            report
                .warnings
                .iter()
                .any(|w| w.contains("shadows manifest repo slug")),
            "missing repo-level warning: {:?}",
            report.warnings
        );
        assert!(
            report
                .warnings
                .iter()
                .any(|w| w.contains("prefix relationship")),
            "missing warnings() prefix warning: {:?}",
            report.warnings
        );
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
