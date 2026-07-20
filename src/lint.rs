use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::{Value, json};

use crate::errors::FileError;
use crate::frontmatter::{self, Field, FieldMutation, Frontmatter, Status};
use crate::git::{self, FileChange};
use crate::{collect_repo_files, is_md, should_skip_file};

pub struct LintOptions {
    pub fix: bool,
    pub repo_dir: PathBuf,
}

#[derive(Default)]
pub struct LintResult {
    pub files_checked: usize,
    pub errors: Vec<FileError>,
    pub files_fixed: Vec<PathBuf>,
}

/// Convert a `LintResult` to JSON.  Used by both the CLI (`--json`) and
/// the MCP server.
pub fn lint_result_to_json(result: &LintResult) -> Value {
    json!({
        "files_checked": result.files_checked,
        "files_fixed": result.files_fixed.iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>(),
        "errors": result.errors.iter()
            .map(|e| {
                let mut obj = json!({
                    "path": e.path.display().to_string(),
                    "message": e.message,
                });
                if let Some(ref repo) = e.repo {
                    obj["repo"] = json!(repo.as_str());
                }
                obj
            })
            .collect::<Vec<Value>>(),
    })
}

const MSG_ON_AND_START: &str =
    "on_date and start_date cannot both be set (use start_date + end_date for ranges)";
const MSG_ON_AND_END: &str =
    "on_date and end_date cannot both be set (use start_date + end_date for ranges)";
const MSG_END_NEEDS_START: &str = "end_date requires start_date";
const MSG_TERMINAL_NO_DATE: &str = "done/cancelled item missing on_date or end_date";
const MSG_TERMINAL_CHECK_IN: &str = "done/cancelled item has check_in_date set";
const MSG_TERMINAL_RRULE: &str =
    "done/cancelled item has rrule set (remove rrule to stop recurrence)";
const MSG_ACTIVE_NO_DATE: &str = "active task missing check_in_date or due_date";

/// Roster of every message `check_rules` can emit.  The rules reference the
/// `MSG_*` constants directly and emit through `emit`, which asserts
/// membership here, so an unregistered message fails in every debug/test run
/// regardless of fixture coverage.  The message-snapshot test checks its
/// fixtures against this array in the other direction: a message listed here
/// that no fixture trips fails the test rather than passing unnoticed.
const RULE_MESSAGES: [&str; 7] = [
    MSG_ON_AND_START,
    MSG_ON_AND_END,
    MSG_END_NEEDS_START,
    MSG_TERMINAL_NO_DATE,
    MSG_TERMINAL_CHECK_IN,
    MSG_TERMINAL_RRULE,
    MSG_ACTIVE_NO_DATE,
];

/// Single emit point for lint violations.  Every message must be declared in
/// `RULE_MESSAGES`; the assertion makes an undeclared one fail loudly in
/// debug and test builds instead of depending on a fixture happening to trip
/// it.
fn emit(errors: &mut Vec<FileError>, path: &Path, msg: &'static str) {
    debug_assert!(
        RULE_MESSAGES.contains(&msg),
        "lint message not declared in RULE_MESSAGES: {msg:?}"
    );
    errors.push(FileError::new(path, msg));
}

/// Check lint rules on a parsed Frontmatter.  Returns errors (if any)
/// without modifying anything.  Used by both `lint_md_file` and the
/// sync protocol.
///
/// Any change to these rules or their message text requires bumping
/// `SCHEMA_VERSION` in `db.rs`.  Messages are persisted per-doc in the cache
/// and deduped against the current pass by exact text: without a bump, an
/// added rule stays silent for every doc not re-upserted since, and reworded
/// ones double-report.  New messages belong in `RULE_MESSAGES`.
///
/// Exemption: a *frozen series* — a terminal item carrying a recurrence with a
/// non-empty completion log — keeps its `check_in_date`, `due_date` and
/// recurrence block frozen at the final occurrence, so the two
/// terminal-scheduling rules do not apply to it.  Terminal items with a
/// recurrence but no completion history, and terminal items with leftover
/// `check_in_date` and no recurrence, are still flagged.
pub fn check_rules(path: &Path, fm: &Frontmatter) -> Vec<FileError> {
    let mut errors = Vec::new();

    // A completed series freezes recurrence / check_in_date / due_date at the
    // final occurrence.  That shape is a valid record, not leftover scheduling
    // state.  Both conjuncts matter: non-recurring completions also append to
    // the log, and a recurrence with an empty log was never completed here.
    let frozen_series = fm.effective_rrule().is_some() && !fm.completion_log.is_empty();

    // Date mutual exclusivity rules (apply to all documents, not just tasks)
    if fm.on_date.is_some() && fm.start_date.is_some() {
        emit(&mut errors, path, MSG_ON_AND_START);
    }
    if fm.on_date.is_some() && fm.end_date.is_some() {
        emit(&mut errors, path, MSG_ON_AND_END);
    }
    if fm.end_date.is_some() && fm.start_date.is_none() {
        emit(&mut errors, path, MSG_END_NEEDS_START);
    }

    if matches!(fm.status, Some(Status::Done | Status::Cancelled)) {
        if fm.on_date.is_none() && fm.end_date.is_none() {
            emit(&mut errors, path, MSG_TERMINAL_NO_DATE);
        }
        if fm.check_in_date.is_some() && !frozen_series {
            emit(&mut errors, path, MSG_TERMINAL_CHECK_IN);
        }
        if fm.effective_rrule().is_some() && !frozen_series {
            emit(&mut errors, path, MSG_TERMINAL_RRULE);
        }
    }

    // Active tasks must have check_in_date or due_date.
    // Documents without a status (knowledge-base entries) are exempt.
    if matches!(
        fm.status,
        Some(Status::Todo | Status::InProgress | Status::Reminder)
    ) && fm.check_in_date.is_none()
        && fm.due_date.is_none()
    {
        emit(&mut errors, path, MSG_ACTIVE_NO_DATE);
    }

    errors
}

