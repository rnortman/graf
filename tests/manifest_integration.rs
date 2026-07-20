use std::path::Path;
use std::process::Command;

use serde_json::Value;

fn graf(args: &[&str], manifest_path: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_graf"))
        .args(args)
        .env("GRAF_MANIFEST", manifest_path)
        .output()
        .expect("failed to run graf")
}

fn graf_in(args: &[&str], manifest_path: &Path, cwd: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_graf"))
        .args(args)
        .env("GRAF_MANIFEST", manifest_path)
        .current_dir(cwd)
        .output()
        .expect("failed to run graf")
}

#[test]
fn test_manifest_init_creates_config() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    let manifest = tmp.path().join("manifest.toml");

    let out = graf(
        &[
            "manifest",
            "init",
            "--id",
            "example.com/life",
            "--domain",
            "example.com/personal",
            "--repo",
            repo.to_str().unwrap(),
        ],
        &manifest,
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let config = std::fs::read_to_string(repo.join(".graf/config.toml")).unwrap();
    assert!(config.contains(r#"id = "example.com/life""#));
    assert!(config.contains(r#"default_slug = "life""#));
    assert!(config.contains(r#"domain = "example.com/personal""#));
}

#[test]
fn test_manifest_init_custom_slug() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    let manifest = tmp.path().join("manifest.toml");

    let out = graf(
        &[
            "manifest",
            "init",
            "--id",
            "example.com/life",
            "--slug",
            "personal",
            "--repo",
            repo.to_str().unwrap(),
        ],
        &manifest,
    );
    assert!(out.status.success());

    let config = std::fs::read_to_string(repo.join(".graf/config.toml")).unwrap();
    assert!(config.contains(r#"default_slug = "personal""#));
}

#[test]
fn test_manifest_init_rejects_duplicate() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    let manifest = tmp.path().join("manifest.toml");

    let out = graf(
        &[
            "manifest",
            "init",
            "--id",
            "example.com/life",
            "--repo",
            repo.to_str().unwrap(),
        ],
        &manifest,
    );
    assert!(out.status.success());

    // Second init should fail
    let out = graf(
        &[
            "manifest",
            "init",
            "--id",
            "example.com/life",
            "--repo",
            repo.to_str().unwrap(),
        ],
        &manifest,
    );
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("already exists"));
}

#[test]
fn test_manifest_init_defaults_to_cwd() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    let manifest = tmp.path().join("manifest.toml");

    let out = graf_in(
        &["manifest", "init", "--id", "example.com/life"],
        &manifest,
        &repo,
    );
    assert!(out.status.success());
    assert!(repo.join(".graf/config.toml").exists());
}

#[test]
fn test_manifest_add_and_list() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    let manifest = tmp.path().join("manifest.toml");

    // Init the repo
    graf(
        &[
            "manifest",
            "init",
            "--id",
            "example.com/life",
            "--repo",
            repo.to_str().unwrap(),
        ],
        &manifest,
    );

    // Add to manifest (using default slug from repo config)
    let out = graf(&["manifest", "add", repo.to_str().unwrap()], &manifest);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // List should show the repo
    let out = graf(&["manifest", "list"], &manifest);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("life"));
    assert!(stdout.contains("example.com/life"));
}

#[test]
fn test_manifest_add_custom_slug() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    let manifest = tmp.path().join("manifest.toml");

    graf(
        &[
            "manifest",
            "init",
            "--id",
            "example.com/life",
            "--repo",
            repo.to_str().unwrap(),
        ],
        &manifest,
    );

    // Add with explicit slug override
    let out = graf(
        &["manifest", "add", repo.to_str().unwrap(), "personal"],
        &manifest,
    );
    assert!(out.status.success());

    let out = graf(&["manifest", "list"], &manifest);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("personal"));
}

#[test]
fn test_manifest_add_rejects_missing_config() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    let manifest = tmp.path().join("manifest.toml");

    // Add without init should fail
    let out = graf(&["manifest", "add", repo.to_str().unwrap()], &manifest);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no .graf/config.toml"));
}

#[test]
fn test_manifest_add_rejects_duplicate_slug() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = tmp.path().join("manifest.toml");

    // Create two repos
    let repo1 = tmp.path().join("repo1");
    std::fs::create_dir(&repo1).unwrap();
    graf(
        &[
            "manifest",
            "init",
            "--id",
            "example.com/life",
            "--slug",
            "life",
            "--repo",
            repo1.to_str().unwrap(),
        ],
        &manifest,
    );
    graf(&["manifest", "add", repo1.to_str().unwrap()], &manifest);

    let repo2 = tmp.path().join("repo2");
    std::fs::create_dir(&repo2).unwrap();
    graf(
        &[
            "manifest",
            "init",
            "--id",
            "example.com/other",
            "--slug",
            "life",
            "--repo",
            repo2.to_str().unwrap(),
        ],
        &manifest,
    );

    // Should fail: duplicate slug
    let out = graf(&["manifest", "add", repo2.to_str().unwrap()], &manifest);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("already exists"));
}

