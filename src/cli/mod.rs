use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use ordered_float::OrderedFloat;

use graf::frontmatter::Status;
use graf::identity::{DocRef, GlobalId, Slug};
use graf::lint::{LintOptions, LintResult};
use graf::manifest::{AppManifest, RepoConfig};
use graf::todo::{QueryOptions, ReorderAnchor, TodoError, TodoItem, TodoQueryResult};

pub mod fix;
pub mod lint;
pub mod manifest;
pub mod mcp;
pub mod reindex;
pub mod todo;
pub mod traverse;

/// A resolved repo: filesystem path with optional identity metadata.
#[derive(Debug, Clone)]
pub struct ResolvedRepo {
    pub path: PathBuf,
    pub slug: Option<Slug>,
    pub domain: Option<GlobalId>,
}

impl ResolvedRepo {
    /// Human-readable label: slug if available, otherwise filesystem path.
    pub fn label(&self) -> String {
        self.slug
            .as_ref()
            .map(|s| s.to_string())
            .unwrap_or_else(|| self.path.display().to_string())
    }
}

/// Resolve the `--repo` argument against the application manifest.
///
/// When `--repo` is given:
///   - If it looks like a path (contains `/`, `~`, or starts with `.`), treat as path.
///   - Otherwise, look up as slug in the manifest.
///
/// When `--repo` is not given:
///   - If a manifest exists: return all repos in the manifest.
///   - Otherwise: return cwd as a single repo.
///
/// The caller supplies the manifest (normally from `AppManifest::load_default()`).
/// `None` means no manifest is active, driving the cwd fallback and the
/// "no manifest found" slug errors.
pub fn resolve_repos(
    repo_arg: Option<&str>,
    manifest: Option<&AppManifest>,
) -> Result<Vec<ResolvedRepo>> {
    match repo_arg {
        Some(arg) => {
            if looks_like_path(arg) {
                // Explicit filesystem path
                let path = graf::expand_tilde(arg)?;
                Ok(vec![resolve_from_path(path)?])
            } else {
                // Slug lookup
                let slug = Slug::new(arg).with_context(|| format!("invalid repo slug: {arg}"))?;
                // TODO(resolve-slug-arm-duplication): shares logic with the
                // resolve_target Qualified arm below.
                let manifest = manifest.ok_or_else(|| {
                    anyhow::anyhow!(
                        "no manifest found — cannot resolve slug {arg:?}. \
                         Did you mean a filesystem path? Use './{arg}' instead."
                    )
                })?;
                let entry = manifest.find_repo_by_slug(&slug).ok_or_else(|| {
                    anyhow::anyhow!(
                        "unknown repo slug {arg:?}. \
                         Did you mean a filesystem path? Use './{arg}' instead."
                    )
                })?;
                let path = AppManifest::expand_path(&entry.path)?;
                let config = load_repo_config_required(&path, &slug)?;
                Ok(vec![ResolvedRepo {
                    path,
                    slug: Some(slug),
                    domain: config.domain,
                }])
            }
        }
        None => {
            match manifest {
                Some(m) if !m.repo.is_empty() => {
                    // Manifest active: return all repos
                    let mut repos = Vec::with_capacity(m.repo.len());
                    for entry in &m.repo {
                        let path = AppManifest::expand_path(&entry.path)?;
                        let config = load_repo_config_required(&path, &entry.slug)?;
                        repos.push(ResolvedRepo {
                            path,
                            slug: Some(entry.slug.clone()),
                            domain: config.domain,
                        });
                    }
                    Ok(repos)
                }
                _ => {
                    // No manifest: fall back to cwd
                    let path = std::env::current_dir()?;
                    Ok(vec![resolve_from_path(path)?])
                }
            }
        }
    }
}

/// Resolve repos, but require exactly one result.  Used by mutation commands.
///
/// The caller supplies the manifest (normally from `AppManifest::load_default()`).
pub fn resolve_single_repo(
    repo_arg: Option<&str>,
    manifest: Option<&AppManifest>,
) -> Result<ResolvedRepo> {
    let repos = resolve_repos(repo_arg, manifest)?;
    if repos.len() > 1 {
        bail!("multiple repos found in manifest — specify --repo for this command");
    }
    Ok(repos
        .into_iter()
        .next()
        .expect("resolve_repos never returns empty"))
}

