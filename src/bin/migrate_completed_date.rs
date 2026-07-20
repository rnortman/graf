//! Migration: Remove `completed_date`, unify with `on_date`/`end_date`.
//!
//! See design-completed-date-removal.md for full details.
//!
//! Usage:
//!   cargo run --bin migrate_completed_date -- --repo-dir ~/grafdata/life
//!   cargo run --bin migrate_completed_date -- --repo-dir ~/grafdata/life --execute

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::NaiveDate;

use graf::frontmatter::{self, Field, FieldMutation};

// ---------------------------------------------------------------------------
// Data structures
// ---------------------------------------------------------------------------

/// What the migration will do to a single file.
enum MigrationAction {
    /// Set target field to completed_date value, remove completed_date.
    MigrateAndRemove {
        target_field: Field,
        value: NaiveDate,
    },
    /// Target field already has the same value; just remove completed_date.
    RemoveOnly,
}

struct FilePlan {
    rel_path: PathBuf,
    action: MigrationAction,
    /// Additional mutations needed (e.g., removing on_date when it conflicts
    /// with start_date under the new rules).
    extra_mutations: Vec<FieldMutation>,
}

// ---------------------------------------------------------------------------
// Frontmatter parsing that preserves completed_date
// ---------------------------------------------------------------------------

/// Temporary struct for migration: parses frontmatter including the
/// now-removed `completed_date` field.
///
/// We can't use the main `Frontmatter` struct because it no longer has
/// `completed_date`. Instead we parse the raw YAML to extract just what
/// we need.
struct MigrationFrontmatter {
    completed_date: Option<NaiveDate>,
    on_date: Option<NaiveDate>,
    start_date: Option<NaiveDate>,
    end_date: Option<NaiveDate>,
}

fn parse_migration_frontmatter(content: &str) -> Result<Option<MigrationFrontmatter>> {
    let range = frontmatter::frontmatter_yaml_range(content)?;
    let Some(range) = range else {
        return Ok(None);
    };

    let yaml_str = &content[range];
    let mapping: serde_yaml::Value =
        serde_yaml::from_str(yaml_str).context("failed to parse YAML")?;

    let map = mapping
        .as_mapping()
        .context("frontmatter is not a YAML mapping")?;

    fn get_date(map: &serde_yaml::Mapping, key: &str) -> Result<Option<NaiveDate>> {
        let Some(val) = map.get(serde_yaml::Value::String(key.to_string())) else {
            return Ok(None);
        };
        let s = val
            .as_str()
            .with_context(|| format!("{key} is not a string (might be a timestamp?)"))?;
        let d = NaiveDate::parse_from_str(s, "%Y-%m-%d")
            .with_context(|| format!("{key} is not a valid date: {s}"))?;
        Ok(Some(d))
    }

    Ok(Some(MigrationFrontmatter {
        completed_date: get_date(map, "completed_date")?,
        on_date: get_date(map, "on_date")?,
        start_date: get_date(map, "start_date")?,
        end_date: get_date(map, "end_date")?,
    }))
}

// ---------------------------------------------------------------------------
// Planning
// ---------------------------------------------------------------------------

fn plan_file(rel_path: &Path, content: &str) -> Result<Option<FilePlan>> {
    let mfm = parse_migration_frontmatter(content)
        .with_context(|| format!("parsing {}", rel_path.display()))?;
    let Some(mfm) = mfm else {
        return Ok(None);
    };
    let Some(completed_date) = mfm.completed_date else {
        return Ok(None);
    };

    let mut extra_mutations = Vec::new();

    // Decide target field based on start_date presence
    let target_field = if mfm.start_date.is_some() {
        Field::EndDate
    } else {
        Field::OnDate
    };

    let action = match (target_field, mfm.on_date, mfm.end_date) {
        (Field::OnDate, Some(existing), _) => {
            if existing == completed_date {
                // Same value — just remove completed_date
                MigrationAction::RemoveOnly
            } else {
                bail!(
                    "{}: on_date ({existing}) differs from completed_date ({completed_date}) — manual review required",
                    rel_path.display()
                );
            }
        }
        (Field::EndDate, on_date, Some(existing)) => {
            if existing == completed_date {
                // end_date already correct; if on_date matches too, remove it
                // (on_date + start_date is invalid under new rules)
                if let Some(od) = on_date {
                    if od == completed_date {
                        extra_mutations.push(FieldMutation::Remove {
                            field: Field::OnDate,
                        });
                    } else {
                        bail!(
                            "{}: has start_date + on_date ({od}) that differs from \
                             completed_date ({completed_date}) — manual review required",
                            rel_path.display()
                        );
                    }
                }
                MigrationAction::RemoveOnly
            } else {
                bail!(
                    "{}: end_date ({existing}) differs from completed_date ({completed_date}) — manual review required",
                    rel_path.display()
                );
            }
        }
        (Field::OnDate, None, _) => MigrationAction::MigrateAndRemove {
            target_field: Field::OnDate,
            value: completed_date,
        },
        (Field::EndDate, on_date, None) => {
            // Migrating completed_date -> end_date. If on_date equals
            // completed_date, also remove on_date (on_date + start_date
            // is invalid under new rules).
            if let Some(od) = on_date {
                if od == completed_date {
                    extra_mutations.push(FieldMutation::Remove {
                        field: Field::OnDate,
                    });
                } else {
                    bail!(
                        "{}: has start_date + on_date ({od}) that differs from \
                         completed_date ({completed_date}) — manual review required",
                        rel_path.display()
                    );
                }
            }
            MigrationAction::MigrateAndRemove {
                target_field: Field::EndDate,
                value: completed_date,
            }
        }
        _ => unreachable!("target_field is always OnDate or EndDate"),
    };

    Ok(Some(FilePlan {
        rel_path: rel_path.to_path_buf(),
        action,
        extra_mutations,
    }))
}

