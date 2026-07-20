//! Identity types for multi-repo graf: global IDs, slugs, and path references.

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

/// A DNS-rooted global identifier for repos and sharing domains.
///
/// Format: `domain/path` — e.g., `example.com/life`, `acme.com/eng/frontend`.
/// The domain (before the first `/`) must be a valid DNS-like name containing a dot.
/// Arbitrary depth after the domain.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct GlobalId(String);

impl GlobalId {
    pub fn new(s: &str) -> Result<Self> {
        Self::validate(s)?;
        Ok(Self(s.to_string()))
    }

    fn validate(s: &str) -> Result<()> {
        let slash_pos = s
            .find('/')
            .ok_or_else(|| anyhow::anyhow!("global ID must contain '/': {s:?}"))?;

        let domain = &s[..slash_pos];
        let path = &s[slash_pos + 1..];

        if domain.is_empty() {
            bail!("global ID domain cannot be empty: {s:?}");
        }
        if !domain.contains('.') {
            bail!("global ID domain must contain '.': {s:?}");
        }
        if domain != domain.to_ascii_lowercase() {
            bail!("global ID domain must be lowercase: {s:?}");
        }
        if domain
            .chars()
            .any(|c| c.is_whitespace() || c == ':' || c == '#')
        {
            bail!("global ID domain contains invalid characters: {s:?}");
        }

        if path.is_empty() {
            bail!("global ID path cannot be empty: {s:?}");
        }
        if path.starts_with('/') || path.contains("//") {
            bail!("global ID path contains empty segment: {s:?}");
        }
        if path.ends_with('/') {
            bail!("global ID path must not end with '/': {s:?}");
        }

        Ok(())
    }

    /// The DNS domain portion (before the first `/`).
    pub fn domain(&self) -> &str {
        &self.0[..self.0.find('/').unwrap()]
    }

    /// The full identifier as a string.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Check if this ID and `other` have a prefix-extension relationship.
    ///
    /// Returns true if either ID is a `/`-boundary prefix of the other.
    /// Used to detect potential URI resolution ambiguity.
    pub fn has_prefix_relationship(&self, other: &GlobalId) -> bool {
        if self.0.len() < other.0.len() {
            other.0.starts_with(&self.0) && other.0.as_bytes()[self.0.len()] == b'/'
        } else if self.0.len() > other.0.len() {
            self.0.starts_with(&other.0) && self.0.as_bytes()[other.0.len()] == b'/'
        } else {
            false
        }
    }
}

impl fmt::Display for GlobalId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for GlobalId {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        Self::new(s)
    }
}

impl TryFrom<String> for GlobalId {
    type Error = anyhow::Error;
    fn try_from(s: String) -> Result<Self> {
        Self::validate(&s)?;
        Ok(Self(s))
    }
}

impl From<GlobalId> for String {
    fn from(id: GlobalId) -> String {
        id.0
    }
}

/// A short, human-friendly alias. Lowercase alphanumeric + hyphens.
///
/// Used as local labels for repos and domains in manifests.
/// Must match `[a-z0-9]([a-z0-9-]*[a-z0-9])?`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Slug(String);

impl Slug {
    pub fn new(s: &str) -> Result<Self> {
        Self::validate(s)?;
        Ok(Self(s.to_string()))
    }

    fn validate(s: &str) -> Result<()> {
        if s.is_empty() {
            bail!("slug cannot be empty");
        }
        if !s
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            bail!("slug must be lowercase alphanumeric + hyphens: {s:?}");
        }
        if s.starts_with('-') || s.ends_with('-') {
            bail!("slug must not start or end with '-': {s:?}");
        }
        Ok(())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Slug {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for Slug {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        Self::new(s)
    }
}

impl TryFrom<String> for Slug {
    type Error = anyhow::Error;
    fn try_from(s: String) -> Result<Self> {
        Self::validate(&s)?;
        Ok(Self(s))
    }
}

impl From<Slug> for String {
    fn from(slug: Slug) -> String {
        slug.0
    }
}

/// A parsed `graf://` URI.
///
/// The URI format is `graf://domain/rest` where `domain` is a DNS hostname
/// and `rest` is the remainder that must be resolved against known repo IDs
/// (longest match) to determine the repo ID and document path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrafUri {
    /// The DNS hostname from the URI authority.
    pub domain: String,
    /// Everything after the domain, without leading slash.
    /// Must be resolved against known repo IDs via longest-match.
    pub rest: String,
}

