//! Legacy-shape frontmatter migration, run eagerly during `do_reindex`.
//!
//! Rewrites files that use the pre-Phase-1 recurring-task shape into the
//! structured `recurrence` block + frontmatter `completion_log` list:
//!
//! - Top-level `rrule: <value>` → `recurrence:\n  rrule: <value>` (top-level
//!   key removed).
//! - Body `## Completion Log` section with `### YYYY-MM-DD` headers → a
//!   `completion_log:` YAML list; free-text between headers becomes each
//!   entry's `comment`. The body section (including trailing blank line) is
//!   stripped.
//!
//! Malformed inputs (non-date header, both legacy YAML `completion_log` and
//! body entries, re-validate failure) are surfaced by returning `Err`. The
//! caller records a `FileError` and leaves the file untouched.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use chrono::NaiveDate;

use crate::frontmatter::{
    self, CompletionLogEntry, Field, FieldMutation, Frontmatter, frontmatter_yaml_range,
    yaml_scalar,
};

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Inspect a parsed frontmatter + the raw file content and, if the file is in
/// the legacy shape, rewrite it to the new shape on disk. Returns the
/// (re-parsed, re-validated) `Frontmatter` the caller should index.
///
/// Returns `Ok(None)` if no migration was needed (caller uses the already-
/// parsed `fm`). Returns `Err` if migration was needed but failed — caller
/// leaves the file untouched and records an error.
pub fn maybe_migrate_legacy(
    abs_path: &Path,
    rel_path: &Path,
    content: &str,
    fm: &Frontmatter,
) -> Result<Option<Frontmatter>> {
    let has_legacy_rrule = fm.rrule.is_some();
    let has_body_log_section = content.contains("\n## Completion Log\n")
        || content.contains("\n## Completion Log\r\n")
        || content.starts_with("## Completion Log\n");

    if !has_legacy_rrule && !has_body_log_section {
        return Ok(None);
    }

    // Refuse to migrate if both shapes are present for completion log — the
    // user has a hand-edited mess and automated merge would lose information.
    if has_body_log_section && !fm.completion_log.is_empty() {
        bail!(
            "{}: both frontmatter `completion_log` and body `## Completion Log` section present; refusing to auto-merge",
            rel_path.display()
        );
    }

    let (content_no_body, body_entries) = strip_completion_log_body(content)
        .with_context(|| format!("parsing body completion log in {}", rel_path.display()))?;

    let mut new_content = content_no_body;

    // Lift the flat rrule into the nested `recurrence` block by first removing
    // the flat key, then setting `recurrence:` as a block (if a nested block
    // already exists, we skip the lift — nested wins).
    if has_legacy_rrule && fm.recurrence.is_none() {
        let rrule_value = fm
            .rrule
            .as_ref()
            .expect("has_legacy_rrule implies rrule is Some");
        new_content = lift_rrule_to_recurrence(&new_content, rrule_value)
            .with_context(|| format!("lifting rrule in {}", rel_path.display()))?;
    } else if has_legacy_rrule {
        // Both present — nested wins, drop the legacy key.
        new_content = frontmatter::rewrite_frontmatter(
            &new_content,
            &[FieldMutation::Remove {
                field: Field::Rrule,
            }],
        )?;
    }

    if !body_entries.is_empty() {
        let mutations: Vec<FieldMutation> = body_entries
            .into_iter()
            .map(|entry| FieldMutation::AppendCompletionLog { entry })
            .collect();
        new_content = frontmatter::rewrite_frontmatter(&new_content, &mutations)
            .with_context(|| format!("appending completion log in {}", rel_path.display()))?;
    }

    // Re-parse and re-validate *in memory* before touching disk. If the
    // rewrite produced a broken file we must leave the original untouched
    // ("file untouched on failure" invariant).
    let new_fm = frontmatter::parse_frontmatter(&new_content)
        .with_context(|| format!("re-parsing migrated {}", rel_path.display()))?
        .with_context(|| {
            format!(
                "missing frontmatter after migration: {}",
                rel_path.display()
            )
        })?;
    new_fm
        .validate()
        .with_context(|| format!("validating migrated {}", rel_path.display()))?;

    // Only now persist.
    fs::write(abs_path, &new_content)
        .with_context(|| format!("writing migrated file {}", abs_path.display()))?;

    Ok(Some(new_fm))
}

