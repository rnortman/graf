//! Migration: Convert controller+instance recurrence model to single-document model.
//!
//! See design-recurrence.md for full details.
//!
//! Usage:
//!   cargo run --bin migrate_recurrence -- --repo-dir ~/grafdata/life
//!   cargo run --bin migrate_recurrence -- --repo-dir ~/grafdata/life --execute

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::NaiveDate;

use graf::frontmatter::{self, Field, FieldMutation, Frontmatter};

// ---------------------------------------------------------------------------
// Data structures
// ---------------------------------------------------------------------------

/// A parsed controller file.
struct Controller {
    rel_path: PathBuf,
    content: String,
    fm: Frontmatter,
}

/// A parsed instance file (active or done).
struct Instance {
    rel_path: PathBuf,
    fm: Frontmatter,
    /// Completion date extracted from done instances.
    completion_date: Option<NaiveDate>,
}

/// Validated migration plan for one controller.
struct ControllerPlan {
    controller: Controller,
    active_instance: Option<Instance>,
    done_instances: Vec<Instance>,
}

// ---------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------

/// Find all controller files under `todo/controllers/`.
fn discover_controllers(repo_dir: &Path) -> Result<Vec<Controller>> {
    let controllers_dir = repo_dir.join("todo/controllers");
    if !controllers_dir.is_dir() {
        bail!(
            "no todo/controllers/ directory found in {}",
            repo_dir.display()
        );
    }

    let mut controllers = Vec::new();
    for entry in std::fs::read_dir(&controllers_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "md") {
            let rel_path = path.strip_prefix(repo_dir)?.to_path_buf();
            let content = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", rel_path.display()))?;
            let fm = frontmatter::parse_frontmatter(&content)
                .with_context(|| format!("parsing {}", rel_path.display()))?
                .with_context(|| format!("no frontmatter in {}", rel_path.display()))?;
            controllers.push(Controller {
                rel_path,
                content,
                fm,
            });
        }
    }
    controllers.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    Ok(controllers)
}

/// Map from controller path to its instances.
type InstanceMap = HashMap<PathBuf, Vec<Instance>>;

/// Find all instance files (in `todo/` and `todo/done/`) whose `context_link`
/// points to a controller.  Also returns paths of files that failed to parse
/// so the caller can report them.
fn discover_instances(
    repo_dir: &Path,
    controller_paths: &[PathBuf],
) -> Result<(InstanceMap, Vec<PathBuf>)> {
    let controller_set: std::collections::HashSet<&PathBuf> = controller_paths.iter().collect();

    // Map: controller_rel_path -> Vec<Instance>
    let mut instances: HashMap<PathBuf, Vec<Instance>> = HashMap::new();
    let mut parse_failures: Vec<PathBuf> = Vec::new();

    // Scan todo/ (active) and todo/done/ (completed)
    for subdir in &["todo", "todo/done"] {
        let dir = repo_dir.join(subdir);
        if !dir.is_dir() {
            continue;
        }
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_file() || path.extension().is_none_or(|e| e != "md") {
                continue;
            }
            let rel_path = path.strip_prefix(repo_dir)?.to_path_buf();

            // Skip controllers themselves
            if rel_path.starts_with("todo/controllers/") {
                continue;
            }

            let fm = match frontmatter::parse_file(&path) {
                Ok(Some(fm)) => fm,
                Ok(None) => continue,
                Err(_) => {
                    parse_failures.push(rel_path);
                    continue;
                }
            };

            let Some(ref ctx_link) = fm.context_link else {
                continue;
            };

            // Normalize: context_link is like "life/todo/controllers/foo.md"
            // but our controller rel_paths are "todo/controllers/foo.md".
            // Try both the raw value and stripping the first component.
            let ctx_path = PathBuf::from(ctx_link);
            let matches_controller = if controller_set.contains(&ctx_path) {
                Some(ctx_path.clone())
            } else {
                // Strip leading component (e.g., "life/" prefix)
                let components: Vec<_> = ctx_path.components().collect();
                if components.len() > 1 {
                    let stripped: PathBuf = components[1..].iter().collect();
                    if controller_set.contains(&stripped) {
                        Some(stripped)
                    } else {
                        None
                    }
                } else {
                    None
                }
            };

            let Some(controller_path) = matches_controller else {
                continue;
            };

            let is_done = *subdir == "todo/done";
            let completion_date = if is_done {
                // Prefer on_date, then end_date, then try to parse updated
                fm.on_date.or(fm.end_date).or_else(|| {
                    fm.updated.as_ref().and_then(|u| {
                        // Try to extract date from ISO timestamp
                        u.get(..10)
                            .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
                    })
                })
            } else {
                None
            };

            instances
                .entry(controller_path)
                .or_default()
                .push(Instance {
                    rel_path,
                    fm,
                    completion_date,
                });
        }
    }

    parse_failures.sort();
    Ok((instances, parse_failures))
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

