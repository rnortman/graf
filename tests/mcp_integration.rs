use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::{Value, json};

/// Send JSON-RPC requests to `graf mcp` and return parsed responses.
fn mcp_session(repo_dir: &std::path::Path, requests: &[Value]) -> Vec<Value> {
    let graf = env!("CARGO_BIN_EXE_graf");

    let mut child = Command::new(graf)
        .env("GRAF_CONFIG", "/nonexistent/graf-test-config.toml")
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
    drop(child.stdin.take()); // close stdin to signal EOF

    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "graf mcp exited with error");

    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

// TODO(git-env-leaks-into-test-repos): every git spawn in this file inherits
// the caller's GIT_* environment.

/// Create a git repo inside a subdirectory of the tempdir.
/// Uses a subdirectory named "repo" to avoid tempdir names starting with "."
/// (which walkdir's dot-file filter would skip).
fn setup_repo(tmp: &std::path::Path) -> std::path::PathBuf {
    let dir = tmp.join("repo");
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

    std::fs::write(
        dir.join("test.md"),
        "---\ntldr: Test document\nstatus: todo\npriority: 3\ncheck_in_date: '2026-05-01'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\nBody text.\n",
    )
    .unwrap();

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

#[test]
fn test_initialize_and_tools_list() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    let responses = mcp_session(
        &repo,
        &[
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
        ],
    );

    assert_eq!(responses.len(), 2);

    // Initialize response
    assert_eq!(responses[0]["result"]["protocolVersion"], "2024-11-05");
    assert_eq!(responses[0]["result"]["serverInfo"]["name"], "graf");
    let instructions = responses[0]["result"]["instructions"]
        .as_str()
        .expect("instructions should be present");
    assert!(
        instructions.contains("graf is a knowledge base"),
        "instructions should have overview"
    );

    // Tools list
    let tools = responses[1]["result"]["tools"].as_array().unwrap();
    let tool_names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(tool_names.contains(&"graf_lint"));
    assert!(tool_names.contains(&"graf_fix"));
    assert!(tool_names.contains(&"graf_ref_frontmatter_schema"));
    assert!(tool_names.contains(&"graf_ref_attachments"));
    assert!(tool_names.contains(&"graf_ref_links"));
    assert!(tool_names.contains(&"graf_ref_context_link"));
}

#[test]
fn test_ref_tools() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    let responses = mcp_session(
        &repo,
        &[
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "graf_ref_frontmatter_schema", "arguments": {}}}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "graf_ref_attachments", "arguments": {}}}),
            json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "graf_ref_links", "arguments": {}}}),
            json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "graf_ref_context_link", "arguments": {}}}),
        ],
    );

    assert_eq!(responses.len(), 4);

    // frontmatter schema should mention tldr
    let text = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(
        text.contains("tldr"),
        "frontmatter schema should mention tldr"
    );

    // attachments should mention sidecar
    let text = responses[1]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(
        text.contains("sidecar"),
        "attachments ref should mention sidecar"
    );

    // links should mention nexus
    let text = responses[2]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(text.contains("nexus"), "links ref should mention nexus");

    // context_link should mention hierarchical
    let text = responses[3]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(
        text.contains("hierarchical"),
        "context_link ref should mention hierarchical"
    );
}

#[test]
fn test_lint_clean_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    let responses = mcp_session(
        &repo,
        &[json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "graf_lint", "arguments": {"changed": false}}
        })],
    );

    assert_eq!(responses.len(), 1);
    let text = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    let result: Value = serde_json::from_str(text).unwrap();
    assert_eq!(result["files_checked"], 1);
    assert_eq!(result["errors"].as_array().unwrap().len(), 0);
}

#[test]
fn test_lint_catches_error() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    // Add a file with a lint error (done status with check_in_date)
    std::fs::write(
        repo.join("bad.md"),
        "---\ntldr: Bad document\nstatus: done\ncheck_in_date: 2025-06-01\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
    )
    .unwrap();

    let responses = mcp_session(
        &repo,
        &[json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "graf_lint", "arguments": {"changed": false}}
        })],
    );

    let text = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    let result: Value = serde_json::from_str(text).unwrap();
    let errors = result["errors"].as_array().unwrap();
    assert!(!errors.is_empty());
    assert!(
        errors
            .iter()
            .any(|e| e["message"].as_str().unwrap().contains("check_in_date")),
        "expected check_in_date error, got: {errors:?}"
    );
}

#[test]
fn test_fix_updates_timestamps() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    // Modify a file so it shows as changed in git
    std::fs::write(
        repo.join("test.md"),
        "---\ntldr: Test document updated\nstatus: todo\npriority: 3\ncheck_in_date: '2026-05-01'\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\nUpdated body.\n",
    )
    .unwrap();

    let responses = mcp_session(
        &repo,
        &[json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "graf_fix", "arguments": {}}
        })],
    );

    let text = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    let result: Value = serde_json::from_str(text).unwrap();
    assert_eq!(result["files_checked"], 1);
    // The fix should have updated the timestamp
    assert_eq!(result["files_fixed"].as_array().unwrap().len(), 1);
}