/// Remove the `## Completion Log` body section and return the stripped
/// content plus any parsed entries.
///
/// The section is expected to run from the `## Completion Log` heading to
/// EOF. If *any* subsequent top-level `## ` heading appears after the log
/// heading, we bail: we can't safely distinguish "this is part of the log"
/// from "this is an unrelated section the user wants preserved," and
/// silently dropping unknown content to EOF would be data loss. Per Brenn's
/// robustness philosophy the file is left untouched and surfaced as a
/// `FileError`.
fn strip_completion_log_body(content: &str) -> Result<(String, Vec<CompletionLogEntry>)> {
    // Find a line exactly equal to "## Completion Log".
    let mut section_start: Option<usize> = None;
    let mut cursor = 0;
    for line in content.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed == "## Completion Log" {
            section_start = Some(cursor);
            break;
        }
        cursor += line.len();
    }
    let Some(start) = section_start else {
        return Ok((content.to_string(), Vec::new()));
    };

    let section_body = &content[start..];

    // Reject if any additional top-level `## ` heading appears after the
    // log heading — we'd be silently dropping that content otherwise.
    // Skip the `## Completion Log` heading line itself before scanning.
    let after_heading = match section_body.find('\n') {
        Some(n) => &section_body[n + 1..],
        None => "",
    };
    for raw_line in after_heading.lines() {
        let line = raw_line.trim_end_matches('\r');
        // `starts_with("## ")` excludes `### ` already (third char is `#`, not space).
        if line.starts_with("## ") {
            bail!(
                "unrecognized content after `## Completion Log` section: {line:?} — refusing to silently drop"
            );
        }
    }

    let entries = parse_completion_log_body(section_body)?;

    // Trim one trailing blank line preceding the section so we don't leave
    // behind "\n\n" gaps.
    let mut effective_start = start;
    let before = &content[..start];
    if before.ends_with("\n\n") {
        effective_start -= 1;
    }

    let mut stripped = String::with_capacity(start);
    stripped.push_str(&content[..effective_start]);
    // Drop everything from the section to EOF.
    Ok((stripped, entries))
}

/// Parse a `## Completion Log` body section. Each `### YYYY-MM-DD` header
/// becomes an entry; free text between headers becomes `comment`.
fn parse_completion_log_body(section: &str) -> Result<Vec<CompletionLogEntry>> {
    // Drop the heading line itself.
    let after_heading = match section.find('\n') {
        Some(n) => &section[n + 1..],
        None => "",
    };

    let mut entries: Vec<CompletionLogEntry> = Vec::new();
    let mut current: Option<(NaiveDate, String)> = None;

    for raw_line in after_heading.lines() {
        let line = raw_line.trim_end_matches('\r');
        if let Some(rest) = line.strip_prefix("### ") {
            if let Some((date, comment)) = current.take() {
                entries.push(build_entry(date, comment));
            }
            let date = NaiveDate::parse_from_str(rest.trim(), "%Y-%m-%d")
                .with_context(|| format!("malformed completion-log header: {line:?}"))?;
            current = Some((date, String::new()));
        } else {
            match &mut current {
                Some((_, comment)) => {
                    if !comment.is_empty() {
                        comment.push('\n');
                    }
                    comment.push_str(line);
                }
                None => {
                    // Content before first `### YYYY-MM-DD` header — tolerate
                    // if it's purely whitespace, reject otherwise.
                    if !line.trim().is_empty() {
                        bail!(
                            "completion-log content before first `### YYYY-MM-DD` header: {line:?}"
                        );
                    }
                }
            }
        }
    }

    if let Some((date, comment)) = current.take() {
        entries.push(build_entry(date, comment));
    }

    Ok(entries)
}

fn build_entry(date: NaiveDate, raw_comment: String) -> CompletionLogEntry {
    let trimmed = raw_comment.trim().to_string();
    CompletionLogEntry {
        completed: date,
        occurrence: None,
        comment: if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        },
    }
}