struct ValidationResult {
    plans: Vec<ControllerPlan>,
    errors: Vec<String>,
}

fn validate(
    controllers: Vec<Controller>,
    mut instances: HashMap<PathBuf, Vec<Instance>>,
) -> ValidationResult {
    let mut plans = Vec::new();
    let mut errors = Vec::new();

    for controller in controllers {
        let ctrl_path = controller.rel_path.display().to_string();

        // Check: controller must have rrule in frontmatter
        if controller.fm.rrule.is_none() {
            errors.push(format!(
                "{ctrl_path}: missing `rrule` in frontmatter. Add the rrule field to the YAML frontmatter."
            ));
            continue;
        }

        // Check: must have recurrence_controller label (or the typo variant)
        let has_controller_label = controller
            .fm
            .labels
            .iter()
            .any(|l| l == "recurrence_controller" || l == "recurring_controller");
        if !has_controller_label {
            errors.push(format!(
                "{ctrl_path}: missing `recurrence_controller` label."
            ));
            // Not fatal — we can still process it, but warn
        }

        let mut ctrl_instances = instances.remove(&controller.rel_path).unwrap_or_default();

        // Partition into active vs done
        let mut active: Vec<Instance> = Vec::new();
        let mut done: Vec<Instance> = Vec::new();

        for inst in ctrl_instances.drain(..) {
            let is_terminal = inst.fm.status.as_ref().is_some_and(|s| s.is_terminal());
            if is_terminal {
                done.push(inst);
            } else {
                active.push(inst);
            }
        }

        // Check: at most one active instance
        if active.len() > 1 {
            let paths: Vec<String> = active
                .iter()
                .map(|i| i.rel_path.display().to_string())
                .collect();
            errors.push(format!(
                "{ctrl_path}: has {} active instances (expected at most 1): {}",
                active.len(),
                paths.join(", ")
            ));
            continue;
        }

        plans.push(ControllerPlan {
            controller,
            active_instance: active.into_iter().next(),
            done_instances: done,
        });
    }

    // Check for orphan instances (context_link to a controller that doesn't exist)
    for (ctrl_path, orphans) in &instances {
        for orphan in orphans {
            errors.push(format!(
                "{}: context_link points to {} which was not found as a controller",
                orphan.rel_path.display(),
                ctrl_path.display()
            ));
        }
    }

    ValidationResult { plans, errors }
}

// ---------------------------------------------------------------------------
// Execution
// ---------------------------------------------------------------------------

struct MigrationStats {
    controllers_processed: usize,
    instances_merged: usize,
    files_deleted: usize,
}

/// Remove a block-style YAML field from frontmatter content.
///
/// `upsert_yaml_field` and `remove_yaml_field` only handle single-line fields.
/// Block-style sequences like `labels:\n- foo\n- bar` leave the continuation
/// lines behind.  This function removes the key line AND all following
/// continuation lines (starting with `- ` or indented).
fn remove_block_field(content: &str, key: &str) -> Result<String> {
    let range = frontmatter::frontmatter_yaml_range(content)?.context("file has no frontmatter")?;
    let yaml = &content[range.clone()];
    let mut lines: Vec<&str> = yaml.lines().collect();

    let prefix = format!("{key}:");
    if let Some(idx) = lines.iter().position(|l| l.starts_with(&prefix)) {
        // Check if this is block-style (value after "key:" is empty or whitespace-only)
        let value_part = lines[idx][prefix.len()..].trim();
        if value_part.is_empty() {
            // Block-style: remove key line + continuation lines
            lines.remove(idx);
            while idx < lines.len()
                && (lines[idx].starts_with("- ") || lines[idx].starts_with("  "))
            {
                lines.remove(idx);
            }
        } else {
            // Inline-style: just remove the line
            lines.remove(idx);
        }
    }

    let new_yaml = lines.join("\n");
    let mut result = String::with_capacity(content.len());
    result.push_str(&content[..range.start]);
    result.push_str(&new_yaml);
    result.push_str(&content[range.end..]);
    Ok(result)
}

