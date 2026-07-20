use std::io::IsTerminal;

use anyhow::Result;
use chrono::NaiveDate;
use clap::{Args, Subcommand};

use graf::clock::Clock;
use graf::config::AppConfig;
use graf::frontmatter::{Effort, Recurrence, Status};
use graf::manifest::AppManifest;
use graf::todo::{self, AddOptions, CancelOptions, DoneOptions, ReorderOptions, ScheduleOptions};

#[derive(Args)]
pub struct TodoArgs {
    #[command(subcommand)]
    command: Option<TodoCommand>,

    /// Only include active items up to N days in the future and
    /// done/cancelled items completed within the last N days
    #[arg(long)]
    horizon: Option<u32>,

    /// Max results (overrides config default_limit)
    #[arg(long)]
    limit: Option<usize>,

    /// Include done/cancelled items
    #[arg(long)]
    include_done: bool,

    /// Filter by status (repeatable)
    #[arg(long)]
    status: Vec<String>,

    /// Filter by label (repeatable, AND semantics)
    #[arg(long)]
    label: Vec<String>,

    /// Repo slug or path (default: all manifest repos, or current directory)
    #[arg(long)]
    repo: Option<String>,

    /// JSON output (default when stdout is not a TTY)
    #[arg(long)]
    json: bool,
}

#[derive(Subcommand)]
enum TodoCommand {
    /// Create a new todo file
    Add(AddArgs),
    /// Mark a task as done
    Done(DoneArgs),
    /// Cancel a task
    Cancel(CancelArgs),
    /// Set a task's tentative date
    Schedule(ScheduleArgs),
    /// Reorder a task between neighbors
    Reorder(ReorderArgs),
}

#[derive(Args)]
struct AddArgs {
    /// Relative path for the new file, or slug:path (e.g. life:todo/my-task.md)
    path: String,

    /// The tldr / title for the task
    title: String,

    /// Priority 1-5 (default: 3)
    #[arg(long, default_value = "3")]
    priority: u8,

    /// Effort level
    #[arg(long)]
    effort: Option<Effort>,

    /// Check-in date (ISO 8601). At least one of --check-in or --due is required.
    #[arg(long)]
    check_in: Option<NaiveDate>,

    /// Due date (ISO 8601). At least one of --check-in or --due is required.
    #[arg(long)]
    due: Option<NaiveDate>,

    /// Label (repeatable)
    #[arg(long)]
    label: Vec<String>,

    /// RFC 5545 rrule string (e.g. "FREQ=WEEKLY;BYDAY=SA"). Writes a
    /// nested `recurrence` block.
    #[arg(long)]
    rrule: Option<String>,

    /// Non-negative integer. Lead time in days; requires --due.
    #[arg(long)]
    lead_days: Option<u32>,

    /// Repo slug or path (default: current directory)
    #[arg(long)]
    repo: Option<String>,
}

pub fn run(args: TodoArgs) -> Result<()> {
    let json = args.json || !std::io::stdout().is_terminal();
    match args.command {
        Some(TodoCommand::Add(add_args)) => run_add(add_args, json),
        Some(TodoCommand::Done(done_args)) => run_done(done_args, json),
        Some(TodoCommand::Cancel(cancel_args)) => run_cancel(cancel_args, json),
        Some(TodoCommand::Schedule(sched_args)) => run_schedule(sched_args, json),
        Some(TodoCommand::Reorder(reorder_args)) => run_reorder(reorder_args, json),
        None => run_query(args),
    }
}

fn run_query(args: TodoArgs) -> Result<()> {
    let manifest = AppManifest::load_default()?;
    let repos = super::resolve_repos(args.repo.as_deref(), manifest.as_ref())?;
    let statuses = parse_statuses(&args.status)?;
    let multi_repo = repos.len() > 1;

    let config = AppConfig::load_default()?;
    let limit = args.limit.or(config.todo.effective_limit());

    let combined = super::multi_repo_todo_query(
        &repos,
        &super::MultiRepoQueryOpts {
            horizon_days: args.horizon,
            include_done: args.include_done,
            statuses,
            labels: args.label,
            limit,
        },
    );

    let use_json = args.json || !std::io::stdout().is_terminal();
    if use_json {
        println!("{}", serde_json::to_string(&combined)?);
    } else {
        let today = Clock::from_env().today();
        let output = todo::format_human(&combined, today, multi_repo);
        if output.is_empty() {
            println!("No tasks found.");
        } else {
            print!("{output}");
        }
    }

    Ok(())
}