/// Replace the top-level `rrule: <value>` line with a nested `recurrence`
/// block containing the same rrule. If no rrule line is found, returns the
/// content unchanged.
fn lift_rrule_to_recurrence(content: &str, rrule_value: &str) -> Result<String> {
    let range =
        frontmatter_yaml_range(content)?.context("cannot lift rrule: file has no frontmatter")?;
    let mut lines: Vec<String> = content[range.clone()]
        .lines()
        .map(|l| l.to_string())
        .collect();

    // Find the flat rrule line. We rewrite it in place with the nested block
    // so the new recurrence lands roughly where the old key was.
    let rrule_idx = lines
        .iter()
        .position(|l| l.starts_with("rrule:"))
        .context("lift_rrule_to_recurrence called but no top-level rrule line found")?;

    let nested = format!("recurrence:\n  rrule: {}", yaml_scalar(rrule_value));
    lines[rrule_idx] = nested;

    let new_yaml = lines.join("\n");
    let mut result = String::with_capacity(content.len() + 16);
    result.push_str(&content[..range.start]);
    result.push_str(&new_yaml);
    result.push_str(&content[range.end..]);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_no_legacy_shape() {
        let content = "---\ntldr: test\n---\n";
        let fm = frontmatter::parse_frontmatter(content).unwrap().unwrap();
        // Use a temp file since maybe_migrate_legacy writes.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        fs::write(tmp.path(), content).unwrap();
        let result = maybe_migrate_legacy(tmp.path(), Path::new("t.md"), content, &fm).unwrap();
        assert!(result.is_none());
        // File unchanged.
        assert_eq!(fs::read_to_string(tmp.path()).unwrap(), content);
    }

    #[test]
    fn migrates_flat_rrule_only() {
        let content = r#"---
tldr: test
status: todo
check_in_date: 2026-04-18
rrule: "FREQ=WEEKLY;BYDAY=SA"
---

Body text.
"#;
        let fm = frontmatter::parse_frontmatter(content).unwrap().unwrap();
        let tmp = tempfile::NamedTempFile::new().unwrap();
        fs::write(tmp.path(), content).unwrap();
        let new_fm = maybe_migrate_legacy(tmp.path(), Path::new("t.md"), content, &fm)
            .unwrap()
            .expect("should have migrated");
        assert!(new_fm.rrule.is_none());
        assert!(new_fm.recurrence.is_some());
        assert_eq!(
            new_fm.recurrence.as_ref().unwrap().rrule,
            "FREQ=WEEKLY;BYDAY=SA"
        );
        // On-disk check.
        let new_content = fs::read_to_string(tmp.path()).unwrap();
        assert!(new_content.contains("recurrence:"));
        assert!(new_content.contains("FREQ=WEEKLY;BYDAY=SA"));
        assert!(!new_content.contains("\nrrule:"));
        assert!(new_content.contains("Body text."));
    }

    #[test]
    fn migrates_body_completion_log_with_comments() {
        let content = r#"---
tldr: test
status: todo
check_in_date: 2026-04-18
rrule: "FREQ=WEEKLY"
---

Body.

## Completion Log

### 2026-03-01
did it early

### 2026-04-01
"#;
        let fm = frontmatter::parse_frontmatter(content).unwrap().unwrap();
        let tmp = tempfile::NamedTempFile::new().unwrap();
        fs::write(tmp.path(), content).unwrap();
        let new_fm = maybe_migrate_legacy(tmp.path(), Path::new("t.md"), content, &fm)
            .unwrap()
            .expect("migrated");
        assert_eq!(new_fm.completion_log.len(), 2);
        assert_eq!(
            new_fm.completion_log[0].completed,
            NaiveDate::from_ymd_opt(2026, 3, 1).unwrap()
        );
        assert_eq!(
            new_fm.completion_log[0].comment.as_deref(),
            Some("did it early")
        );
        assert_eq!(
            new_fm.completion_log[1].completed,
            NaiveDate::from_ymd_opt(2026, 4, 1).unwrap()
        );
        assert!(new_fm.completion_log[1].comment.is_none());

        let new_content = fs::read_to_string(tmp.path()).unwrap();
        assert!(!new_content.contains("## Completion Log"));
        assert!(new_content.contains("Body."));
    }

    #[test]
    fn migrates_flat_rrule_no_body() {
        let content = r#"---
tldr: test
status: todo
check_in_date: 2026-04-18
rrule: "FREQ=MONTHLY"
---
"#;
        let fm = frontmatter::parse_frontmatter(content).unwrap().unwrap();
        let tmp = tempfile::NamedTempFile::new().unwrap();
        fs::write(tmp.path(), content).unwrap();
        let new_fm = maybe_migrate_legacy(tmp.path(), Path::new("t.md"), content, &fm)
            .unwrap()
            .expect("migrated");
        assert!(new_fm.rrule.is_none());
        assert!(new_fm.completion_log.is_empty());
    }

    #[test]
    fn noop_on_preexisting_new_shape() {
        let content = r#"---
tldr: test
status: todo
check_in_date: 2026-04-18
recurrence:
  rrule: "FREQ=MONTHLY"
completion_log:
  - completed: 2026-04-01
---
"#;
        let fm = frontmatter::parse_frontmatter(content).unwrap().unwrap();
        let tmp = tempfile::NamedTempFile::new().unwrap();
        fs::write(tmp.path(), content).unwrap();
        let result = maybe_migrate_legacy(tmp.path(), Path::new("t.md"), content, &fm).unwrap();
        assert!(result.is_none());
        assert_eq!(fs::read_to_string(tmp.path()).unwrap(), content);
    }

    #[test]
    fn rejects_malformed_header() {
        let content = r#"---
tldr: test
status: todo
check_in_date: 2026-04-18
---

## Completion Log

### not-a-date
"#;
        let fm = frontmatter::parse_frontmatter(content).unwrap().unwrap();
        let tmp = tempfile::NamedTempFile::new().unwrap();
        fs::write(tmp.path(), content).unwrap();
        let result = maybe_migrate_legacy(tmp.path(), Path::new("t.md"), content, &fm);
        assert!(result.is_err());
        // File unchanged on failure.
        assert_eq!(fs::read_to_string(tmp.path()).unwrap(), content);
    }

    #[test]
    fn rejects_both_shapes_for_completion_log() {
        let content = r#"---
tldr: test
status: todo
check_in_date: 2026-04-18
completion_log:
  - completed: 2026-04-01
---

## Completion Log

### 2026-03-01
"#;
        let fm = frontmatter::parse_frontmatter(content).unwrap().unwrap();
        let tmp = tempfile::NamedTempFile::new().unwrap();
        fs::write(tmp.path(), content).unwrap();
        let result = maybe_migrate_legacy(tmp.path(), Path::new("t.md"), content, &fm);
        assert!(result.is_err());
        assert_eq!(fs::read_to_string(tmp.path()).unwrap(), content);
    }

    #[test]
    fn rejects_unknown_section_after_completion_log() {
        // `## Notes` following the completion-log section would be silently
        // dropped if we naively truncate to EOF. Verify we bail and leave
        // the file untouched.
        let content = r#"---
tldr: test
status: todo
check_in_date: 2026-04-18
---

## Completion Log

### 2026-03-01

## Notes

follow-up text
"#;
        let fm = frontmatter::parse_frontmatter(content).unwrap().unwrap();
        let tmp = tempfile::NamedTempFile::new().unwrap();
        fs::write(tmp.path(), content).unwrap();
        let result = maybe_migrate_legacy(tmp.path(), Path::new("t.md"), content, &fm);
        assert!(
            result.is_err(),
            "expected bail on unknown section after completion log"
        );
        // File must be untouched.
        assert_eq!(fs::read_to_string(tmp.path()).unwrap(), content);
    }

    #[test]
    fn both_rrule_shapes_drops_legacy() {
        // Legacy flat rrule coexists with nested — nested wins, flat is dropped.
        let content = r#"---
tldr: test
status: todo
check_in_date: 2026-04-18
rrule: "FREQ=WEEKLY"
recurrence:
  rrule: "FREQ=MONTHLY"
---
"#;
        let fm = frontmatter::parse_frontmatter(content).unwrap().unwrap();
        let tmp = tempfile::NamedTempFile::new().unwrap();
        fs::write(tmp.path(), content).unwrap();
        let new_fm = maybe_migrate_legacy(tmp.path(), Path::new("t.md"), content, &fm)
            .unwrap()
            .expect("migrated");
        assert!(new_fm.rrule.is_none());
        assert_eq!(new_fm.recurrence.as_ref().unwrap().rrule, "FREQ=MONTHLY");
    }
}