fn execute_plan(repo_dir: &Path, plan: &ControllerPlan, dry_run: bool) -> Result<(usize, usize)> {
    let ctrl = &plan.controller;
    let ctrl_path_str = ctrl.rel_path.display().to_string();

    // Build the new file content from the controller.
    // Pre-process: remove block-style fields that we'll replace with flow-style.
    let mut content = remove_block_field(&ctrl.content, "labels")?;
    content = remove_block_field(&content, "blocked_by")?;

    // --- Frontmatter mutations ---
    let mut mutations: Vec<FieldMutation> = Vec::new();

    // Strip "Recurrence: " prefix from tldr
    let new_tldr = ctrl
        .fm
        .tldr
        .strip_prefix("Recurrence: ")
        .unwrap_or(&ctrl.fm.tldr);
    mutations.push(FieldMutation::Set {
        field: Field::Tldr,
        value: format!("'{}'", new_tldr.replace('\'', "''")),
    });

    // Set status: todo
    mutations.push(FieldMutation::Set {
        field: Field::Status,
        value: graf::frontmatter::Status::Todo.to_string(),
    });

    // Update labels: remove recurrence_controller/recurring_controller, add recurring
    let new_labels: Vec<String> = ctrl
        .fm
        .labels
        .iter()
        .filter(|l| *l != "recurrence_controller" && *l != "recurring_controller")
        .cloned()
        .chain(std::iter::once("recurring".to_string()))
        .collect();
    let labels_yaml: Vec<String> = new_labels.iter().map(|l| format!("\"{l}\"")).collect();
    mutations.push(FieldMutation::Set {
        field: Field::Labels,
        value: format!("[{}]", labels_yaml.join(", ")),
    });

    // Get scheduling info from active instance
    if let Some(ref active) = plan.active_instance {
        // tentative_date: prefer active instance's tentative_date, then check_in_date, then due_date
        let sched_date = active
            .fm
            .tentative_date
            .or(active.fm.check_in_date)
            .or(active.fm.due_date);
        if let Some(date) = sched_date {
            mutations.push(FieldMutation::Set {
                field: Field::TentativeDate,
                value: format!("'{date}'"),
            });
        }

        // sort_order from active instance, or default to priority
        if let Some(so) = active.fm.sort_order {
            mutations.push(FieldMutation::Set {
                field: Field::SortOrder,
                value: if so.fract() == 0.0 {
                    format!("{}", so as i64)
                } else {
                    format!("{so}")
                },
            });
        } else if let Some(p) = active.fm.priority.or(ctrl.fm.priority) {
            mutations.push(FieldMutation::Set {
                field: Field::SortOrder,
                value: format!("{p}"),
            });
        }

        // Carry over priority from active instance if controller doesn't have one
        if ctrl.fm.priority.is_none()
            && let Some(p) = active.fm.priority
        {
            mutations.push(FieldMutation::Set {
                field: Field::Priority,
                value: format!("{p}"),
            });
        }

        // Carry over effort from active instance if controller doesn't have one
        if ctrl.fm.effort.is_none()
            && let Some(ref e) = active.fm.effort
        {
            mutations.push(FieldMutation::Set {
                field: Field::Effort,
                value: e.to_string(),
            });
        }
    }

    // Remove check_in_date and due_date (controller shouldn't have these, but clean up)
    mutations.push(FieldMutation::Remove {
        field: Field::CheckInDate,
    });
    mutations.push(FieldMutation::Remove {
        field: Field::DueDate,
    });
    mutations.push(FieldMutation::Remove {
        field: Field::OnDate,
    });

    // Apply frontmatter mutations
    content = frontmatter::rewrite_frontmatter(&content, &mutations)?;

    // --- Completion log ---
    let mut completion_dates: Vec<(NaiveDate, String)> = Vec::new();
    for inst in &plan.done_instances {
        if let Some(date) = inst.completion_date {
            // Include summary from done instance if it has interesting notes
            completion_dates.push((date, String::new()));
        }
    }
    completion_dates.sort_by_key(|(d, _)| *d);

    if !completion_dates.is_empty() {
        // Append completion log section
        if !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str("\n## Completion Log\n");
        for (date, _note) in &completion_dates {
            content.push_str(&format!("\n### {date}\n"));
        }
    }

    // --- Determine new path ---
    let filename = ctrl.rel_path.file_name().unwrap().to_str().unwrap();
    let new_rel_path = PathBuf::from("todo").join(filename);

    // Collect files to delete (all instances for this controller)
    let mut files_to_delete: Vec<PathBuf> = Vec::new();
    if let Some(ref active) = plan.active_instance {
        files_to_delete.push(active.rel_path.clone());
    }
    for inst in &plan.done_instances {
        files_to_delete.push(inst.rel_path.clone());
    }

    if dry_run {
        println!("  {ctrl_path_str}:");
        println!("    -> move to {}", new_rel_path.display());
        println!("    tldr: {new_tldr}");
        println!("    labels: {new_labels:?}");
        if let Some(ref active) = plan.active_instance {
            let sched = active
                .fm
                .tentative_date
                .or(active.fm.check_in_date)
                .or(active.fm.due_date);
            println!(
                "    tentative_date: {} (from active instance {})",
                sched.map_or("none".to_string(), |d| d.to_string()),
                active.rel_path.display()
            );
        } else {
            println!("    tentative_date: none (no active instance)");
        }
        println!("    completion log entries: {}", completion_dates.len());
        println!("    files to delete: {}", files_to_delete.len());
        for f in &files_to_delete {
            println!("      - {}", f.display());
        }
    } else {
        // Check: new path must not already exist (unless it's an active instance we're deleting)
        let new_abs = repo_dir.join(&new_rel_path);
        if new_abs.exists() {
            // It's OK if the new path is one of the files we're about to delete (active instance)
            let new_rel_canonical = new_rel_path.clone();
            if !files_to_delete.contains(&new_rel_canonical) {
                bail!(
                    "target path {} already exists and is not an instance being migrated",
                    new_rel_path.display()
                );
            }
        }

        // Validate the new content parses correctly before making any changes
        let new_fm = frontmatter::parse_frontmatter(&content)
            .with_context(|| format!("validating migrated content for {ctrl_path_str}"))?
            .with_context(|| format!("no frontmatter in migrated content for {ctrl_path_str}"))?;
        new_fm
            .validate()
            .with_context(|| format!("validation failed for migrated {ctrl_path_str}"))?;

        // Delete instance files first
        for f in &files_to_delete {
            let abs = repo_dir.join(f);
            if abs.exists() {
                std::fs::remove_file(&abs).with_context(|| format!("deleting {}", f.display()))?;
            }
        }

        // Write the new file
        std::fs::write(&new_abs, &content)
            .with_context(|| format!("writing {}", new_rel_path.display()))?;

        // Delete the original controller (if different path)
        let old_abs = repo_dir.join(&ctrl.rel_path);
        if old_abs != new_abs && old_abs.exists() {
            std::fs::remove_file(&old_abs)
                .with_context(|| format!("deleting original {}", ctrl.rel_path.display()))?;
        }
    }

    let deleted = files_to_delete.len();
    let merged = plan.done_instances.len() + plan.active_instance.as_ref().map_or(0, |_| 1);
    Ok((merged, deleted))
}

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

    // Phase 1: Discovery
    let controllers = discover_controllers(&repo_dir)?;
    println!("Found {} controllers", controllers.len());

    let controller_paths: Vec<PathBuf> = controllers.iter().map(|c| c.rel_path.clone()).collect();
    let (instances, parse_failures) = discover_instances(&repo_dir, &controller_paths)?;

    let total_instances: usize = instances.values().map(|v| v.len()).sum();
    println!("Found {total_instances} instances linked to controllers");

    if !parse_failures.is_empty() {
        println!(
            "\nWarning: {} files could not be parsed (verify none are recurrence instances):",
            parse_failures.len()
        );
        for f in &parse_failures {
            println!("  - {}", f.display());
        }
    }

    // Phase 2: Validation
    let validation = validate(controllers, instances);

    if !validation.errors.is_empty() {
        println!("\nValidation errors ({}):", validation.errors.len());
        for err in &validation.errors {
            println!("  ERROR: {err}");
        }
        bail!(
            "fix {} validation error(s) and re-run",
            validation.errors.len()
        );
    }

    println!(
        "\nValidation passed for {} controllers",
        validation.plans.len()
    );

    // Phase 3: Print plan / execute
    if !execute {
        println!("\nDry run (pass --execute to apply):\n");
    } else {
        println!("\nExecuting migration:\n");
    }

    let mut stats = MigrationStats {
        controllers_processed: 0,
        instances_merged: 0,
        files_deleted: 0,
    };

    for plan in &validation.plans {
        let (merged, deleted) = execute_plan(&repo_dir, plan, !execute).with_context(|| {
            format!(
                "migrating controller {}",
                plan.controller.rel_path.display()
            )
        })?;
        stats.controllers_processed += 1;
        stats.instances_merged += merged;
        stats.files_deleted += deleted;
    }

    // Delete empty controllers directory if executing
    if execute {
        let controllers_dir = repo_dir.join("todo/controllers");
        if let Ok(mut entries) = std::fs::read_dir(&controllers_dir) {
            if entries.next().is_none() {
                std::fs::remove_dir(&controllers_dir)?;
                println!("\nDeleted empty todo/controllers/ directory");
            } else {
                println!("\nWarning: todo/controllers/ is not empty after migration");
            }
        }
    }

    println!("\nSummary:");
    println!("  Controllers processed: {}", stats.controllers_processed);
    println!("  Instances merged: {}", stats.instances_merged);
    println!("  Files deleted: {}", stats.files_deleted);

    if !execute {
        println!("\nThis was a dry run. Pass --execute to apply changes.");
    } else {
        println!("\nMigration complete. Review with `git diff`, then run `graf reindex`.");
    }

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

    /// Create a minimal controller file content.
    fn make_controller(tldr: &str, rrule: &str, labels: &[&str]) -> String {
        let label_yaml: Vec<String> = labels.iter().map(|l| format!("- {l}")).collect();
        format!(
            "---\ntldr: 'Recurrence: {tldr}'\nlabels:\n{}\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\neffort: small\nrrule: {rrule}\n---\n\nSome body content.\n",
            label_yaml.join("\n")
        )
    }

    /// Create a minimal instance file content.
    fn make_instance(
        tldr: &str,
        context_link: &str,
        status: &str,
        on_date: Option<&str>,
    ) -> String {
        let mut yaml = format!(
            "---\ntldr: '{tldr}'\ncontext_link: {context_link}\nstatus: {status}\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n"
        );
        if let Some(date) = on_date {
            yaml.push_str(&format!("on_date: '{date}'\n"));
        }
        yaml.push_str("---\n");
        yaml
    }

    #[test]
    fn test_discover_and_validate_basic() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();

        // Create controller
        fs::create_dir_all(repo.join("todo/controllers")).unwrap();
        fs::write(
            repo.join("todo/controllers/test_task.md"),
            make_controller("Test task", "FREQ=WEEKLY", &["recurrence_controller"]),
        )
        .unwrap();

        // Create done instance
        fs::create_dir_all(repo.join("todo/done")).unwrap();
        fs::write(
            repo.join("todo/done/inst1.md"),
            make_instance(
                "Test task",
                "todo/controllers/test_task.md",
                "done",
                Some("2025-03-01"),
            ),
        )
        .unwrap();

        // Create active instance
        fs::write(
            repo.join("todo/inst2.md"),
            make_instance("Test task", "todo/controllers/test_task.md", "todo", None),
        )
        .unwrap();

        let controllers = discover_controllers(repo).unwrap();
        assert_eq!(controllers.len(), 1);

        let ctrl_paths: Vec<PathBuf> = controllers.iter().map(|c| c.rel_path.clone()).collect();
        let (instances, _) = discover_instances(repo, &ctrl_paths).unwrap();
        assert_eq!(instances.len(), 1);
        assert_eq!(
            instances[&PathBuf::from("todo/controllers/test_task.md")].len(),
            2
        );

        let result = validate(controllers, instances);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        assert_eq!(result.plans.len(), 1);
        assert!(result.plans[0].active_instance.is_some());
        assert_eq!(result.plans[0].done_instances.len(), 1);
    }

    #[test]
    fn test_validate_missing_rrule() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();

        fs::create_dir_all(repo.join("todo/controllers")).unwrap();
        // Controller without rrule
        fs::write(
            repo.join("todo/controllers/bad.md"),
            "---\ntldr: 'Recurrence: Bad'\nlabels:\n- recurrence_controller\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\n---\n",
        )
        .unwrap();

        let controllers = discover_controllers(repo).unwrap();
        let instances = HashMap::new();
        let result = validate(controllers, instances);
        assert_eq!(result.errors.len(), 1);
        assert!(result.errors[0].contains("missing `rrule`"));
    }

    #[test]
    fn test_validate_multiple_active_instances() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();

        fs::create_dir_all(repo.join("todo/controllers")).unwrap();
        fs::write(
            repo.join("todo/controllers/test.md"),
            make_controller("Test", "FREQ=WEEKLY", &["recurrence_controller"]),
        )
        .unwrap();

        // Two active instances
        fs::write(
            repo.join("todo/inst1.md"),
            make_instance("Test 1", "todo/controllers/test.md", "todo", None),
        )
        .unwrap();
        fs::write(
            repo.join("todo/inst2.md"),
            make_instance("Test 2", "todo/controllers/test.md", "todo", None),
        )
        .unwrap();

        let controllers = discover_controllers(repo).unwrap();
        let ctrl_paths: Vec<PathBuf> = controllers.iter().map(|c| c.rel_path.clone()).collect();
        let (instances, _) = discover_instances(repo, &ctrl_paths).unwrap();
        let result = validate(controllers, instances);
        assert_eq!(result.errors.len(), 1);
        assert!(result.errors[0].contains("active instances"));
    }

    #[test]
    fn test_execute_migration() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();

        fs::create_dir_all(repo.join("todo/controllers")).unwrap();
        fs::create_dir_all(repo.join("todo/done")).unwrap();

        fs::write(
            repo.join("todo/controllers/weekly_task.md"),
            make_controller(
                "Do weekly thing",
                "FREQ=WEEKLY",
                &["recurrence_controller", "home"],
            ),
        )
        .unwrap();

        // Active instance with scheduling info
        let active_content = "---\ntldr: 'Do weekly thing'\ncontext_link: todo/controllers/weekly_task.md\nstatus: todo\ncreated: '2025-01-01T00:00:00Z'\nupdated: '2025-01-01T00:00:00Z'\ncheck_in_date: '2025-04-15'\npriority: 2\n---\n";
        fs::write(repo.join("todo/active_inst.md"), active_content).unwrap();

        // Done instances
        fs::write(
            repo.join("todo/done/done1.md"),
            make_instance(
                "Do weekly thing",
                "todo/controllers/weekly_task.md",
                "done",
                Some("2025-03-01"),
            ),
        )
        .unwrap();
        fs::write(
            repo.join("todo/done/done2.md"),
            make_instance(
                "Do weekly thing",
                "todo/controllers/weekly_task.md",
                "done",
                Some("2025-03-08"),
            ),
        )
        .unwrap();

        let controllers = discover_controllers(repo).unwrap();
        let ctrl_paths: Vec<PathBuf> = controllers.iter().map(|c| c.rel_path.clone()).collect();
        let (instances, _) = discover_instances(repo, &ctrl_paths).unwrap();
        let result = validate(controllers, instances);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

        // Execute
        for plan in &result.plans {
            execute_plan(repo, plan, false).unwrap();
        }

        // Verify: controller moved to todo/weekly_task.md
        assert!(repo.join("todo/weekly_task.md").exists());
        assert!(!repo.join("todo/controllers/weekly_task.md").exists());

        // Verify: instances deleted
        assert!(!repo.join("todo/active_inst.md").exists());
        assert!(!repo.join("todo/done/done1.md").exists());
        assert!(!repo.join("todo/done/done2.md").exists());

        // Verify content
        let content = fs::read_to_string(repo.join("todo/weekly_task.md")).unwrap();
        assert!(
            content.contains("tldr: 'Do weekly thing'"),
            "tldr not stripped"
        );
        assert!(content.contains("status: todo"));
        assert!(content.contains("rrule: FREQ=WEEKLY"));
        assert!(content.contains("tentative_date: '2025-04-15'"));
        assert!(content.contains("priority: 2"));
        assert!(content.contains("## Completion Log"));
        assert!(content.contains("### 2025-03-01"));
        assert!(content.contains("### 2025-03-08"));
        assert!(content.contains("[\"home\", \"recurring\"]"));

        // Verify frontmatter parses
        let fm = frontmatter::parse_file(&repo.join("todo/weekly_task.md"))
            .unwrap()
            .unwrap();
        assert_eq!(fm.tldr, "Do weekly thing");
        assert_eq!(fm.status, Some(frontmatter::Status::Todo));
        assert!(fm.labels.contains(&"recurring".to_string()));
        assert!(!fm.labels.contains(&"recurrence_controller".to_string()));
    }

    #[test]
    fn test_context_link_with_vault_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();

        fs::create_dir_all(repo.join("todo/controllers")).unwrap();
        fs::create_dir_all(repo.join("todo/done")).unwrap();

        fs::write(
            repo.join("todo/controllers/test.md"),
            make_controller("Test", "FREQ=WEEKLY", &["recurrence_controller"]),
        )
        .unwrap();

        // Instance with "life/" prefix on context_link
        fs::write(
            repo.join("todo/done/inst.md"),
            make_instance(
                "Test",
                "life/todo/controllers/test.md",
                "done",
                Some("2025-03-01"),
            ),
        )
        .unwrap();

        let controllers = discover_controllers(repo).unwrap();
        let ctrl_paths: Vec<PathBuf> = controllers.iter().map(|c| c.rel_path.clone()).collect();
        let (instances, _) = discover_instances(repo, &ctrl_paths).unwrap();

        assert_eq!(instances.len(), 1);
        assert!(instances.contains_key(&PathBuf::from("todo/controllers/test.md")));
    }

    #[test]
    fn test_controller_no_active_instance() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();

        fs::create_dir_all(repo.join("todo/controllers")).unwrap();
        fs::create_dir_all(repo.join("todo/done")).unwrap();

        fs::write(
            repo.join("todo/controllers/orphan.md"),
            make_controller("Orphan task", "FREQ=MONTHLY", &["recurrence_controller"]),
        )
        .unwrap();

        // Only done instances, no active
        fs::write(
            repo.join("todo/done/d1.md"),
            make_instance(
                "Orphan",
                "todo/controllers/orphan.md",
                "done",
                Some("2025-02-01"),
            ),
        )
        .unwrap();

        let controllers = discover_controllers(repo).unwrap();
        let ctrl_paths: Vec<PathBuf> = controllers.iter().map(|c| c.rel_path.clone()).collect();
        let (instances, _) = discover_instances(repo, &ctrl_paths).unwrap();
        let result = validate(controllers, instances);
        assert!(result.errors.is_empty());
        assert_eq!(result.plans.len(), 1);
        assert!(result.plans[0].active_instance.is_none());

        // Execute should work — no tentative_date set
        for plan in &result.plans {
            execute_plan(repo, plan, false).unwrap();
        }

        let content = fs::read_to_string(repo.join("todo/orphan.md")).unwrap();
        assert!(content.contains("## Completion Log"));
        assert!(content.contains("### 2025-02-01"));
        // No tentative_date since no active instance
        assert!(!content.contains("tentative_date"));
    }
}