#[test]
fn test_manifest_check_ok() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    let manifest = tmp.path().join("manifest.toml");

    graf(
        &[
            "manifest",
            "init",
            "--id",
            "example.com/life",
            "--repo",
            repo.to_str().unwrap(),
        ],
        &manifest,
    );
    graf(&["manifest", "add", repo.to_str().unwrap()], &manifest);

    let out = graf(&["manifest", "check"], &manifest);
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    let resp: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(resp["ok"], true);
    assert_eq!(resp["repos"], 1);
}

#[test]
fn test_manifest_check_detects_missing_path() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = tmp.path().join("manifest.toml");

    // Create a manifest pointing to a nonexistent path
    std::fs::write(
        &manifest,
        r#"
[[repo]]
slug = "life"
path = "/nonexistent/path"
id = "example.com/life"
"#,
    )
    .unwrap();

    let out = graf(&["manifest", "check"], &manifest);
    assert!(!out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    let resp: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(resp["ok"], false);
    let errors = resp["errors"].as_array().unwrap();
    assert!(
        errors
            .iter()
            .any(|e| e.as_str().unwrap().contains("does not exist"))
    );
}

#[test]
fn test_manifest_check_detects_id_mismatch() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    let manifest = tmp.path().join("manifest.toml");

    // Init repo with one ID
    graf(
        &[
            "manifest",
            "init",
            "--id",
            "example.com/life",
            "--repo",
            repo.to_str().unwrap(),
        ],
        &manifest,
    );

    // Manually write manifest with a different ID
    std::fs::write(
        &manifest,
        format!(
            r#"
[[repo]]
slug = "life"
path = "{}"
id = "example.com/other"
"#,
            repo.display()
        ),
    )
    .unwrap();

    let out = graf(&["manifest", "check"], &manifest);
    assert!(!out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    let resp: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(resp["ok"], false);
    let errors = resp["errors"].as_array().unwrap();
    assert!(
        errors
            .iter()
            .any(|e| e.as_str().unwrap().contains("does not match"))
    );
}

#[test]
fn test_manifest_init_rejects_invalid_id() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    let manifest = tmp.path().join("manifest.toml");

    let out = graf(
        &[
            "manifest",
            "init",
            "--id",
            "not-valid",
            "--repo",
            repo.to_str().unwrap(),
        ],
        &manifest,
    );
    assert!(!out.status.success());
}

#[test]
fn test_manifest_list_no_manifest() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = tmp.path().join("nonexistent.toml");

    let out = graf(&["manifest", "list"], &manifest);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no manifest found"));
}

#[test]
fn test_manifest_check_no_manifest() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = tmp.path().join("nonexistent.toml");

    let out = graf(&["manifest", "check"], &manifest);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no manifest found"));
}

#[test]
fn test_manifest_add_rejects_duplicate_id() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = tmp.path().join("manifest.toml");

    // Create first repo and add it
    let repo1 = tmp.path().join("repo1");
    std::fs::create_dir(&repo1).unwrap();
    graf(
        &[
            "manifest",
            "init",
            "--id",
            "example.com/life",
            "--repo",
            repo1.to_str().unwrap(),
        ],
        &manifest,
    );
    graf(&["manifest", "add", repo1.to_str().unwrap()], &manifest);

    // Create second repo with same ID but different slug
    let repo2 = tmp.path().join("repo2");
    std::fs::create_dir(&repo2).unwrap();
    graf(
        &[
            "manifest",
            "init",
            "--id",
            "example.com/life",
            "--slug",
            "life2",
            "--repo",
            repo2.to_str().unwrap(),
        ],
        &manifest,
    );

    // Should fail: duplicate ID
    let out = graf(
        &["manifest", "add", repo2.to_str().unwrap(), "life2"],
        &manifest,
    );
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("already exists"));
}

#[test]
fn test_manifest_check_prefix_is_warning_not_error() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = tmp.path().join("manifest.toml");

    // Create two repos with prefix relationship
    let repo1 = tmp.path().join("repo1");
    std::fs::create_dir(&repo1).unwrap();
    graf(
        &[
            "manifest",
            "init",
            "--id",
            "acme.com/eng",
            "--repo",
            repo1.to_str().unwrap(),
        ],
        &manifest,
    );
    graf(&["manifest", "add", repo1.to_str().unwrap()], &manifest);

    let repo2 = tmp.path().join("repo2");
    std::fs::create_dir(&repo2).unwrap();
    graf(
        &[
            "manifest",
            "init",
            "--id",
            "acme.com/eng/frontend",
            "--repo",
            repo2.to_str().unwrap(),
        ],
        &manifest,
    );
    graf(
        &["manifest", "add", repo2.to_str().unwrap(), "frontend"],
        &manifest,
    );

    // Check should succeed (prefix is a warning, not an error)
    let out = graf(&["manifest", "check"], &manifest);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    let resp: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(resp["ok"], true);
    let warnings = resp["warnings"].as_array().unwrap();
    assert!(!warnings.is_empty(), "should have prefix warning");
}