/// Resolve a target document path for mutation commands.
///
/// Accepts two forms:
///   - `slug:relative/path.md` — look up slug in manifest, return (repo, path)
///   - bare `relative/path.md` — use repo args to resolve repo
///
/// `repo_arg` is the user's explicit `--repo` (CLI) or tool-level `repo` param (MCP).
/// If the path is qualified (`slug:path`), `repo_arg` must be `None` — conflicting is
/// an error.
///
/// `default_repo` is a fallback for bare paths when `repo_arg` is `None`.
/// Used by MCP tools to pass the server-level `--repo` default. CLI callers pass `None`.
///
/// The caller supplies the manifest (normally from `AppManifest::load_default()`).
pub fn resolve_target(
    path_str: &str,
    repo_arg: Option<&str>,
    default_repo: Option<&str>,
    manifest: Option<&AppManifest>,
) -> Result<(ResolvedRepo, PathBuf)> {
    let doc_ref = DocRef::parse(path_str).with_context(|| format!("invalid path: {path_str:?}"))?;

    match doc_ref {
        DocRef::Qualified { slug, path } => {
            if repo_arg.is_some() {
                bail!(
                    "conflicting repo: path {path_str:?} includes repo slug, \
                     but --repo was also specified"
                );
            }
            // TODO(resolve-slug-arm-duplication): shares logic with the
            // resolve_repos slug arm above.
            let manifest = manifest.ok_or_else(|| {
                anyhow::anyhow!("no manifest found — cannot resolve slug in path {path_str:?}")
            })?;
            let entry = manifest.find_repo_by_slug(&slug).ok_or_else(|| {
                anyhow::anyhow!("unknown repo slug \"{slug}\" in path {path_str:?}")
            })?;
            let repo_path = AppManifest::expand_path(&entry.path)?;
            let config = load_repo_config_required(&repo_path, &slug)?;
            Ok((
                ResolvedRepo {
                    path: repo_path,
                    slug: Some(slug),
                    domain: config.domain,
                },
                path,
            ))
        }
        DocRef::Bare(path) => {
            let effective_repo = repo_arg.or(default_repo);
            let repo = resolve_single_repo(effective_repo, manifest)?;
            Ok((repo, path))
        }
        DocRef::Uri(_) => {
            bail!("graf:// URIs not yet supported as mutation targets")
        }
    }
}

/// Resolve a single repo from a filesystem path, loading config best-effort.
fn resolve_from_path(path: PathBuf) -> Result<ResolvedRepo> {
    let config = RepoConfig::load(&path)?;
    Ok(ResolvedRepo {
        path,
        slug: config.as_ref().map(|c| c.default_slug.clone()),
        domain: config.and_then(|c| c.domain),
    })
}

/// Load repo config, failing hard if absent. Used for manifest repos.
fn load_repo_config_required(path: &Path, slug: &Slug) -> Result<RepoConfig> {
    RepoConfig::load(path)?.ok_or_else(|| {
        anyhow::anyhow!(
            "repo {slug} at {} has no .graf/config.toml — run `graf manifest init` first",
            path.display()
        )
    })
}

/// Heuristic: does this string look like a filesystem path rather than a slug?
fn looks_like_path(s: &str) -> bool {
    s.contains('/') || s.contains('~') || s.starts_with('.')
}

// ---------------------------------------------------------------------------
// Multi-repo query
// ---------------------------------------------------------------------------

/// Options for a multi-repo todo query (shared by CLI and MCP).
pub struct MultiRepoQueryOpts {
    pub horizon_days: Option<u32>,
    pub include_done: bool,
    pub statuses: Vec<Status>,
    pub labels: Vec<String>,
    pub limit: Option<usize>,
}

