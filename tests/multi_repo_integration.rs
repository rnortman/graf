//! Integration tests for multi-repo functionality:
//! slug resolution, multi-repo queries, repo/domain metadata, and error handling.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

fn graf_bin() -> &'static str {
    env!("CARGO_BIN_EXE_graf")
}

/// Run graf with a custom manifest path, returning the output.
/// Isolates GRAF_CONFIG so user config doesn't affect test results.
fn graf(args: &[&str], manifest_path: &Path) -> std::process::Output {
    Command::new(graf_bin())
        .args(args)
        .env("GRAF_MANIFEST", manifest_path)
        .env("GRAF_CONFIG", "/nonexistent/graf-test-config.toml")
        .output()
        .expect("failed to run graf")
}

/// Create a git repo with a .graf/config.toml and some todo files.
/// Returns the repo directory.
fn setup_repo_with_config(
    parent: &Path,
    name: &str,
    id: &str,
    slug: &str,
    domain: Option<&str>,
    manifest_path: &Path,
) -> PathBuf {
    let dir = parent.join(name);
    std::fs::create_dir(&dir).unwrap();
    std::fs::create_dir(dir.join("todo")).unwrap();

    Command::new("git")
        .args(["init"])
        .current_dir(&dir)
        .output()
        .unwrap();
    Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(&dir)
        .output()
        .unwrap();
    Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(&dir)
        .output()
        .unwrap();

    // Initialize .graf/config.toml
    let mut init_args = vec![
        "manifest",
        "init",
        "--id",
        id,
        "--slug",
        slug,
        "--repo",
        dir.to_str().unwrap(),
    ];
    let domain_str;
    if let Some(d) = domain {
        domain_str = d.to_string();
        init_args.push("--domain");
        init_args.push(&domain_str);
    }
    let out = graf(&init_args, manifest_path);
    assert!(
        out.status.success(),
        "manifest init failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Add to manifest
    let out = graf(&["manifest", "add", dir.to_str().unwrap()], manifest_path);
    assert!(
        out.status.success(),
        "manifest add failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    dir
}

/// Add a todo file, commit, and return the path.
fn add_task(repo: &Path, rel_path: &str, frontmatter: &str) {
    let abs_path = repo.join(rel_path);
    if let Some(parent) = abs_path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&abs_path, frontmatter).unwrap();
    Command::new("git")
        .args(["add", "."])
        .current_dir(repo)
        .output()
        .unwrap();
    Command::new("git")
        .args(["commit", "-m", "add task"])
        .current_dir(repo)
        .output()
        .unwrap();
}

// ---------------------------------------------------------------------------
// Multi-repo query tests
// ---------------------------------------------------------------------------

#[test]
fn test_multi_repo_query_returns_tasks_from_both_repos() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = tmp.path().join("manifest.toml");

    let repo1 = setup_repo_with_config(
        tmp.path(),
        "life",
        "test.org/life",
        "life",
        Some("test.org/personal"),
        &manifest,
    );
    let repo2 = setup_repo_with_config(
        tmp.path(),
        "work",
        "test.org/work",
        "work",
        Some("test.org/company"),
        &manifest,
    );

    add_task(
        &repo1,
        "todo/dentist.md",
        "---\ntldr: Call dentist\nstatus: todo\npriority: 2\ncheck_in_date: '2020-01-01'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    );
    add_task(
        &repo2,
        "todo/deploy.md",
        "---\ntldr: Fix deployment\nstatus: todo\npriority: 1\ncheck_in_date: '2020-01-01'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    );

    // Query all repos (no --repo)
    let out = graf(&["todo", "--json"], &manifest);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    let result: Value = serde_json::from_str(&stdout).unwrap();

    let tasks = result["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 2, "should see tasks from both repos");

    // Verify repo slugs are present
    let repos: Vec<&str> = tasks.iter().map(|t| t["repo"].as_str().unwrap()).collect();
    assert!(repos.contains(&"life"));
    assert!(repos.contains(&"work"));

    // Verify domain metadata is present
    let domains = result["domains"].as_array().unwrap();
    let domain_strs: Vec<&str> = domains.iter().map(|d| d.as_str().unwrap()).collect();
    assert!(domain_strs.contains(&"test.org/personal"));
    assert!(domain_strs.contains(&"test.org/company"));

    // Verify each task has a domain field
    for task in tasks {
        assert!(task["domain"].as_str().is_some(), "task should have domain");
    }
}

/// Multi-repo variant of the null-effective_date partition test. A
/// lint-dirty task in one repo plus a valid task in another yields
/// `combined.tasks` containing only the valid task, and `combined.lint_errors`
/// carrying the partition error tagged with the correct `repo` slug.
#[test]
fn test_null_effective_date_partitioned_multi_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = tmp.path().join("manifest.toml");

    let repo_a =
        setup_repo_with_config(tmp.path(), "life", "test.org/life", "life", None, &manifest);
    let repo_b =
        setup_repo_with_config(tmp.path(), "work", "test.org/work", "work", None, &manifest);

    // Repo A: lint-dirty task (no scheduling dates).
    add_task(
        &repo_a,
        "todo/broken.md",
        "---\ntldr: Broken\nstatus: todo\npriority: 3\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    );
    // Repo B: valid task.
    add_task(
        &repo_b,
        "todo/good.md",
        "---\ntldr: Good\nstatus: todo\npriority: 2\ncheck_in_date: '2020-01-01'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    );

    let out = graf(&["todo", "--json"], &manifest);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    let result: Value = serde_json::from_str(&stdout).unwrap();

    // combined.tasks: only the valid task, correctly tagged with its repo.
    let tasks = result["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 1, "expected only valid task; got: {tasks:?}");
    assert_eq!(tasks[0]["tldr"], "Good");
    assert_eq!(tasks[0]["repo"], "work");

    // combined.lint_errors: the broken task is reported via its persisted
    // lint state, tagged with repo: "life". The message names the rule it
    // broke rather than the generic no-effective_date partition text.
    let lint_errors = result["lint_errors"].as_array().unwrap();
    let found = lint_errors.iter().any(|e| {
        e["repo"].as_str() == Some("life")
            && e["message"].as_str() == Some("active task missing check_in_date or due_date")
            && e["path"].as_str() == Some("todo/broken.md")
    });
    assert!(
        found,
        "expected lint_error for repo 'life' with path 'todo/broken.md'; got: {lint_errors:?}"
    );
}

#[test]
fn test_slug_resolution_via_repo_flag() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = tmp.path().join("manifest.toml");

    let repo1 =
        setup_repo_with_config(tmp.path(), "life", "test.org/life", "life", None, &manifest);
    setup_repo_with_config(tmp.path(), "work", "test.org/work", "work", None, &manifest);

    add_task(
        &repo1,
        "todo/task.md",
        "---\ntldr: Life task\nstatus: todo\npriority: 3\ncheck_in_date: '2020-01-01'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    );

    // Query just the "life" repo by slug
    let out = graf(&["todo", "--json", "--repo", "life"], &manifest);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    let result: Value = serde_json::from_str(&stdout).unwrap();

    let tasks = result["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0]["tldr"], "Life task");
    assert_eq!(tasks[0]["repo"], "life");
}

#[test]
fn test_mutation_rejects_multi_repo_without_repo_flag() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = tmp.path().join("manifest.toml");

    let repo1 =
        setup_repo_with_config(tmp.path(), "life", "test.org/life", "life", None, &manifest);
    setup_repo_with_config(tmp.path(), "work", "test.org/work", "work", None, &manifest);

    add_task(
        &repo1,
        "todo/task.md",
        "---\ntldr: Life task\nstatus: todo\npriority: 3\ncheck_in_date: '2020-01-01'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    );

    // todo done without --repo should fail when manifest has multiple repos
    let out = graf(
        &[
            "todo",
            "done",
            "todo/task.md",
            "--completion-date",
            "2026-04-20",
        ],
        &manifest,
    );
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("multiple repos"),
        "expected 'multiple repos' error, got: {stderr}"
    );
}

#[test]
fn test_mutation_with_slug_resolves_correctly() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = tmp.path().join("manifest.toml");

    let repo1 =
        setup_repo_with_config(tmp.path(), "life", "test.org/life", "life", None, &manifest);
    setup_repo_with_config(tmp.path(), "work", "test.org/work", "work", None, &manifest);

    add_task(
        &repo1,
        "todo/task.md",
        "---\ntldr: Life task\nstatus: todo\npriority: 3\ncheck_in_date: '2020-01-01'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    );

    // todo done with --repo slug should work
    let out = graf(
        &[
            "todo",
            "done",
            "todo/task.md",
            "--completion-date",
            "2026-04-20",
            "--repo",
            "life",
        ],
        &manifest,
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    let resp: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(resp["path"], "todo/task.md");
    assert!(resp["on_date"].as_str().is_some());

    // Verify file was updated
    let content = std::fs::read_to_string(repo1.join("todo/task.md")).unwrap();
    assert!(content.contains("status: done"));
}

#[test]
fn test_unknown_slug_gives_helpful_error() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = tmp.path().join("manifest.toml");

    setup_repo_with_config(tmp.path(), "life", "test.org/life", "life", None, &manifest);

    let out = graf(&["todo", "--json", "--repo", "bogus"], &manifest);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unknown repo slug") || stderr.contains("Did you mean a filesystem path"),
        "expected helpful error, got: {stderr}"
    );
}

#[test]
fn test_single_repo_no_manifest_omits_metadata() {
    // Without a manifest, query results should not include repo/domain fields
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("repo");
    std::fs::create_dir(&dir).unwrap();
    std::fs::create_dir(dir.join("todo")).unwrap();

    Command::new("git")
        .args(["init"])
        .current_dir(&dir)
        .output()
        .unwrap();
    Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(&dir)
        .output()
        .unwrap();
    Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(&dir)
        .output()
        .unwrap();
    std::fs::write(
        dir.join("todo/task.md"),
        "---\ntldr: A task\nstatus: todo\npriority: 3\ncheck_in_date: '2020-01-01'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    ).unwrap();
    Command::new("git")
        .args(["add", "."])
        .current_dir(&dir)
        .output()
        .unwrap();
    Command::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(&dir)
        .output()
        .unwrap();

    // Use a nonexistent manifest path so no manifest is loaded
    let fake_manifest = tmp.path().join("no-such-manifest.toml");
    let out = Command::new(graf_bin())
        .args(["todo", "--json", "--repo", dir.to_str().unwrap()])
        .env("GRAF_MANIFEST", &fake_manifest)
        .output()
        .unwrap();

    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    let result: Value = serde_json::from_str(&stdout).unwrap();

    let tasks = result["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 1);
    // repo and domain should be absent (None -> skip_serializing)
    assert!(
        tasks[0].get("repo").is_none(),
        "repo should be absent without manifest"
    );
    assert!(
        tasks[0].get("domain").is_none(),
        "domain should be absent without manifest"
    );
    assert!(
        result.get("domains").is_none(),
        "domains should be absent without manifest"
    );
}

// ---------------------------------------------------------------------------
// Multi-repo sort tests
// ---------------------------------------------------------------------------

#[test]
fn test_multi_repo_query_sorts_globally_by_date() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = tmp.path().join("manifest.toml");

    let repo1 =
        setup_repo_with_config(tmp.path(), "life", "test.org/life", "life", None, &manifest);
    let repo2 =
        setup_repo_with_config(tmp.path(), "work", "test.org/work", "work", None, &manifest);

    // repo1 has dates Jan 3 and Jan 5
    add_task(
        &repo1,
        "todo/task-a.md",
        "---\ntldr: Life Jan3\nstatus: todo\npriority: 3\ncheck_in_date: '2020-01-03'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    );
    add_task(
        &repo1,
        "todo/task-b.md",
        "---\ntldr: Life Jan5\nstatus: todo\npriority: 3\ncheck_in_date: '2020-01-05'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    );

    // repo2 has dates Jan 2 and Jan 4
    add_task(
        &repo2,
        "todo/task-c.md",
        "---\ntldr: Work Jan2\nstatus: todo\npriority: 3\ncheck_in_date: '2020-01-02'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    );
    add_task(
        &repo2,
        "todo/task-d.md",
        "---\ntldr: Work Jan4\nstatus: todo\npriority: 3\ncheck_in_date: '2020-01-04'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    );

    let out = graf(&["todo", "--json"], &manifest);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let result: Value = serde_json::from_str(&String::from_utf8(out.stdout).unwrap()).unwrap();
    let tasks = result["tasks"].as_array().unwrap();
    let tldrs: Vec<&str> = tasks.iter().map(|t| t["tldr"].as_str().unwrap()).collect();
    assert_eq!(
        tldrs,
        ["Work Jan2", "Life Jan3", "Work Jan4", "Life Jan5"],
        "tasks should be globally sorted by date across repos"
    );
}

#[test]
fn test_multi_repo_limit_applied_after_sort() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = tmp.path().join("manifest.toml");

    let repo1 =
        setup_repo_with_config(tmp.path(), "life", "test.org/life", "life", None, &manifest);
    let repo2 =
        setup_repo_with_config(tmp.path(), "work", "test.org/work", "work", None, &manifest);

    // repo1 has a late date, repo2 has an early date
    add_task(
        &repo1,
        "todo/late.md",
        "---\ntldr: Late task\nstatus: todo\npriority: 3\ncheck_in_date: '2020-01-10'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    );
    add_task(
        &repo2,
        "todo/early.md",
        "---\ntldr: Early task\nstatus: todo\npriority: 3\ncheck_in_date: '2020-01-01'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    );

    // --limit 1 should return the globally-earliest task (from repo2)
    let out = graf(&["todo", "--json", "--limit", "1"], &manifest);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let result: Value = serde_json::from_str(&String::from_utf8(out.stdout).unwrap()).unwrap();
    let tasks = result["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(
        tasks[0]["tldr"], "Early task",
        "limit should select globally-earliest, not first-repo's task"
    );
}

// ---------------------------------------------------------------------------
// Cross-repo reorder tests
// ---------------------------------------------------------------------------

#[test]
fn test_reorder_with_cross_repo_anchor() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = tmp.path().join("manifest.toml");

    let repo1 =
        setup_repo_with_config(tmp.path(), "life", "test.org/life", "life", None, &manifest);
    let repo2 =
        setup_repo_with_config(tmp.path(), "work", "test.org/work", "work", None, &manifest);

    add_task(
        &repo1,
        "todo/target.md",
        "---\ntldr: Target\nstatus: todo\npriority: 3\ncheck_in_date: '2020-01-01'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    );
    add_task(
        &repo2,
        "todo/anchor.md",
        "---\ntldr: Anchor\nstatus: todo\npriority: 3\nsort_order: 10.0\ncheck_in_date: '2020-06-15'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    );

    // First query both repos to populate caches
    let out = graf(&["todo", "--json"], &manifest);
    assert!(out.status.success());

    // Reorder target in life repo, using work:todo/anchor.md as --after
    let out = graf(
        &[
            "todo",
            "reorder",
            "todo/target.md",
            "--repo",
            "life",
            "--after",
            "work:todo/anchor.md",
        ],
        &manifest,
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let result: Value = serde_json::from_str(&String::from_utf8(out.stdout).unwrap()).unwrap();
    // sort_order should be anchor's 10.0 + 1.0 = 11.0
    assert_eq!(result["sort_order"], 11.0);
    // tentative_date inferred from anchor's effective date
    assert_eq!(result["tentative_date"], "2020-06-15");
}

#[test]
fn test_reorder_bare_path_with_colon_falls_through() {
    // A bare path containing ':' where the prefix is not a known slug
    // should be treated as a bare path in the target repo.
    let tmp = tempfile::tempdir().unwrap();
    let manifest = tmp.path().join("manifest.toml");

    let repo1 =
        setup_repo_with_config(tmp.path(), "life", "test.org/life", "life", None, &manifest);

    add_task(
        &repo1,
        "todo/target.md",
        "---\ntldr: Target\nstatus: todo\npriority: 3\ncheck_in_date: '2020-01-01'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    );
    add_task(
        &repo1,
        "todo/neighbor.md",
        "---\ntldr: Neighbor\nstatus: todo\npriority: 3\nsort_order: 5.0\ncheck_in_date: '2020-01-01'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    );

    // Query to populate cache
    let out = graf(&["todo", "--json", "--repo", "life"], &manifest);
    assert!(out.status.success());

    // Use "notaslug:todo/neighbor.md" — should fail because it'll try to
    // find it as a bare path (the whole string) and it won't exist
    let out = graf(
        &[
            "todo",
            "reorder",
            "todo/target.md",
            "--repo",
            "life",
            "--after",
            "notaslug:todo/neighbor.md",
        ],
        &manifest,
    );
    assert!(
        !out.status.success(),
        "should fail: 'notaslug' is not a slug, so whole string is treated as bare path"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not found in cache"),
        "expected 'not found in cache', got: {stderr}"
    );
}

/// Extract the ordered `tldr` sequence from a `graf todo --json` run.
fn query_tldrs(args: &[&str], manifest: &Path) -> Vec<String> {
    let out = graf(args, manifest);
    assert!(
        out.status.success(),
        "query {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_str(&String::from_utf8(out.stdout).unwrap()).unwrap();
    v["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["tldr"].as_str().unwrap().to_string())
        .collect()
}

/// Cross-check the SQL `ORDER BY` against the Rust merge comparator for a set
/// of items, and pin the absolute expected order.
///
/// `items` is `(filename, frontmatter)`. `repo_of` assigns each item to repo 0
/// (`life`) or 1 (`work`) for the multi-repo path, where each repo is SQL-sorted
/// and the merged order comes from `todo_sort_cmp`. The SQL oracle places every
/// item in a single repo, so its order is pure SQL `ORDER BY`. Both orderings
/// must equal `expected` — the test fails even if both engines drifted in
/// tandem. Distribute competing items across both repos so the comparator, not
/// SQL, decides their relative order.
fn assert_sql_matches_merge(
    query_args: &[&str],
    items: &[(&str, &str)],
    repo_of: &[usize],
    expected: &[&str],
) {
    assert_eq!(items.len(), repo_of.len());
    assert!(
        repo_of.contains(&0) && repo_of.contains(&1),
        "comparator is only exercised cross-repo; each repo also needs >=1 commit to sync"
    );
    let tmp = tempfile::tempdir().unwrap();

    // SQL oracle: all items in one repo, pure SQL ORDER BY.
    let oracle_manifest = tmp.path().join("oracle.toml");
    let oracle = setup_repo_with_config(
        tmp.path(),
        "oracle",
        "test.org/oracle",
        "oracle",
        None,
        &oracle_manifest,
    );
    for (name, fm) in items {
        add_task(&oracle, name, fm);
    }
    let sql_order = query_tldrs(query_args, &oracle_manifest);

    // Rust merge: items split across two repos so the comparator decides
    // cross-repo order.
    let split_manifest = tmp.path().join("split.toml");
    let life = setup_repo_with_config(
        tmp.path(),
        "life",
        "test.org/life",
        "life",
        None,
        &split_manifest,
    );
    let work = setup_repo_with_config(
        tmp.path(),
        "work",
        "test.org/work",
        "work",
        None,
        &split_manifest,
    );
    let repos = [&life, &work];
    for ((name, fm), &r) in items.iter().zip(repo_of) {
        add_task(repos[r], name, fm);
    }
    let merge_order = query_tldrs(query_args, &split_manifest);

    let sql: Vec<&str> = sql_order.iter().map(String::as_str).collect();
    let merge: Vec<&str> = merge_order.iter().map(String::as_str).collect();
    assert_eq!(sql.as_slice(), expected, "SQL ORDER BY must match expected");
    assert_eq!(
        merge.as_slice(),
        expected,
        "Rust merge comparator must match expected"
    );
}

#[test]
fn test_multi_repo_sort_matches_single_repo_sql_order() {
    // Verify the Rust merge comparator produces the same ordering as the SQL
    // ORDER BY across effective date, priority rank, and sort_order override.
    // Competing jan2 items live in different repos so the comparator, not SQL,
    // decides their order.
    assert_sql_matches_merge(
        &["todo", "--json"],
        &[
            (
                "todo/jan1.md",
                "---\ntldr: Jan1\nstatus: todo\npriority: 3\ncheck_in_date: '2020-01-01'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
            ),
            (
                "todo/jan2-so.md",
                "---\ntldr: Jan2 so0.5\nstatus: todo\npriority: 5\nsort_order: 0.5\ncheck_in_date: '2020-01-02'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
            ),
            (
                "todo/jan2-pri1.md",
                "---\ntldr: Jan2 pri1\nstatus: todo\npriority: 1\ncheck_in_date: '2020-01-02'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
            ),
            (
                "todo/jan2-pri5.md",
                "---\ntldr: Jan2 pri5\nstatus: todo\npriority: 5\ncheck_in_date: '2020-01-02'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
            ),
        ],
        // Jan1→life, so0.5→work, pri1→life, pri5→work: every adjacent pair in
        // the expected order spans repos.
        &[0, 1, 0, 1],
        &["Jan1", "Jan2 so0.5", "Jan2 pri1", "Jan2 pri5"],
    );
}

#[test]
fn test_multi_repo_sort_unranked_matches_sql() {
    // Verify SQL and Rust agree on where unranked items (no sort_order,
    // no priority) sort — they use UNRANKED (6.0), landing after all
    // prioritized items. Same date, so rank alone orders them; competitors
    // split across repos so the comparator decides. The unranked item's tldr
    // ("A unranked") is byte-order-first, so byte order contradicts rank
    // order: only correct rank handling (UNRANKED sorts last) yields the
    // expected sequence — dropping the comparator's rank term reorders it.
    let expected = ["Pri1", "Pri5", "A unranked"];
    assert_sql_matches_merge(
        &["todo", "--json"],
        &[
            (
                "todo/pri1.md",
                "---\ntldr: Pri1\nstatus: todo\npriority: 1\ncheck_in_date: '2020-01-01'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
            ),
            (
                "todo/pri5.md",
                "---\ntldr: Pri5\nstatus: todo\npriority: 5\ncheck_in_date: '2020-01-01'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
            ),
            (
                "todo/unranked.md",
                "---\ntldr: A unranked\nstatus: todo\ncheck_in_date: '2020-01-01'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
            ),
        ],
        // Pri1→life, Pri5→work, A unranked→life: the Pri5/unranked decision is
        // cross-repo.
        &[0, 1, 0],
        &expected,
    );
    // Unranked sorts last (rank 6.0 > priority 5) despite its byte-first tldr;
    // enforced by the expected order inside assert_sql_matches_merge.
}

#[test]
fn test_multi_repo_sort_terminal_matches_sql() {
    // Terminal items (done/cancelled) must sort after all active items even
    // when their effective dates are earlier. SQL uses `status IN
    // ('done','cancelled')`; Rust uses `is_terminal()`. Both active and
    // terminal items live in both repos, so the comparator decides the
    // active/terminal boundary cross-repo. Query with --include-done.
    let expected = ["Active life", "Active work", "Done life", "Cancelled work"];
    assert_sql_matches_merge(
        &["todo", "--json", "--include-done"],
        &[
            (
                "todo/active-life.md",
                "---\ntldr: Active life\nstatus: todo\npriority: 3\ncheck_in_date: '2020-06-01'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
            ),
            (
                "todo/active-work.md",
                "---\ntldr: Active work\nstatus: todo\npriority: 3\ncheck_in_date: '2020-06-02'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
            ),
            // Terminal items carry on_date (their post-completion effective
            // date), set earlier than every active item's date.
            (
                "todo/done-life.md",
                "---\ntldr: Done life\nstatus: done\non_date: '2020-01-01'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
            ),
            (
                "todo/cancelled-work.md",
                "---\ntldr: Cancelled work\nstatus: cancelled\non_date: '2020-01-02'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
            ),
        ],
        // life holds Active life + Done life; work holds Active work +
        // Cancelled work. Placing Active work before Done life despite Done
        // life's earlier date exercises terminal-last handling cross-repo.
        &[0, 1, 0, 1],
        &expected,
    );
    // Terminal items sort last, in effective-date order among themselves;
    // enforced by the expected order inside assert_sql_matches_merge.
}

#[test]
fn test_multi_repo_sort_title_tiebreak_matches_sql() {
    // Items identical on status, effective date, and rank, differing only in
    // tldr, split across repos and created in non-alphabetical order. The
    // uppercase/lowercase pair (`B` / `a`) pins the shared byte-order
    // semantics: SQLite's default BINARY collation is memcmp over UTF-8, and
    // Rust str Ord is byte-wise, so 'B' (0x42) < 'a' (0x61) on both sides.
    assert_sql_matches_merge(
        &["todo", "--json"],
        &[
            // Added in non-alphabetical order; SQL/Rust must sort by tldr.
            (
                "todo/a.md",
                "---\ntldr: a task\nstatus: todo\npriority: 3\ncheck_in_date: '2020-01-01'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
            ),
            (
                "todo/d.md",
                "---\ntldr: D task\nstatus: todo\npriority: 3\ncheck_in_date: '2020-01-01'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
            ),
            (
                "todo/b.md",
                "---\ntldr: B task\nstatus: todo\npriority: 3\ncheck_in_date: '2020-01-01'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
            ),
            (
                "todo/c.md",
                "---\ntldr: C task\nstatus: todo\npriority: 3\ncheck_in_date: '2020-01-01'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
            ),
        ],
        // B→life, C→work, D→life, a→work: C-before-D and D-before-`a` (the
        // case corner) are both cross-repo decisions.
        &[1, 0, 0, 1],
        &["B task", "C task", "D task", "a task"],
    );
}

#[test]
fn test_reorder_cross_repo_anchor_no_cache() {
    // Cross-repo anchor referencing a repo that has never been queried
    // should fail with a helpful "no cache DB" message.
    let tmp = tempfile::tempdir().unwrap();
    let manifest = tmp.path().join("manifest.toml");

    let repo1 =
        setup_repo_with_config(tmp.path(), "life", "test.org/life", "life", None, &manifest);
    let repo2 =
        setup_repo_with_config(tmp.path(), "work", "test.org/work", "work", None, &manifest);

    add_task(
        &repo1,
        "todo/target.md",
        "---\ntldr: Target\nstatus: todo\npriority: 3\ncheck_in_date: '2020-01-01'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    );
    add_task(
        &repo2,
        "todo/anchor.md",
        "---\ntldr: Anchor\nstatus: todo\npriority: 3\nsort_order: 5.0\ncheck_in_date: '2020-01-01'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    );

    // Do NOT query work repo — its cache doesn't exist
    // Only query life repo to populate its cache
    let out = graf(&["todo", "--json", "--repo", "life"], &manifest);
    assert!(out.status.success());

    // Reorder using cross-repo anchor from never-queried work repo
    let out = graf(
        &[
            "todo",
            "reorder",
            "todo/target.md",
            "--repo",
            "life",
            "--after",
            "work:todo/anchor.md",
        ],
        &manifest,
    );
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no cache DB") || stderr.contains("query this repo"),
        "expected helpful 'no cache' error, got: {stderr}"
    );
}

#[test]
fn test_reorder_same_repo_anchor_via_slug_syntax() {
    // Using slug:path where the slug refers to the same repo as the target
    // should work — it takes the cross-repo code path but points at the same DB.
    let tmp = tempfile::tempdir().unwrap();
    let manifest = tmp.path().join("manifest.toml");

    let repo1 =
        setup_repo_with_config(tmp.path(), "life", "test.org/life", "life", None, &manifest);
    setup_repo_with_config(tmp.path(), "work", "test.org/work", "work", None, &manifest);

    add_task(
        &repo1,
        "todo/target.md",
        "---\ntldr: Target\nstatus: todo\npriority: 3\ncheck_in_date: '2020-01-01'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    );
    add_task(
        &repo1,
        "todo/anchor.md",
        "---\ntldr: Anchor\nstatus: todo\npriority: 3\nsort_order: 7.0\ncheck_in_date: '2020-03-01'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    );

    // Query life repo to populate cache
    let out = graf(&["todo", "--json", "--repo", "life"], &manifest);
    assert!(
        out.status.success(),
        "query failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Reorder using life:todo/anchor.md (same repo as target)
    let out = graf(
        &[
            "todo",
            "reorder",
            "todo/target.md",
            "--repo",
            "life",
            "--after",
            "life:todo/anchor.md",
        ],
        &manifest,
    );
    assert!(
        out.status.success(),
        "same-repo slug:path should work, stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let result: Value = serde_json::from_str(&String::from_utf8(out.stdout).unwrap()).unwrap();
    assert_eq!(result["sort_order"], 8.0); // 7.0 + 1.0
    assert_eq!(result["tentative_date"], "2020-03-01");
}

// ---------------------------------------------------------------------------
// MCP manifest tool tests
// ---------------------------------------------------------------------------

#[test]
fn test_mcp_tools_list_includes_manifest_tools() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("repo");
    std::fs::create_dir(&dir).unwrap();
    Command::new("git")
        .args(["init"])
        .current_dir(&dir)
        .output()
        .unwrap();
    Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(&dir)
        .output()
        .unwrap();
    Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(&dir)
        .output()
        .unwrap();

    let mut child = Command::new(graf_bin())
        .args(["mcp", "--repo", dir.to_str().unwrap()])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();

    let stdin = child.stdin.as_mut().unwrap();
    use std::io::Write;
    let requests = [
        serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
        serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
    ];
    for req in &requests {
        writeln!(stdin, "{}", serde_json::to_string(req).unwrap()).unwrap();
    }
    drop(child.stdin.take());

    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());

    let responses: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();

    let tools = responses[1]["result"]["tools"].as_array().unwrap();
    let tool_names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(
        tool_names.contains(&"graf_manifest_list"),
        "missing graf_manifest_list, got: {tool_names:?}"
    );
    assert!(
        tool_names.contains(&"graf_manifest_check"),
        "missing graf_manifest_check, got: {tool_names:?}"
    );
}

// ---------------------------------------------------------------------------
// Manifest JSON output tests
// ---------------------------------------------------------------------------

#[test]
fn test_manifest_list_json_output() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = tmp.path().join("manifest.toml");

    setup_repo_with_config(
        tmp.path(),
        "life",
        "test.org/life",
        "life",
        Some("test.org/personal"),
        &manifest,
    );

    let out = graf(&["manifest", "list", "--json"], &manifest);
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    let result: Value = serde_json::from_str(&stdout).unwrap();

    let repos = result["repos"].as_array().unwrap();
    assert_eq!(repos.len(), 1);
    assert_eq!(repos[0]["slug"], "life");
    assert_eq!(repos[0]["id"], "test.org/life");
}

#[test]
fn test_manifest_check_json_output() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = tmp.path().join("manifest.toml");

    setup_repo_with_config(tmp.path(), "life", "test.org/life", "life", None, &manifest);

    let out = graf(&["manifest", "check", "--json"], &manifest);
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    let result: Value = serde_json::from_str(&stdout).unwrap();

    assert_eq!(result["ok"], true);
    assert_eq!(result["repos"], 1);
    assert_eq!(result["domains"], 0);
    assert!(result["errors"].as_array().unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// slug:path target resolution tests
// ---------------------------------------------------------------------------

#[test]
fn test_done_with_slug_path() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = tmp.path().join("manifest.toml");

    let repo = setup_repo_with_config(tmp.path(), "life", "test.org/life", "life", None, &manifest);

    add_task(
        &repo,
        "todo/task.md",
        "---\ntldr: Test task\nstatus: todo\npriority: 3\ncheck_in_date: '2020-01-01'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    );

    // Mark done using slug:path instead of --repo + bare path
    let out = graf(
        &[
            "todo",
            "--json",
            "done",
            "life:todo/task.md",
            "--completion-date",
            "2026-04-20",
        ],
        &manifest,
    );
    assert!(
        out.status.success(),
        "slug:path done failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let result: Value = serde_json::from_str(&String::from_utf8(out.stdout).unwrap()).unwrap();
    assert!(result.get("on_date").is_some());
}

#[test]
fn test_cancel_with_slug_path() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = tmp.path().join("manifest.toml");

    let repo = setup_repo_with_config(tmp.path(), "life", "test.org/life", "life", None, &manifest);

    add_task(
        &repo,
        "todo/task.md",
        "---\ntldr: Test task\nstatus: todo\npriority: 3\ncheck_in_date: '2020-01-01'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    );

    // Cancel using slug:path instead of --repo + bare path
    let out = graf(
        &["todo", "--json", "cancel", "life:todo/task.md"],
        &manifest,
    );
    assert!(
        out.status.success(),
        "slug:path cancel failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let result: Value = serde_json::from_str(&String::from_utf8(out.stdout).unwrap()).unwrap();
    assert!(result.get("on_date").is_some());

    // Verify file was updated
    let content = std::fs::read_to_string(repo.join("todo/task.md")).unwrap();
    assert!(content.contains("status: cancelled"));
}

#[test]
fn test_schedule_with_slug_path() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = tmp.path().join("manifest.toml");

    let repo = setup_repo_with_config(tmp.path(), "life", "test.org/life", "life", None, &manifest);

    add_task(
        &repo,
        "todo/task.md",
        "---\ntldr: Test task\nstatus: todo\npriority: 3\ncheck_in_date: '2020-01-01'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    );

    // Schedule using slug:path
    let out = graf(
        &[
            "todo",
            "--json",
            "schedule",
            "life:todo/task.md",
            "2026-06-01",
        ],
        &manifest,
    );
    assert!(
        out.status.success(),
        "slug:path schedule failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let result: Value = serde_json::from_str(&String::from_utf8(out.stdout).unwrap()).unwrap();
    assert_eq!(result["tentative_date"], "2026-06-01");
}

#[test]
fn test_reorder_target_with_slug_path() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = tmp.path().join("manifest.toml");

    let repo = setup_repo_with_config(tmp.path(), "life", "test.org/life", "life", None, &manifest);

    add_task(
        &repo,
        "todo/target.md",
        "---\ntldr: Target\nstatus: todo\npriority: 3\ncheck_in_date: '2020-01-01'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    );
    add_task(
        &repo,
        "todo/anchor.md",
        "---\ntldr: Anchor\nstatus: todo\npriority: 3\nsort_order: 5.0\ncheck_in_date: '2020-03-01'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    );

    // Populate cache
    let out = graf(&["todo", "--json", "--repo", "life"], &manifest);
    assert!(out.status.success());

    // Reorder using slug:path for the target (no --repo needed)
    let out = graf(
        &[
            "todo",
            "--json",
            "reorder",
            "life:todo/target.md",
            "--after",
            "life:todo/anchor.md",
        ],
        &manifest,
    );
    assert!(
        out.status.success(),
        "slug:path reorder failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let result: Value = serde_json::from_str(&String::from_utf8(out.stdout).unwrap()).unwrap();
    assert_eq!(result["sort_order"], 6.0); // 5.0 + 1.0
}

#[test]
fn test_add_with_slug_path() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = tmp.path().join("manifest.toml");

    let repo = setup_repo_with_config(tmp.path(), "life", "test.org/life", "life", None, &manifest);

    // Need at least one commit for graf to work
    add_task(
        &repo,
        "todo/placeholder.md",
        "---\ntldr: Placeholder\nstatus: done\non_date: '2020-01-01'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    );

    // Add using slug:path
    let out = graf(
        &[
            "todo",
            "--json",
            "add",
            "life:todo/new-task.md",
            "A new task",
            "--check-in",
            "2026-05-01",
        ],
        &manifest,
    );
    assert!(
        out.status.success(),
        "slug:path add failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let result: Value = serde_json::from_str(&String::from_utf8(out.stdout).unwrap()).unwrap();
    assert_eq!(result["path"], "todo/new-task.md");
}

#[test]
fn test_slug_path_conflicts_with_repo_flag() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = tmp.path().join("manifest.toml");

    setup_repo_with_config(tmp.path(), "life", "test.org/life", "life", None, &manifest);

    // Using both slug:path and --repo should error
    let out = graf(
        &[
            "todo",
            "--json",
            "done",
            "life:todo/task.md",
            "--completion-date",
            "2026-04-20",
            "--repo",
            "life",
        ],
        &manifest,
    );
    assert!(
        !out.status.success(),
        "should fail when slug:path and --repo are both given"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("conflicting repo"),
        "expected conflict error, got: {stderr}"
    );
}

#[test]
fn test_unknown_slug_in_slug_path_gives_helpful_error() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = tmp.path().join("manifest.toml");

    setup_repo_with_config(tmp.path(), "life", "test.org/life", "life", None, &manifest);

    // Use a slug:path where the slug doesn't exist in the manifest
    let out = graf(
        &[
            "todo",
            "--json",
            "done",
            "bogus:todo/task.md",
            "--completion-date",
            "2026-04-20",
        ],
        &manifest,
    );
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unknown repo slug"),
        "expected 'unknown repo slug' error, got: {stderr}"
    );
}

#[test]
fn test_bare_path_still_works_with_repo_flag() {
    // Verify bare paths (no slug:) continue to work as before with --repo
    let tmp = tempfile::tempdir().unwrap();
    let manifest = tmp.path().join("manifest.toml");

    let repo = setup_repo_with_config(tmp.path(), "life", "test.org/life", "life", None, &manifest);

    add_task(
        &repo,
        "todo/task.md",
        "---\ntldr: Test task\nstatus: todo\npriority: 3\ncheck_in_date: '2020-01-01'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    );

    // Use bare path + --repo (the old way) — should still work
    let out = graf(
        &[
            "todo",
            "--json",
            "done",
            "todo/task.md",
            "--completion-date",
            "2026-04-20",
            "--repo",
            "life",
        ],
        &manifest,
    );
    assert!(
        out.status.success(),
        "bare path with --repo should still work: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let result: Value = serde_json::from_str(&String::from_utf8(out.stdout).unwrap()).unwrap();
    assert!(result.get("on_date").is_some());
}
