use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::{Value, json};

fn graf_bin() -> &'static str {
    env!("CARGO_BIN_EXE_graf")
}

/// Build a Command for graf with GRAF_CONFIG isolated so user config
/// doesn't affect test results.
fn graf_cmd() -> Command {
    let mut cmd = Command::new(graf_bin());
    cmd.env("GRAF_CONFIG", "/nonexistent/graf-test-config.toml");
    cmd
}

/// Create a git repo with some todo files.
fn setup_repo(tmp: &std::path::Path) -> std::path::PathBuf {
    let dir = tmp.join("repo");
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

    // Add a todo item with a past check-in date
    std::fs::write(
        dir.join("todo/task-a.md"),
        "---\ntldr: Task A\nstatus: todo\npriority: 2\ncheck_in_date: '2020-01-01'\nsort_order: 1\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    ).unwrap();

    // Add a todo item with a future check-in date
    std::fs::write(
        dir.join("todo/task-b.md"),
        "---\ntldr: Task B\nstatus: todo\npriority: 3\ncheck_in_date: '2030-01-01'\nsort_order: 1\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    ).unwrap();

    // Add a third active task (used as target in reorder tests)
    std::fs::write(
        dir.join("todo/task-c.md"),
        "---\ntldr: Task C\nstatus: todo\npriority: 4\ncheck_in_date: '2030-06-01'\nsort_order: 10\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    ).unwrap();

    // Add a done item
    std::fs::write(
        dir.join("todo/done-task.md"),
        "---\ntldr: Done Task\nstatus: done\npriority: 3\non_date: '2026-04-10'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    ).unwrap();

    Command::new("git")
        .args(["add", "."])
        .current_dir(&dir)
        .output()
        .unwrap();
    Command::new("git")
        .args(["commit", "-m", "initial"])
        .current_dir(&dir)
        .output()
        .unwrap();

    dir
}

/// Write a file under the repo and commit it. Used by tests that need
/// additional files beyond `setup_repo`'s baseline.
fn write_and_commit(repo: &std::path::Path, rel_path: &str, content: &str, msg: &str) {
    std::fs::write(repo.join(rel_path), content).unwrap();
    Command::new("git")
        .args(["add", "."])
        .current_dir(repo)
        .output()
        .unwrap();
    Command::new("git")
        .args(["commit", "-m", msg])
        .current_dir(repo)
        .output()
        .unwrap();
}

// ---------------------------------------------------------------------------
// CLI tests
// ---------------------------------------------------------------------------

#[test]
fn test_reindex_cli() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    let output = graf_cmd()
        .args(["reindex", "--repo", repo.to_str().unwrap()])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let resp: Value = serde_json::from_str(&stdout).unwrap();
    assert!(resp["files_indexed"].as_u64().unwrap() > 0);
}

#[test]
fn test_todo_query_json() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    let output = graf_cmd()
        .args(["todo", "--json", "--repo", repo.to_str().unwrap()])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let result: Value = serde_json::from_str(&stdout).unwrap();

    let tasks = result["tasks"].as_array().unwrap();
    // Should include active tasks (Task A, Task B, Task C) but not Done Task
    assert_eq!(tasks.len(), 3);
}

#[test]
fn test_todo_query_include_done() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    let output = graf_cmd()
        .args([
            "todo",
            "--json",
            "--include-done",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let result: Value = serde_json::from_str(&String::from_utf8(output.stdout).unwrap()).unwrap();
    let tasks = result["tasks"].as_array().unwrap();
    // Task A + Task B + Task C + Done Task
    assert_eq!(tasks.len(), 4);

    // Active items should sort before terminal items.
    // The done task (on_date: 2026-04-10) should be last.
    let tldrs: Vec<&str> = tasks.iter().map(|t| t["tldr"].as_str().unwrap()).collect();
    assert_eq!(
        tldrs.last().unwrap(),
        &"Done Task",
        "terminal item should sort after active items: {tldrs:?}"
    );

    // Active items should be in ascending date order.
    // Task A (check_in 2020-01-01), Task B (check_in 2030-01-01), Task C (check_in 2030-06-01)
    assert_eq!(&tldrs[..3], &["Task A", "Task B", "Task C"]);
}

#[test]
fn test_todo_query_include_done_sort_order() {
    // Verify the full sort contract: active ascending by date,
    // then terminal descending by on_date.
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    // Add more terminal items with different on_dates
    std::fs::write(
        repo.join("todo/done-old.md"),
        "---\ntldr: Done Old\nstatus: done\npriority: 3\non_date: '2026-01-15'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    ).unwrap();
    std::fs::write(
        repo.join("todo/cancelled-recent.md"),
        "---\ntldr: Cancelled Recent\nstatus: cancelled\npriority: 3\non_date: '2026-04-12'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    ).unwrap();

    let output = graf_cmd()
        .args([
            "todo",
            "--json",
            "--include-done",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let result: Value = serde_json::from_str(&String::from_utf8(output.stdout).unwrap()).unwrap();
    let tasks = result["tasks"].as_array().unwrap();
    let tldrs: Vec<&str> = tasks.iter().map(|t| t["tldr"].as_str().unwrap()).collect();

    // Active items first in ascending date order:
    //   Task A (2020-01-01), Task B (2030-01-01), Task C (2030-06-01)
    // Then terminal items in ascending on_date order (SQL sort):
    //   Done Old (2026-01-15), Done Task (2026-04-10), Cancelled Recent (2026-04-12)
    assert_eq!(
        tldrs,
        [
            "Task A",
            "Task B",
            "Task C",
            "Done Old",
            "Done Task",
            "Cancelled Recent",
        ],
        "active ascending then terminal ascending"
    );
}

#[test]
fn test_todo_query_default_no_horizon() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    // Default: no date filtering, returns all active tasks
    let output = graf_cmd()
        .args(["todo", "--json", "--repo", repo.to_str().unwrap()])
        .output()
        .unwrap();

    assert!(output.status.success());
    let result: Value = serde_json::from_str(&String::from_utf8(output.stdout).unwrap()).unwrap();
    let tasks = result["tasks"].as_array().unwrap();
    // Task A (past) + Task B (future) + Task C — all active, no date filtering.
    // Done Task excluded (not active status).
    assert_eq!(tasks.len(), 3, "should return all 3 active tasks");
}

/// A task missing both `check_in_date` and `due_date` has null
/// effective_date in the cache. Such rows stay out of `tasks` (the
/// `TodoQueryResult` invariant) and are reported in `lint_errors` via
/// their persisted lint state, which names the rule they broke. The
/// generic partition message is a last resort and must not appear when
/// persisted state exists.
#[test]
fn test_null_effective_date_partitioned_to_lint_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    // Add a lint-dirty file: active status, no scheduling dates at all.
    // EFF_DATE = COALESCE(null, null, null, null, null) = NULL.
    write_and_commit(
        &repo,
        "todo/no-dates.md",
        "---\ntldr: No Dates Task\nstatus: todo\npriority: 3\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
        "add lint-dirty",
    );

    let output = graf_cmd()
        .args(["todo", "--json", "--repo", repo.to_str().unwrap()])
        .output()
        .unwrap();

    assert!(output.status.success());
    let result: Value = serde_json::from_str(&String::from_utf8(output.stdout).unwrap()).unwrap();

    // Lint-dirty task must not appear in tasks.
    let tasks = result["tasks"].as_array().unwrap();
    assert!(
        tasks.iter().all(|t| t["path"] != "todo/no-dates.md"),
        "null-effective_date row must not appear in tasks: {tasks:?}"
    );

    // It must appear in lint_errors exactly once, naming the rule it broke.
    let lint_errors = result["lint_errors"].as_array().unwrap();
    let broken: Vec<&Value> = lint_errors
        .iter()
        .filter(|e| e["path"] == "todo/no-dates.md")
        .collect();
    assert_eq!(
        broken.len(),
        1,
        "expected exactly one lint_error for todo/no-dates.md; got: {broken:?}"
    );
    assert_eq!(
        broken[0]["message"].as_str(),
        Some("active task missing check_in_date or due_date")
    );
}

/// Terminal (done/cancelled) items that fail their own scheduling-date
/// lint rule are reported the same way. The persisted set is scanned
/// independently of the query's status filter, so the violation surfaces
/// even though the item is out of query scope without `--include-done`.
#[test]
fn test_null_effective_date_partition_for_terminal_status() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    // Done item with no on_date/end_date/due_date: effective_date is NULL
    // and lint emits "done/cancelled item missing on_date or end_date".
    write_and_commit(
        &repo,
        "todo/broken-done.md",
        "---\ntldr: Broken Done\nstatus: done\npriority: 3\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
        "add broken done",
    );

    // Default status filter: the done item is NOT in query scope.
    let output = graf_cmd()
        .args(["todo", "--json", "--repo", repo.to_str().unwrap()])
        .output()
        .unwrap();

    assert!(output.status.success());
    let result: Value = serde_json::from_str(&String::from_utf8(output.stdout).unwrap()).unwrap();

    // Broken done item must not appear in tasks.
    let tasks = result["tasks"].as_array().unwrap();
    assert!(
        tasks.iter().all(|t| t["path"] != "todo/broken-done.md"),
        "terminal row must not appear in tasks under the default filter: {tasks:?}"
    );

    // It is nonetheless reported, with the rule's own message.
    let lint_errors = result["lint_errors"].as_array().unwrap();
    let found = lint_errors.iter().any(|e| {
        e["path"] == "todo/broken-done.md"
            && e["message"].as_str() == Some("done/cancelled item missing on_date or end_date")
    });
    assert!(
        found,
        "expected lint_error for todo/broken-done.md; got: {lint_errors:?}"
    );
}

/// Running the query twice without changing files reports the same
/// rule-specific error both times, exactly once per pass.
///
/// Mechanics: the graf cache persists at `<repo>/.graf.db` between CLI
/// invocations. Pass 1 cold-starts with no cache, triggering `do_reindex`,
/// which lints every file and persists the messages. Pass 2 finds the cache
/// populated and `last_indexed_commit == HEAD`, so the incremental sync
/// skips both reconcile-dirty (no dirty rows) and the commit-diff step —
/// `sync.errors` is empty and the persisted set is the only source. This is
/// the regression the persisted lint state exists to prevent.
#[test]
fn test_null_effective_date_partitioned_across_queries() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    write_and_commit(
        &repo,
        "todo/no-dates.md",
        "---\ntldr: No Dates Task\nstatus: todo\npriority: 3\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
        "add lint-dirty",
    );

    let rule_msg = "active task missing check_in_date or due_date";
    let generic_msg = "item has no effective_date (missing scheduling or completed date)";

    for pass in 1..=2 {
        let output = graf_cmd()
            .args(["todo", "--json", "--repo", repo.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(output.status.success(), "pass {pass} failed");
        let result: Value =
            serde_json::from_str(&String::from_utf8(output.stdout).unwrap()).unwrap();

        let tasks = result["tasks"].as_array().unwrap();
        assert!(
            tasks.iter().all(|t| t["path"] != "todo/no-dates.md"),
            "pass {pass}: null-date row must not appear in tasks"
        );

        let lint_errors = result["lint_errors"].as_array().unwrap();
        let broken_errors: Vec<&Value> = lint_errors
            .iter()
            .filter(|e| e["path"] == "todo/no-dates.md")
            .collect();

        // Exactly one error, naming the rule, on both passes. Pass 1 has it
        // from sync and the persisted set at once — dedup collapses them.
        assert_eq!(
            broken_errors.len(),
            1,
            "pass {pass}: expected exactly one lint_error for todo/no-dates.md; got: {broken_errors:?}"
        );
        assert_eq!(
            broken_errors[0]["message"].as_str(),
            Some(rule_msg),
            "pass {pass}: wrong message"
        );
        assert!(
            !lint_errors
                .iter()
                .any(|e| e["message"].as_str() == Some(generic_msg)),
            "pass {pass}: generic message must not appear when persisted state exists"
        );
    }
}

#[test]
fn test_todo_query_explicit_horizon() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    // --horizon 0: only items with effective_date <= today
    let output = graf_cmd()
        .args([
            "todo",
            "--json",
            "--horizon",
            "0",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let result: Value = serde_json::from_str(&String::from_utf8(output.stdout).unwrap()).unwrap();
    let tasks = result["tasks"].as_array().unwrap();
    // Only Task A (2020-01-01) is in the past.
    assert_eq!(tasks.len(), 1, "only past-due Task A should be returned");
    assert_eq!(tasks[0]["tldr"], "Task A");
}

#[test]
fn test_todo_query_explicit_status_overrides_include_done() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    // --status todo --include-done: --status wins, done items excluded
    let output = graf_cmd()
        .args([
            "todo",
            "--json",
            "--status",
            "todo",
            "--include-done",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let result: Value = serde_json::from_str(&String::from_utf8(output.stdout).unwrap()).unwrap();
    let tasks = result["tasks"].as_array().unwrap();
    // --status todo: should return Task A, Task B, Task C (all status=todo)
    // --include-done is overridden by explicit --status, so Done Task is excluded
    assert_eq!(
        tasks.len(),
        3,
        "should return exactly the 3 todo-status items"
    );
    for task in tasks {
        let status = task["status"].as_str().unwrap();
        assert_eq!(status, "todo", "only todo items should be returned");
    }
}

#[test]
fn test_todo_add_cli() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    let output = graf_cmd()
        .args([
            "todo",
            "add",
            "todo/new-task.md",
            "My New Task",
            "--priority",
            "2",
            "--check-in",
            "2026-05-01",
            "--label",
            "test",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("todo/new-task.md"));

    // Verify the file was created with correct frontmatter
    let content = std::fs::read_to_string(repo.join("todo/new-task.md")).unwrap();
    assert!(content.contains("tldr: \"My New Task\""));
    assert!(content.contains("status: todo"));
    assert!(content.contains("priority: 2"));
    assert!(content.contains("check_in_date: '2026-05-01'"));
    assert!(content.contains("labels: [test]"));
    // sort_order is no longer eagerly assigned; rank falls through to priority.
    assert!(
        !content.contains("sort_order:"),
        "todo_add must not emit sort_order; got:\n{content}"
    );

    // Verify it appears in a subsequent query, and that sort_order is null
    // (priority drives the rank instead).
    let output = graf_cmd()
        .args(["todo", "--json", "--repo", repo.to_str().unwrap()])
        .output()
        .unwrap();

    let result: Value = serde_json::from_str(&String::from_utf8(output.stdout).unwrap()).unwrap();
    let tasks = result["tasks"].as_array().unwrap();
    let new_task = tasks
        .iter()
        .find(|t| t["tldr"] == "My New Task")
        .expect("new task should appear in query");
    assert!(
        new_task["sort_order"].is_null(),
        "expected sort_order null on freshly added task; got {new_task}"
    );
}

#[test]
fn test_todo_add_with_due_date_only() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    let output = graf_cmd()
        .args([
            "todo",
            "add",
            "todo/due-only.md",
            "Due Date Task",
            "--due",
            "2026-06-15",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let content = std::fs::read_to_string(repo.join("todo/due-only.md")).unwrap();
    assert!(content.contains("due_date: '2026-06-15'"));
    assert!(!content.contains("check_in_date:"));
    assert!(!content.contains("tentative_date:"));
}

#[test]
fn test_todo_add_does_not_set_tentative_date() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    let output = graf_cmd()
        .args([
            "todo",
            "add",
            "todo/no-tentative.md",
            "No Tentative",
            "--check-in",
            "2026-05-01",
            "--due",
            "2026-06-01",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let content = std::fs::read_to_string(repo.join("todo/no-tentative.md")).unwrap();
    assert!(content.contains("check_in_date: '2026-05-01'"));
    assert!(content.contains("due_date: '2026-06-01'"));
    assert!(
        !content.contains("tentative_date:"),
        "todo_add should not set tentative_date"
    );
}

#[test]
fn test_config_default_limit_applied() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    // Write a config with a small default_limit
    let config_path = tmp.path().join("config.toml");
    std::fs::write(&config_path, "[todo]\ndefault_limit = 1\n").unwrap();

    // Query without --limit — config default_limit should cap results
    let output = graf_cmd()
        .env("GRAF_CONFIG", &config_path)
        .args(["todo", "--json", "--repo", repo.to_str().unwrap()])
        .output()
        .unwrap();

    assert!(output.status.success());
    let result: Value = serde_json::from_str(&String::from_utf8(output.stdout).unwrap()).unwrap();
    let tasks = result["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 1, "default_limit=1 should cap to 1 result");
}

#[test]
fn test_explicit_limit_overrides_config() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    // Write a config with default_limit = 1
    let config_path = tmp.path().join("config.toml");
    std::fs::write(&config_path, "[todo]\ndefault_limit = 1\n").unwrap();

    // Query with --limit 2 — should override config
    let output = graf_cmd()
        .env("GRAF_CONFIG", &config_path)
        .args([
            "todo",
            "--json",
            "--limit",
            "2",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let result: Value = serde_json::from_str(&String::from_utf8(output.stdout).unwrap()).unwrap();
    let tasks = result["tasks"].as_array().unwrap();
    assert_eq!(
        tasks.len(),
        2,
        "--limit 2 should override config default_limit=1"
    );
}

#[test]
fn test_todo_add_rejects_missing_dates() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    // Neither --check-in nor --due provided
    let output = graf_cmd()
        .args([
            "todo",
            "add",
            "todo/no-date.md",
            "No Date Task",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("check_in_date") || stderr.contains("due_date"),
        "expected date requirement error, got: {stderr}"
    );
}

#[test]
fn test_mutation_rejects_invalid_doc() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    // Create a task that fails lint (no check_in_date or due_date)
    std::fs::write(
        repo.join("todo/invalid.md"),
        "---\ntldr: Invalid Task\nstatus: todo\npriority: 3\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    ).unwrap();

    // Attempt to mark it done — should be rejected by lint-gate
    let output = graf_cmd()
        .args([
            "todo",
            "done",
            "todo/invalid.md",
            "--completion-date",
            "2026-04-20",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("lint errors"),
        "expected lint-gate rejection, got: {stderr}"
    );

    // Attempt to schedule — should also be rejected. Phase 3 emits the
    // lint-gate rejection as a structured `path_not_found` envelope on
    // stdout (see cli/todo.rs `run_schedule`).
    let output = graf_cmd()
        .args([
            "todo",
            "schedule",
            "todo/invalid.md",
            "2030-01-01",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let resp: Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("expected JSON envelope on stdout, got {stdout:?} ({e})");
    });
    assert_eq!(resp["error"], "path_not_found");
    assert!(
        resp["reason"]
            .as_str()
            .is_some_and(|r| r.contains("lint errors")),
        "expected lint-gate reason, got: {resp}"
    );
}

#[test]
fn test_todo_add_rejects_existing_file() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    let output = graf_cmd()
        .args([
            "todo",
            "add",
            "todo/task-a.md",
            "Duplicate",
            "--check-in",
            "2026-05-01",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("already exists"));
}

#[test]
fn test_todo_add_rejects_non_md() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    let output = graf_cmd()
        .args([
            "todo",
            "add",
            "todo/task.txt",
            "Bad Extension",
            "--check-in",
            "2026-05-01",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains(".md"));
}

// ---------------------------------------------------------------------------
// MCP tests
// ---------------------------------------------------------------------------

fn mcp_session(repo_dir: &std::path::Path, requests: &[Value]) -> Vec<Value> {
    let mut child = graf_cmd()
        .args(["mcp", "--repo", repo_dir.to_str().unwrap()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn graf mcp");

    let stdin = child.stdin.as_mut().unwrap();
    for req in requests {
        writeln!(stdin, "{}", serde_json::to_string(req).unwrap()).unwrap();
    }
    drop(child.stdin.take());

    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "graf mcp exited with error");

    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[test]
fn test_mcp_tools_list_includes_new_tools() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    let responses = mcp_session(
        &repo,
        &[
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
        ],
    );

    let tools = responses[1]["result"]["tools"].as_array().unwrap();
    let tool_names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(tool_names.contains(&"graf_reindex"));
    assert!(tool_names.contains(&"graf_todo_query"));
    assert!(tool_names.contains(&"graf_todo_add"));
}

#[test]
fn test_mcp_reindex() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    let responses = mcp_session(
        &repo,
        &[json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "graf_reindex", "arguments": {}}
        })],
    );

    let text = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    let result: Value = serde_json::from_str(text).unwrap();
    assert!(result["files_indexed"].as_u64().unwrap() >= 4);
}

#[test]
fn test_mcp_todo_query() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    let responses = mcp_session(
        &repo,
        &[json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "graf_todo_query", "arguments": {}}
        })],
    );

    let text = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    let result: Value = serde_json::from_str(text).unwrap();
    let tasks = result["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 3); // 3 active tasks (Task A, Task B, Task C)
}

#[test]
fn test_mcp_todo_add() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    let responses = mcp_session(
        &repo,
        &[json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {
                "name": "graf_todo_add",
                "arguments": {
                    "path": "todo/mcp-task.md",
                    "title": "MCP Created Task",
                    "priority": 1,
                    "effort": "small",
                    "check_in_date": "2026-05-01"
                }
            }
        })],
    );

    let text = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    let result: Value = serde_json::from_str(text).unwrap();
    assert_eq!(result["path"], "todo/mcp-task.md");

    // Verify file exists
    assert!(repo.join("todo/mcp-task.md").exists());
}

// ---------------------------------------------------------------------------
// Done tests
// ---------------------------------------------------------------------------

#[test]
fn test_todo_done_cli() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    let output = graf_cmd()
        .args([
            "todo",
            "done",
            "todo/task-a.md",
            "--completion-date",
            "2026-04-20",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let resp: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(resp["path"], "todo/task-a.md");
    assert_eq!(resp["on_date"], "2026-04-20");
    assert_eq!(resp["terminal"], false);

    // Verify file was updated
    let content = std::fs::read_to_string(repo.join("todo/task-a.md")).unwrap();
    assert!(content.contains("status: done"));
    assert!(content.contains("on_date:"));
    assert!(!content.contains("tentative_date:"));
}

#[test]
fn test_todo_done_rejects_already_done() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    let output = graf_cmd()
        .args([
            "todo",
            "done",
            "todo/done-task.md",
            "--completion-date",
            "2026-04-20",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    // Terminal-status rejection surfaces via the `already_terminal`
    // structured envelope under JSON mode (default in non-TTY tests),
    // printing to stdout.
    let stdout = String::from_utf8(output.stdout).unwrap();
    let resp: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(resp["error"], "already_terminal");
    assert_eq!(resp["status"], "done");
    assert!(
        resp["reason"]
            .as_str()
            .unwrap()
            .contains("already has terminal status")
    );
}

#[test]
fn test_todo_done_recurring_advances() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    // Slip recurring task (bare FREQ=WEEKLY, no BY).
    std::fs::write(
        repo.join("todo/recurring.md"),
        "---\ntldr: Recurring Task\nstatus: todo\npriority: 3\ncheck_in_date: '2026-04-06'\nrrule: \"FREQ=WEEKLY\"\ntentative_date: '2026-04-06'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n\nSome instructions.\n",
    ).unwrap();

    // Positive control for the lint-freshness assertion below: a file that
    // must appear under the same path filter.
    std::fs::write(
        repo.join("todo/known-bad.md"),
        "---\ntldr: Known Bad\nstatus: done\npriority: 3\ncheck_in_date: '2026-04-01'\non_date: '2026-04-01'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    ).unwrap();

    let output = graf_cmd()
        .args([
            "todo",
            "done",
            "todo/recurring.md",
            "--completion-date",
            "2026-04-06",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let resp: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(resp["path"], "todo/recurring.md");
    // Phase 2 response shape: next_check_in_date replaces next_date.
    assert_eq!(resp["next_check_in_date"], "2026-04-13");
    assert_eq!(resp["terminal"], false);

    // Verify the file was updated: slip advance clears tentative_date and
    // writes check_in_date = completion + period.
    let content = std::fs::read_to_string(repo.join("todo/recurring.md")).unwrap();
    assert!(content.contains("check_in_date: '2026-04-13'"));
    assert!(!content.contains("tentative_date"));
    assert!(content.contains("status: todo")); // still active
    assert!(content.contains("recurrence:"));
    assert!(content.contains("completion_log:"));
    assert!(content.contains("Some instructions.")); // body preserved

    // The advanced shape is lint-clean: persisted lint state after the
    // mutation carries no entry for the file.
    let output = graf_cmd()
        .args([
            "todo",
            "--json",
            "--include-done",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let result: Value = serde_json::from_str(&String::from_utf8(output.stdout).unwrap()).unwrap();
    let messages_for = |path: &str| -> Vec<&str> {
        result["lint_errors"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["path"] == path)
            .filter_map(|e| e["message"].as_str())
            .collect()
    };
    let control = messages_for("todo/known-bad.md");
    assert!(
        control.contains(&"done/cancelled item has check_in_date set"),
        "positive control missing — the path filter no longer matches: {control:?}"
    );
    let messages = messages_for("todo/recurring.md");
    assert!(
        messages.is_empty(),
        "recurring advance left persisted lint errors: {messages:?}"
    );
}

#[test]
fn test_todo_done_recurring_exhausted() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    // Slip recurring with COUNT=1 — exhausts on first done call.
    // (Slip exhaustion strips the recurrence; anchored exhaustion retains it.)
    std::fs::write(
        repo.join("todo/recurring.md"),
        "---\ntldr: One-shot Recurring\nstatus: todo\npriority: 3\ncheck_in_date: '2026-04-01'\nrrule: \"FREQ=MONTHLY;COUNT=1\"\ntentative_date: '2026-04-01'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n\nBody text.\n",
    ).unwrap();

    let output = graf_cmd()
        .args([
            "todo",
            "done",
            "todo/recurring.md",
            "--completion-date",
            "2026-04-01",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let resp: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(resp["path"], "todo/recurring.md");
    assert_eq!(resp["terminal"], true);
    assert_eq!(resp["on_date"], "2026-04-01");

    // Slip exhaustion path: rrule stripped, status=done.
    let content = std::fs::read_to_string(repo.join("todo/recurring.md")).unwrap();
    assert!(content.contains("status: done"));
    assert!(!content.contains("rrule:"));
    assert!(!content.contains("recurrence:"));
    assert!(content.contains("on_date:"));
    assert!(!content.contains("tentative_date:"));
    assert!(content.contains("completion_log:"));
    assert!(content.contains("Body text.")); // body preserved
}

/// Phase-2 finding: slip-rule COUNT exhaustion on a task that carried a
/// `due_date` must also strip `due_date` (terminal_task_mutations now
/// includes Remove(DueDate)). Without the fix the file was left in a
/// zombie state: status=done + recurrence gone + due_date stale.
#[test]
fn test_todo_done_slip_exhausted_with_due_date_strips_due_date() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    // Slip recurring with COUNT=1, due_date anchor + lead_days so both
    // date fields are live. `sort_order: 99` so the test pins the
    // slip-exhaustion branch's clearance of sort_order via
    // terminal_task_mutations.
    std::fs::write(
        repo.join("todo/slip-due.md"),
        "---\ntldr: Slip Due\nstatus: todo\npriority: 3\ncheck_in_date: '2026-03-25'\ndue_date: '2026-04-01'\nrecurrence:\n  rrule: \"FREQ=MONTHLY;COUNT=1\"\n  lead_days: 7\ntentative_date: '2026-04-01'\nsort_order: 99\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    ).unwrap();

    let output = graf_cmd()
        .args([
            "todo",
            "done",
            "todo/slip-due.md",
            "--completion-date",
            "2026-04-01",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let resp: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(resp["terminal"], true);

    let content = std::fs::read_to_string(repo.join("todo/slip-due.md")).unwrap();
    assert!(content.contains("status: done"));
    assert!(content.contains("on_date:"));
    assert!(!content.contains("rrule:"));
    assert!(!content.contains("recurrence:"));
    assert!(!content.contains("check_in_date:"));
    assert!(
        !content.contains("due_date:"),
        "due_date must be stripped on slip exhaustion, got: {content}"
    );
    assert!(!content.contains("tentative_date:"));
    assert!(
        !content.contains("sort_order:"),
        "slip exhaustion must clear sort_order; got: {content}"
    );
}

#[test]
fn test_todo_done_recurring_no_tentative_date() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    // Slip recurring task, no tentative_date. Phase 2: slip uses
    // completion_date as DTSTART; next anchor = completion_date + 1 month.
    std::fs::write(
        repo.join("todo/recurring.md"),
        "---\ntldr: No Date Recurring\nstatus: todo\npriority: 3\ncheck_in_date: '2026-04-01'\nrrule: \"FREQ=MONTHLY\"\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    ).unwrap();

    let output = graf_cmd()
        .args([
            "todo",
            "done",
            "todo/recurring.md",
            "--completion-date",
            "2026-04-15",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let resp: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(resp["path"], "todo/recurring.md");
    assert_eq!(resp["next_check_in_date"], "2026-05-15");

    let content = std::fs::read_to_string(repo.join("todo/recurring.md")).unwrap();
    assert!(content.contains("check_in_date: '2026-05-15'"));
    // Phase 2 clears tentative_date on every advance.
    assert!(!content.contains("tentative_date"));
    assert!(content.contains("status: todo"));
    assert!(content.contains("completion_log:"));
}

#[test]
fn test_todo_done_recurring_twice() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    std::fs::write(
        repo.join("todo/recurring.md"),
        "---\ntldr: Weekly Task\nstatus: todo\npriority: 3\ncheck_in_date: '2026-04-06'\nrrule: \"FREQ=WEEKLY\"\ntentative_date: '2026-04-06'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    ).unwrap();

    // First completion (slip: anchor becomes completion + 1 week)
    let output = graf_cmd()
        .args([
            "todo",
            "done",
            "todo/recurring.md",
            "--completion-date",
            "2026-04-06",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let resp: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(resp["next_check_in_date"], "2026-04-13");

    // Second completion on a different day
    let output = graf_cmd()
        .args([
            "todo",
            "done",
            "todo/recurring.md",
            "--completion-date",
            "2026-04-13",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let resp: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(resp["next_check_in_date"], "2026-04-20");

    // File should have two log entries under a single completion_log key.
    let content = std::fs::read_to_string(repo.join("todo/recurring.md")).unwrap();
    assert_eq!(content.matches("completion_log:").count(), 1);
    assert_eq!(content.matches("  - { completed:").count(), 2);
    // Phase 2 clears tentative_date on every advance; anchor lives on check_in_date.
    assert!(content.contains("check_in_date: '2026-04-20'"));
    assert!(!content.contains("tentative_date"));
}

#[test]
fn test_todo_done_no_longer_in_active_query() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    // Mark task-a done
    graf_cmd()
        .args([
            "todo",
            "done",
            "todo/task-a.md",
            "--completion-date",
            "2026-04-20",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    // Query active tasks
    let output = graf_cmd()
        .args(["todo", "--json", "--repo", repo.to_str().unwrap()])
        .output()
        .unwrap();

    let result: Value = serde_json::from_str(&String::from_utf8(output.stdout).unwrap()).unwrap();
    let tasks = result["tasks"].as_array().unwrap();
    // Was 3 active, now should be 2 (Task B + Task C)
    assert_eq!(tasks.len(), 2);
    assert!(tasks.iter().all(|t| t["tldr"] != "Task A"));
}

#[test]
fn test_todo_done_clears_check_in_date() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    // Create a task with check_in_date
    std::fs::write(
        repo.join("todo/checkin-task.md"),
        "---\ntldr: Checkin Task\nstatus: todo\npriority: 3\ncheck_in_date: '2026-05-01'\ntentative_date: '2026-04-10'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    ).unwrap();

    let output = graf_cmd()
        .args([
            "todo",
            "done",
            "todo/checkin-task.md",
            "--completion-date",
            "2026-04-20",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let content = std::fs::read_to_string(repo.join("todo/checkin-task.md")).unwrap();
    assert!(content.contains("status: done"));
    assert!(!content.contains("check_in_date:"));
    assert!(!content.contains("tentative_date:"));
    assert!(content.contains("on_date:"));
}

#[test]
fn test_todo_done_with_start_date_sets_end_date() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    // Create a task with start_date (in-progress range task)
    std::fs::write(
        repo.join("todo/range-task.md"),
        "---\ntldr: Range Task\nstatus: in_progress\npriority: 3\ncheck_in_date: '2026-04-01'\nstart_date: '2026-04-01'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    ).unwrap();

    let output = graf_cmd()
        .args([
            "todo",
            "done",
            "todo/range-task.md",
            "--completion-date",
            "2026-04-20",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let resp: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(resp["end_date"], "2026-04-20");
    assert!(resp["on_date"].is_null());

    // Verify: end_date set (not on_date), start_date preserved
    let content = std::fs::read_to_string(repo.join("todo/range-task.md")).unwrap();
    assert!(content.contains("status: done"));
    assert!(content.contains("end_date:"));
    assert!(content.contains("start_date:"));
    assert!(!content.contains("on_date:"));
    assert!(!content.contains("check_in_date:"));
}

#[test]
fn test_todo_cancel_with_start_date_sets_end_date() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    // Create a task with start_date
    std::fs::write(
        repo.join("todo/range-cancel.md"),
        "---\ntldr: Range Cancel\nstatus: in_progress\npriority: 3\ncheck_in_date: '2026-04-01'\nstart_date: '2026-04-01'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    ).unwrap();

    let output = graf_cmd()
        .args([
            "todo",
            "cancel",
            "todo/range-cancel.md",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let resp: Value = serde_json::from_str(&stdout).unwrap();
    assert!(resp["end_date"].is_string());
    assert!(resp["on_date"].is_null());

    // Verify: end_date set (not on_date), start_date preserved
    let content = std::fs::read_to_string(repo.join("todo/range-cancel.md")).unwrap();
    assert!(content.contains("status: cancelled"));
    assert!(content.contains("end_date:"));
    assert!(content.contains("start_date:"));
    assert!(!content.contains("on_date:"));
}

#[test]
fn test_todo_done_rejects_cancelled() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    std::fs::write(
        repo.join("todo/cancelled.md"),
        "---\ntldr: Cancelled Task\nstatus: cancelled\non_date: '2026-04-01'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    ).unwrap();

    let output = graf_cmd()
        .args([
            "todo",
            "done",
            "todo/cancelled.md",
            "--completion-date",
            "2026-04-20",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let resp: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(resp["error"], "already_terminal");
    assert_eq!(resp["status"], "cancelled");
    assert!(
        resp["reason"]
            .as_str()
            .unwrap()
            .contains("already has terminal status")
    );
}

// ---------------------------------------------------------------------------
// Cancel tests
// ---------------------------------------------------------------------------

#[test]
fn test_todo_cancel_cli() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    let output = graf_cmd()
        .args([
            "todo",
            "cancel",
            "todo/task-a.md",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let resp: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(resp["path"], "todo/task-a.md");
    assert!(resp["on_date"].is_string());

    // Verify file was updated
    let content = std::fs::read_to_string(repo.join("todo/task-a.md")).unwrap();
    assert!(content.contains("status: cancelled"));
    assert!(content.contains("on_date:"));
    assert!(!content.contains("tentative_date:"));
    assert!(!content.contains("check_in_date:"));
    // Cancel must clear any prior `sort_order` (terminal items rank by
    // on_date in the query layer; the only writer of sort_order is
    // `todo_reorder`). The setup_repo fixture seeds task-a with
    // `sort_order: 1`, so this assertion exercises the actual fix.
    assert!(
        !content.contains("sort_order:"),
        "cancel must clear sort_order; got:\n{content}"
    );
}

#[test]
fn test_todo_cancel_rejects_already_done() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    let output = graf_cmd()
        .args([
            "todo",
            "cancel",
            "todo/done-task.md",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("already has status"));
}

#[test]
fn test_todo_cancel_rejects_already_cancelled() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    std::fs::write(
        repo.join("todo/cancelled.md"),
        "---\ntldr: Cancelled Task\nstatus: cancelled\non_date: '2026-04-01'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    ).unwrap();

    let output = graf_cmd()
        .args([
            "todo",
            "cancel",
            "todo/cancelled.md",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("already has status"));
}

#[test]
fn test_todo_cancel_removes_rrule() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    std::fs::write(
        repo.join("todo/recurring.md"),
        "---\ntldr: Weekly Task\nstatus: todo\npriority: 3\ncheck_in_date: '2026-04-06'\nrrule: \"FREQ=WEEKLY\"\ntentative_date: '2026-04-06'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    ).unwrap();

    let output = graf_cmd()
        .args([
            "todo",
            "cancel",
            "todo/recurring.md",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(output.status.success());

    let content = std::fs::read_to_string(repo.join("todo/recurring.md")).unwrap();
    assert!(content.contains("status: cancelled"));
    assert!(content.contains("on_date:"));
    assert!(!content.contains("rrule:"));
    assert!(!content.contains("tentative_date:"));
    assert!(!content.contains("check_in_date:"));
}

// ---------------------------------------------------------------------------
// Completion-date interlock tests (done + cancel)
// ---------------------------------------------------------------------------
//
// These cover the read-before-overwrite interlock for the completion-date
// field (on_date when no start_date, end_date when start_date is present).
// See `design-todo-done-preserve-end-date.md` for the resolution table.

/// Write a range task with a historical start_date and end_date that a new
/// `done` or `cancel` call would otherwise clobber.
fn write_historical_range_task(repo: &std::path::Path, rel: &str) {
    std::fs::write(
        repo.join(rel),
        "---\n\
         tldr: Historical Range\n\
         status: in_progress\n\
         priority: 3\n\
         check_in_date: '2025-03-01'\n\
         start_date: '2025-03-04'\n\
         end_date: '2025-03-07'\n\
         created: '2025-01-01T00:00:00Z'\n\
         updated: '2025-01-01T00:00:00Z'\n\
         ---\n",
    )
    .unwrap();
}

/// Write a task with a pre-existing on_date (no start_date).
fn write_task_with_on_date(repo: &std::path::Path, rel: &str) {
    std::fs::write(
        repo.join(rel),
        "---\n\
         tldr: Historical On-Date\n\
         status: in_progress\n\
         priority: 3\n\
         check_in_date: '2025-03-01'\n\
         on_date: '2025-03-07'\n\
         created: '2025-01-01T00:00:00Z'\n\
         updated: '2025-01-01T00:00:00Z'\n\
         ---\n",
    )
    .unwrap();
}

// ------- done: end_date branch -------

#[test]
fn test_todo_done_stored_end_date_requires_ack() {
    // Phase 2: non-recurring conflict returns the structured
    // already_done_or_not_due_yet envelope (via stdout + exit 1 in CLI).
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());
    write_historical_range_task(&repo, "todo/hist.md");

    let output = graf_cmd()
        .args([
            "todo",
            "--json",
            "done",
            "todo/hist.md",
            "--completion-date",
            "2026-04-16",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let resp: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("expected JSON error envelope, got: {stdout:?} / {e}"));
    assert_eq!(resp["error"], "already_done_or_not_due_yet");
    assert_eq!(resp["stored_on_date"], "2025-03-07");
    assert_eq!(resp["completion_date"], "2026-04-16");

    // File unchanged.
    let content = std::fs::read_to_string(repo.join("todo/hist.md")).unwrap();
    assert!(content.contains("status: in_progress"));
    assert!(content.contains("end_date: '2025-03-07'"));
}

#[test]
fn test_todo_done_completion_date_matches_stored_succeeds() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());
    write_historical_range_task(&repo, "todo/hist.md");

    let output = graf_cmd()
        .args([
            "todo",
            "done",
            "todo/hist.md",
            "--completion-date",
            "2025-03-07",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let resp: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(resp["end_date"], "2025-03-07");

    // end_date preserved, but status/scheduling/updated mutations still applied
    let content = std::fs::read_to_string(repo.join("todo/hist.md")).unwrap();
    assert!(content.contains("status: done"));
    assert!(content.contains("end_date: '2025-03-07'"));
    assert!(content.contains("start_date: '2025-03-04'"));
    assert!(!content.contains("check_in_date:"));
    // `updated` advanced from the fixture's 2025-01-01 timestamp.
    assert!(
        !content.contains("updated: '2025-01-01T00:00:00Z'"),
        "expected updated timestamp to advance, got: {content}"
    );
}

#[test]
fn test_todo_done_replacing_matches_overwrites() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());
    write_historical_range_task(&repo, "todo/hist.md");

    let output = graf_cmd()
        .args([
            "todo",
            "done",
            "todo/hist.md",
            "--completion-date",
            "2026-04-16",
            "--replacing",
            "2025-03-07",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let resp: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(resp["end_date"], "2026-04-16");

    let content = std::fs::read_to_string(repo.join("todo/hist.md")).unwrap();
    assert!(content.contains("status: done"));
    assert!(content.contains("end_date: '2026-04-16'"));
    assert!(content.contains("start_date: '2025-03-04'"));
}

#[test]
fn test_todo_done_replacing_mismatch_errors() {
    // Phase 2: supplying a non-matching `replacing` falls to the same
    // `already_done_or_not_due_yet` envelope (replacing is ignored for
    // the match test; only stored vs. completion_date matters).
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());
    write_historical_range_task(&repo, "todo/hist.md");

    let output = graf_cmd()
        .args([
            "todo",
            "--json",
            "done",
            "todo/hist.md",
            "--completion-date",
            "2026-04-16",
            "--replacing",
            "2025-03-08", // wrong
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let resp: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(resp["error"], "already_done_or_not_due_yet");
    assert_eq!(resp["stored_on_date"], "2025-03-07");

    // File unchanged
    let content = std::fs::read_to_string(repo.join("todo/hist.md")).unwrap();
    assert!(content.contains("status: in_progress"));
    assert!(content.contains("end_date: '2025-03-07'"));
}

#[test]
fn test_todo_done_replacing_without_stored_errors() {
    // Phase 2: `replacing` supplied but no stored field → Invalid (non-JSON text).
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    // Task with start_date but no end_date stored
    std::fs::write(
        repo.join("todo/no-end.md"),
        "---\ntldr: No End\nstatus: in_progress\npriority: 3\ncheck_in_date: '2026-04-01'\nstart_date: '2026-04-01'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    ).unwrap();

    let output = graf_cmd()
        .args([
            "todo",
            "--json",
            "done",
            "todo/no-end.md",
            "--completion-date",
            "2026-04-20",
            "--replacing",
            "2026-04-01",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    // `Invalid` still uses the structured envelope with error=invalid.
    let stdout = String::from_utf8(output.stdout).unwrap();
    let resp: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(resp["error"], "invalid");
    assert!(
        resp["reason"].as_str().unwrap().contains("no stored"),
        "reason: {}",
        resp["reason"]
    );
}

#[test]
fn test_todo_done_replacing_with_matching_date_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());
    write_historical_range_task(&repo, "todo/hist.md");

    // completion_date == stored; replacing is pointless — Invalid.
    let output = graf_cmd()
        .args([
            "todo",
            "--json",
            "done",
            "todo/hist.md",
            "--completion-date",
            "2025-03-07",
            "--replacing",
            "2025-03-07",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let resp: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(resp["error"], "invalid");
    assert!(
        resp["reason"]
            .as_str()
            .unwrap()
            .contains("nothing to overwrite"),
        "reason: {}",
        resp["reason"]
    );

    // File unchanged
    let content = std::fs::read_to_string(repo.join("todo/hist.md")).unwrap();
    assert!(content.contains("status: in_progress"));
}

// ------- done: on_date branch -------

#[test]
fn test_todo_done_on_date_branch_requires_ack() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());
    write_task_with_on_date(&repo, "todo/with-on.md");

    let output = graf_cmd()
        .args([
            "todo",
            "--json",
            "done",
            "todo/with-on.md",
            "--completion-date",
            "2026-04-20",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let resp: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(resp["error"], "already_done_or_not_due_yet");
    assert_eq!(resp["stored_on_date"], "2025-03-07");
    assert_eq!(resp["completion_date"], "2026-04-20");
}

// ------- done: recurring exhaustion interlock -------

#[test]
fn test_todo_done_rrule_exhausted_applies_interlock() {
    // Phase 2: slip-rule exhaustion on a task with pre-existing end_date.
    // The completion-date interlock still fires.
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    std::fs::write(
        repo.join("todo/recurring.md"),
        "---\ntldr: Exhausted Range\nstatus: in_progress\npriority: 3\n\
         check_in_date: '2025-03-01'\n\
         rrule: \"FREQ=MONTHLY;COUNT=1\"\n\
         tentative_date: '2025-03-01'\n\
         start_date: '2025-03-04'\n\
         end_date: '2025-03-07'\n\
         created: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    )
    .unwrap();

    // completion_date differs from stored end_date → already_done_or_not_due_yet.
    let output = graf_cmd()
        .args([
            "todo",
            "--json",
            "done",
            "todo/recurring.md",
            "--completion-date",
            "2026-04-20",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let resp: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(resp["error"], "already_done_or_not_due_yet");
    assert_eq!(resp["stored_on_date"], "2025-03-07");
}

/// `replacing` on a slip recurring task is rejected up-front with the
/// named `replacing_on_recurring` error code — it would otherwise be
/// silently ignored as the slip branch advances the task.
///
/// Restored in substance from the Phase-1 `test_todo_done_rrule_active_rejects_replacing_param`,
/// updated to assert the new structured error.
#[test]
fn test_todo_done_replacing_on_slip_recurring_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    std::fs::write(
        repo.join("todo/recurring.md"),
        "---\ntldr: Slip Recurring\nstatus: todo\npriority: 3\ncheck_in_date: '2026-04-06'\nrrule: \"FREQ=WEEKLY\"\ntentative_date: '2026-04-06'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    ).unwrap();

    let output = graf_cmd()
        .args([
            "todo",
            "--json",
            "done",
            "todo/recurring.md",
            "--completion-date",
            "2026-04-10",
            "--replacing",
            "2026-04-06",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let resp: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(resp["error"], "replacing_on_recurring");
    assert!(
        resp["reason"]
            .as_str()
            .unwrap()
            .contains("only valid on non-recurring")
    );

    // File must be unchanged — the rejection happens before any write.
    let content = std::fs::read_to_string(repo.join("todo/recurring.md")).unwrap();
    assert!(content.contains("status: todo"));
    assert!(content.contains("tentative_date: '2026-04-06'"));
    assert!(!content.contains("completion_log:"));
}

/// `replacing` on an anchored recurring task is rejected with the same
/// `replacing_on_recurring` error. Mirror of the slip test; anchored
/// is what the Phase-1 test exercised before deletion.
#[test]
fn test_todo_done_replacing_on_anchored_recurring_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    // Anchored: BYDAY=SA makes it calendar-anchored.
    std::fs::write(
        repo.join("todo/anchored.md"),
        "---\ntldr: Anchored\nstatus: todo\npriority: 3\ncheck_in_date: '2026-04-04'\nrrule: \"FREQ=WEEKLY;BYDAY=SA\"\ntentative_date: '2026-04-04'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    ).unwrap();

    let output = graf_cmd()
        .args([
            "todo",
            "--json",
            "done",
            "todo/anchored.md",
            "--completion-date",
            "2026-04-04",
            "--replacing",
            "2026-04-04",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let resp: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(resp["error"], "replacing_on_recurring");

    let content = std::fs::read_to_string(repo.join("todo/anchored.md")).unwrap();
    assert!(content.contains("status: todo"));
}

/// Anchored skip_past window: if the rrule is finite and exhausted
/// past `completion_date`, there is no "skip_past: true" target, so
/// the call is treated as forced terminal completion (not a
/// stale_anchor error that would offer a nonsensical choice).
#[test]
fn test_anchored_skip_past_rrule_exhausted_past_completion_returns_terminal() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    // Anchored (BYDAY=SA) rrule with UNTIL=2026-02-28 starting check_in
    // 2026-01-03 (Saturday). Occurrences: 2026-01-03, -10, -17, -24,
    // -31, 2026-02-07, -14, -21, -28. Advancing from 2026-01-03 lands
    // on 2026-01-10. With completion_date in May 2026 (far past the
    // UNTIL), naive_next (2026-01-10) is before completion_date, and
    // `next_after(2026-05-01)` is None (UNTIL exhausted). Forced
    // terminal — no stale_anchor error, no skip_past required.
    //
    // UNTIL value uses RFC 5545 UTC form.
    std::fs::write(
        repo.join("todo/anchored-finite.md"),
        "---\ntldr: Anchored Finite\nstatus: todo\npriority: 3\ncheck_in_date: '2026-01-03'\nrrule: \"FREQ=WEEKLY;BYDAY=SA;UNTIL=20260228T000000Z\"\ntentative_date: '2026-01-03'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    ).unwrap();

    let output = graf_cmd()
        .args([
            "todo",
            "--json",
            "done",
            "todo/anchored-finite.md",
            "--completion-date",
            "2026-05-01",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "expected forced-terminal success, stdout: {}, stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let resp: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(resp["terminal"], true);
    assert_eq!(resp["on_date"], "2026-05-01");

    // Anchored terminal completion preserves recurrence/check_in_date,
    // unlike slip exhaustion which strips them.
    let content = std::fs::read_to_string(repo.join("todo/anchored-finite.md")).unwrap();
    assert!(content.contains("status: done"));
    assert!(content.contains("on_date: '2026-05-01'"));
    assert!(content.contains("rrule:"));
    assert!(content.contains("check_in_date:"));
    // Completion log credits the previous anchor (2026-01-03) as the
    // occurrence. Log format is `- { completed: YYYY-MM-DD, occurrence: YYYY-MM-DD }`.
    assert!(content.contains("completion_log:"));
    assert!(
        content.contains("completed: 2026-05-01"),
        "expected completed: 2026-05-01 in log, got: {content}"
    );
    assert!(
        content.contains("occurrence: 2026-01-03"),
        "expected occurrence: 2026-01-03 in log, got: {content}"
    );
}

/// Same as above but with `skip_past: true` supplied — also returns
/// terminal, doesn't error on SkipPastUnnecessary (the window condition
/// is stale, not normal). Proves skip_past is moot when the series is
/// over past completion_date.
#[test]
fn test_anchored_skip_past_true_on_exhausted_rrule_returns_terminal() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    std::fs::write(
        repo.join("todo/anchored-finite.md"),
        "---\ntldr: Anchored Finite\nstatus: todo\npriority: 3\ncheck_in_date: '2026-01-03'\nrrule: \"FREQ=WEEKLY;BYDAY=SA;UNTIL=20260228T000000Z\"\ntentative_date: '2026-01-03'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    ).unwrap();

    let output = graf_cmd()
        .args([
            "todo",
            "--json",
            "done",
            "todo/anchored-finite.md",
            "--completion-date",
            "2026-05-01",
            "--skip-past",
            "true",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stdout: {}, stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let resp: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(resp["terminal"], true);
}

// ------- cancel parallel tests -------

#[test]
fn test_todo_cancel_stored_end_date_requires_ack() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());
    write_historical_range_task(&repo, "todo/hist.md");

    let output = graf_cmd()
        .args([
            "todo",
            "cancel",
            "todo/hist.md",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("already_done_or_not_due_yet") && stderr.contains("2025-03-07"),
        "stderr: {stderr}"
    );
}

#[test]
fn test_todo_cancel_date_matches_stored_succeeds() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());
    write_historical_range_task(&repo, "todo/hist.md");

    let output = graf_cmd()
        .args([
            "todo",
            "cancel",
            "todo/hist.md",
            "--date",
            "2025-03-07",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let content = std::fs::read_to_string(repo.join("todo/hist.md")).unwrap();
    assert!(content.contains("status: cancelled"));
    assert!(content.contains("end_date: '2025-03-07'"));
    assert!(!content.contains("check_in_date:"));
    // `updated` advanced from the fixture's 2025-01-01 timestamp.
    assert!(
        !content.contains("updated: '2025-01-01T00:00:00Z'"),
        "expected updated timestamp to advance, got: {content}"
    );
}

#[test]
fn test_todo_cancel_replacing_matches_overwrites() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());
    write_historical_range_task(&repo, "todo/hist.md");

    let output = graf_cmd()
        .args([
            "todo",
            "cancel",
            "todo/hist.md",
            "--date",
            "2026-04-16",
            "--replacing",
            "2025-03-07",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let content = std::fs::read_to_string(repo.join("todo/hist.md")).unwrap();
    assert!(content.contains("status: cancelled"));
    assert!(content.contains("end_date: '2026-04-16'"));
}

#[test]
fn test_todo_cancel_replacing_mismatch_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());
    write_historical_range_task(&repo, "todo/hist.md");

    let output = graf_cmd()
        .args([
            "todo",
            "cancel",
            "todo/hist.md",
            "--date",
            "2026-04-16",
            "--replacing",
            "2025-03-08",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("already_done_or_not_due_yet") && stderr.contains("does not match"),
        "stderr: {stderr}"
    );
}

#[test]
fn test_todo_cancel_replacing_without_stored_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    std::fs::write(
        repo.join("todo/no-end.md"),
        "---\ntldr: No End\nstatus: in_progress\npriority: 3\ncheck_in_date: '2026-04-01'\nstart_date: '2026-04-01'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    ).unwrap();

    let output = graf_cmd()
        .args([
            "todo",
            "cancel",
            "todo/no-end.md",
            "--replacing",
            "2026-04-01",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("invalid") && stderr.contains("no stored on_date/end_date to replace"),
        "stderr: {stderr}"
    );
}

#[test]
fn test_todo_cancel_replacing_with_matching_date_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());
    write_historical_range_task(&repo, "todo/hist.md");

    let output = graf_cmd()
        .args([
            "todo",
            "cancel",
            "todo/hist.md",
            "--date",
            "2025-03-07",
            "--replacing",
            "2025-03-07",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("invalid") && stderr.contains("nothing to overwrite"),
        "stderr: {stderr}"
    );
}

#[test]
fn test_todo_cancel_on_date_branch_requires_ack() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());
    write_task_with_on_date(&repo, "todo/with-on.md");

    let output = graf_cmd()
        .args([
            "todo",
            "cancel",
            "todo/with-on.md",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("already_done_or_not_due_yet") && stderr.contains("2025-03-07"),
        "stderr: {stderr}"
    );
}

#[test]
fn test_todo_done_on_date_date_matches_stored_succeeds() {
    // Positive exercise of the on_date branch (no start_date).
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());
    write_task_with_on_date(&repo, "todo/with-on.md");

    let output = graf_cmd()
        .args([
            "todo",
            "done",
            "todo/with-on.md",
            "--completion-date",
            "2025-03-07",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let resp: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(resp["on_date"], "2025-03-07");
    assert!(resp["end_date"].is_null());

    let content = std::fs::read_to_string(repo.join("todo/with-on.md")).unwrap();
    assert!(content.contains("status: done"));
    assert!(content.contains("on_date: '2025-03-07'"));
    // Sanity: on_date branch doesn't accidentally write end_date.
    assert!(!content.contains("end_date:"));
    assert!(!content.contains("check_in_date:"));
}

#[test]
fn test_todo_done_replacing_without_stored_on_date_errors() {
    // Mirror of the end_date variant but for the on_date branch.
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    // Task without start_date and without on_date.
    std::fs::write(
        repo.join("todo/no-on.md"),
        "---\ntldr: No On\nstatus: todo\npriority: 3\ncheck_in_date: '2026-04-01'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    ).unwrap();

    let output = graf_cmd()
        .args([
            "todo",
            "--json",
            "done",
            "todo/no-on.md",
            "--completion-date",
            "2026-04-20",
            "--replacing",
            "2026-04-01",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let resp: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(resp["error"], "invalid");
    assert!(
        resp["reason"].as_str().unwrap().contains("no stored"),
        "reason: {}",
        resp["reason"]
    );
}

// ------- MCP layer: new `completion_date` / `replacing` args -------

#[test]
fn test_mcp_todo_done_accepts_completion_date_and_replacing() {
    // Confirms the MCP argument-extraction path wires `completion_date`
    // and `replacing` through to `DoneOptions`.
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());
    write_historical_range_task(&repo, "todo/hist.md");

    let responses = mcp_session(
        &repo,
        &[json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {
                "name": "graf_todo_done",
                "arguments": {
                    "path": "todo/hist.md",
                    "completion_date": "2026-04-16",
                    "replacing": "2025-03-07",
                }
            }
        })],
    );

    assert!(
        responses[0]["result"].get("isError").is_none(),
        "expected success, got: {}",
        responses[0]
    );
    let text = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    let result: Value = serde_json::from_str(text).unwrap();
    assert_eq!(result["end_date"], "2026-04-16");
    assert_eq!(result["terminal"], false);

    let content = std::fs::read_to_string(repo.join("todo/hist.md")).unwrap();
    assert!(content.contains("end_date: '2026-04-16'"));
}

#[test]
fn test_mcp_todo_done_interlock_error_surfaces() {
    // Confirms interlock bails propagate through the MCP transport as
    // `isError: true` with the teaching-error text intact, so an LLM caller
    // can re-invoke correctly.
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());
    write_historical_range_task(&repo, "todo/hist.md");

    let responses = mcp_session(
        &repo,
        &[json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {
                "name": "graf_todo_done",
                "arguments": {
                    "path": "todo/hist.md",
                    "completion_date": "2026-04-20",
                }
            }
        })],
    );

    // Phase 2: structured errors come back as `Ok(envelope_json)` — the
    // MCP framework wraps them in a success `result.content[0].text`
    // payload, NOT `isError: true`.
    assert!(
        responses[0]["result"].get("isError").is_none(),
        "structured error should NOT set isError: {:?}",
        responses[0]
    );
    let text = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    let envelope: Value = serde_json::from_str(text).unwrap();
    assert_eq!(envelope["error"], "already_done_or_not_due_yet");
    assert_eq!(envelope["stored_on_date"], "2025-03-07");
    assert_eq!(envelope["completion_date"], "2026-04-20");

    // File unchanged.
    let content = std::fs::read_to_string(repo.join("todo/hist.md")).unwrap();
    assert!(content.contains("status: in_progress"));
    assert!(content.contains("end_date: '2025-03-07'"));
}

/// A nonexistent `path` returns the structured
/// `path_not_found` envelope rather than propagating as a stringified
/// anyhow error through the brenn-mcp framework.
#[test]
fn test_mcp_todo_done_path_not_found_structured_error() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    let responses = mcp_session(
        &repo,
        &[json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {
                "name": "graf_todo_done",
                "arguments": {
                    "path": "todo/nonexistent.md",
                    "completion_date": "2026-04-20",
                }
            }
        })],
    );

    let text = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    let result: Value = serde_json::from_str(text).unwrap();
    assert_eq!(result["error"], "path_not_found");
    assert_eq!(result["path"], "todo/nonexistent.md");
    assert!(result["reason"].as_str().is_some_and(|r| !r.is_empty()));
}

/// A malformed `completion_date` returns the structured
/// `invalid_date` envelope. Also tests the novel `Ok(None)` branch (absent
/// completion_date) which yields `invalid_date` rather than propagating as
/// `?.context(...)`.
#[test]
fn test_mcp_todo_done_invalid_date_structured_error() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    // Case 1: malformed date string.
    let responses = mcp_session(
        &repo,
        &[json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {
                "name": "graf_todo_done",
                "arguments": { "path": "todo/task-a.md", "completion_date": "not-a-date" }
            }
        })],
    );
    let text = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    let result: Value = serde_json::from_str(text).unwrap();
    assert_eq!(result["error"], "invalid_date");
    assert!(result["reason"].as_str().is_some_and(|r| !r.is_empty()));

    // Case 2: absent completion_date (Ok(None) branch → InvalidDate).
    let tmp2 = tempfile::tempdir().unwrap();
    let repo2 = setup_repo(tmp2.path());
    let responses2 = mcp_session(
        &repo2,
        &[json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {
                "name": "graf_todo_done",
                "arguments": { "path": "todo/task-a.md" }
            }
        })],
    );
    let text2 = responses2[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    let result2: Value = serde_json::from_str(text2).unwrap();
    assert_eq!(result2["error"], "invalid_date");
    assert!(result2["reason"].as_str().is_some_and(|r| !r.is_empty()));
}

// ---------------------------------------------------------------------------
// Schedule tests
// ---------------------------------------------------------------------------

#[test]
fn test_todo_schedule_cli() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    let output = graf_cmd()
        .args([
            "todo",
            "schedule",
            "todo/task-a.md",
            "2030-07-01",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let resp: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(resp["path"], "todo/task-a.md");
    assert_eq!(resp["tentative_date"], "2030-07-01");
    // sort_order is no longer in the schedule response: the field was
    // removed from ScheduleResult and the JSON key dropped.
    assert!(
        resp.get("sort_order").is_none(),
        "schedule JSON must not carry sort_order; got: {resp}"
    );

    let content = std::fs::read_to_string(repo.join("todo/task-a.md")).unwrap();
    assert!(content.contains("tentative_date: '2030-07-01'"));
    // task-a.md started with `sort_order: 1` (see setup_repo); schedule
    // must have stripped it so priority drives the new-bucket ranking.
    assert!(
        !content.contains("sort_order:"),
        "schedule must clear sort_order; got:\n{content}"
    );
}

/// `graf todo schedule` on a terminal
/// task must reject with the structured `terminal_task` envelope on
/// stdout when JSON mode is in effect (the always-active path under
/// `Command::output()`, which pipes stdout — `is_terminal()` returns
/// false and `run_schedule` prints the envelope rather than bubbling
/// an anyhow `Err`). The equivalent TTY-mode path bubbles the error
/// via `anyhow::anyhow!("{}", other.reason())` producing
/// `Error: cannot schedule ...: status is done` on stderr; that path
/// cannot be exercised from `Command::output()` (no PTY), but the
/// content is the same `reason()` string the envelope's `reason`
/// field carries here.
#[test]
fn test_todo_schedule_cli_rejects_terminal_task() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    let output = graf_cmd()
        .args([
            "todo",
            "schedule",
            "todo/done-task.md",
            "2030-07-01",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let resp: Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("expected JSON envelope on stdout, got {stdout:?} ({e})");
    });
    assert_eq!(resp["error"], "terminal_task");
    assert_eq!(resp["status"], "done");
    assert_eq!(resp["path"], "todo/done-task.md");
    assert!(
        resp["reason"]
            .as_str()
            .is_some_and(|r| r.contains("cannot schedule") && r.contains("status is done")),
        "expected reason to match the human-readable shape, got: {resp}"
    );
}

/// Two tasks added on the same date with different priorities must
/// rank P2 ahead of P3, since `todo_add` no longer eagerly assigns
/// `sort_order`. Rank falls through `COALESCE(sort_order, priority,
/// UNRANKED)` to `priority`.
#[test]
fn test_todo_add_priority_drives_rank() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    for (slug, prio, title) in [
        ("todo/p3-task.md", "3", "P3 Task"),
        ("todo/p2-task.md", "2", "P2 Task"),
    ] {
        let output = graf_cmd()
            .args([
                "todo",
                "add",
                slug,
                title,
                "--priority",
                prio,
                "--check-in",
                "2030-08-01",
                "--repo",
                repo.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(output.status.success(), "todo add {slug} failed");
    }

    let output = graf_cmd()
        .args(["todo", "--json", "--repo", repo.to_str().unwrap()])
        .output()
        .unwrap();
    let result: Value = serde_json::from_str(&String::from_utf8(output.stdout).unwrap()).unwrap();
    let tasks = result["tasks"].as_array().unwrap();

    let p2_pos = tasks
        .iter()
        .position(|t| t["tldr"] == "P2 Task")
        .expect("P2 Task must appear in query");
    let p3_pos = tasks
        .iter()
        .position(|t| t["tldr"] == "P3 Task")
        .expect("P3 Task must appear in query");
    assert!(
        p2_pos < p3_pos,
        "expected P2 before P3 (priority drives rank); got tasks={tasks:?}"
    );
}

/// Schedule clears any prior manual `sort_order`. The fixture's
/// task-a.md starts with `sort_order: 1`; after scheduling to a new
/// date, the field must be absent so priority drives the new-bucket
/// rank.
#[test]
fn test_todo_schedule_clears_sort_order() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    // Sanity-check fixture state.
    let before = std::fs::read_to_string(repo.join("todo/task-a.md")).unwrap();
    assert!(
        before.contains("sort_order: 1"),
        "fixture invariant: task-a starts with sort_order: 1"
    );

    let output = graf_cmd()
        .args([
            "todo",
            "schedule",
            "todo/task-a.md",
            "2030-09-01",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(output.status.success());

    let after = std::fs::read_to_string(repo.join("todo/task-a.md")).unwrap();
    assert!(
        !after.contains("sort_order:"),
        "schedule must remove sort_order; got:\n{after}"
    );
}

// ---------------------------------------------------------------------------
// Reorder tests
// ---------------------------------------------------------------------------

#[test]
fn test_todo_reorder_between() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    // Create two items on the same date with known sort_orders
    std::fs::write(
        repo.join("todo/top.md"),
        "---\ntldr: Top\nstatus: todo\npriority: 3\ncheck_in_date: '2020-01-01'\nsort_order: 1\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    ).unwrap();
    std::fs::write(
        repo.join("todo/bottom.md"),
        "---\ntldr: Bottom\nstatus: todo\npriority: 3\ncheck_in_date: '2020-01-01'\nsort_order: 5\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    ).unwrap();
    std::fs::write(
        repo.join("todo/target.md"),
        "---\ntldr: Target\nstatus: todo\npriority: 3\ncheck_in_date: '2020-01-01'\nsort_order: 10\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    ).unwrap();

    let output = graf_cmd()
        .args([
            "todo",
            "reorder",
            "todo/target.md",
            "--after",
            "todo/top.md",
            "--before",
            "todo/bottom.md",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let resp: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(resp["path"], "todo/target.md");
    assert_eq!(resp["sort_order"], 3.0); // midpoint of 1 and 5

    let content = std::fs::read_to_string(repo.join("todo/target.md")).unwrap();
    assert!(content.contains("sort_order: 3"));
}

#[test]
fn test_todo_reorder_after_only() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    let output = graf_cmd()
        .args([
            "todo",
            "reorder",
            "todo/task-c.md",
            "--after",
            "todo/task-a.md",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let resp: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(resp["sort_order"], 2.0); // task-a has sort_order 1, so +1 = 2
}

#[test]
fn test_todo_reorder_requires_neighbor() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    let output = graf_cmd()
        .args([
            "todo",
            "reorder",
            "todo/task-a.md",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("--after") || stderr.contains("--before"));
}

#[test]
fn test_todo_reorder_before_only() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    // task-a has sort_order 1, placing before it gives sort_order 0
    let output = graf_cmd()
        .args([
            "todo",
            "reorder",
            "todo/task-c.md",
            "--before",
            "todo/task-a.md",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let resp: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(resp["sort_order"], 0.0); // task-a has sort_order 1, so -1 = 0
}

#[test]
fn test_todo_reorder_rejects_done_target() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    let output = graf_cmd()
        .args([
            "todo",
            "reorder",
            "todo/done-task.md",
            "--after",
            "todo/task-a.md",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("cannot reorder"));
}

#[test]
fn test_todo_reorder_nonexistent_neighbor() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    let output = graf_cmd()
        .args([
            "todo",
            "reorder",
            "todo/task-a.md",
            "--after",
            "todo/nonexistent.md",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("not found"));
}

#[test]
fn test_todo_reorder_inherits_date_from_neighbor() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    // task-a has check_in_date 2020-01-01; reorder task-c after it
    // should give task-c that tentative_date
    let output = graf_cmd()
        .args([
            "todo",
            "reorder",
            "todo/task-c.md",
            "--after",
            "todo/task-a.md",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let content = std::fs::read_to_string(repo.join("todo/task-c.md")).unwrap();
    // task-a's effective_date is check_in_date 2020-01-01 (no tentative_date).
    // Reorder infers that date as the neighbor's effective_date.
    assert!(
        content.contains("tentative_date: '2020-01-01'"),
        "should inherit date from neighbor's effective_date, got: {content}"
    );
}

// ---------------------------------------------------------------------------
// MCP mutation tests
// ---------------------------------------------------------------------------

#[test]
fn test_mcp_todo_done() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    let responses = mcp_session(
        &repo,
        &[json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {
                "name": "graf_todo_done",
                "arguments": {
                    "path": "todo/task-a.md",
                    "completion_date": "2026-04-20",
                }
            }
        })],
    );

    let text = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    let result: Value = serde_json::from_str(text).unwrap();
    assert_eq!(result["path"], "todo/task-a.md");
    assert_eq!(result["on_date"], "2026-04-20");
    assert_eq!(result["terminal"], false);
    // Phase 2 response shape: recurring fields absent on non-recurring tasks.
    assert!(result["next_check_in_date"].is_null());
}

#[test]
fn test_mcp_todo_done_recurring() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    // Slip recurring task.
    std::fs::write(
        repo.join("todo/recurring.md"),
        "---\ntldr: Weekly Task\nstatus: todo\npriority: 3\ncheck_in_date: '2026-04-06'\nrrule: \"FREQ=WEEKLY\"\ntentative_date: '2026-04-06'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    ).unwrap();

    let responses = mcp_session(
        &repo,
        &[json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {
                "name": "graf_todo_done",
                "arguments": {
                    "path": "todo/recurring.md",
                    "completion_date": "2026-04-06",
                }
            }
        })],
    );

    let text = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    let result: Value = serde_json::from_str(text).unwrap();
    assert_eq!(result["path"], "todo/recurring.md");
    assert_eq!(result["terminal"], false);
    assert_eq!(result["next_check_in_date"], "2026-04-13");
    assert!(result["on_date"].is_null());
    assert!(result["end_date"].is_null());
}

/// Phase 2: full MCP-layer response shape for success, already_done,
/// and stale_anchor — pins the JSON byte-for-byte.
#[test]
fn test_mcp_todo_done_response_shapes() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    // Stale-anchor case: monthly-on-1st, stored anchor 2025-11-01,
    // done on 2026-04-20.
    std::fs::write(
        repo.join("todo/stale.md"),
        "---\ntldr: Stale\nstatus: todo\npriority: 3\ncheck_in_date: '2025-11-01'\nrecurrence:\n  rrule: \"FREQ=MONTHLY;BYMONTHDAY=1\"\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    ).unwrap();

    // Slip idempotent no-op case: pre-seeded completion_log entry.
    std::fs::write(
        repo.join("todo/slip-dup.md"),
        "---\ntldr: Slip Dup\nstatus: todo\npriority: 3\ncheck_in_date: '2026-04-18'\nrecurrence:\n  rrule: \"FREQ=WEEKLY\"\ncompletion_log:\n  - {completed: 2026-04-18}\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    ).unwrap();

    let responses = mcp_session(
        &repo,
        &[
            // 1. Success: non-recurring advance.
            json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": {
                    "name": "graf_todo_done",
                    "arguments": {
                        "path": "todo/task-a.md",
                        "completion_date": "2026-04-20",
                    }
                }
            }),
            // 2. Stale-anchor error.
            json!({
                "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                "params": {
                    "name": "graf_todo_done",
                    "arguments": {
                        "path": "todo/stale.md",
                        "completion_date": "2026-04-20",
                    }
                }
            }),
            // 3. Slip already_done.
            json!({
                "jsonrpc": "2.0", "id": 3, "method": "tools/call",
                "params": {
                    "name": "graf_todo_done",
                    "arguments": {
                        "path": "todo/slip-dup.md",
                        "completion_date": "2026-04-18",
                    }
                }
            }),
        ],
    );

    let text_of = |r: &Value| -> Value {
        let t = r["result"]["content"][0]["text"].as_str().unwrap();
        serde_json::from_str(t).unwrap()
    };

    // Success non-recurring
    let r1 = text_of(&responses[0]);
    assert_eq!(r1["path"], "todo/task-a.md");
    assert_eq!(r1["on_date"], "2026-04-20");
    assert_eq!(r1["terminal"], false);

    // Stale-anchor envelope.
    let r2 = text_of(&responses[1]);
    assert_eq!(r2["error"], "stale_anchor");
    assert!(r2["reason"].is_string());
    assert_eq!(r2["stored_anchor"], "2025-11-01");
    assert_eq!(r2["completion_date"], "2026-04-20");
    assert_eq!(r2["next_anchor_if_skip_past_false"], "2025-12-01");
    assert_eq!(r2["next_anchor_if_skip_past_true"], "2026-05-01");

    // Slip already_done envelope.
    let r3 = text_of(&responses[2]);
    assert_eq!(r3["already_done"], true);
    assert_eq!(r3["existing_entry"]["completed"], "2026-04-18");
    assert_eq!(r3["comment_discarded"], false);
    assert!(r3["message"].as_str().unwrap().contains("2026-04-18"));
}

#[test]
fn test_mcp_todo_cancel() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    let responses = mcp_session(
        &repo,
        &[json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {
                "name": "graf_todo_cancel",
                "arguments": { "path": "todo/task-a.md", "today": "2026-04-18" }
            }
        })],
    );

    let text = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    let result: Value = serde_json::from_str(text).unwrap();
    assert_eq!(result["path"], "todo/task-a.md");
    assert!(result["on_date"].as_str().is_some());

    // Verify file was updated
    let content = std::fs::read_to_string(repo.join("todo/task-a.md")).unwrap();
    assert!(content.contains("status: cancelled"));
}

#[test]
fn test_mcp_todo_schedule() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    let responses = mcp_session(
        &repo,
        &[json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {
                "name": "graf_todo_schedule",
                "arguments": { "path": "todo/task-a.md", "date": "2030-11-01" }
            }
        })],
    );

    let text = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    let result: Value = serde_json::from_str(text).unwrap();
    assert_eq!(result["tentative_date"], "2030-11-01");
}

/// Scheduling a terminal task returns the structured
/// `terminal_task` envelope, not a free-form error string.
#[test]
fn test_mcp_todo_schedule_terminal_task_structured_error() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    let responses = mcp_session(
        &repo,
        &[json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {
                "name": "graf_todo_schedule",
                "arguments": { "path": "todo/done-task.md", "date": "2030-11-01" }
            }
        })],
    );

    let text = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    let result: Value = serde_json::from_str(text).unwrap();
    assert_eq!(result["error"], "terminal_task");
    assert_eq!(result["status"], "done");
    assert_eq!(result["path"], "todo/done-task.md");
    assert!(result["reason"].as_str().is_some());
}

/// A malformed `date` argument returns the
/// structured `invalid_date` envelope rather than propagating through
/// the brenn-mcp framework as a stringified anyhow error.
#[test]
fn test_mcp_todo_schedule_invalid_date_structured_error() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    let responses = mcp_session(
        &repo,
        &[json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {
                "name": "graf_todo_schedule",
                "arguments": { "path": "todo/task-a.md", "date": "not-a-date" }
            }
        })],
    );

    let text = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    let result: Value = serde_json::from_str(text).unwrap();
    assert_eq!(result["error"], "invalid_date");
    assert!(result["reason"].as_str().is_some_and(|r| !r.is_empty()));
}

#[test]
fn test_mcp_todo_reorder() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    let responses = mcp_session(
        &repo,
        &[json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {
                "name": "graf_todo_reorder",
                "arguments": {
                    "path": "todo/task-c.md",
                    "after": "todo/task-a.md"
                }
            }
        })],
    );

    let text = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    let result: Value = serde_json::from_str(text).unwrap();
    assert!(result["sort_order"].as_f64().is_some());
}

#[test]
fn test_todo_reorder_after_anchor_without_sort_order() {
    // Anchor has priority but no sort_order.  Rank falls back to priority
    // (1.0), so the reordered task should get sort_order 2.0.
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    // Create two tasks on the same date.  Anchor has priority=1, no sort_order.
    // Target has priority=3, no sort_order.
    std::fs::write(
        repo.join("todo/anchor.md"),
        "---\ntldr: Anchor\nstatus: todo\npriority: 1\ncheck_in_date: '2030-01-01'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("todo/target.md"),
        "---\ntldr: Target\nstatus: todo\npriority: 3\ncheck_in_date: '2030-01-01'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    )
    .unwrap();

    // Reorder target to be after anchor
    let output = graf_cmd()
        .args([
            "todo",
            "reorder",
            "todo/target.md",
            "--after",
            "todo/anchor.md",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let resp: Value = serde_json::from_str(&stdout).unwrap();

    // Anchor's effective rank is 1.0 (from priority).  sort_order should be > 1.0.
    let so = resp["sort_order"].as_f64().unwrap();
    assert!(
        so > 1.0,
        "sort_order should be > anchor's rank (1.0), got {so}"
    );

    // Verify query order: anchor should appear before target
    let output = graf_cmd()
        .args([
            "todo",
            "--json",
            "--repo",
            repo.to_str().unwrap(),
            "--limit",
            "50",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let result: Value = serde_json::from_str(&stdout).unwrap();
    let tasks = result["tasks"].as_array().unwrap();

    // Find positions — only look at tasks on 2030-01-01
    let anchor_pos = tasks.iter().position(|t| t["path"] == "todo/anchor.md");
    let target_pos = tasks.iter().position(|t| t["path"] == "todo/target.md");
    assert!(
        anchor_pos < target_pos,
        "anchor (pos {:?}) should sort before target (pos {:?})",
        anchor_pos,
        target_pos
    );
}

#[test]
fn test_todo_reorder_before_anchor_without_sort_order() {
    // Place target before an anchor that has priority=2 but no sort_order.
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    std::fs::write(
        repo.join("todo/anchor.md"),
        "---\ntldr: Anchor\nstatus: todo\npriority: 2\ncheck_in_date: '2030-01-01'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("todo/target.md"),
        "---\ntldr: Target\nstatus: todo\npriority: 3\ncheck_in_date: '2030-01-01'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    )
    .unwrap();

    let output = graf_cmd()
        .args([
            "todo",
            "reorder",
            "todo/target.md",
            "--before",
            "todo/anchor.md",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8(output.stderr.clone()).unwrap();
    assert!(output.status.success(), "reorder failed: {stderr}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    let resp: Value = serde_json::from_str(&stdout).unwrap();

    // Anchor's effective rank is 2.0 (from priority).  sort_order should be < 2.0.
    let so = resp["sort_order"].as_f64().unwrap();
    assert!(
        so < 2.0,
        "sort_order should be < anchor's rank (2.0), got {so}"
    );

    // Verify query order: target should appear before anchor
    let output = graf_cmd()
        .args([
            "todo",
            "--json",
            "--repo",
            repo.to_str().unwrap(),
            "--limit",
            "50",
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8(output.stderr.clone()).unwrap();
    assert!(output.status.success(), "query failed: {stderr}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    let result: Value = serde_json::from_str(&stdout).unwrap();
    let tasks = result["tasks"].as_array().unwrap();

    let anchor_pos = tasks.iter().position(|t| t["path"] == "todo/anchor.md");
    let target_pos = tasks.iter().position(|t| t["path"] == "todo/target.md");
    assert!(
        target_pos < anchor_pos,
        "target (pos {:?}) should sort before anchor (pos {:?})",
        target_pos,
        anchor_pos
    );
}

#[test]
fn test_todo_reorder_between_unranked_before_anchor() {
    // The before anchor has no sort_order or priority, so its rank is
    // UNRANKED (6.0).  The after anchor has sort_order 1.  Midpoint
    // should be (1.0 + 6.0) / 2.0 = 3.5.
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    // after anchor: sort_order 1
    std::fs::write(
        repo.join("todo/after.md"),
        "---\ntldr: After\nstatus: todo\ncheck_in_date: '2030-01-01'\nsort_order: 1\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    )
    .unwrap();
    // before anchor: no sort_order, no priority → rank 6.0
    std::fs::write(
        repo.join("todo/before.md"),
        "---\ntldr: Before\nstatus: todo\ncheck_in_date: '2030-01-01'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("todo/target.md"),
        "---\ntldr: Target\nstatus: todo\ncheck_in_date: '2030-01-01'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    )
    .unwrap();

    let output = graf_cmd()
        .args([
            "todo",
            "reorder",
            "todo/target.md",
            "--after",
            "todo/after.md",
            "--before",
            "todo/before.md",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let resp: Value = serde_json::from_str(&stdout).unwrap();

    let so = resp["sort_order"].as_f64().unwrap();
    assert_eq!(so, 3.5, "midpoint of 1.0 and 6.0");
}

#[test]
fn test_todo_reorder_rejects_inverted_anchors() {
    // When before's rank <= after's rank, the request is unsatisfiable.
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    std::fs::write(
        repo.join("todo/high.md"),
        "---\ntldr: High\nstatus: todo\ncheck_in_date: '2030-01-01'\nsort_order: 10\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("todo/low.md"),
        "---\ntldr: Low\nstatus: todo\ncheck_in_date: '2030-01-01'\nsort_order: 2\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("todo/target.md"),
        "---\ntldr: Target\nstatus: todo\ncheck_in_date: '2030-01-01'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    )
    .unwrap();

    let output = graf_cmd()
        .args([
            "todo",
            "reorder",
            "todo/target.md",
            "--after",
            "todo/high.md",
            "--before",
            "todo/low.md",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("must rank below"),
        "expected validation error, got: {stderr}"
    );
}

#[test]
fn test_todo_reorder_between_anchors_without_sort_order() {
    // Place target between two anchors that have priority but no sort_order.
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    std::fs::write(
        repo.join("todo/above.md"),
        "---\ntldr: Above\nstatus: todo\npriority: 1\ncheck_in_date: '2030-01-01'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("todo/below.md"),
        "---\ntldr: Below\nstatus: todo\npriority: 3\ncheck_in_date: '2030-01-01'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("todo/target.md"),
        "---\ntldr: Target\nstatus: todo\npriority: 5\ncheck_in_date: '2030-01-01'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    )
    .unwrap();

    let output = graf_cmd()
        .args([
            "todo",
            "reorder",
            "todo/target.md",
            "--after",
            "todo/above.md",
            "--before",
            "todo/below.md",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let resp: Value = serde_json::from_str(&stdout).unwrap();

    // Midpoint of rank 1.0 and rank 3.0 should be 2.0
    let so = resp["sort_order"].as_f64().unwrap();
    assert_eq!(so, 2.0, "sort_order should be midpoint of 1.0 and 3.0");

    // Verify query order: above, target, below
    let output = graf_cmd()
        .args([
            "todo",
            "--json",
            "--repo",
            repo.to_str().unwrap(),
            "--limit",
            "50",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let result: Value = serde_json::from_str(&stdout).unwrap();
    let tasks = result["tasks"].as_array().unwrap();

    let above_pos = tasks.iter().position(|t| t["path"] == "todo/above.md");
    let target_pos = tasks.iter().position(|t| t["path"] == "todo/target.md");
    let below_pos = tasks.iter().position(|t| t["path"] == "todo/below.md");
    assert!(
        above_pos < target_pos && target_pos < below_pos,
        "expected above < target < below, got {:?} < {:?} < {:?}",
        above_pos,
        target_pos,
        below_pos
    );
}

#[test]
fn test_mcp_tools_list_includes_mutation_tools() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    let responses = mcp_session(
        &repo,
        &[
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
        ],
    );

    let tools = responses[1]["result"]["tools"].as_array().unwrap();
    let tool_names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(tool_names.contains(&"graf_todo_done"));
    assert!(tool_names.contains(&"graf_todo_schedule"));
    assert!(tool_names.contains(&"graf_todo_reorder"));
}

// ---------------------------------------------------------------------------
// Tz-aware date math (graf-user-tz.md)
// ---------------------------------------------------------------------------

/// Must-pass test from the design. At 2026-04-17T02:00:00Z, a user in
/// America/New_York is still on 2026-04-16. Before the clock helper
/// landed, `todo done` on a FREQ=DAILY task wrote `### 2026-04-17` to the
/// completion log and advanced `tentative_date` to 2026-04-18. Correct
/// behavior: `### 2026-04-16` and `tentative_date: '2026-04-17'`.
///
/// Primary assertions are the completion-log header and the
/// tentative_date mutation. Negative assertions explicitly exclude the
/// buggy outputs so the test fails loudly on the old behavior rather
/// than on unrelated format changes. `next_date` is incidental — it
/// derives from `tentative_date` not from `today`, so it would pass
/// even with the bug.
#[test]
fn test_todo_done_daily_with_completion_date_is_server_clock_independent() {
    // Phase 2: completion_date is supplied explicitly — the server wall
    // clock no longer feeds into completion logic. Passing
    // completion_date: 2026-04-16 always produces the same result no
    // matter what the env says the zone / "now" is.
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    std::fs::write(
        repo.join("todo/daily.md"),
        "---\ntldr: Daily\nstatus: todo\npriority: 3\n\
         check_in_date: '2026-04-16'\nrrule: \"FREQ=DAILY\"\n\
         tentative_date: '2026-04-16'\ncreated: '2025-01-01T00:00:00Z'\n\
         updated: '2025-01-01T00:00:00Z'\n---\n",
    )
    .unwrap();

    let output = graf_cmd()
        .env("GRAF_USER_TZ", "UTC")
        .env("GRAF_NOW_OVERRIDE", "2026-04-17T02:00:00Z")
        .args([
            "todo",
            "done",
            "todo/daily.md",
            "--completion-date",
            "2026-04-16",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "graf done failed: stderr={}",
        String::from_utf8_lossy(&output.stderr),
    );

    let content = std::fs::read_to_string(repo.join("todo/daily.md")).unwrap();
    assert!(content.contains("completed: 2026-04-16"));
    // Slip advance: new check_in_date = completion_date + 1 day = 2026-04-17.
    assert!(content.contains("check_in_date: '2026-04-17'"));
    // Phase 2 clears tentative_date on every advance.
    assert!(!content.contains("tentative_date"));

    let resp: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(resp["next_check_in_date"], "2026-04-17");
}

/// Phase 2: `completion_date` is the sole source of truth for what
/// gets written. Even with a wrong env zone, the stored date equals
/// the supplied `completion_date` exactly.
#[test]
fn test_todo_done_non_recurring_with_completion_date_ignores_server_clock() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    std::fs::write(
        repo.join("todo/once.md"),
        "---\ntldr: One-shot\nstatus: todo\npriority: 3\ncheck_in_date: '2026-04-01'\n\
         created: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    )
    .unwrap();

    let output = graf_cmd()
        .env("GRAF_USER_TZ", "UTC")
        .env("GRAF_NOW_OVERRIDE", "2026-04-17T02:00:00Z")
        .args([
            "todo",
            "done",
            "todo/once.md",
            "--completion-date",
            "2026-04-16",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "graf done failed: stderr={}",
        String::from_utf8_lossy(&output.stderr),
    );

    let content = std::fs::read_to_string(repo.join("todo/once.md")).unwrap();
    assert!(content.contains("on_date: '2026-04-16'"));
    assert!(!content.contains("on_date: '2026-04-17'"));

    let resp: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(resp["on_date"], "2026-04-16");
}

/// `todo cancel`: `--today` supplies the default `on_date` / `end_date`
/// when `--date` is absent. Parity with `done` — same override
/// semantics live on the same code path (`completion_date`).
#[test]
fn test_todo_cancel_with_today_override() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    std::fs::write(
        repo.join("todo/abandon.md"),
        "---\ntldr: Abandon\nstatus: todo\npriority: 3\ncheck_in_date: '2026-04-01'\n\
         created: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    )
    .unwrap();

    let output = graf_cmd()
        .env("GRAF_USER_TZ", "UTC")
        .env("GRAF_NOW_OVERRIDE", "2026-04-17T02:00:00Z")
        .args([
            "todo",
            "cancel",
            "todo/abandon.md",
            "--today",
            "2026-04-16",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "graf cancel failed: stderr={}",
        String::from_utf8_lossy(&output.stderr),
    );

    let content = std::fs::read_to_string(repo.join("todo/abandon.md")).unwrap();
    assert!(
        content.contains("status: cancelled"),
        "cancel didn't apply status:\n{content}",
    );
    assert!(
        content.contains("on_date: '2026-04-16'"),
        "--today override not honored for on_date on cancel:\n{content}",
    );
    assert!(
        !content.contains("on_date: '2026-04-17'"),
        "cancel on_date has buggy UTC date:\n{content}",
    );
}

/// A violating file dirty in the working tree is linted by the current sync
/// pass AND carried in the persisted set. The two must collapse to a single
/// reported error rather than double-reporting.
#[test]
fn test_lint_error_deduped_when_file_dirty() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    write_and_commit(
        &repo,
        "todo/bad.md",
        "---\ntldr: Bad\nstatus: done\non_date: '2026-04-10'\ncheck_in_date: '2026-05-01'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
        "add violating file",
    );

    // Prime the cache so the violation is persisted.
    let primed = graf_cmd()
        .args(["todo", "--json", "--repo", repo.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(primed.status.success());

    // Now dirty the file in the working tree, keeping the same violation.
    std::fs::write(
        repo.join("todo/bad.md"),
        "---\ntldr: Bad Edited\nstatus: done\non_date: '2026-04-10'\ncheck_in_date: '2026-05-01'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-02T00:00:00Z'\n---\n",
    )
    .unwrap();

    let output = graf_cmd()
        .args(["todo", "--json", "--repo", repo.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(output.status.success());
    let result: Value = serde_json::from_str(&String::from_utf8(output.stdout).unwrap()).unwrap();

    let lint_errors = result["lint_errors"].as_array().unwrap();
    let occurrences = lint_errors
        .iter()
        .filter(|e| {
            e["path"] == "todo/bad.md"
                && e["message"].as_str() == Some("done/cancelled item has check_in_date set")
        })
        .count();
    assert_eq!(
        occurrences, 1,
        "persisted and current-pass errors must collapse; got: {lint_errors:?}"
    );
}

/// A horizon-bounded query excludes the violating row from `tasks`, but the
/// persisted set is scanned independently of the filter, so the violation is
/// still reported.
#[test]
fn test_horizon_does_not_silence_lint_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    write_and_commit(
        &repo,
        "todo/no-dates.md",
        "---\ntldr: No Dates Task\nstatus: todo\npriority: 3\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
        "add lint-dirty",
    );

    // Two passes: the second is quiescent, with no sync errors to fall back on.
    for pass in 1..=2 {
        let output = graf_cmd()
            .args([
                "todo",
                "--json",
                "--horizon",
                "7",
                "--repo",
                repo.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(output.status.success(), "pass {pass} failed");
        let result: Value =
            serde_json::from_str(&String::from_utf8(output.stdout).unwrap()).unwrap();

        let tasks = result["tasks"].as_array().unwrap();
        assert!(
            tasks.iter().all(|t| t["path"] != "todo/no-dates.md"),
            "pass {pass}: null-date row must not appear in tasks"
        );

        let lint_errors = result["lint_errors"].as_array().unwrap();
        let found = lint_errors.iter().any(|e| {
            e["path"] == "todo/no-dates.md"
                && e["message"].as_str() == Some("active task missing check_in_date or due_date")
        });
        assert!(
            found,
            "pass {pass}: horizon must not silence the violation; got: {lint_errors:?}"
        );
    }
}

/// Statusless knowledge-base docs are never returned by `query_tasks`, but
/// their date-exclusivity violations still reach `lint_errors` through the
/// persisted set.
#[test]
fn test_non_task_doc_lint_surfaces_in_query() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    write_and_commit(
        &repo,
        "kb-entry.md",
        "---\ntldr: KB Entry\nend_date: '2026-04-15'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\nBody.\n",
        "add kb entry",
    );

    let output = graf_cmd()
        .args(["todo", "--json", "--repo", repo.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(output.status.success());
    let result: Value = serde_json::from_str(&String::from_utf8(output.stdout).unwrap()).unwrap();

    let tasks = result["tasks"].as_array().unwrap();
    assert!(
        tasks.iter().all(|t| t["path"] != "kb-entry.md"),
        "statusless doc must not appear in tasks; got: {tasks:?}"
    );

    let lint_errors = result["lint_errors"].as_array().unwrap();
    let found = lint_errors.iter().any(|e| {
        e["path"] == "kb-entry.md" && e["message"].as_str() == Some("end_date requires start_date")
    });
    assert!(
        found,
        "statusless doc's violation must surface; got: {lint_errors:?}"
    );
}

/// The mutation paths (`add`, `schedule`, `done`, `cancel`) write frontmatter
/// through `upsert_doc`, which recomputes persisted lint state. A mutation
/// that wrote a lint-tripping shape would leave a violation reported on every
/// later query, so each family asserts a clean path after mutating.
///
/// A deliberately violating file provides the positive control: the same
/// filter must match something, otherwise the clean-path assertions are
/// vacuously true.
#[test]
fn test_mutation_paths_leave_no_persisted_lint_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    write_and_commit(
        &repo,
        "todo/known-bad.md",
        "---\ntldr: Known Bad\nstatus: done\npriority: 3\ncheck_in_date: '2026-04-01'\non_date: '2026-04-01'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
        "add known-bad",
    );

    let lint_errors_for = |path: &str| -> Vec<Value> {
        let output = graf_cmd()
            .args([
                "todo",
                "--json",
                "--include-done",
                "--repo",
                repo.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let result: Value =
            serde_json::from_str(&String::from_utf8(output.stdout).unwrap()).unwrap();
        result["lint_errors"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["path"] == path)
            .cloned()
            .collect()
    };

    let run = |args: &[&str]| {
        let mut full = vec!["todo"];
        full.extend_from_slice(args);
        full.extend_from_slice(&["--repo", repo.to_str().unwrap()]);
        let output = graf_cmd().args(&full).output().unwrap();
        assert!(
            output.status.success(),
            "{args:?} failed: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    };

    // Positive control: the filter and the path format it matches on both
    // work, so the emptiness assertions below mean something.
    let control: Vec<String> = lint_errors_for("todo/known-bad.md")
        .iter()
        .filter_map(|e| e["message"].as_str().map(str::to_string))
        .collect();
    assert!(
        control
            .iter()
            .any(|m| m == "done/cancelled item has check_in_date set"),
        "positive control missing — the path filter no longer matches: {control:?}"
    );

    run(&[
        "add",
        "todo/fresh.md",
        "Fresh Task",
        "--priority",
        "2",
        "--check-in",
        "2026-05-01",
    ]);
    let after_add = lint_errors_for("todo/fresh.md");
    assert!(
        after_add.is_empty(),
        "add wrote a lint-tripping shape: {after_add:?}"
    );

    run(&["schedule", "todo/fresh.md", "2026-06-01"]);
    let after_schedule = lint_errors_for("todo/fresh.md");
    assert!(
        after_schedule.is_empty(),
        "schedule wrote a lint-tripping shape: {after_schedule:?}"
    );

    run(&["done", "todo/fresh.md", "--completion-date", "2026-06-01"]);
    let after_done = lint_errors_for("todo/fresh.md");
    assert!(
        after_done.is_empty(),
        "done wrote a lint-tripping shape: {after_done:?}"
    );

    run(&[
        "add",
        "todo/cancel-me.md",
        "Cancel Me",
        "--priority",
        "2",
        "--check-in",
        "2026-05-01",
    ]);
    run(&["cancel", "todo/cancel-me.md"]);
    let after_cancel = lint_errors_for("todo/cancel-me.md");
    assert!(
        after_cancel.is_empty(),
        "cancel wrote a lint-tripping shape: {after_cancel:?}"
    );
}

/// Anchored terminal completion preserves `check_in_date` and the recurrence
/// block, frozen at the final occurrence. `check_rules` exempts that shape,
/// so a real drive to exhaustion leaves no persisted lint errors.
#[test]
fn test_anchored_exhaustion_leaves_no_persisted_lint_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    write_and_commit(
        &repo,
        "todo/anchored-finite.md",
        "---\ntldr: Anchored Finite\nstatus: todo\npriority: 3\ncheck_in_date: '2026-01-03'\nrrule: \"FREQ=WEEKLY;BYDAY=SA;UNTIL=20260228T000000Z\"\ntentative_date: '2026-01-03'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
        "add anchored finite",
    );

    // Positive control for the lint assertion below: a file that must appear
    // under the same path filter.
    write_and_commit(
        &repo,
        "todo/known-bad.md",
        "---\ntldr: Known Bad\nstatus: done\npriority: 3\ncheck_in_date: '2026-04-01'\non_date: '2026-04-01'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
        "add known-bad",
    );

    let output = graf_cmd()
        .args([
            "todo",
            "--json",
            "done",
            "todo/anchored-finite.md",
            "--completion-date",
            "2026-05-01",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let output = graf_cmd()
        .args([
            "todo",
            "--json",
            "--include-done",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let result: Value = serde_json::from_str(&String::from_utf8(output.stdout).unwrap()).unwrap();
    let messages_for = |path: &str| -> Vec<&str> {
        result["lint_errors"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["path"] == path)
            .filter_map(|e| e["message"].as_str())
            .collect()
    };
    let control = messages_for("todo/known-bad.md");
    assert!(
        control.contains(&"done/cancelled item has check_in_date set"),
        "positive control missing — the path filter no longer matches: {control:?}"
    );
    let messages = messages_for("todo/anchored-finite.md");

    assert!(
        messages.is_empty(),
        "frozen series must be lint-clean; got: {messages:?}"
    );

    // The lint gate no longer blocks mutations on the frozen file: `done`
    // reaches its own terminal-status rejection.
    let output = graf_cmd()
        .args([
            "todo",
            "--json",
            "done",
            "todo/anchored-finite.md",
            "--completion-date",
            "2026-05-02",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let resp: Value = serde_json::from_str(&stdout).unwrap();
    let reason = resp["reason"].as_str().unwrap();
    assert!(
        reason.contains("already has terminal status"),
        "expected the terminal-status rejection, not the lint gate; got: {reason}"
    );
}