/// Lint specific files by path (relative to repo root).
pub fn lint_files(options: &LintOptions, files: &[PathBuf]) -> Result<LintResult> {
    let mut result = LintResult::default();

    for rel_path in files {
        if should_skip_file(rel_path) {
            continue;
        }
        let abs = options.repo_dir.join(rel_path);
        if !abs.exists() {
            result
                .errors
                .push(FileError::new(rel_path, "file not found"));
            continue;
        }
        if !is_md(rel_path) {
            result
                .errors
                .push(FileError::new(rel_path, "not a markdown file"));
            continue;
        }
        result.files_checked += 1;
        // Explicit paths: change=None (treated as modified, no `created` fix)
        lint_md_file(&abs, None, options.fix, &mut result)?;
    }

    Ok(result)
}

/// Lint all files in the repo (full scan).
pub fn lint_all(options: &LintOptions) -> Result<LintResult> {
    let all_files = collect_repo_files(&options.repo_dir)?;

    let md_files: Vec<&PathBuf> = all_files.iter().filter(|p| is_md(p)).collect();
    let non_md_files: Vec<&PathBuf> = all_files.iter().filter(|p| !is_md(p)).collect();

    let mut result = LintResult::default();

    for path in &md_files {
        let abs = options.repo_dir.join(path);
        result.files_checked += 1;
        lint_md_file(&abs, None, options.fix, &mut result)?;
    }

    check_orphan_attachments(&non_md_files, &md_files, &mut result);

    Ok(result)
}

/// Lint only git-changed files.
pub fn lint_changed(options: &LintOptions) -> Result<LintResult> {
    let changed = git::changed_files(&options.repo_dir)?;

    let mut result = LintResult::default();

    let md_changes: Vec<_> = changed.iter().filter(|f| is_md(&f.path)).collect();
    let non_md_changes: Vec<_> = changed
        .iter()
        .filter(|f| !is_md(&f.path) && f.change != FileChange::Deleted)
        .collect();

    let md_paths: Vec<PathBuf> = md_changes.iter().map(|f| f.path.clone()).collect();

    for cf in &md_changes {
        if cf.change == FileChange::Deleted {
            continue;
        }
        let abs = options.repo_dir.join(&cf.path);
        result.files_checked += 1;
        lint_md_file(&abs, Some(&cf.change), options.fix, &mut result)?;
    }

    for cf in &non_md_changes {
        let abs = options.repo_dir.join(&cf.path);
        if should_skip_file(&cf.path) {
            continue;
        }
        let md_sibling = abs.with_extension("md");
        if !md_sibling.exists() {
            // Also check if there's a .md with the same stem in the changed set
            let stem = cf.path.file_stem().and_then(|s| s.to_str());
            let has_md_sibling = stem.is_some_and(|stem| {
                md_paths.iter().any(|p| {
                    p.parent() == cf.path.parent()
                        && p.file_stem().and_then(|s| s.to_str()) == Some(stem)
                })
            });
            if !has_md_sibling {
                result.errors.push(FileError::new(
                    &cf.path,
                    "orphan attachment: no corresponding .md file",
                ));
            }
        }
    }

    Ok(result)
}

/// Lint a single .md file. If `fix` is true and `change` is provided, apply timestamp fixes.
fn lint_md_file(
    abs_path: &Path,
    change: Option<&FileChange>,
    fix: bool,
    result: &mut LintResult,
) -> Result<()> {
    let content = std::fs::read_to_string(abs_path)
        .with_context(|| format!("failed to read {}", abs_path.display()))?;

    let fm = match frontmatter::parse_frontmatter(&content) {
        Ok(Some(fm)) => fm,
        Ok(None) => {
            result
                .errors
                .push(FileError::new(abs_path, "missing frontmatter".to_string()));
            return Ok(());
        }
        Err(e) => {
            result
                .errors
                .push(FileError::new(abs_path, format!("{e:#}")));
            return Ok(());
        }
    };

    if let Err(e) = fm.validate() {
        result
            .errors
            .push(FileError::new(abs_path, format!("{e:#}")));
        return Ok(());
    }

    let errors_before = result.errors.len();
    result.errors.extend(check_rules(abs_path, &fm));

    // Apply fixes only if no lint errors were found for this file
    if fix && result.errors.len() == errors_before {
        let new_content = apply_fixes(&content, abs_path, change)?;
        if new_content != content {
            std::fs::write(abs_path, &new_content)
                .with_context(|| format!("failed to write {}", abs_path.display()))?;
            result.files_fixed.push(abs_path.to_path_buf());
        }
    }

    Ok(())
}

/// Apply timestamp fixes to file content, preserving formatting.
fn apply_fixes(content: &str, path: &Path, change: Option<&FileChange>) -> Result<String> {
    let mtime = file_mtime_utc(path)?;
    let timestamp = format_timestamp(mtime);

    let mut mutations = vec![FieldMutation::Set {
        field: Field::Updated,
        value: format!("'{timestamp}'"),
    }];

    // Overwrite created for new files — LLMs don't know the real time, mtime is truth
    if matches!(change, Some(FileChange::New)) {
        mutations.push(FieldMutation::Set {
            field: Field::Created,
            value: format!("'{timestamp}'"),
        });
    }

    frontmatter::rewrite_frontmatter(content, &mutations)
}