#[test]
fn test_lint_with_paths_parameter() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    // Add a file with a lint error
    std::fs::write(
        repo.join("bad.md"),
        "---\ntldr: Bad\nstatus: done\ncheck_in_date: 2025-06-01\n---\n",
    )
    .unwrap();

    // Lint only the good file via paths — should find no errors
    let responses = mcp_session(
        &repo,
        &[json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "graf_lint", "arguments": {"paths": ["test.md"]}}
        })],
    );

    let text = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    let result: Value = serde_json::from_str(text).unwrap();
    assert_eq!(result["files_checked"], 1);
    assert_eq!(result["errors"].as_array().unwrap().len(), 0);

    // Now lint only the bad file — should find the error
    let responses = mcp_session(
        &repo,
        &[json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "graf_lint", "arguments": {"paths": ["bad.md"]}}
        })],
    );

    let text = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    let result: Value = serde_json::from_str(text).unwrap();
    assert_eq!(result["files_checked"], 1);
    assert!(result["errors"].as_array().unwrap().len() > 0);
}

// ---------------------------------------------------------------------------
// graf_todo_add tests
// ---------------------------------------------------------------------------

#[test]
fn test_mcp_todo_add_with_body() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    let responses = mcp_session(
        &repo,
        &[json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {
                "name": "graf_todo_add",
                "arguments": {
                    "path": "todo/task.md",
                    "title": "Body test",
                    "check_in_date": "2026-06-01",
                    "body": "  Hello\n\nWorld  "
                }
            }
        })],
    );

    assert_eq!(responses.len(), 1);
    assert!(
        responses[0]["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("task.md"),
        "expected path in result"
    );

    let content = std::fs::read_to_string(repo.join("todo/task.md")).unwrap();
    // Body stripped of leading/trailing whitespace, single trailing newline
    assert!(
        content.ends_with("---\nHello\n\nWorld\n"),
        "expected stripped body, got:\n{content}"
    );
}

#[test]
fn test_mcp_todo_add_body_with_dash_line() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    let responses = mcp_session(
        &repo,
        &[json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {
                "name": "graf_todo_add",
                "arguments": {
                    "path": "todo/task.md",
                    "title": "Dash in body",
                    "check_in_date": "2026-06-01",
                    "body": "intro\n---\nstill body"
                }
            }
        })],
    );

    assert_eq!(responses.len(), 1);

    let content = std::fs::read_to_string(repo.join("todo/task.md")).unwrap();

    // Frontmatter must parse correctly; the body's --- must not be treated as
    // the closing frontmatter delimiter.
    use graf::frontmatter::parse_frontmatter;
    let fm = parse_frontmatter(&content)
        .expect("parse_frontmatter error")
        .expect("missing frontmatter");
    assert_eq!(fm.tldr, "Dash in body");
    fm.validate().unwrap();

    // Body content must follow the closing --- and be verbatim (stripped).
    assert!(
        content.ends_with("---\nintro\n---\nstill body\n"),
        "body not correct:\n{content}"
    );
}

#[test]
fn test_mcp_todo_add_with_all_new_fields() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    let responses = mcp_session(
        &repo,
        &[json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {
                "name": "graf_todo_add",
                "arguments": {
                    "path": "todo/full.md",
                    "title": "Full fields test",
                    "check_in_date": "2026-06-01",
                    "summary": "A summary",
                    "context_link": "projects/alpha.md",
                    "links": ["docs/a.md", "docs/b.md"],
                    "blocked_by": ["todo/blocker.md"],
                    "assigned_to": "alice",
                    "on_date": "2026-06-15",
                    "start_date": "2026-06-01",
                    "end_date": "2026-06-30",
                    "verified_date": "2026-05-20",
                    "tentative_date": "2026-07-04",
                    "body": "The body text."
                }
            }
        })],
    );

    assert_eq!(responses.len(), 1);
    assert!(
        !responses[0]["error"].is_object(),
        "unexpected error: {:?}",
        responses[0]
    );

    let abs = repo.join("todo/full.md");
    let content = std::fs::read_to_string(&abs).unwrap();

    // Use frontmatter::parse_frontmatter for round-trip check
    use chrono::NaiveDate;
    use graf::frontmatter::parse_frontmatter;
    let fm = parse_frontmatter(&content).unwrap().unwrap();
    fm.validate().unwrap();

    assert_eq!(fm.summary.as_deref(), Some("A summary"));
    assert_eq!(fm.context_link.as_deref(), Some("projects/alpha.md"));
    assert_eq!(fm.links, vec!["docs/a.md", "docs/b.md"]);
    assert_eq!(fm.blocked_by, vec!["todo/blocker.md"]);
    assert_eq!(fm.assigned_to.as_deref(), Some("alice"));
    assert_eq!(
        fm.on_date,
        Some(NaiveDate::from_ymd_opt(2026, 6, 15).unwrap())
    );
    assert_eq!(
        fm.start_date,
        Some(NaiveDate::from_ymd_opt(2026, 6, 1).unwrap())
    );
    assert_eq!(
        fm.end_date,
        Some(NaiveDate::from_ymd_opt(2026, 6, 30).unwrap())
    );
    assert_eq!(
        fm.verified_date,
        Some(NaiveDate::from_ymd_opt(2026, 5, 20).unwrap())
    );
    assert_eq!(
        fm.tentative_date,
        Some(NaiveDate::from_ymd_opt(2026, 7, 4).unwrap())
    );
    assert!(content.ends_with("---\nThe body text.\n"));
}