fn run_add(args: AddArgs, json: bool) -> Result<()> {
    let manifest = AppManifest::load_default()?;
    let (resolved, path) =
        super::resolve_target(&args.path, args.repo.as_deref(), None, manifest.as_ref())?;

    let recurrence = match (args.rrule.as_deref(), args.lead_days) {
        (Some(rrule), lead_days) => Some(Recurrence {
            rrule: rrule.to_string(),
            lead_days,
        }),
        (None, Some(_)) => {
            anyhow::bail!(
                "--lead-days requires --rrule (lead_days belongs to the recurrence block)"
            );
        }
        (None, None) => None,
    };

    let opts = AddOptions {
        repo_dir: resolved.path,
        path,
        title: args.title,
        priority: args.priority,
        effort: args.effort,
        check_in_date: args.check_in,
        due_date: args.due,
        labels: args.label,
        recurrence,
        ..Default::default()
    };

    let result = todo::todo_add(&opts)?;
    if json {
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "path": result.path.display().to_string(),
            }))?
        );
    } else {
        println!("{}", result.path.display());
    }
    Ok(())
}

#[derive(Args)]
struct DoneArgs {
    /// Relative path to the task file, or slug:path (e.g. life:todo/my-task.md)
    path: String,

    /// Completion date (ISO 8601, required) — the date the user actually
    /// did the task, in their local timezone. No server-clock fallback.
    #[arg(long)]
    completion_date: NaiveDate,

    /// Non-recurring only: acknowledges that a stored on_date/end_date
    /// will be overwritten. Must equal the current stored value.
    #[arg(long)]
    replacing: Option<NaiveDate>,

    /// Anchored rule only: whether to jump past stale anchors (`true`)
    /// or advance one occurrence at a time (`false`). Server rejects
    /// with `stale_anchor` when this is needed and absent.
    #[arg(long)]
    skip_past: Option<bool>,

    /// Optional markdown comment attached to the log entry.
    #[arg(long)]
    comment: Option<String>,

    /// Repo slug or path (default: current directory)
    #[arg(long)]
    repo: Option<String>,
}

#[derive(Args)]
struct CancelArgs {
    /// Relative path to the task file, or slug:path (e.g. life:todo/my-task.md)
    path: String,

    /// Completion date (ISO 8601, default: today). Written to end_date if
    /// start_date is present, otherwise on_date.
    #[arg(long)]
    date: Option<NaiveDate>,

    /// Required ONLY when overwriting a stored on_date/end_date with a
    /// different value. Must equal the current stored value.
    #[arg(long)]
    replacing: Option<NaiveDate>,

    /// See [`DoneArgs::today`].
    #[arg(long)]
    today: Option<NaiveDate>,

    /// Repo slug or path (default: current directory)
    #[arg(long)]
    repo: Option<String>,
}

#[derive(Args)]
struct ScheduleArgs {
    /// Relative path to the task file, or slug:path (e.g. life:todo/my-task.md)
    path: String,

    /// Target date (ISO 8601)
    date: NaiveDate,

    /// Repo slug or path (default: current directory)
    #[arg(long)]
    repo: Option<String>,
}

#[derive(Args)]
struct ReorderArgs {
    /// Relative path to the task file, or slug:path (e.g. life:todo/my-task.md)
    path: String,

    /// Place after this item (relative path, or slug:path for cross-repo)
    #[arg(long)]
    after: Option<String>,

    /// Place before this item (relative path, or slug:path for cross-repo)
    #[arg(long)]
    before: Option<String>,

    /// Repo slug or path (default: current directory)
    #[arg(long)]
    repo: Option<String>,
}

fn run_done(args: DoneArgs, json: bool) -> Result<()> {
    let manifest = AppManifest::load_default()?;
    let (resolved, path) =
        super::resolve_target(&args.path, args.repo.as_deref(), None, manifest.as_ref())?;
    let call_result = todo::todo_done(&DoneOptions {
        repo_dir: resolved.path,
        path,
        completion_date: args.completion_date,
        replacing: args.replacing,
        skip_past: args.skip_past,
        comment: args.comment,
    });

    if json {
        match call_result {
            Ok(result) => {
                let resp = todo::done_result_to_json(&result);
                println!("{}", serde_json::to_string(&resp)?);
                Ok(())
            }
            Err(todo::DoneError::Infra(e)) => Err(e),
            Err(other) => {
                let resp = other.to_json();
                println!("{}", serde_json::to_string(&resp)?);
                // Non-zero exit is appropriate for these structured errors.
                std::process::exit(1);
            }
        }
    } else {
        let result = call_result.map_err(|e| match e {
            todo::DoneError::Infra(inner) => inner,
            // Use `reason()` rather than `Display` so the code prefix
            // (e.g. `stale_anchor:`) doesn't leak into the human-facing
            // "Error: ..." line, matching `run_schedule`'s behavior.
            other => anyhow::anyhow!("{}", other.reason()),
        })?;
        match result.outcome {
            todo::DoneOutcome::Recurring {
                next_check_in_date,
                next_due_date,
            } => {
                if let Some(d) = next_due_date {
                    println!(
                        "Recurring: {} next check-in {next_check_in_date} (due {d})",
                        result.path.display()
                    );
                } else {
                    println!(
                        "Recurring: {} next check-in {next_check_in_date}",
                        result.path.display()
                    );
                }
            }
            todo::DoneOutcome::Completed {
                completion,
                terminal,
            } => {
                if terminal {
                    println!(
                        "Done (final occurrence): {} (completed {})",
                        result.path.display(),
                        completion.date()
                    );
                } else {
                    println!(
                        "Done: {} (completed {})",
                        result.path.display(),
                        completion.date()
                    );
                }
            }
            todo::DoneOutcome::AlreadyDone {
                existing,
                comment_discarded,
            } => {
                println!(
                    "Already done: {} (existing completed {}{})",
                    result.path.display(),
                    existing.completed,
                    if comment_discarded {
                        "; comment discarded"
                    } else {
                        ""
                    },
                );
            }
        }
        Ok(())
    }
}