/// Run a todo query across one or more resolved repos, collecting
/// partial failures and domain metadata.
pub fn multi_repo_todo_query(repos: &[ResolvedRepo], opts: &MultiRepoQueryOpts) -> TodoQueryResult {
    let multi_repo = repos.len() > 1;
    let mut combined = TodoQueryResult {
        domains: None,
        tasks: Vec::new(),
        lint_errors: Vec::new(),
    };
    let mut domains_seen: BTreeSet<GlobalId> = BTreeSet::new();
    let mut has_any_identity = false;

    let mut task_lists: Vec<Vec<TodoItem>> = Vec::new();

    for repo in repos {
        let query_opts = QueryOptions {
            repo_dir: repo.path.clone(),
            repo_slug: repo.slug.clone(),
            repo_domain: repo.domain.clone(),
            horizon_days: opts.horizon_days,
            include_done: opts.include_done,
            statuses: opts.statuses.clone(),
            labels: opts.labels.clone(),
            limit: if multi_repo { None } else { opts.limit },
        };

        match graf::todo::todo_query(&query_opts) {
            Ok(result) => {
                if repo.slug.is_some() {
                    has_any_identity = true;
                }
                if let Some(ref domain) = repo.domain {
                    domains_seen.insert(domain.clone());
                }
                task_lists.push(result.tasks);
                combined.lint_errors.extend(result.lint_errors);
            }
            Err(e) => {
                let label = repo.label();
                combined.lint_errors.push(TodoError {
                    path: String::new(),
                    repo: repo.slug.clone(),
                    message: format!("repo {label}: {e}"),
                });
            }
        }
    }

    let limit = if multi_repo { opts.limit } else { None };
    combined.tasks = merge_sorted_tasks(task_lists, limit);

    // Only include domains when operating with identity
    if has_any_identity && !domains_seen.is_empty() {
        combined.domains = Some(domains_seen.into_iter().collect());
    }

    combined
}

// ---------------------------------------------------------------------------
// Multi-repo lint
// ---------------------------------------------------------------------------

/// Options for a multi-repo lint (shared by CLI and MCP).
pub struct MultiRepoLintOpts {
    pub fix: bool,
    pub changed: bool,
    pub files: Vec<PathBuf>,
}

/// Run lint across one or more resolved repos, collecting partial failures.
pub fn multi_repo_lint(repos: &[ResolvedRepo], opts: &MultiRepoLintOpts) -> LintResult {
    let mut combined = LintResult {
        files_checked: 0,
        files_fixed: Vec::new(),
        errors: Vec::new(),
    };

    for repo in repos {
        let options = LintOptions {
            fix: opts.fix,
            repo_dir: repo.path.clone(),
        };

        let result = if !opts.files.is_empty() {
            graf::lint::lint_files(&options, &opts.files)
        } else if opts.changed {
            graf::lint::lint_changed(&options)
        } else {
            graf::lint::lint_all(&options)
        };

        match result {
            Ok(mut r) => {
                combined.files_checked += r.files_checked;
                combined.files_fixed.extend(r.files_fixed);
                if repo.slug.is_some() {
                    for err in &mut r.errors {
                        err.repo = repo.slug.clone();
                    }
                }
                combined.errors.extend(r.errors);
            }
            Err(e) => {
                let mut err =
                    graf::errors::FileError::new(&repo.path, format!("repo {}: {e}", repo.label()));
                err.repo = repo.slug.clone();
                combined.errors.push(err);
            }
        }
    }

    combined
}

// ---------------------------------------------------------------------------
// Manifest JSON helpers (shared by CLI and MCP)
// ---------------------------------------------------------------------------

/// Serialize an `AppManifest` to the standard list JSON format.
pub fn manifest_list_json(manifest: &AppManifest) -> serde_json::Value {
    let repos: Vec<serde_json::Value> = manifest
        .repo
        .iter()
        .map(|r| {
            serde_json::json!({
                "slug": r.slug.as_str(),
                "path": r.path,
                "id": r.id.as_str(),
            })
        })
        .collect();
    let domains: Vec<serde_json::Value> = manifest
        .domain
        .iter()
        .map(|d| {
            serde_json::json!({
                "slug": d.slug.as_str(),
                "id": d.id.as_str(),
            })
        })
        .collect();
    serde_json::json!({ "repos": repos, "domains": domains })
}

/// Validate an `AppManifest` and return the standard check JSON format.
pub fn manifest_check_json(manifest: &AppManifest) -> serde_json::Value {
    let report = manifest.check_report();
    serde_json::json!({
        "ok": report.errors.is_empty(),
        "errors": report.errors,
        "warnings": report.warnings,
        "repos": manifest.repo.len(),
        "domains": manifest.domain.len(),
    })
}

/// Resolve repo path from CLI arg (always a filesystem path, no slug lookup).
/// Used by `manifest init` which creates a config file in a directory.
pub fn resolve_repo(repo: Option<PathBuf>) -> Result<PathBuf> {
    match repo {
        Some(p) => Ok(p),
        None => std::env::current_dir().map_err(Into::into),
    }
}

// ---------------------------------------------------------------------------
// K-way merge of pre-sorted task lists
// ---------------------------------------------------------------------------