impl GrafUri {
    pub fn parse(s: &str) -> Result<Self> {
        let after_scheme = s
            .strip_prefix("graf://")
            .ok_or_else(|| anyhow::anyhow!("URI must start with 'graf://': {s:?}"))?;

        let slash_pos = after_scheme
            .find('/')
            .ok_or_else(|| anyhow::anyhow!("URI must contain a path after the domain: {s:?}"))?;

        let domain = &after_scheme[..slash_pos];
        let path = &after_scheme[slash_pos + 1..];

        if domain.is_empty() {
            bail!("URI domain cannot be empty: {s:?}");
        }
        if !domain.contains('.') {
            bail!("URI domain must contain '.': {s:?}");
        }
        if path.is_empty() {
            bail!("URI path cannot be empty: {s:?}");
        }

        Ok(Self {
            domain: domain.to_ascii_lowercase(),
            rest: path.to_string(),
        })
    }

    /// Resolve this URI against a set of known repo IDs.
    ///
    /// Returns the matching repo ID and the document path within that repo.
    /// Uses longest-match: if both `acme.com/eng` and `acme.com/eng/frontend`
    /// are known, `graf://acme.com/eng/frontend/foo.md` resolves to
    /// `acme.com/eng/frontend` with doc path `foo.md`.
    pub fn resolve<'a>(
        &self,
        repo_ids: impl IntoIterator<Item = &'a GlobalId>,
    ) -> Result<(&'a GlobalId, PathBuf)> {
        let mut best_match: Option<(&GlobalId, &str)> = None;

        for id in repo_ids {
            if id.domain() != self.domain {
                continue;
            }
            // id_path is the repo's path within its domain (e.g., "life" or "eng/frontend")
            let id_path = &id.as_str()[id.domain().len() + 1..];

            if self.rest.starts_with(id_path) {
                let after = &self.rest[id_path.len()..];
                // Must be followed by '/' and then the doc path
                if let Some(doc_path) = after.strip_prefix('/') {
                    if doc_path.is_empty() {
                        continue;
                    }
                    let dominated = best_match.is_some_and(|(_, cur)| doc_path.len() < cur.len());
                    if best_match.is_none() || dominated {
                        best_match = Some((id, doc_path));
                    }
                }
            }
        }

        match best_match {
            Some((id, doc_path)) => Ok((id, PathBuf::from(doc_path))),
            None => bail!(
                "no known repo matches URI graf://{}/{}",
                self.domain,
                self.rest
            ),
        }
    }
}

impl fmt::Display for GrafUri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "graf://{}/{}", self.domain, self.rest)
    }
}

/// A document reference in any of the three supported forms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocRef {
    /// Bare path relative to repo root: `todo/call-dentist.md`
    Bare(PathBuf),
    /// Qualified path with repo slug: `life:todo/call-dentist.md`
    Qualified { slug: Slug, path: PathBuf },
    /// Graf URI: `graf://example.com/life/todo/call-dentist.md`
    Uri(GrafUri),
}

impl DocRef {
    /// Parse a document reference string into one of the three forms.
    ///
    /// - Starts with `graf://` -> URI
    /// - Contains `:` where the prefix is a valid slug -> qualified path
    /// - Otherwise -> bare path
    pub fn parse(s: &str) -> Result<Self> {
        if s.starts_with("graf://") {
            return Ok(Self::Uri(GrafUri::parse(s)?));
        }

        if let Some(colon_pos) = s.find(':') {
            let maybe_slug = &s[..colon_pos];
            let path = &s[colon_pos + 1..];
            if Slug::validate(maybe_slug).is_ok() && !path.is_empty() {
                return Ok(Self::Qualified {
                    slug: Slug(maybe_slug.to_string()),
                    path: PathBuf::from(path),
                });
            }
        }

        if s.is_empty() {
            bail!("document reference cannot be empty");
        }
        Ok(Self::Bare(PathBuf::from(s)))
    }
}