// ---------------------------------------------------------------------------
// Execution
// ---------------------------------------------------------------------------

/// Remove a YAML field by key from raw frontmatter content.
///
/// Like `frontmatter::remove_yaml_field` but works with a raw key string
/// instead of requiring a `Field` enum variant. Needed because `completed_date`
/// is no longer in the `Field` enum.
fn remove_raw_yaml_field(content: &str, key: &str) -> Result<String> {
    let range = frontmatter::frontmatter_yaml_range(content)?.context("file has no frontmatter")?;
    let yaml = &content[range.clone()];
    let prefix = format!("{key}:");
    let lines: Vec<&str> = yaml
        .lines()
        .filter(|line| !line.starts_with(&prefix))
        .collect();
    let new_yaml = lines.join("\n");
    let mut result = String::with_capacity(content.len());
    result.push_str(&content[..range.start]);
    result.push_str(&new_yaml);
    result.push_str(&content[range.end..]);
    Ok(result)
}

fn execute_plan(repo_dir: &Path, plan: &FilePlan) -> Result<()> {
    let abs_path = repo_dir.join(&plan.rel_path);
    let content = std::fs::read_to_string(&abs_path)
        .with_context(|| format!("reading {}", plan.rel_path.display()))?;

    let mut mutations: Vec<FieldMutation> = Vec::new();

    match &plan.action {
        MigrationAction::MigrateAndRemove {
            target_field,
            value,
        } => {
            mutations.push(FieldMutation::Set {
                field: *target_field,
                value: format!("'{value}'"),
            });
        }
        MigrationAction::RemoveOnly => {}
    }

    // Apply any extra mutations (e.g., removing on_date)
    for m in &plan.extra_mutations {
        mutations.push(m.clone());
    }

    // Apply typed mutations first, then remove completed_date via raw key
    // (completed_date is no longer in the Field enum)
    let new_content = frontmatter::rewrite_frontmatter(&content, &mutations)?;
    let new_content = remove_raw_yaml_field(&new_content, "completed_date")?;

    std::fs::write(&abs_path, &new_content)
        .with_context(|| format!("writing {}", plan.rel_path.display()))?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();

    let mut repo_dir: Option<PathBuf> = None;
    let mut execute = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--repo-dir" => {
                i += 1;
                repo_dir = Some(PathBuf::from(
                    args.get(i).context("--repo-dir requires a value")?,
                ));
            }
            "--execute" => {
                execute = true;
            }
            other => bail!("unknown argument: {other}"),
        }
        i += 1;
    }

    let repo_dir = repo_dir.context("--repo-dir is required")?;
    if !repo_dir.is_dir() {
        bail!("{} is not a directory", repo_dir.display());
    }

    println!("Scanning {}...", repo_dir.display());

    // Phase 1: Discover and plan
    let all_files = graf::collect_repo_files(&repo_dir)?;
    let md_files: Vec<&PathBuf> = all_files.iter().filter(|p| graf::is_md(p)).collect();
    println!("Found {} markdown files", md_files.len());

    let mut plans = Vec::new();
    let mut errors = Vec::new();

    for rel_path in &md_files {
        let abs = repo_dir.join(rel_path);
        let content = match std::fs::read_to_string(&abs) {
            Ok(c) => c,
            Err(e) => {
                errors.push(format!("{}: read error: {e}", rel_path.display()));
                continue;
            }
        };

        match plan_file(rel_path, &content) {
            Ok(Some(plan)) => plans.push(plan),
            Ok(None) => {} // No completed_date, skip
            Err(e) => errors.push(format!("{e:#}")),
        }
    }

    // Report
    let mut migrate_on = 0usize;
    let mut migrate_end = 0usize;
    let mut remove_only = 0usize;
    let mut extra_on_date_removals = 0usize;

    for plan in &plans {
        match &plan.action {
            MigrationAction::MigrateAndRemove {
                target_field: Field::OnDate,
                ..
            } => migrate_on += 1,
            MigrationAction::MigrateAndRemove {
                target_field: Field::EndDate,
                ..
            } => migrate_end += 1,
            MigrationAction::RemoveOnly => remove_only += 1,
            _ => {}
        }
        if !plan.extra_mutations.is_empty() {
            extra_on_date_removals += plan.extra_mutations.len();
        }
    }

    println!("\nPlan summary:");
    println!("  Files with completed_date: {}", plans.len());
    println!("  Migrate completed_date -> on_date: {migrate_on}");
    println!("  Migrate completed_date -> end_date: {migrate_end}");
    println!("  Remove completed_date (target already set): {remove_only}");
    if extra_on_date_removals > 0 {
        println!("  Extra on_date removals (start+on conflict): {extra_on_date_removals}");
    }

    if !errors.is_empty() {
        println!("\nErrors ({}):", errors.len());
        for err in &errors {
            println!("  ERROR: {err}");
        }
        bail!("fix {} error(s) and re-run", errors.len());
    }

    if plans.is_empty() {
        println!("\nNo files need migration.");
        return Ok(());
    }

    // Phase 2: Execute (only with --execute)
    if !execute {
        println!("\nDry run — files that would be modified:");
        for plan in &plans {
            let desc = match &plan.action {
                MigrationAction::MigrateAndRemove {
                    target_field,
                    value,
                } => format!("completed_date -> {} ({})", target_field.yaml_key(), value),
                MigrationAction::RemoveOnly => {
                    "remove completed_date (target already set)".to_string()
                }
            };
            let extra = if plan.extra_mutations.is_empty() {
                String::new()
            } else {
                let descs: Vec<String> = plan
                    .extra_mutations
                    .iter()
                    .map(|m| match m {
                        FieldMutation::Remove { field } => {
                            format!("remove {}", field.yaml_key())
                        }
                        FieldMutation::Set { field, value } => {
                            format!("set {} = {}", field.yaml_key(), value)
                        }
                        FieldMutation::AppendCompletionLog { .. } => {
                            "append completion_log entry".to_string()
                        }
                    })
                    .collect();
                format!(" + {}", descs.join(", "))
            };
            println!("  {}: {desc}{extra}", plan.rel_path.display());
        }
        println!("\nPass --execute to apply changes.");
        return Ok(());
    }

    println!("\nExecuting migration...");
    for plan in &plans {
        execute_plan(&repo_dir, plan)
            .with_context(|| format!("migrating {}", plan.rel_path.display()))?;
    }
    println!("Migrated {} files.", plans.len());
    println!("\nReview with `git diff`, then run `graf reindex`.");

    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("Error: {e:#}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn make_file(yaml: &str) -> String {
        format!("---\n{yaml}\n---\n")
    }

    #[test]
    fn test_simple_migration_to_on_date() {
        let content = make_file("tldr: test\nstatus: done\ncompleted_date: '2026-04-10'");
        let plan = plan_file(Path::new("test.md"), &content).unwrap().unwrap();
        assert!(matches!(
            plan.action,
            MigrationAction::MigrateAndRemove {
                target_field: Field::OnDate,
                ..
            }
        ));
    }

    #[test]
    fn test_migration_with_start_date_goes_to_end_date() {
        let content = make_file(
            "tldr: test\nstatus: done\nstart_date: '2026-04-01'\ncompleted_date: '2026-04-10'",
        );
        let plan = plan_file(Path::new("test.md"), &content).unwrap().unwrap();
        assert!(matches!(
            plan.action,
            MigrationAction::MigrateAndRemove {
                target_field: Field::EndDate,
                ..
            }
        ));
    }

    #[test]
    fn test_on_date_already_matches_remove_only() {
        let content = make_file(
            "tldr: test\nstatus: done\non_date: '2026-04-10'\ncompleted_date: '2026-04-10'",
        );
        let plan = plan_file(Path::new("test.md"), &content).unwrap().unwrap();
        assert!(matches!(plan.action, MigrationAction::RemoveOnly));
    }

    #[test]
    fn test_on_date_differs_is_error() {
        let content = make_file(
            "tldr: test\nstatus: done\non_date: '2026-04-09'\ncompleted_date: '2026-04-10'",
        );
        let result = plan_file(Path::new("test.md"), &content);
        assert!(result.is_err());
        let msg = format!("{:#}", result.err().unwrap());
        assert!(msg.contains("manual review required"), "{msg}");
    }

    #[test]
    fn test_start_date_with_matching_on_date_removes_on_date() {
        let content = make_file(
            "tldr: test\nstatus: done\nstart_date: '2026-04-01'\non_date: '2026-04-10'\ncompleted_date: '2026-04-10'",
        );
        let plan = plan_file(Path::new("test.md"), &content).unwrap().unwrap();
        assert!(matches!(
            plan.action,
            MigrationAction::MigrateAndRemove {
                target_field: Field::EndDate,
                ..
            }
        ));
        assert_eq!(plan.extra_mutations.len(), 1);
        assert!(matches!(
            plan.extra_mutations[0],
            FieldMutation::Remove {
                field: Field::OnDate
            }
        ));
    }

    #[test]
    fn test_start_date_with_different_on_date_is_error() {
        let content = make_file(
            "tldr: test\nstatus: done\nstart_date: '2026-04-01'\non_date: '2026-04-05'\ncompleted_date: '2026-04-10'",
        );
        let result = plan_file(Path::new("test.md"), &content);
        assert!(result.is_err());
        let msg = format!("{:#}", result.err().unwrap());
        assert!(msg.contains("manual review required"), "{msg}");
    }

    #[test]
    fn test_end_date_already_matches_remove_only() {
        let content = make_file(
            "tldr: test\nstatus: done\nstart_date: '2026-04-01'\nend_date: '2026-04-10'\ncompleted_date: '2026-04-10'",
        );
        let plan = plan_file(Path::new("test.md"), &content).unwrap().unwrap();
        assert!(matches!(plan.action, MigrationAction::RemoveOnly));
    }

    #[test]
    fn test_end_date_differs_is_error() {
        let content = make_file(
            "tldr: test\nstatus: done\nstart_date: '2026-04-01'\nend_date: '2026-04-09'\ncompleted_date: '2026-04-10'",
        );
        let result = plan_file(Path::new("test.md"), &content);
        assert!(result.is_err());
        let msg = format!("{:#}", result.err().unwrap());
        assert!(msg.contains("manual review required"), "{msg}");
    }

    #[test]
    fn test_no_completed_date_returns_none() {
        let content = make_file("tldr: test\nstatus: todo");
        let plan = plan_file(Path::new("test.md"), &content).unwrap();
        assert!(plan.is_none());
    }

    #[test]
    fn test_no_frontmatter_returns_none() {
        let plan = plan_file(Path::new("test.md"), "Just text.").unwrap();
        assert!(plan.is_none());
    }

    #[test]
    fn test_execute_removes_completed_date() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        let content = make_file("tldr: test\nstatus: done\ncompleted_date: '2026-04-10'");
        fs::write(repo.join("test.md"), &content).unwrap();

        let plan = plan_file(Path::new("test.md"), &content).unwrap().unwrap();
        execute_plan(repo, &plan).unwrap();

        let result = fs::read_to_string(repo.join("test.md")).unwrap();
        assert!(!result.contains("completed_date"));
        assert!(result.contains("on_date: '2026-04-10'"));
    }

    #[test]
    fn test_execute_start_date_sets_end_date() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        let content = make_file(
            "tldr: test\nstatus: done\nstart_date: '2026-04-01'\ncompleted_date: '2026-04-10'",
        );
        fs::write(repo.join("test.md"), &content).unwrap();

        let plan = plan_file(Path::new("test.md"), &content).unwrap().unwrap();
        execute_plan(repo, &plan).unwrap();

        let result = fs::read_to_string(repo.join("test.md")).unwrap();
        assert!(!result.contains("completed_date"));
        assert!(result.contains("end_date: '2026-04-10'"));
        assert!(result.contains("start_date: '2026-04-01'"));
    }

    #[test]
    fn test_execute_removes_conflicting_on_date() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        let content = make_file(
            "tldr: test\nstatus: done\nstart_date: '2026-04-01'\non_date: '2026-04-10'\ncompleted_date: '2026-04-10'",
        );
        fs::write(repo.join("test.md"), &content).unwrap();

        let plan = plan_file(Path::new("test.md"), &content).unwrap().unwrap();
        execute_plan(repo, &plan).unwrap();

        let result = fs::read_to_string(repo.join("test.md")).unwrap();
        assert!(!result.contains("completed_date"));
        assert!(!result.contains("on_date"));
        assert!(result.contains("end_date: '2026-04-10'"));
    }

    #[test]
    fn test_timestamp_in_completed_date_is_error() {
        let content = make_file("tldr: test\nstatus: done\ncompleted_date: 2026-04-10T14:30:00Z");
        let result = plan_file(Path::new("test.md"), &content);
        assert!(result.is_err());
    }
}