/// Get file modification time as UTC DateTime.
fn file_mtime_utc(path: &Path) -> Result<DateTime<Utc>> {
    let metadata =
        std::fs::metadata(path).with_context(|| format!("failed to stat {}", path.display()))?;
    let mtime: SystemTime = metadata
        .modified()
        .with_context(|| format!("mtime not available for {}", path.display()))?;
    Ok(DateTime::<Utc>::from(mtime))
}

/// Format a DateTime as an ISO 8601 timestamp matching existing convention.
fn format_timestamp(dt: DateTime<Utc>) -> String {
    dt.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

/// Check for non-.md files without a corresponding .md file.
fn check_orphan_attachments(
    non_md_files: &[&PathBuf],
    md_files: &[&PathBuf],
    result: &mut LintResult,
) {
    for path in non_md_files {
        let stem = path.file_stem().and_then(|s| s.to_str());
        let parent = path.parent();
        let has_md = stem.is_some_and(|stem| {
            md_files.iter().any(|md| {
                md.parent() == parent && md.file_stem().and_then(|s| s.to_str()) == Some(stem)
            })
        });
        if !has_md {
            result.errors.push(FileError::new(
                path,
                "orphan attachment: no corresponding .md file",
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use std::fs;
    use tempfile::TempDir;

    fn bare_fm() -> Frontmatter {
        Frontmatter {
            tldr: "test".to_string(),
            labels: vec![],
            created: None,
            updated: None,
            summary: None,
            on_date: None,
            start_date: None,
            end_date: None,
            due_date: None,
            check_in_date: None,
            verified_date: None,
            context_link: None,
            links: vec![],
            attachments: vec![],
            status: None,
            priority: None,
            effort: None,
            blocked_by: vec![],
            rrule: None,
            tentative_date: None,
            sort_order: None,
            assigned_to: None,
            recurrence: None,
            completion_log: vec![],
        }
    }

    fn log_entry(day: u32) -> crate::frontmatter::CompletionLogEntry {
        crate::frontmatter::CompletionLogEntry {
            completed: NaiveDate::from_ymd_opt(2026, 4, day).unwrap(),
            occurrence: Some(NaiveDate::from_ymd_opt(2026, 4, day).unwrap()),
            comment: None,
        }
    }

    /// The frozen-series shape written on anchored exhaustion is exempt from
    /// the two terminal-scheduling rules.  The exemption is status-symmetric:
    /// a hand-abandoned series with preserved history is the same valid
    /// record, so `cancelled` is exempt on the same terms as `done`.
    #[test]
    fn test_frozen_series_exempt_from_terminal_rules() {
        let d = |y, m, day| NaiveDate::from_ymd_opt(y, m, day).unwrap();
        for status in [Status::Done, Status::Cancelled] {
            let label = status.to_string();
            let fm = Frontmatter {
                status: Some(status),
                check_in_date: Some(d(2026, 4, 10)),
                due_date: Some(d(2026, 4, 10)),
                on_date: Some(d(2026, 4, 10)),
                rrule: Some("FREQ=WEEKLY;COUNT=2".to_string()),
                completion_log: vec![log_entry(10)],
                ..bare_fm()
            };
            let errors = check_rules(Path::new("test.md"), &fm);
            assert!(
                errors.is_empty(),
                "frozen series ({label}) must be clean; got: {:?}",
                errors.iter().map(|e| &e.message).collect::<Vec<_>>()
            );
        }
    }

    /// A completion log without any recurrence exempts nothing: non-recurring
    /// completions append log entries too, so leftover `check_in_date` on a
    /// plain done item is still a violation.
    #[test]
    fn test_completion_log_alone_does_not_exempt() {
        let d = |y, m, day| NaiveDate::from_ymd_opt(y, m, day).unwrap();
        let fm = Frontmatter {
            status: Some(Status::Done),
            check_in_date: Some(d(2026, 4, 10)),
            on_date: Some(d(2026, 4, 10)),
            completion_log: vec![log_entry(10)],
            ..bare_fm()
        };
        let messages: Vec<String> = check_rules(Path::new("test.md"), &fm)
            .into_iter()
            .map(|e| e.message)
            .collect();
        assert_eq!(messages, vec![MSG_TERMINAL_CHECK_IN]);
    }

    /// A recurrence with an empty completion log is a series never completed
    /// through graf: both terminal rules still fire.
    #[test]
    fn test_empty_completion_log_still_flags_terminal_rules() {
        let d = |y, m, day| NaiveDate::from_ymd_opt(y, m, day).unwrap();
        let fm = Frontmatter {
            status: Some(Status::Done),
            check_in_date: Some(d(2026, 4, 10)),
            on_date: Some(d(2026, 4, 10)),
            rrule: Some("FREQ=WEEKLY".to_string()),
            ..bare_fm()
        };
        let messages: Vec<String> = check_rules(Path::new("test.md"), &fm)
            .into_iter()
            .map(|e| e.message)
            .collect();
        assert_eq!(messages, vec![MSG_TERMINAL_CHECK_IN, MSG_TERMINAL_RRULE,]);
    }

    /// A frozen series with no completion date still trips the date rule:
    /// graf's own exhaustion always writes `on_date`.
    #[test]
    fn test_frozen_series_still_needs_completion_date() {
        let d = |y, m, day| NaiveDate::from_ymd_opt(y, m, day).unwrap();
        let fm = Frontmatter {
            status: Some(Status::Done),
            check_in_date: Some(d(2026, 4, 10)),
            rrule: Some("FREQ=WEEKLY".to_string()),
            completion_log: vec![log_entry(10)],
            ..bare_fm()
        };
        let messages: Vec<String> = check_rules(Path::new("test.md"), &fm)
            .into_iter()
            .map(|e| e.message)
            .collect();
        assert_eq!(messages, vec![MSG_TERMINAL_NO_DATE]);
    }

    /// Canary: asserts the fixtures below collectively emit exactly the
    /// messages declared in `RULE_MESSAGES`, and pins the schema version
    /// those messages are persisted under.  Lint messages live in
    /// `docs.lint_errors` and are deduped against the current pass by exact
    /// text, so stale persisted sets must be rebuilt whenever the rules
    /// change.  Comparing against the declaration rather than a hand-copied
    /// list means a rule added to `RULE_MESSAGES` but tripped by no fixture
    /// also fails here, instead of passing unnoticed.
    #[test]
    fn test_check_rules_message_snapshot() {
        let path = Path::new("test.md");
        let d = |y, m, day| NaiveDate::from_ymd_opt(y, m, day).unwrap();

        let mut fixtures = Vec::new();

        // Date mutual-exclusivity rules.
        fixtures.push(Frontmatter {
            on_date: Some(d(2026, 4, 10)),
            start_date: Some(d(2026, 4, 1)),
            end_date: Some(d(2026, 4, 15)),
            ..bare_fm()
        });
        fixtures.push(Frontmatter {
            end_date: Some(d(2026, 4, 15)),
            ..bare_fm()
        });

        // Terminal-item rules.
        fixtures.push(Frontmatter {
            status: Some(Status::Done),
            check_in_date: Some(d(2026, 4, 10)),
            rrule: Some("FREQ=WEEKLY".to_string()),
            ..bare_fm()
        });

        // Active-task rule.
        fixtures.push(Frontmatter {
            status: Some(Status::Todo),
            ..bare_fm()
        });

        let mut messages: Vec<String> = fixtures
            .iter()
            .flat_map(|fm| check_rules(path, fm))
            .map(|e| e.message)
            .collect();
        messages.sort();
        messages.dedup();

        let mut expected: Vec<String> = RULE_MESSAGES.iter().map(|m| m.to_string()).collect();
        expected.sort();

        assert_eq!(
            messages, expected,
            "check_rules output no longer matches RULE_MESSAGES — bump SCHEMA_VERSION in db.rs, \
             and add a fixture here for any newly declared message"
        );
        assert_eq!(
            crate::db::SCHEMA_VERSION,
            "7",
            "SCHEMA_VERSION changed — update this canary alongside it"
        );
    }

    #[test]
    fn test_format_timestamp() {
        let dt = DateTime::parse_from_rfc3339("2025-05-20T20:35:36.732Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(format_timestamp(dt), "2025-05-20T20:35:36.732Z");
    }

    #[test]
    fn test_lint_valid_file() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("test.md");
        fs::write(
            &file,
            "---\ntldr: Valid document\ncreated: '2025-01-01T00:00:00.000Z'\nupdated: '2025-01-01T00:00:00.000Z'\n---\nBody.\n",
        )
        .unwrap();

        let mut result = LintResult {
            files_checked: 0,
            errors: Vec::new(),
            files_fixed: Vec::new(),
        };
        lint_md_file(&file, None, false, &mut result).unwrap();
        assert!(result.errors.is_empty());
    }

    #[test]
    fn test_lint_missing_frontmatter() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("test.md");
        fs::write(&file, "No frontmatter here.\n").unwrap();

        let mut result = LintResult {
            files_checked: 0,
            errors: Vec::new(),
            files_fixed: Vec::new(),
        };
        lint_md_file(&file, None, false, &mut result).unwrap();
        assert_eq!(result.errors.len(), 1);
        assert!(result.errors[0].message.contains("missing frontmatter"));
    }

    #[test]
    fn test_lint_unknown_field() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("test.md");
        fs::write(&file, "---\ntldr: test\nbogus: true\n---\nBody.\n").unwrap();

        let mut result = LintResult {
            files_checked: 0,
            errors: Vec::new(),
            files_fixed: Vec::new(),
        };
        lint_md_file(&file, None, false, &mut result).unwrap();
        assert_eq!(result.errors.len(), 1);
    }

    #[test]
    fn test_apply_fixes_updates_timestamp() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("test.md");
        let content = "---\ntldr: test\nupdated: '2020-01-01T00:00:00.000Z'\n---\nBody.\n";
        fs::write(&file, content).unwrap();

        let fixed = apply_fixes(content, &file, Some(&FileChange::Modified)).unwrap();
        assert_ne!(fixed, content);
        assert!(fixed.contains("updated: '20")); // has a timestamp
        assert!(!fixed.contains("created:")); // not added for modified files
    }

    #[test]
    fn test_apply_fixes_adds_created_for_new() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("test.md");
        let content = "---\ntldr: test\n---\nBody.\n";
        fs::write(&file, content).unwrap();

        let fixed = apply_fixes(content, &file, Some(&FileChange::New)).unwrap();
        assert!(fixed.contains("updated:"));
        assert!(fixed.contains("created:"));
    }

    #[test]
    fn test_apply_fixes_no_created_for_modified() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("test.md");
        let content = "---\ntldr: test\n---\nBody.\n";
        fs::write(&file, content).unwrap();

        let fixed = apply_fixes(content, &file, Some(&FileChange::Modified)).unwrap();
        assert!(fixed.contains("updated:"));
        assert!(!fixed.contains("created:"));
    }

    #[test]
    fn test_apply_fixes_preserves_body() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("test.md");
        let content = "---\ntldr: test\nlabels: [a, b]\n---\n\n# My Document\n\nBody text here.\n";
        fs::write(&file, content).unwrap();

        let fixed = apply_fixes(content, &file, Some(&FileChange::Modified)).unwrap();
        assert!(fixed.contains("# My Document"));
        assert!(fixed.contains("Body text here."));
        assert!(fixed.contains("labels: [a, b]"));
    }

    #[test]
    fn test_apply_fixes_overwrites_created_for_new() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("test.md");
        let content = "---\ntldr: test\ncreated: '2020-01-01T00:00:00.000Z'\n---\nBody.\n";
        fs::write(&file, content).unwrap();

        let fixed = apply_fixes(content, &file, Some(&FileChange::New)).unwrap();
        // Should overwrite — LLMs don't know the real time, mtime is the truth
        assert!(!fixed.contains("2020-01-01"));
        assert!(fixed.contains("created: '20"));
    }

    #[test]
    fn test_orphan_attachment_detected() {
        let orphan = PathBuf::from("tech/backup/orphan.csv");
        let md_file = PathBuf::from("tech/backup/strategy.md");
        let non_md: Vec<&PathBuf> = vec![&orphan];
        let md: Vec<&PathBuf> = vec![&md_file];

        let mut result = LintResult {
            files_checked: 0,
            errors: Vec::new(),
            files_fixed: Vec::new(),
        };
        check_orphan_attachments(&non_md, &md, &mut result);
        assert_eq!(result.errors.len(), 1);
        assert!(result.errors[0].message.contains("orphan attachment"));
    }

    #[test]
    fn test_valid_attachment_not_flagged() {
        let attachment = PathBuf::from("tech/backup/strategy.csv");
        let md_file = PathBuf::from("tech/backup/strategy.md");
        let non_md: Vec<&PathBuf> = vec![&attachment];
        let md: Vec<&PathBuf> = vec![&md_file];

        let mut result = LintResult {
            files_checked: 0,
            errors: Vec::new(),
            files_fixed: Vec::new(),
        };
        check_orphan_attachments(&non_md, &md, &mut result);
        assert!(result.errors.is_empty());
    }

    #[test]
    fn test_should_skip_claude_md() {
        assert!(should_skip_file(Path::new("CLAUDE.md")));
        assert!(should_skip_file(Path::new("tech/CLAUDE.md")));
        assert!(!should_skip_file(Path::new("tech/overview.md")));
    }

    #[test]
    fn test_done_with_check_in_date_is_error() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("test.md");
        fs::write(
            &file,
            "---\ntldr: test\nstatus: done\non_date: 2026-04-10\ncheck_in_date: 2026-05-01\n---\n",
        )
        .unwrap();

        let mut result = LintResult {
            files_checked: 0,
            errors: Vec::new(),
            files_fixed: Vec::new(),
        };
        lint_md_file(&file, None, false, &mut result).unwrap();
        assert_eq!(result.errors.len(), 1);
        assert!(
            result.errors[0]
                .message
                .contains("done/cancelled item has check_in_date set")
        );
    }

    #[test]
    fn test_done_with_tentative_date_is_tolerated() {
        // Phase 1 dropped the belt-and-suspenders rule that forbade
        // tentative_date on terminal items. The tool path still clears it.
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("test.md");
        fs::write(
            &file,
            "---\ntldr: test\nstatus: done\non_date: 2026-04-10\ntentative_date: 2026-05-01\n---\n",
        )
        .unwrap();

        let mut result = LintResult::default();
        lint_md_file(&file, None, false, &mut result).unwrap();
        assert!(result.errors.is_empty());
    }

    #[test]
    fn test_done_with_check_in_date_plus_tentative_is_one_error() {
        // check_in_date is still forbidden on terminal items; tentative_date
        // is no longer. Result: exactly one error.
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("test.md");
        fs::write(
            &file,
            "---\ntldr: test\nstatus: done\non_date: 2026-04-10\ncheck_in_date: 2026-05-01\ntentative_date: 2026-05-01\n---\n",
        )
        .unwrap();

        let mut result = LintResult {
            files_checked: 0,
            errors: Vec::new(),
            files_fixed: Vec::new(),
        };
        lint_md_file(&file, None, false, &mut result).unwrap();
        assert_eq!(result.errors.len(), 1);
        assert!(result.errors[0].message.contains("check_in_date"));
    }

    #[test]
    fn test_active_status_with_scheduling_dates_ok() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("test.md");
        fs::write(
            &file,
            "---\ntldr: test\nstatus: todo\ncheck_in_date: 2026-05-01\ntentative_date: 2026-05-01\n---\n",
        )
        .unwrap();

        let mut result = LintResult {
            files_checked: 0,
            errors: Vec::new(),
            files_fixed: Vec::new(),
        };
        lint_md_file(&file, None, false, &mut result).unwrap();
        assert!(result.errors.is_empty());
    }

    #[test]
    fn test_cancelled_with_check_in_date_is_error() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("test.md");
        fs::write(
            &file,
            "---\ntldr: test\nstatus: cancelled\non_date: 2026-04-10\ncheck_in_date: 2026-05-01\n---\n",
        )
        .unwrap();

        let mut result = LintResult {
            files_checked: 0,
            errors: Vec::new(),
            files_fixed: Vec::new(),
        };
        lint_md_file(&file, None, false, &mut result).unwrap();
        assert_eq!(result.errors.len(), 1);
        assert!(
            result.errors[0]
                .message
                .contains("done/cancelled item has check_in_date set")
        );
    }

    #[test]
    fn test_done_without_scheduling_dates_ok() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("test.md");
        fs::write(
            &file,
            "---\ntldr: test\nstatus: done\non_date: 2026-04-10\n---\n",
        )
        .unwrap();

        let mut result = LintResult {
            files_checked: 0,
            errors: Vec::new(),
            files_fixed: Vec::new(),
        };
        lint_md_file(&file, None, false, &mut result).unwrap();
        assert!(result.errors.is_empty());
    }

    #[test]
    fn test_lint_files_valid() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("a.md"), "---\ntldr: A\n---\n").unwrap();
        fs::write(dir.path().join("b.md"), "---\ntldr: B\n---\n").unwrap();

        let options = LintOptions {
            fix: false,
            repo_dir: dir.path().to_path_buf(),
        };
        let files = vec![PathBuf::from("a.md"), PathBuf::from("b.md")];
        let result = lint_files(&options, &files).unwrap();
        assert_eq!(result.files_checked, 2);
        assert!(result.errors.is_empty());
    }

    #[test]
    fn test_lint_files_nonexistent() {
        let dir = TempDir::new().unwrap();

        let options = LintOptions {
            fix: false,
            repo_dir: dir.path().to_path_buf(),
        };
        let files = vec![PathBuf::from("nope.md")];
        let result = lint_files(&options, &files).unwrap();
        assert_eq!(result.files_checked, 0);
        assert_eq!(result.errors.len(), 1);
        assert!(result.errors[0].message.contains("file not found"));
    }

    #[test]
    fn test_lint_files_non_md() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("data.csv"), "a,b,c\n").unwrap();

        let options = LintOptions {
            fix: false,
            repo_dir: dir.path().to_path_buf(),
        };
        let files = vec![PathBuf::from("data.csv")];
        let result = lint_files(&options, &files).unwrap();
        assert_eq!(result.files_checked, 0);
        assert_eq!(result.errors.len(), 1);
        assert!(result.errors[0].message.contains("not a markdown file"));
    }

    #[test]
    fn test_lint_files_with_error() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("bad.md"),
            "---\ntldr: test\nstatus: done\non_date: 2026-04-10\ncheck_in_date: 2026-05-01\n---\n",
        )
        .unwrap();

        let options = LintOptions {
            fix: false,
            repo_dir: dir.path().to_path_buf(),
        };
        let files = vec![PathBuf::from("bad.md")];
        let result = lint_files(&options, &files).unwrap();
        assert_eq!(result.files_checked, 1);
        assert_eq!(result.errors.len(), 1);
    }

    #[test]
    fn test_lint_files_skips_claude_md() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("CLAUDE.md"), "---\ntldr: skip me\n---\n").unwrap();

        let options = LintOptions {
            fix: false,
            repo_dir: dir.path().to_path_buf(),
        };
        let files = vec![PathBuf::from("CLAUDE.md")];
        let result = lint_files(&options, &files).unwrap();
        assert_eq!(result.files_checked, 0);
        assert!(result.errors.is_empty());
    }

    #[test]
    fn test_lint_files_fix_does_not_set_created() {
        let dir = TempDir::new().unwrap();
        let content = "---\ntldr: test\n---\nBody.\n";
        fs::write(dir.path().join("a.md"), content).unwrap();

        let options = LintOptions {
            fix: true,
            repo_dir: dir.path().to_path_buf(),
        };
        let files = vec![PathBuf::from("a.md")];
        let result = lint_files(&options, &files).unwrap();
        assert_eq!(result.files_checked, 1);
        assert_eq!(result.files_fixed.len(), 1);

        // Explicit paths use change=None, so `created` should NOT be set
        let fixed_content = fs::read_to_string(dir.path().join("a.md")).unwrap();
        assert!(fixed_content.contains("updated:"));
        assert!(!fixed_content.contains("created:"));
    }

    #[test]
    fn test_lint_files_mixed_valid_and_invalid() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("good.md"), "---\ntldr: Good\n---\n").unwrap();
        fs::write(dir.path().join("bad.md"), "No frontmatter here.\n").unwrap();

        let options = LintOptions {
            fix: false,
            repo_dir: dir.path().to_path_buf(),
        };
        let files = vec![
            PathBuf::from("good.md"),
            PathBuf::from("bad.md"),
            PathBuf::from("nope.md"),
        ];
        let result = lint_files(&options, &files).unwrap();
        // good.md + bad.md checked, nope.md errored before checking
        assert_eq!(result.files_checked, 2);
        // bad.md: missing frontmatter, nope.md: file not found
        assert_eq!(result.errors.len(), 2);
    }

    #[test]
    fn test_done_with_rrule_is_error() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("test.md");
        fs::write(
            &file,
            "---\ntldr: test\nstatus: done\non_date: 2026-04-10\nrrule: \"FREQ=MONTHLY\"\n---\n",
        )
        .unwrap();

        let mut result = LintResult {
            files_checked: 0,
            errors: Vec::new(),
            files_fixed: Vec::new(),
        };
        lint_md_file(&file, None, false, &mut result).unwrap();
        assert_eq!(result.errors.len(), 1);
        assert!(result.errors[0].message.contains("rrule"));
    }

    #[test]
    fn test_cancelled_with_rrule_is_error() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("test.md");
        fs::write(
            &file,
            "---\ntldr: test\nstatus: cancelled\non_date: 2026-04-10\nrrule: \"FREQ=WEEKLY\"\n---\n",
        )
        .unwrap();

        let mut result = LintResult {
            files_checked: 0,
            errors: Vec::new(),
            files_fixed: Vec::new(),
        };
        lint_md_file(&file, None, false, &mut result).unwrap();
        assert_eq!(result.errors.len(), 1);
        assert!(result.errors[0].message.contains("rrule"));
    }

    #[test]
    fn test_active_with_rrule_ok() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("test.md");
        fs::write(
            &file,
            "---\ntldr: test\nstatus: todo\ncheck_in_date: '2026-05-01'\nrrule: \"FREQ=MONTHLY\"\n---\n",
        )
        .unwrap();

        let mut result = LintResult {
            files_checked: 0,
            errors: Vec::new(),
            files_fixed: Vec::new(),
        };
        lint_md_file(&file, None, false, &mut result).unwrap();
        assert!(result.errors.is_empty());
    }

    #[test]
    fn test_active_with_recurrence_block_ok() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("test.md");
        fs::write(
            &file,
            "---\ntldr: test\nstatus: todo\ncheck_in_date: '2026-05-01'\nrecurrence:\n  rrule: \"FREQ=MONTHLY\"\n---\n",
        )
        .unwrap();

        let mut result = LintResult::default();
        lint_md_file(&file, None, false, &mut result).unwrap();
        assert!(result.errors.is_empty(), "got errors: {:?}", result.errors);
    }

    #[test]
    fn test_done_with_recurrence_block_is_error() {
        // The done-has-rrule rule reads `effective_rrule()`, so it fires
        // uniformly on both legacy flat rrule and nested recurrence shapes.
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("test.md");
        fs::write(
            &file,
            "---\ntldr: test\nstatus: done\non_date: '2026-04-10'\nrecurrence:\n  rrule: \"FREQ=MONTHLY\"\n---\n",
        )
        .unwrap();

        let mut result = LintResult::default();
        lint_md_file(&file, None, false, &mut result).unwrap();
        assert!(result.errors.iter().any(|e| e.message.contains("rrule")));
    }

    #[test]
    fn test_active_recurring_with_check_in_and_tentative_date_ok() {
        // Recurring + check_in_date + tentative_date
        // must lint clean. Phase 2 will tolerate tentative_date on terminal
        // recurring tasks; Phase 1 just needs to not flag the active combo.
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("test.md");
        fs::write(
            &file,
            "---\ntldr: test\nstatus: todo\ncheck_in_date: '2026-05-01'\ntentative_date: '2026-05-03'\nrecurrence:\n  rrule: \"FREQ=WEEKLY\"\n---\n",
        )
        .unwrap();

        let mut result = LintResult::default();
        lint_md_file(&file, None, false, &mut result).unwrap();
        assert!(result.errors.is_empty(), "got errors: {:?}", result.errors);
    }

    #[test]
    fn test_active_todo_missing_dates_is_error() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("test.md");
        fs::write(&file, "---\ntldr: test\nstatus: todo\npriority: 3\n---\n").unwrap();

        let mut result = LintResult::default();
        lint_md_file(&file, None, false, &mut result).unwrap();
        assert_eq!(result.errors.len(), 1);
        assert!(
            result.errors[0]
                .message
                .contains("active task missing check_in_date or due_date")
        );
    }

    #[test]
    fn test_active_in_progress_missing_dates_is_error() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("test.md");
        fs::write(
            &file,
            "---\ntldr: test\nstatus: in_progress\npriority: 3\n---\n",
        )
        .unwrap();

        let mut result = LintResult::default();
        lint_md_file(&file, None, false, &mut result).unwrap();
        assert_eq!(result.errors.len(), 1);
        assert!(
            result.errors[0]
                .message
                .contains("active task missing check_in_date or due_date")
        );
    }

    #[test]
    fn test_active_reminder_missing_dates_is_error() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("test.md");
        fs::write(
            &file,
            "---\ntldr: test\nstatus: reminder\npriority: 3\n---\n",
        )
        .unwrap();

        let mut result = LintResult::default();
        lint_md_file(&file, None, false, &mut result).unwrap();
        assert_eq!(result.errors.len(), 1);
        assert!(
            result.errors[0]
                .message
                .contains("active task missing check_in_date or due_date")
        );
    }

    #[test]
    fn test_active_with_check_in_date_only_ok() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("test.md");
        fs::write(
            &file,
            "---\ntldr: test\nstatus: todo\ncheck_in_date: '2026-05-01'\n---\n",
        )
        .unwrap();

        let mut result = LintResult::default();
        lint_md_file(&file, None, false, &mut result).unwrap();
        assert!(result.errors.is_empty());
    }

    #[test]
    fn test_active_with_due_date_only_ok() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("test.md");
        fs::write(
            &file,
            "---\ntldr: test\nstatus: todo\ndue_date: '2026-05-01'\n---\n",
        )
        .unwrap();

        let mut result = LintResult::default();
        lint_md_file(&file, None, false, &mut result).unwrap();
        assert!(result.errors.is_empty());
    }

    #[test]
    fn test_active_with_tentative_date_only_is_error() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("test.md");
        fs::write(
            &file,
            "---\ntldr: test\nstatus: todo\ntentative_date: '2026-05-01'\n---\n",
        )
        .unwrap();

        let mut result = LintResult::default();
        lint_md_file(&file, None, false, &mut result).unwrap();
        assert_eq!(result.errors.len(), 1);
        assert!(
            result.errors[0]
                .message
                .contains("active task missing check_in_date or due_date")
        );
    }

    #[test]
    fn test_no_status_missing_dates_ok() {
        // Knowledge-base entries (no status) are exempt from the date rule
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("test.md");
        fs::write(&file, "---\ntldr: A knowledge base note\n---\n").unwrap();

        let mut result = LintResult::default();
        lint_md_file(&file, None, false, &mut result).unwrap();
        assert!(result.errors.is_empty());
    }

    #[test]
    fn test_done_missing_scheduling_dates_ok() {
        // Done items don't need check_in_date/due_date
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("test.md");
        fs::write(
            &file,
            "---\ntldr: test\nstatus: done\non_date: '2026-04-10'\n---\n",
        )
        .unwrap();

        let mut result = LintResult::default();
        lint_md_file(&file, None, false, &mut result).unwrap();
        assert!(result.errors.is_empty());
    }

    #[test]
    fn test_done_with_end_date_ok() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("test.md");
        fs::write(
            &file,
            "---\ntldr: test\nstatus: done\nstart_date: '2026-04-01'\nend_date: '2026-04-10'\n---\n",
        )
        .unwrap();

        let mut result = LintResult::default();
        lint_md_file(&file, None, false, &mut result).unwrap();
        assert!(result.errors.is_empty());
    }

    #[test]
    fn test_done_missing_on_or_end_date_is_error() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("test.md");
        fs::write(&file, "---\ntldr: test\nstatus: done\n---\n").unwrap();

        let mut result = LintResult::default();
        lint_md_file(&file, None, false, &mut result).unwrap();
        assert!(
            result
                .errors
                .iter()
                .any(|e| e.message.contains("missing on_date or end_date")),
            "expected missing on_date or end_date error, got: {:?}",
            result.errors
        );
    }

    #[test]
    fn test_cancelled_missing_on_or_end_date_is_error() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("test.md");
        fs::write(&file, "---\ntldr: test\nstatus: cancelled\n---\n").unwrap();

        let mut result = LintResult::default();
        lint_md_file(&file, None, false, &mut result).unwrap();
        assert!(
            result
                .errors
                .iter()
                .any(|e| e.message.contains("missing on_date or end_date")),
            "expected missing on_date or end_date error, got: {:?}",
            result.errors
        );
    }

    #[test]
    fn test_on_date_and_start_date_invalid() {
        let path = Path::new("test.md");
        let fm = Frontmatter {
            on_date: Some(NaiveDate::from_ymd_opt(2026, 4, 10).unwrap()),
            start_date: Some(NaiveDate::from_ymd_opt(2026, 4, 1).unwrap()),
            ..bare_fm()
        };
        let errors = check_rules(path, &fm);
        assert!(errors.iter().any(|e| {
            e.message
                .contains("on_date and start_date cannot both be set")
        }));
    }

    #[test]
    fn test_on_date_and_end_date_invalid() {
        let path = Path::new("test.md");
        let fm = Frontmatter {
            on_date: Some(NaiveDate::from_ymd_opt(2026, 4, 10).unwrap()),
            end_date: Some(NaiveDate::from_ymd_opt(2026, 4, 15).unwrap()),
            ..bare_fm()
        };
        let errors = check_rules(path, &fm);
        assert!(errors.iter().any(|e| {
            e.message
                .contains("on_date and end_date cannot both be set")
        }));
    }

    #[test]
    fn test_end_date_without_start_date_invalid() {
        let path = Path::new("test.md");
        let fm = Frontmatter {
            end_date: Some(NaiveDate::from_ymd_opt(2026, 4, 15).unwrap()),
            ..bare_fm()
        };
        let errors = check_rules(path, &fm);
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("end_date requires start_date"))
        );
    }

    #[test]
    fn test_start_date_alone_valid() {
        let path = Path::new("test.md");
        let fm = Frontmatter {
            start_date: Some(NaiveDate::from_ymd_opt(2026, 4, 1).unwrap()),
            ..bare_fm()
        };
        let errors = check_rules(path, &fm);
        assert!(errors.is_empty());
    }

    #[test]
    fn test_start_date_plus_end_date_valid() {
        let path = Path::new("test.md");
        let fm = Frontmatter {
            start_date: Some(NaiveDate::from_ymd_opt(2026, 4, 1).unwrap()),
            end_date: Some(NaiveDate::from_ymd_opt(2026, 4, 10).unwrap()),
            ..bare_fm()
        };
        let errors = check_rules(path, &fm);
        assert!(errors.is_empty());
    }

    #[test]
    fn test_lint_result_to_json_format() {
        let result = LintResult {
            files_checked: 2,
            errors: vec![FileError::new(
                Path::new("todo/bad.md"),
                "missing frontmatter",
            )],
            files_fixed: vec![PathBuf::from("todo/fixed.md")],
        };
        let json = lint_result_to_json(&result);
        assert_eq!(json["files_checked"], 2);
        assert_eq!(json["files_fixed"][0], "todo/fixed.md");
        assert_eq!(json["errors"][0]["path"], "todo/bad.md");
        assert_eq!(json["errors"][0]["message"], "missing frontmatter");
    }
}
