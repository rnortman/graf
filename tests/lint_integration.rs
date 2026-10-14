use std::process::Command;

// TODO(git-env-leaks-into-test-repos): every git spawn in this file inherits
// the caller's GIT_* environment.

/// Create a git repo with a valid .md file.
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
        dir.join("good.md"),
        "---\ntldr: Good document\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\nBody.\n",
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

fn graf() -> Command {
    Command::new(env!("CARGO_BIN_EXE_graf"))
}

/// Parse JSON from stdout. In tests, stdout is not a TTY, so JSON auto-enables.
fn parse_json_stdout(output: &std::process::Output) -> serde_json::Value {
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(&stdout).expect("stdout should be valid JSON")
}

#[test]
fn test_lint_positional_files_good() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    // Lint only the good file — should succeed
    let output = graf()
        .args(["lint", "--repo", repo.to_str().unwrap(), "good.md"])
        .output()
        .unwrap();
    assert!(output.status.success(), "lint good.md should succeed");

    let result = parse_json_stdout(&output);
    assert_eq!(result["files_checked"], 1);
    assert_eq!(result["errors"].as_array().unwrap().len(), 0);
}

#[test]
fn test_lint_positional_files_bad() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    std::fs::write(
        repo.join("bad.md"),
        "---\ntldr: Bad\nstatus: done\ncheck_in_date: 2025-06-01\n---\n",
    )
    .unwrap();

    // Lint only the bad file — should fail with error in JSON output
    let output = graf()
        .args(["lint", "--repo", repo.to_str().unwrap(), "bad.md"])
        .output()
        .unwrap();
    assert!(!output.status.success(), "lint bad.md should fail");

    let result = parse_json_stdout(&output);
    assert_eq!(result["files_checked"], 1);
    let errors = result["errors"].as_array().unwrap();
    assert!(
        errors
            .iter()
            .any(|e| e["message"].as_str().unwrap().contains("check_in_date")),
        "expected check_in_date error, got: {errors:?}"
    );
}

#[test]
fn test_lint_positional_files_selective() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    // Add a bad file — but only lint the good one
    std::fs::write(
        repo.join("bad.md"),
        "---\ntldr: Bad\nstatus: done\ncheck_in_date: 2025-06-01\n---\n",
    )
    .unwrap();

    let output = graf()
        .args(["lint", "--repo", repo.to_str().unwrap(), "good.md"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "lint good.md should succeed even when bad.md exists"
    );
}

#[test]
fn test_lint_changed_and_files_conflict() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    let output = graf()
        .args([
            "lint",
            "--repo",
            repo.to_str().unwrap(),
            "--changed",
            "good.md",
        ])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "--changed + files should be an error"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("mutually exclusive"));
}

#[test]
fn test_lint_json_output() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    // Explicit --json flag (also auto-enables since not a TTY, but testing the flag)
    let output = graf()
        .args(["lint", "--repo", repo.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());

    let result = parse_json_stdout(&output);
    assert_eq!(result["files_checked"], 1);
    assert_eq!(result["errors"].as_array().unwrap().len(), 0);
}

#[test]
fn test_lint_json_with_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    std::fs::write(
        repo.join("bad.md"),
        "---\ntldr: Bad\nstatus: done\ncheck_in_date: 2025-06-01\n---\n",
    )
    .unwrap();

    let output = graf()
        .args(["lint", "--repo", repo.to_str().unwrap(), "--json", "bad.md"])
        .output()
        .unwrap();
    // Should exit 1 but still produce valid JSON
    assert!(!output.status.success());

    let result = parse_json_stdout(&output);
    assert_eq!(result["files_checked"], 1);
    assert!(result["errors"].as_array().unwrap().len() > 0);
}

#[test]
fn test_fix_json_output() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    // Modify a file so it shows as changed
    std::fs::write(
        repo.join("good.md"),
        "---\ntldr: Updated\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\nNew body.\n",
    )
    .unwrap();

    let output = graf()
        .args(["fix", "--repo", repo.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());

    let result = parse_json_stdout(&output);
    assert_eq!(result["files_checked"], 1);
    assert_eq!(result["files_fixed"].as_array().unwrap().len(), 1);
}

#[test]
fn test_lint_nonexistent_file() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = setup_repo(tmp.path());

    let output = graf()
        .args(["lint", "--repo", repo.to_str().unwrap(), "nope.md"])
        .output()
        .unwrap();
    assert!(!output.status.success());

    // In non-TTY mode, errors are in JSON stdout
    let result = parse_json_stdout(&output);
    let errors = result["errors"].as_array().unwrap();
    assert_eq!(errors.len(), 1);
    assert!(
        errors[0]["message"]
            .as_str()
            .unwrap()
            .contains("file not found")
    );
}