fn run_cancel(args: CancelArgs, json: bool) -> Result<()> {
    let manifest = AppManifest::load_default()?;
    let (resolved, path) =
        super::resolve_target(&args.path, args.repo.as_deref(), None, manifest.as_ref())?;
    let result = todo::todo_cancel(&CancelOptions {
        repo_dir: resolved.path,
        path,
        date: args.date,
        replacing: args.replacing,
        today: args.today.or_else(|| Some(Clock::from_env().today())),
    })?;
    if json {
        let mut resp = serde_json::json!({
            "path": result.path.display().to_string(),
        });
        result.date.set_json(&mut resp);
        println!("{}", serde_json::to_string(&resp)?);
    } else {
        println!(
            "Cancelled: {} ({})",
            result.path.display(),
            result.date.date()
        );
    }
    Ok(())
}

fn run_schedule(args: ScheduleArgs, json: bool) -> Result<()> {
    let manifest = AppManifest::load_default()?;
    let (resolved, path) =
        super::resolve_target(&args.path, args.repo.as_deref(), None, manifest.as_ref())?;
    let call_result = todo::todo_schedule(&ScheduleOptions {
        repo_dir: resolved.path,
        path,
        date: args.date,
    });
    let result = match call_result {
        Ok(r) => r,
        Err(todo::ScheduleError::Infra(e)) => return Err(e),
        Err(other) => {
            if json {
                let resp = other.to_json();
                println!("{}", serde_json::to_string(&resp)?);
                std::process::exit(1);
            } else {
                // Human-facing CLI: use `reason()` rather than `Display`
                // so the code prefix (e.g. `terminal_task:`) doesn't
                // leak into the anyhow-wrapped "Error: ..." line.
                return Err(anyhow::anyhow!("{}", other.reason()));
            }
        }
    };
    if json {
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "path": result.path.display().to_string(),
                "tentative_date": result.tentative_date.to_string(),
            }))?
        );
    } else {
        println!(
            "Scheduled: {} -> {}",
            result.path.display(),
            result.tentative_date
        );
    }
    Ok(())
}

fn run_reorder(args: ReorderArgs, json: bool) -> Result<()> {
    let manifest = AppManifest::load_default()?;
    let (resolved, path) =
        super::resolve_target(&args.path, args.repo.as_deref(), None, manifest.as_ref())?;
    let target_sync = graf::db::open_and_sync(&resolved.path)?;

    let after = args
        .after
        .as_deref()
        .map(|s| super::resolve_anchor(s, &target_sync.cache, manifest.as_ref()))
        .transpose()?;
    let before = args
        .before
        .as_deref()
        .map(|s| super::resolve_anchor(s, &target_sync.cache, manifest.as_ref()))
        .transpose()?;
    drop(target_sync);

    let result = todo::todo_reorder(&ReorderOptions {
        repo_dir: resolved.path,
        path,
        after,
        before,
    })?;
    if json {
        let mut resp = serde_json::json!({
            "path": result.path.display().to_string(),
            "sort_order": result.sort_order,
        });
        if let Some(date) = result.tentative_date {
            resp["tentative_date"] = serde_json::json!(date.to_string());
        }
        println!("{}", serde_json::to_string(&resp)?);
    } else {
        println!(
            "Reordered: {} (sort_order: {})",
            result.path.display(),
            result.sort_order
        );
    }
    Ok(())
}

fn parse_statuses(status_strs: &[String]) -> Result<Vec<Status>> {
    status_strs
        .iter()
        .map(|s| s.parse::<Status>().map_err(|e| anyhow::anyhow!("{e}")))
        .collect()
}