impl fmt::Display for DocRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bare(p) => write!(f, "{}", p.display()),
            Self::Qualified { slug, path } => write!(f, "{}:{}", slug, path.display()),
            Self::Uri(uri) => write!(f, "{uri}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- GlobalId ----

    #[test]
    fn global_id_valid() {
        assert!(GlobalId::new("example.com/life").is_ok());
        assert!(GlobalId::new("acme.com/eng").is_ok());
        assert!(GlobalId::new("acme.com/eng/frontend").is_ok());
        assert!(GlobalId::new("example.co.uk/my-repo").is_ok());
        assert!(GlobalId::new("a.b/c").is_ok());
    }

    #[test]
    fn global_id_invalid_no_slash() {
        assert!(GlobalId::new("example.com").is_err());
    }

    #[test]
    fn global_id_invalid_no_dot() {
        assert!(GlobalId::new("example/life").is_err());
    }

    #[test]
    fn global_id_invalid_empty_domain() {
        assert!(GlobalId::new("/life").is_err());
    }

    #[test]
    fn global_id_invalid_empty_path() {
        assert!(GlobalId::new("example.com/").is_err());
    }

    #[test]
    fn global_id_invalid_empty_segment() {
        assert!(GlobalId::new("example.com//life").is_err());
    }

    #[test]
    fn global_id_invalid_trailing_slash() {
        assert!(GlobalId::new("example.com/life/").is_err());
    }

    #[test]
    fn global_id_invalid_uppercase() {
        assert!(GlobalId::new("Example.com/life").is_err());
    }

    #[test]
    fn global_id_invalid_colon_in_domain() {
        assert!(GlobalId::new("example:8080.com/life").is_err());
    }

    #[test]
    fn global_id_domain() {
        let id = GlobalId::new("example.com/life").unwrap();
        assert_eq!(id.domain(), "example.com");

        let id = GlobalId::new("acme.com/eng/frontend").unwrap();
        assert_eq!(id.domain(), "acme.com");
    }

    #[test]
    fn global_id_has_prefix_relationship() {
        let a = GlobalId::new("acme.com/eng").unwrap();
        let b = GlobalId::new("acme.com/eng/frontend").unwrap();
        assert!(a.has_prefix_relationship(&b));
        assert!(b.has_prefix_relationship(&a));
    }

    #[test]
    fn global_id_has_prefix_relationship_false_substring() {
        // "eng" is a substring of "engineering" but not at a segment boundary.
        let a = GlobalId::new("acme.com/eng").unwrap();
        let c = GlobalId::new("acme.com/engineering").unwrap();
        assert!(!a.has_prefix_relationship(&c));
    }

    #[test]
    fn global_id_has_prefix_relationship_different_domain() {
        let a = GlobalId::new("acme.com/eng").unwrap();
        let d = GlobalId::new("example.com/life").unwrap();
        assert!(!a.has_prefix_relationship(&d));
    }

    #[test]
    fn global_id_has_prefix_relationship_same_length() {
        let a = GlobalId::new("acme.com/eng").unwrap();
        let b = GlobalId::new("acme.com/ops").unwrap();
        assert!(!a.has_prefix_relationship(&b));
    }

    #[test]
    fn global_id_display() {
        let id = GlobalId::new("example.com/life").unwrap();
        assert_eq!(id.to_string(), "example.com/life");
    }

    #[test]
    fn global_id_serde_roundtrip() {
        let id = GlobalId::new("example.com/life").unwrap();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, r#""example.com/life""#);
        let parsed: GlobalId = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, id);
    }

    #[test]
    fn global_id_serde_rejects_invalid() {
        let result: Result<GlobalId, _> = serde_json::from_str(r#""not-valid""#);
        assert!(result.is_err());
    }

    // ---- Slug ----

    #[test]
    fn slug_valid() {
        assert!(Slug::new("life").is_ok());
        assert!(Slug::new("a").is_ok());
        assert!(Slug::new("my-repo").is_ok());
        assert!(Slug::new("repo123").is_ok());
        assert!(Slug::new("x9").is_ok());
    }

    #[test]
    fn slug_invalid_empty() {
        assert!(Slug::new("").is_err());
    }

    #[test]
    fn slug_invalid_uppercase() {
        assert!(Slug::new("Life").is_err());
    }

    #[test]
    fn slug_invalid_leading_hyphen() {
        assert!(Slug::new("-life").is_err());
    }

    #[test]
    fn slug_invalid_trailing_hyphen() {
        assert!(Slug::new("life-").is_err());
    }

    #[test]
    fn slug_invalid_space() {
        assert!(Slug::new("my repo").is_err());
    }

    #[test]
    fn slug_invalid_underscore() {
        assert!(Slug::new("my_repo").is_err());
    }

    #[test]
    fn slug_invalid_dot() {
        assert!(Slug::new("my.repo").is_err());
    }

    #[test]
    fn slug_display() {
        let slug = Slug::new("my-repo").unwrap();
        assert_eq!(slug.to_string(), "my-repo");
    }

    #[test]
    fn slug_serde_roundtrip() {
        let slug = Slug::new("my-repo").unwrap();
        let json = serde_json::to_string(&slug).unwrap();
        assert_eq!(json, r#""my-repo""#);
        let parsed: Slug = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, slug);
    }

    #[test]
    fn slug_serde_rejects_invalid() {
        let result: Result<Slug, _> = serde_json::from_str(r#""Not-Valid""#);
        assert!(result.is_err());
    }

    // ---- GrafUri ----

    #[test]
    fn graf_uri_parse_basic() {
        let uri = GrafUri::parse("graf://example.com/life/todo/foo.md").unwrap();
        assert_eq!(uri.domain, "example.com");
        assert_eq!(uri.rest, "life/todo/foo.md");
    }

    #[test]
    fn graf_uri_parse_deep_path() {
        let uri = GrafUri::parse("graf://acme.com/eng/frontend/src/app.md").unwrap();
        assert_eq!(uri.domain, "acme.com");
        assert_eq!(uri.rest, "eng/frontend/src/app.md");
    }

    #[test]
    fn graf_uri_parse_normalizes_domain_case() {
        let uri = GrafUri::parse("graf://Example.Com/life/todo/foo.md").unwrap();
        assert_eq!(uri.domain, "example.com");
        assert_eq!(uri.rest, "life/todo/foo.md");
    }

    #[test]
    fn graf_uri_resolve_case_insensitive_domain() {
        let ids = vec![GlobalId::new("example.com/life").unwrap()];
        let uri = GrafUri::parse("graf://EXAMPLE.COM/life/todo/foo.md").unwrap();
        let (matched, doc_path) = uri.resolve(&ids).unwrap();
        assert_eq!(matched, &ids[0]);
        assert_eq!(doc_path, PathBuf::from("todo/foo.md"));
    }

    #[test]
    fn graf_uri_parse_invalid_scheme() {
        assert!(GrafUri::parse("https://example.com/foo").is_err());
    }

    #[test]
    fn graf_uri_parse_invalid_no_path() {
        assert!(GrafUri::parse("graf://example.com").is_err());
    }

    #[test]
    fn graf_uri_parse_invalid_empty_path() {
        assert!(GrafUri::parse("graf://example.com/").is_err());
    }

    #[test]
    fn graf_uri_parse_invalid_no_dot() {
        assert!(GrafUri::parse("graf://localhost/foo").is_err());
    }

    #[test]
    fn graf_uri_resolve_basic() {
        let ids = vec![GlobalId::new("example.com/life").unwrap()];
        let uri = GrafUri::parse("graf://example.com/life/todo/foo.md").unwrap();
        let (matched, doc_path) = uri.resolve(&ids).unwrap();
        assert_eq!(matched, &ids[0]);
        assert_eq!(doc_path, PathBuf::from("todo/foo.md"));
    }

    #[test]
    fn graf_uri_resolve_longest_match() {
        let ids = vec![
            GlobalId::new("acme.com/eng").unwrap(),
            GlobalId::new("acme.com/eng/frontend").unwrap(),
        ];
        let uri = GrafUri::parse("graf://acme.com/eng/frontend/src/app.md").unwrap();
        let (matched, doc_path) = uri.resolve(&ids).unwrap();
        assert_eq!(matched, &ids[1]);
        assert_eq!(doc_path, PathBuf::from("src/app.md"));
    }

    #[test]
    fn graf_uri_resolve_shorter_match() {
        // URI matches the shorter repo ID when the longer one doesn't match.
        let ids = vec![
            GlobalId::new("acme.com/eng").unwrap(),
            GlobalId::new("acme.com/eng/frontend").unwrap(),
        ];
        let uri = GrafUri::parse("graf://acme.com/eng/backend/main.md").unwrap();
        let (matched, doc_path) = uri.resolve(&ids).unwrap();
        assert_eq!(matched, &ids[0]);
        assert_eq!(doc_path, PathBuf::from("backend/main.md"));
    }

    #[test]
    fn graf_uri_resolve_no_match() {
        let ids = vec![GlobalId::new("example.com/life").unwrap()];
        let uri = GrafUri::parse("graf://acme.com/eng/todo.md").unwrap();
        assert!(uri.resolve(&ids).is_err());
    }

    #[test]
    fn graf_uri_resolve_no_match_different_path() {
        let ids = vec![GlobalId::new("example.com/life").unwrap()];
        let uri = GrafUri::parse("graf://example.com/work/todo.md").unwrap();
        assert!(uri.resolve(&ids).is_err());
    }

    #[test]
    fn graf_uri_resolve_no_doc_path() {
        // URI that matches a repo ID exactly but has no document path after it.
        // This should fail — a URI must reference a document, not just a repo.
        let ids = vec![GlobalId::new("example.com/life").unwrap()];
        let uri = GrafUri {
            domain: "example.com".to_string(),
            rest: "life".to_string(),
        };
        assert!(uri.resolve(&ids).is_err());
    }

    #[test]
    fn graf_uri_resolve_empty_ids() {
        let ids: Vec<GlobalId> = vec![];
        let uri = GrafUri::parse("graf://example.com/life/foo.md").unwrap();
        assert!(uri.resolve(&ids).is_err());
    }

    #[test]
    fn graf_uri_display() {
        let uri = GrafUri::parse("graf://example.com/life/todo/foo.md").unwrap();
        assert_eq!(uri.to_string(), "graf://example.com/life/todo/foo.md");
    }

    // ---- DocRef ----

    #[test]
    fn doc_ref_parse_bare() {
        let r = DocRef::parse("todo/call-dentist.md").unwrap();
        assert_eq!(r, DocRef::Bare(PathBuf::from("todo/call-dentist.md")));
    }

    #[test]
    fn doc_ref_parse_bare_simple() {
        let r = DocRef::parse("notes.md").unwrap();
        assert_eq!(r, DocRef::Bare(PathBuf::from("notes.md")));
    }

    #[test]
    fn doc_ref_parse_qualified() {
        let r = DocRef::parse("life:todo/call-dentist.md").unwrap();
        assert_eq!(
            r,
            DocRef::Qualified {
                slug: Slug::new("life").unwrap(),
                path: PathBuf::from("todo/call-dentist.md"),
            }
        );
    }

    #[test]
    fn doc_ref_parse_uri() {
        let r = DocRef::parse("graf://example.com/life/todo/foo.md").unwrap();
        match r {
            DocRef::Uri(uri) => {
                assert_eq!(uri.domain, "example.com");
                assert_eq!(uri.rest, "life/todo/foo.md");
            }
            other => panic!("expected Uri, got {other:?}"),
        }
    }

    #[test]
    fn doc_ref_parse_bare_with_invalid_slug_prefix() {
        // Uppercase before colon isn't a valid slug, so it's a bare path.
        let r = DocRef::parse("My-Doc:something.md").unwrap();
        assert_eq!(r, DocRef::Bare(PathBuf::from("My-Doc:something.md")));
    }

    #[test]
    fn doc_ref_parse_bare_colon_only_prefix() {
        // Hyphen-leading prefix isn't a valid slug.
        let r = DocRef::parse("-bad:foo.md").unwrap();
        assert_eq!(r, DocRef::Bare(PathBuf::from("-bad:foo.md")));
    }

    #[test]
    fn doc_ref_parse_empty() {
        assert!(DocRef::parse("").is_err());
    }

    #[test]
    fn doc_ref_display_roundtrip() {
        let cases = vec![
            "todo/call-dentist.md",
            "life:todo/call-dentist.md",
            "graf://example.com/life/todo/foo.md",
        ];
        for s in cases {
            let r = DocRef::parse(s).unwrap();
            assert_eq!(r.to_string(), s);
        }
    }

    #[test]
    fn doc_ref_colon_with_empty_path_is_bare() {
        // `slug:` with nothing after the colon should be a bare path.
        let r = DocRef::parse("life:").unwrap();
        assert_eq!(r, DocRef::Bare(PathBuf::from("life:")));
    }
}