/// Compare two todo items for sort order.  Matches the SQL `ORDER BY` in
/// `db.rs`: active items first, then ascending by effective_date,
/// COALESCE(sort_order, priority), tldr.
fn todo_sort_cmp(a: &TodoItem, b: &TodoItem) -> std::cmp::Ordering {
    a.is_terminal()
        .cmp(&b.is_terminal())
        .then_with(|| a.effective_date.cmp(&b.effective_date))
        .then_with(|| rank(a).cmp(&rank(b)))
        .then_with(|| a.tldr.cmp(&b.tldr))
}

/// Sort rank: `COALESCE(sort_order, priority, UNRANKED)`.
/// Matches the SQL `ORDER BY` in `db.rs`.
fn rank(item: &TodoItem) -> OrderedFloat<f64> {
    OrderedFloat(
        item.sort_order
            .or(item.priority.map(|p| p as f64))
            .unwrap_or(graf::frontmatter::UNRANKED),
    )
}

/// K-way merge of pre-sorted task lists. Each input list must be sorted
/// by the SQL ORDER BY (which matches `todo_sort_cmp`). Caller guarantees
/// every item has a non-null `effective_date` (see `TodoQueryResult`
/// invariant).
fn merge_sorted_tasks(task_lists: Vec<Vec<TodoItem>>, limit: Option<usize>) -> Vec<TodoItem> {
    let max = limit.unwrap_or(usize::MAX);
    let mut iters: Vec<std::vec::IntoIter<TodoItem>> =
        task_lists.into_iter().map(|v| v.into_iter()).collect();
    let mut heads: Vec<Option<TodoItem>> = iters.iter_mut().map(|it| it.next()).collect();
    let mut result = Vec::new();

    loop {
        // Find the head with the smallest sort key.
        let min_idx = heads
            .iter()
            .enumerate()
            .filter_map(|(i, h)| h.as_ref().map(|item| (i, item)))
            .min_by(|(_, a), (_, b)| todo_sort_cmp(a, b))
            .map(|(i, _)| i);

        let Some(idx) = min_idx else { break };
        let item = heads[idx].take().unwrap();
        heads[idx] = iters[idx].next();

        result.push(item);
        if result.len() >= max {
            break;
        }
    }

    result
}

// ---------------------------------------------------------------------------
// Anchor resolution for reorder
// ---------------------------------------------------------------------------