#[test]
fn test_mcp_todo_add_invalid_date_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    let responses = mcp_session(
        &repo,
        &[json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {
                "name": "graf_todo_add",
                "arguments": {
                    "path": "todo/bad.md",
                    "title": "Bad date",
                    "check_in_date": "2026-06-01",
                    "on_date": "not-a-date"
                }
            }
        })],
    );

    assert_eq!(responses.len(), 1);
    let text = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("");
    let is_error = responses[0]["result"]["isError"].as_bool().unwrap_or(false)
        || responses[0]["error"].is_object();
    assert!(
        is_error || text.to_lowercase().contains("invalid"),
        "expected error for invalid date, got: {:?}",
        responses[0]
    );
    assert!(
        !repo.join("todo/bad.md").exists(),
        "file should not be created when date is invalid"
    );
}

#[test]
fn test_mcp_todo_add_input_schema_advertises_new_fields() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    let responses = mcp_session(
        &repo,
        &[json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}})],
    );

    assert_eq!(responses.len(), 1);
    let tools = responses[0]["result"]["tools"].as_array().unwrap();
    let todo_add = tools
        .iter()
        .find(|t| t["name"] == "graf_todo_add")
        .expect("graf_todo_add tool not found");

    let props = &todo_add["inputSchema"]["properties"];
    for field in &[
        "body",
        "summary",
        "context_link",
        "links",
        "blocked_by",
        "assigned_to",
        "on_date",
        "start_date",
        "end_date",
        "verified_date",
        "tentative_date",
    ] {
        assert!(
            !props[field].is_null(),
            "input_schema missing field: {field}"
        );
    }

    // Check types
    assert_eq!(props["body"]["type"], "string");
    assert_eq!(props["summary"]["type"], "string");
    assert_eq!(props["context_link"]["type"], "string");
    assert_eq!(props["links"]["type"], "array");
    assert_eq!(props["links"]["items"]["type"], "string");
    assert_eq!(props["blocked_by"]["type"], "array");
    assert_eq!(props["blocked_by"]["items"]["type"], "string");
    assert_eq!(props["assigned_to"]["type"], "string");
    assert_eq!(props["on_date"]["type"], "string");
    assert_eq!(props["start_date"]["type"], "string");
    assert_eq!(props["end_date"]["type"], "string");
    assert_eq!(props["verified_date"]["type"], "string");
    assert_eq!(props["tentative_date"]["type"], "string");

    // New fields must NOT be in required
    let required = &todo_add["inputSchema"]["required"];
    for field in &[
        "body",
        "summary",
        "context_link",
        "links",
        "blocked_by",
        "assigned_to",
        "on_date",
        "start_date",
        "end_date",
        "verified_date",
        "tentative_date",
    ] {
        assert!(
            !required
                .as_array()
                .unwrap_or(&vec![])
                .iter()
                .any(|v| v == field),
            "field {field} should not be required"
        );
    }
}

/// MCP-level byte-identical test: calling graf_todo_add with only the
/// pre-existing params (no new fields) must produce byte-identical output to
/// the pre-change format, verifying that the MCP parse path (parse_string_array
/// etc.) doesn't inject spurious non-None values for absent new fields.
#[test]
fn test_mcp_todo_add_no_new_params_output_unchanged() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    let responses = mcp_session(
        &repo,
        &[json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {
                "name": "graf_todo_add",
                "arguments": {
                    "path": "todo/minimal.md",
                    "title": "Minimal task",
                    "check_in_date": "2026-05-01"
                }
            }
        })],
    );

    assert_eq!(responses.len(), 1);
    assert!(
        !responses[0]["error"].is_object(),
        "unexpected error: {:?}",
        responses[0]
    );

    let content = std::fs::read_to_string(repo.join("todo/minimal.md")).unwrap();

    // Extract clock-derived fields so we can build a byte-exact expected string.
    let created = content
        .lines()
        .find_map(|l| l.strip_prefix("created: '"))
        .and_then(|s| s.strip_suffix('\''))
        .expect("created line missing");
    let updated = content
        .lines()
        .find_map(|l| l.strip_prefix("updated: '"))
        .and_then(|s| s.strip_suffix('\''))
        .expect("updated line missing");

    let expected = format!(
        "---\ntldr: \"Minimal task\"\nstatus: todo\npriority: 3\ncheck_in_date: '2026-05-01'\ncreated: '{created}'\nupdated: '{updated}'\n---\n"
    );
    assert_eq!(
        content, expected,
        "MCP byte-identical format check failed; got:\n{content}"
    );
}