/// Parse an anchor string that may be `slug:path` or a bare path,
/// and resolve it to a `ReorderAnchor` by looking up the item in
/// the appropriate repo's cache.
///
/// - If `anchor_str` contains `:` and the prefix matches a known manifest
///   repo slug, opens that repo's cache (no sync) and looks up the path
///   portion.
/// - Otherwise, looks up `anchor_str` as a bare path in `target_cache`.
pub fn resolve_anchor(
    anchor_str: &str,
    target_cache: &graf::db::Cache,
    manifest: Option<&AppManifest>,
) -> Result<ReorderAnchor> {
    if let Some((prefix, suffix)) = anchor_str.split_once(':')
        && let Some(manifest) = manifest
        && let Ok(slug) = Slug::new(prefix)
        && let Some(entry) = manifest.find_repo_by_slug(&slug)
    {
        let repo_path = AppManifest::expand_path(&entry.path)?;
        let cache = graf::db::open_cache_no_sync(&repo_path)?;
        let row = cache
            .get_doc(Path::new(suffix))?
            .with_context(|| format!("anchor item not found in {slug} cache: {suffix}"))?;
        return Ok(ReorderAnchor {
            sort_order: row.sort_order,
            priority: row.priority,
            effective_date: row.effective_date,
        });
    }

    // Bare path — look up in target repo's cache
    let path = Path::new(anchor_str);
    let row = target_cache
        .get_doc(path)?
        .with_context(|| format!("anchor item not found in cache: {anchor_str}"))?;
    Ok(ReorderAnchor {
        sort_order: row.sort_order,
        priority: row.priority,
        effective_date: row.effective_date,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    #[test]
    fn looks_like_path_with_slash() {
        assert!(looks_like_path("./foo"));
        assert!(looks_like_path("/tmp/repo"));
        assert!(looks_like_path("~/grafdata/life"));
        assert!(looks_like_path("../other"));
    }

    #[test]
    fn looks_like_path_slug() {
        assert!(!looks_like_path("life"));
        assert!(!looks_like_path("work"));
        assert!(!looks_like_path("my-repo"));
    }

    // -----------------------------------------------------------------------
    // todo_sort_cmp tests
    // -----------------------------------------------------------------------

    /// Create an active (no status) todo item for sort tests.
    fn make_item(
        effective_date: NaiveDate,
        sort_order: Option<f64>,
        priority: Option<u8>,
        tldr: &str,
    ) -> TodoItem {
        TodoItem {
            path: String::new(),
            repo: None,
            domain: None,
            tldr: tldr.to_string(),
            status: None,
            effective_date,
            tentative_date: None,
            check_in_date: None,
            due_date: None,
            sort_order,
            priority,
            effort: None,
            rrule: None,
            labels: Vec::new(),
        }
    }

    /// Create a terminal (done) item for sort tests.
    /// `effective_date` represents `on_date`/`end_date` (the COALESCE in SQL
    /// falls through to it when scheduling dates are absent).
    fn make_done_item(
        effective_date: NaiveDate,
        sort_order: Option<f64>,
        priority: Option<u8>,
        tldr: &str,
    ) -> TodoItem {
        TodoItem {
            status: Some(Status::Done),
            ..make_item(effective_date, sort_order, priority, tldr)
        }
    }

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn sort_interleaves_dates_across_repos() {
        let mut items = vec![
            make_item(d(2026, 1, 3), Some(1.0), None, "repo1-a"),
            make_item(d(2026, 1, 5), Some(1.0), None, "repo1-b"),
            make_item(d(2026, 1, 2), Some(1.0), None, "repo2-a"),
            make_item(d(2026, 1, 4), Some(1.0), None, "repo2-b"),
        ];
        items.sort_by(|a, b| todo_sort_cmp(a, b));
        let tldrs: Vec<&str> = items.iter().map(|i| i.tldr.as_str()).collect();
        assert_eq!(tldrs, ["repo2-a", "repo1-a", "repo2-b", "repo1-b"]);
    }

    #[test]
    fn sort_dates_ascending() {
        let mut items = vec![
            make_item(d(2026, 1, 1), Some(1.0), None, "dated-late"),
            make_item(d(2025, 12, 1), Some(1.0), None, "dated-early"),
        ];
        items.sort_by(|a, b| todo_sort_cmp(a, b));
        let tldrs: Vec<&str> = items.iter().map(|i| i.tldr.as_str()).collect();
        assert_eq!(tldrs, ["dated-early", "dated-late"]);
    }

    #[test]
    fn sort_tiebreak_by_priority() {
        let mut items = vec![
            make_item(d(2026, 1, 1), None, Some(5), "low-pri"),
            make_item(d(2026, 1, 1), None, Some(1), "high-pri"),
            make_item(d(2026, 1, 1), None, Some(3), "mid-pri"),
        ];
        items.sort_by(|a, b| todo_sort_cmp(a, b));
        let tldrs: Vec<&str> = items.iter().map(|i| i.tldr.as_str()).collect();
        assert_eq!(tldrs, ["high-pri", "mid-pri", "low-pri"]);
    }

    #[test]
    fn sort_order_beats_priority() {
        // sort_order is used when present; priority used as fallback.
        // so=0.5 < pri=1 (as f64) < so=10.0
        let mut items = vec![
            make_item(d(2026, 1, 1), None, Some(1), "pri-1-no-so"),
            make_item(d(2026, 1, 1), Some(10.0), Some(5), "so-10-pri-5"),
            make_item(d(2026, 1, 1), Some(0.5), Some(5), "so-half-pri-5"),
        ];
        items.sort_by(|a, b| todo_sort_cmp(a, b));
        let tldrs: Vec<&str> = items.iter().map(|i| i.tldr.as_str()).collect();
        assert_eq!(tldrs, ["so-half-pri-5", "pri-1-no-so", "so-10-pri-5"]);
    }

    #[test]
    fn sort_no_rank_sorts_after_prioritized() {
        // COALESCE(sort_order, priority, 6): unranked items get rank 6,
        // sorting after all prioritized items (max priority is 5).
        let mut items = vec![
            make_item(d(2026, 1, 1), Some(5.0), None, "has-sort-order"),
            make_item(d(2026, 1, 1), None, None, "no-rank"),
            make_item(d(2026, 1, 1), None, Some(1), "has-priority"),
        ];
        items.sort_by(|a, b| todo_sort_cmp(a, b));
        let tldrs: Vec<&str> = items.iter().map(|i| i.tldr.as_str()).collect();
        assert_eq!(tldrs, ["has-priority", "has-sort-order", "no-rank"]);
    }

    #[test]
    fn sort_tiebreak_by_tldr() {
        let mut items = vec![
            make_item(d(2026, 1, 1), Some(1.0), None, "Zebra"),
            make_item(d(2026, 1, 1), Some(1.0), None, "Apple"),
        ];
        items.sort_by(|a, b| todo_sort_cmp(a, b));
        let tldrs: Vec<&str> = items.iter().map(|i| i.tldr.as_str()).collect();
        assert_eq!(tldrs, ["Apple", "Zebra"]);
    }

    #[test]
    fn sort_limit_applied_after_sort() {
        let mut items = vec![
            make_item(d(2026, 1, 5), Some(1.0), None, "late"),
            make_item(d(2026, 1, 1), Some(1.0), None, "early"),
            make_item(d(2026, 1, 3), Some(1.0), None, "mid"),
            make_item(d(2026, 1, 2), Some(1.0), None, "second"),
        ];
        items.sort_by(|a, b| todo_sort_cmp(a, b));
        items.truncate(2);
        let tldrs: Vec<&str> = items.iter().map(|i| i.tldr.as_str()).collect();
        assert_eq!(tldrs, ["early", "second"]);
    }

    #[test]
    fn sort_active_before_terminal() {
        let mut items = vec![
            make_done_item(d(2026, 4, 10), Some(1.0), None, "done-task"),
            make_item(d(2026, 6, 1), Some(1.0), None, "active-task"),
        ];
        items.sort_by(todo_sort_cmp);
        let tldrs: Vec<&str> = items.iter().map(|i| i.tldr.as_str()).collect();
        assert_eq!(tldrs, ["active-task", "done-task"]);
    }

    #[test]
    fn sort_terminal_ascending_by_date() {
        let mut items = vec![
            make_done_item(d(2026, 1, 1), Some(1.0), None, "old-done"),
            make_done_item(d(2026, 4, 10), Some(1.0), None, "recent-done"),
            make_done_item(d(2026, 2, 15), Some(1.0), None, "mid-done"),
        ];
        items.sort_by(todo_sort_cmp);
        let tldrs: Vec<&str> = items.iter().map(|i| i.tldr.as_str()).collect();
        assert_eq!(tldrs, ["old-done", "mid-done", "recent-done"]);
    }

    #[test]
    fn sort_mixed_active_and_terminal() {
        // Active ascending by date, then terminal ascending by on_date/end_date.
        let mut items = vec![
            make_done_item(d(2026, 4, 10), Some(1.0), None, "done-recent"),
            make_item(d(2026, 5, 1), Some(1.0), None, "active-later"),
            make_done_item(d(2026, 3, 1), Some(1.0), None, "done-old"),
            make_item(d(2026, 4, 15), Some(1.0), None, "active-sooner"),
        ];
        items.sort_by(todo_sort_cmp);
        let tldrs: Vec<&str> = items.iter().map(|i| i.tldr.as_str()).collect();
        assert_eq!(
            tldrs,
            ["active-sooner", "active-later", "done-old", "done-recent"]
        );
    }

    // -----------------------------------------------------------------------
    // merge_sorted_tasks tests
    // -----------------------------------------------------------------------

    #[test]
    fn merge_interleaves_sorted_lists() {
        let list1 = vec![
            make_item(d(2026, 1, 1), Some(1.0), None, "a"),
            make_item(d(2026, 1, 3), Some(1.0), None, "c"),
        ];
        let list2 = vec![
            make_item(d(2026, 1, 2), Some(1.0), None, "b"),
            make_item(d(2026, 1, 4), Some(1.0), None, "d"),
        ];
        let merged = merge_sorted_tasks(vec![list1, list2], None);
        let tldrs: Vec<&str> = merged.iter().map(|i| i.tldr.as_str()).collect();
        assert_eq!(tldrs, ["a", "b", "c", "d"]);
    }

    #[test]
    fn merge_respects_limit() {
        let list1 = vec![
            make_item(d(2026, 1, 1), Some(1.0), None, "a"),
            make_item(d(2026, 1, 3), Some(1.0), None, "c"),
        ];
        let list2 = vec![
            make_item(d(2026, 1, 2), Some(1.0), None, "b"),
            make_item(d(2026, 1, 4), Some(1.0), None, "d"),
        ];
        let merged = merge_sorted_tasks(vec![list1, list2], Some(2));
        let tldrs: Vec<&str> = merged.iter().map(|i| i.tldr.as_str()).collect();
        assert_eq!(tldrs, ["a", "b"]);
    }

    #[test]
    fn merge_single_list_passthrough() {
        let list = vec![
            make_item(d(2026, 1, 1), Some(1.0), None, "a"),
            make_item(d(2026, 1, 2), Some(1.0), None, "b"),
        ];
        let merged = merge_sorted_tasks(vec![list], None);
        let tldrs: Vec<&str> = merged.iter().map(|i| i.tldr.as_str()).collect();
        assert_eq!(tldrs, ["a", "b"]);
    }

    #[test]
    fn merge_empty_lists() {
        let merged = merge_sorted_tasks(vec![vec![], vec![]], None);
        assert!(merged.is_empty());
    }

    // -----------------------------------------------------------------------
    // resolve_* tests
    // -----------------------------------------------------------------------

    use graf::manifest::ManifestRepo;
    use tempfile::TempDir;

    /// Create a temp repo dir containing a minimal `.graf/config.toml`, and
    /// return the dir guard plus a matching `ManifestRepo` entry.
    fn test_repo(slug: &str, domain: Option<&str>) -> (TempDir, ManifestRepo) {
        let dir = TempDir::new().unwrap();
        let slug = Slug::new(slug).unwrap();
        let id = GlobalId::new(&format!("example.com/{slug}")).unwrap();
        let config = RepoConfig {
            id: id.clone(),
            default_slug: slug.clone(),
            domain: domain.map(|d| GlobalId::new(d).unwrap()),
            refs: Default::default(),
        };
        config.save(dir.path()).unwrap();
        let repo = ManifestRepo {
            slug,
            path: dir.path().to_string_lossy().into_owned(),
            id,
        };
        (dir, repo)
    }

    /// Build an in-memory manifest from repo entries (no domains).
    fn manifest_of(repos: Vec<ManifestRepo>) -> AppManifest {
        AppManifest {
            repo: repos,
            domain: Vec::new(),
        }
    }

    #[test]
    fn resolve_target_conflict_slug_path_with_repo_arg() {
        let err = resolve_target("life:todo/foo.md", Some("life"), None, None).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("conflicting repo"),
            "expected conflict error, got: {msg}"
        );
    }

    #[test]
    fn resolve_target_conflict_different_slug_and_repo() {
        let err = resolve_target("life:todo/foo.md", Some("work"), None, None).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("conflicting repo"),
            "expected conflict error, got: {msg}"
        );
    }

    #[test]
    fn resolve_target_uri_rejected() {
        let err =
            resolve_target("graf://example.com/life/todo/foo.md", None, None, None).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("graf:// URIs not yet supported"),
            "expected URI rejection, got: {msg}"
        );
    }

    #[test]
    fn resolve_target_qualified_no_conflict_with_default_repo() {
        // A qualified path resolves via its own slug; default_repo is ignored on
        // that branch (only repo_arg conflicts). default_repo points at a
        // *different* repo to prove it does not steer the result.
        let (life_dir, life) = test_repo("life", None);
        let (_other_dir, other) = test_repo("other", None);
        let manifest = manifest_of(vec![life, other]);
        let (resolved, path) =
            resolve_target("life:todo/foo.md", None, Some("other"), Some(&manifest)).unwrap();
        assert_eq!(resolved.slug.as_ref().unwrap().to_string(), "life");
        assert_eq!(resolved.path, life_dir.path());
        assert_eq!(path, PathBuf::from("todo/foo.md"));
    }

    #[test]
    fn resolve_target_qualified_unknown_slug() {
        let (_dir, repo) = test_repo("life", None);
        let manifest = manifest_of(vec![repo]);
        let err = resolve_target("work:todo/foo.md", None, None, Some(&manifest)).unwrap_err();
        assert!(err.to_string().contains("unknown repo slug"), "got: {err}");
    }

    #[test]
    fn resolve_target_qualified_no_manifest() {
        let err = resolve_target("life:todo/foo.md", None, None, None).unwrap_err();
        assert!(err.to_string().contains("no manifest found"), "got: {err}");
    }

    #[test]
    fn resolve_target_bare_path_uses_default_repo() {
        // default_repo=Some("nonexistent") with a manifest lacking it → the slug
        // lookup fails, proving default_repo was used (not a cwd fall-through).
        let (_dir, repo) = test_repo("life", None);
        let manifest = manifest_of(vec![repo]);
        let err =
            resolve_target("todo/foo.md", None, Some("nonexistent"), Some(&manifest)).unwrap_err();
        assert!(err.to_string().contains("unknown repo slug"), "got: {err}");
    }

    #[test]
    fn resolve_target_bare_path_default_repo_positive() {
        let (dir, repo) = test_repo("life", None);
        let manifest = manifest_of(vec![repo]);
        let (resolved, path) =
            resolve_target("todo/foo.md", None, Some("life"), Some(&manifest)).unwrap();
        assert_eq!(resolved.slug.as_ref().unwrap().to_string(), "life");
        assert_eq!(resolved.path, dir.path());
        assert_eq!(path, PathBuf::from("todo/foo.md"));
    }

    #[test]
    fn resolve_target_bare_path_repo_arg_takes_precedence_over_default() {
        // With both repo_arg and default_repo set, repo_arg wins: the resolved
        // repo is `explicit`, not `default`.
        let (explicit_dir, explicit) = test_repo("explicit", None);
        let (_default_dir, default) = test_repo("default", None);
        let manifest = manifest_of(vec![explicit, default]);
        let (resolved, _path) = resolve_target(
            "todo/foo.md",
            Some("explicit"),
            Some("default"),
            Some(&manifest),
        )
        .unwrap();
        assert_eq!(resolved.slug.as_ref().unwrap().to_string(), "explicit");
        assert_eq!(resolved.path, explicit_dir.path());
    }

    #[test]
    fn resolve_repos_no_arg_returns_all_in_order() {
        let (_a_dir, a) = test_repo("alpha", None);
        let (_b_dir, b) = test_repo("beta", None);
        let manifest = manifest_of(vec![a, b]);
        let repos = resolve_repos(None, Some(&manifest)).unwrap();
        let slugs: Vec<String> = repos
            .iter()
            .map(|r| r.slug.as_ref().unwrap().to_string())
            .collect();
        assert_eq!(slugs, ["alpha", "beta"]);
    }

    #[test]
    fn resolve_repos_slug_plumbs_domain_from_config() {
        let (_dir, repo) = test_repo("life", Some("example.com/household"));
        let manifest = manifest_of(vec![repo]);
        let repos = resolve_repos(Some("life"), Some(&manifest)).unwrap();
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0].slug.as_ref().unwrap().to_string(), "life");
        assert_eq!(
            repos[0].domain.as_ref().map(|d| d.to_string()),
            Some("example.com/household".to_string())
        );
    }

    #[test]
    fn resolve_repos_slug_no_manifest() {
        let err = resolve_repos(Some("life"), None).unwrap_err();
        assert!(err.to_string().contains("no manifest found"), "got: {err}");
    }

    #[test]
    fn resolve_repos_no_arg_no_manifest_falls_back_to_cwd() {
        let repos = resolve_repos(None, None).unwrap();
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0].path, std::env::current_dir().unwrap());
    }

    #[test]
    fn resolve_repos_no_arg_empty_manifest_falls_back_to_cwd() {
        // A present-but-empty manifest with no --repo falls to cwd, same as no
        // manifest at all.
        let manifest = manifest_of(vec![]);
        let repos = resolve_repos(None, Some(&manifest)).unwrap();
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0].path, std::env::current_dir().unwrap());
    }

    #[test]
    fn resolve_single_repo_multiple_repos_errors() {
        let (_a_dir, a) = test_repo("alpha", None);
        let (_b_dir, b) = test_repo("beta", None);
        let manifest = manifest_of(vec![a, b]);
        let err = resolve_single_repo(None, Some(&manifest)).unwrap_err();
        assert!(
            err.to_string().contains("multiple repos found"),
            "got: {err}"
        );
    }

    #[test]
    fn resolve_repos_slug_missing_config_errors() {
        // Manifest entry pointing at a dir without `.graf/config.toml`.
        let dir = TempDir::new().unwrap();
        let repo = ManifestRepo {
            slug: Slug::new("life").unwrap(),
            path: dir.path().to_string_lossy().into_owned(),
            id: GlobalId::new("example.com/repo").unwrap(),
        };
        let manifest = manifest_of(vec![repo]);
        let err = resolve_repos(Some("life"), Some(&manifest)).unwrap_err();
        assert!(
            err.to_string().contains("no .graf/config.toml"),
            "got: {err}"
        );
    }
}
