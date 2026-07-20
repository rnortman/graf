use std::fmt;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use chrono::{NaiveDate, NaiveTime, TimeZone, Utc};
use rrule::RRuleSet;
use serde::Serialize;

use crate::clock::Clock;
use crate::db::{self, HorizonFilter, TaskFilter, TaskRow};
use crate::errors::FileError;
use crate::frontmatter::{
    self, CompletionLogEntry, Effort, Field, FieldMutation, Recurrence, Status, yaml_scalar,
};
use crate::identity::{GlobalId, Slug};

// ---------------------------------------------------------------------------
// Query
// ---------------------------------------------------------------------------

/// Result of a todo query: tasks + any lint errors found during sync.
///
/// Invariant: every item in `tasks` has a non-null `effective_date`. Rows
/// whose `effective_date` resolves to NULL in the cache (indicating a
/// scheduling-date lint violation) are partitioned into `lint_errors`
/// instead of `tasks`. This holds regardless of whether the current
/// sync pass re-linted the violating file.
///
/// Horizon note: when `horizon_days` is set, the SQL WHERE clause in
/// `db::Cache::query_tasks` filters out active items with NULL
/// `effective_date` before they reach the partition (terminal items
/// bypass the horizon filter). The partition only observes null-date
/// rows that the DB layer actually returned. Brenn's todo query does
/// not set a horizon so the invariant holds end-to-end there; callers
/// that set a horizon should understand that null-date active items
/// are silent under horizon-bounded queries by design.
#[derive(Debug, Serialize)]
pub struct TodoQueryResult {
    /// Sharing domains that contributed results (for contamination tracking).
    /// `None` when operating without a manifest.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domains: Option<Vec<GlobalId>>,
    pub tasks: Vec<TodoItem>,
    pub lint_errors: Vec<TodoError>,
}

/// A single task in query output.
///
/// `effective_date` is non-optional: rows whose computed effective_date
/// is NULL are partitioned out at the query-response layer (see
/// `TodoQueryResult`'s invariant).
#[derive(Debug, Serialize)]
pub struct TodoItem {
    pub path: String,
    /// Repo slug, if operating with a manifest.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<Slug>,
    /// Sharing domain global ID, if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<GlobalId>,
    pub tldr: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<Status>,
    pub effective_date: NaiveDate,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tentative_date: Option<NaiveDate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub check_in_date: Option<NaiveDate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due_date: Option<NaiveDate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort_order: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<Effort>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rrule: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
}

impl TodoItem {
    /// Returns true if this item has a terminal status (Done, Cancelled).
    pub fn is_terminal(&self) -> bool {
        self.status.as_ref().is_some_and(|s| s.is_terminal())
    }
}

/// A lint error in query output.
#[derive(Debug, Serialize)]
pub struct TodoError {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<Slug>,
    pub message: String,
}

/// Options for `todo_query`, mapping 1:1 to CLI/MCP inputs.
pub struct QueryOptions {
    pub repo_dir: PathBuf,
    /// Repo slug to tag onto results (when operating with manifest).
    pub repo_slug: Option<Slug>,
    /// Sharing domain to tag onto results (when operating with manifest).
    pub repo_domain: Option<GlobalId>,
    /// When set, only include items with effective_date <= today + N days.
    /// None means no date filtering (the default).
    pub horizon_days: Option<u32>,
    pub include_done: bool,
    pub statuses: Vec<Status>,
    pub labels: Vec<String>,
    pub limit: Option<usize>,
}

pub fn todo_query(opts: &QueryOptions) -> Result<TodoQueryResult> {
    let sync = db::open_and_sync(&opts.repo_dir)?;
    let clock = Clock::from_env();

    let horizon = opts.horizon_days.map(|days| HorizonFilter {
        reference_date: clock.today(),
        days,
    });

    let filter = TaskFilter {
        horizon,
        statuses: if opts.statuses.is_empty() {
            vec![Status::Todo, Status::InProgress, Status::Reminder]
        } else {
            opts.statuses.clone()
        },
        // --include-done only applies to the default status set.
        // If explicit --status was given, it wins entirely.
        include_done: opts.statuses.is_empty() && opts.include_done,
        labels: opts.labels.clone(),
        limit: opts.limit,
    };

    let rows = sync.cache.query_tasks(&filter)?;

    let slug = &opts.repo_slug;
    let domain = &opts.repo_domain;

    // Partition rows: those with a resolved effective_date become TodoItems;
    // those with null effective_date (a scheduling-date lint violation in
    // the stored data) become synthesized lint_errors. See
    // TodoQueryResult's invariant doc.
    let mut tasks = Vec::with_capacity(rows.len());
    let mut lint_errors: Vec<TodoError> = sync
        .errors
        .into_iter()
        .map(|e| TodoError {
            path: e.path.display().to_string(),
            repo: slug.clone(),
            message: e.message,
        })
        .collect();

    for row in rows {
        match task_row_to_item(row, slug.clone(), domain.clone()) {
            Ok(item) => tasks.push(item),
            Err(err) => lint_errors.push(err),
        }
    }

    Ok(TodoQueryResult {
        domains: None, // Set by multi-repo caller if needed
        tasks,
        lint_errors,
    })
}

/// Error message for a row whose `effective_date` resolves to NULL — a
/// scheduling-date lint violation that reached the cache.
const NULL_EFFECTIVE_DATE_MSG: &str =
    "item has no effective_date (missing scheduling or completed date)";

/// Convert a `TaskRow` into a `TodoItem`, or into a `TodoError` if the
/// row has a null `effective_date` (invariant enforcement — see
/// `TodoQueryResult` doc).
fn task_row_to_item(
    row: TaskRow,
    repo: Option<Slug>,
    domain: Option<GlobalId>,
) -> std::result::Result<TodoItem, TodoError> {
    let Some(effective_date) = row.effective_date else {
        return Err(TodoError {
            path: row.path.display().to_string(),
            repo,
            message: NULL_EFFECTIVE_DATE_MSG.to_string(),
        });
    };
    Ok(TodoItem {
        path: row.path.display().to_string(),
        repo,
        domain,
        tldr: row.tldr,
        status: row.status,
        effective_date,
        tentative_date: row.tentative_date,
        check_in_date: row.check_in_date,
        due_date: row.due_date,
        sort_order: row.sort_order,
        priority: row.priority,
        effort: row.effort,
        rrule: row.rrule,
        labels: row.labels,
    })
}

// ---------------------------------------------------------------------------
// Add
// ---------------------------------------------------------------------------

/// Options for `todo_add`.
#[derive(Default)]
pub struct AddOptions {
    pub repo_dir: PathBuf,
    pub path: PathBuf,
    pub title: String,
    pub priority: u8,
    pub effort: Option<Effort>,
    /// At least one of `check_in_date` or `due_date` is required.
    pub check_in_date: Option<NaiveDate>,
    pub due_date: Option<NaiveDate>,
    pub labels: Vec<String>,
    /// When set, writes a nested `recurrence` block. Flat `rrule` is not
    /// accepted on create (legacy-compat is read-only).
    pub recurrence: Option<Recurrence>,

    // New optional frontmatter fields
    pub summary: Option<String>,
    pub context_link: Option<String>,
    pub links: Vec<String>,
    pub blocked_by: Vec<String>,
    pub assigned_to: Option<String>,
    pub on_date: Option<NaiveDate>,
    pub start_date: Option<NaiveDate>,
    pub end_date: Option<NaiveDate>,
    pub verified_date: Option<NaiveDate>,
    pub tentative_date: Option<NaiveDate>,

    /// Optional markdown body. Written after the closing `---` of the
    /// frontmatter. Stripped of leading/trailing whitespace before writing;
    /// if trimmed value is empty, no body is written.
    pub body: Option<String>,
}

/// Result of `todo_add`.
pub struct AddResult {
    pub path: PathBuf,
}

pub fn todo_add(opts: &AddOptions) -> Result<AddResult> {
    ensure!(
        crate::is_md(&opts.path),
        "path must end in .md: {}",
        opts.path.display()
    );
    ensure!(
        (1..=5).contains(&opts.priority),
        "priority must be 1-5, got {}",
        opts.priority
    );
    ensure!(
        opts.check_in_date.is_some() || opts.due_date.is_some(),
        "at least one of check_in_date or due_date is required"
    );

    let abs_path = opts.repo_dir.join(&opts.path);

    // Ensure parent directory exists
    if let Some(parent) = abs_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating directory {}", parent.display()))?;
    }

    let sync = db::open_and_sync(&opts.repo_dir)?;
    let clock = Clock::from_env();

    let now = format_tz_timestamp(&clock.now());

    // Build YAML frontmatter manually to control field order and formatting.
    //
    // No `sort_order:` is emitted: rank falls through `COALESCE(sort_order,
    // priority, UNRANKED)` to `priority` for new tasks. An explicit
    // `sort_order` is only written by `todo_reorder` (manual placement).
    let mut yaml = String::new();
    yaml.push_str(&format!("tldr: {:?}\n", opts.title));
    yaml.push_str("status: todo\n");
    yaml.push_str(&format!("priority: {}\n", opts.priority));
    if let Some(ref effort) = opts.effort {
        yaml.push_str(&format!("effort: {effort}\n"));
    }
    if let Some(ref summary) = opts.summary {
        yaml.push_str(&format!("summary: {}\n", yaml_scalar(summary)));
    }
    if let Some(on) = opts.on_date {
        yaml.push_str(&format!("on_date: '{on}'\n"));
    }
    if let Some(start) = opts.start_date {
        yaml.push_str(&format!("start_date: '{start}'\n"));
    }
    if let Some(end) = opts.end_date {
        yaml.push_str(&format!("end_date: '{end}'\n"));
    }
    if let Some(check_in) = opts.check_in_date {
        yaml.push_str(&format!("check_in_date: '{check_in}'\n"));
    }
    if let Some(due) = opts.due_date {
        yaml.push_str(&format!("due_date: '{due}'\n"));
    }
    if let Some(verified) = opts.verified_date {
        yaml.push_str(&format!("verified_date: '{verified}'\n"));
    }
    if let Some(ref ctx) = opts.context_link {
        yaml.push_str(&format!("context_link: {}\n", yaml_scalar(ctx)));
    }
    if !opts.links.is_empty() {
        yaml.push_str("links:\n");
        for link in &opts.links {
            yaml.push_str(&format!("  - {}\n", yaml_scalar(link)));
        }
    }
    if !opts.blocked_by.is_empty() {
        yaml.push_str("blocked_by:\n");
        for dep in &opts.blocked_by {
            yaml.push_str(&format!("  - {}\n", yaml_scalar(dep)));
        }
    }
    if let Some(ref rec) = opts.recurrence {
        yaml.push_str("recurrence:\n");
        yaml.push_str(&format!("  rrule: {}\n", yaml_scalar(&rec.rrule)));
        if let Some(lead) = rec.lead_days {
            yaml.push_str(&format!("  lead_days: {lead}\n"));
        }
    }
    if !opts.labels.is_empty() {
        let labels_yaml: Vec<String> = opts.labels.iter().map(|l| yaml_scalar(l)).collect();
        yaml.push_str(&format!("labels: [{}]\n", labels_yaml.join(", ")));
    }
    if let Some(tentative) = opts.tentative_date {
        yaml.push_str(&format!("tentative_date: '{tentative}'\n"));
    }
    if let Some(ref assigned) = opts.assigned_to {
        yaml.push_str(&format!("assigned_to: {}\n", yaml_scalar(assigned)));
    }
    yaml.push_str(&format!("created: '{now}'\n"));
    yaml.push_str(&format!("updated: '{now}'\n"));

    let mut content = format!("---\n{yaml}---\n");
    if let Some(b) = opts.body.as_deref() {
        let trimmed = b.trim();
        if !trimmed.is_empty() {
            content.push_str(trimmed);
            content.push('\n');
        }
    }

    // Atomic create — fails if file already exists (no TOCTOU race)
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&abs_path)
        .with_context(|| format!("creating {} (already exists?)", opts.path.display()))?;
    file.write_all(content.as_bytes())?;

    // Parse back and cache (validates what we wrote)
    let fm = crate::frontmatter::parse_file(&abs_path)
        .with_context(|| format!("re-parsing created file {}", opts.path.display()))?
        .with_context(|| {
            format!(
                "missing frontmatter in created file {}",
                opts.path.display()
            )
        })?;
    fm.validate()?;
    sync.cache.upsert_doc(&opts.path, &fm, true)?;

    Ok(AddResult {
        path: opts.path.clone(),
    })
}

// ---------------------------------------------------------------------------
// Done
// ---------------------------------------------------------------------------

/// Options for `todo_done`.
///
/// `completion_date` is required; there is no server-clock fallback. The
/// LLM / UI is responsible for sourcing the real date in the user's local
/// timezone. The server still reads `Clock::from_env().now()` for the
/// `updated` audit timestamp, never for completion logic.
pub struct DoneOptions {
    pub repo_dir: PathBuf,
    pub path: PathBuf,
    /// Required. The date the user actually did the task, in their local zone.
    pub completion_date: NaiveDate,
    /// Non-recurring idempotency: acknowledges that a stored
    /// `on_date`/`end_date` will be overwritten. Must equal the stored
    /// value; required when the stored value differs from
    /// `completion_date`.
    pub replacing: Option<NaiveDate>,
    /// Anchored-only: whether to skip past the naive next anchor when it's
    /// still before `completion_date`. `None` = let the server decide
    /// (error if the stale-anchor condition fires). `Some(_)` is rejected
    /// on non-recurring and slip shapes, and on anchored when the
    /// condition doesn't fire.
    pub skip_past: Option<bool>,
    /// Optional markdown comment attached to the log entry this call writes.
    /// On slip same-day no-ops and non-recurring conflicts the comment is
    /// discarded (not written); the response flags `comment_discarded: true`.
    pub comment: Option<String>,
}

/// Result of `todo_done`.
#[derive(Debug)]
pub struct DoneResult {
    pub path: PathBuf,
    pub outcome: DoneOutcome,
}

/// Which date field was set when completing/cancelling a task.
/// `OnDate` for point-in-time events, `EndDate` for range completions
/// (when `start_date` was present).
#[derive(Debug)]
pub enum CompletionDate {
    OnDate(NaiveDate),
    EndDate(NaiveDate),
}

impl CompletionDate {
    /// Pick `EndDate` when `use_end_date` is true (range task — `start_date`
    /// present), `OnDate` otherwise.
    pub fn from_date(use_end_date: bool, d: NaiveDate) -> Self {
        if use_end_date {
            CompletionDate::EndDate(d)
        } else {
            CompletionDate::OnDate(d)
        }
    }

    /// The date value regardless of which field it was stored in.
    pub fn date(&self) -> NaiveDate {
        match self {
            CompletionDate::OnDate(d) | CompletionDate::EndDate(d) => *d,
        }
    }

    /// `FieldMutation::Set` targeting the correct field (`on_date` or
    /// `end_date`) with this completion's date value.
    pub fn to_mutation(&self) -> FieldMutation {
        match self {
            CompletionDate::OnDate(d) => FieldMutation::Set {
                field: Field::OnDate,
                value: format!("'{d}'"),
            },
            CompletionDate::EndDate(d) => FieldMutation::Set {
                field: Field::EndDate,
                value: format!("'{d}'"),
            },
        }
    }

    /// Insert the appropriate key/value into a JSON object.
    pub fn set_json(&self, resp: &mut serde_json::Value) {
        match self {
            CompletionDate::OnDate(d) => {
                resp["on_date"] = serde_json::json!(d.to_string());
            }
            CompletionDate::EndDate(d) => {
                resp["end_date"] = serde_json::json!(d.to_string());
            }
        }
    }
}

/// What happened when a task was marked done.
#[derive(Debug)]
pub enum DoneOutcome {
    /// Non-recurring task (or exhausted recurrence) was completed.
    /// `terminal = true` signals rrule-exhaustion (anchored tasks whose
    /// final occurrence has been completed); `false` is plain
    /// non-recurring completion.
    Completed {
        completion: CompletionDate,
        terminal: bool,
    },
    /// Recurring task advanced to the next occurrence.
    Recurring {
        next_check_in_date: NaiveDate,
        next_due_date: Option<NaiveDate>,
    },
    /// Slip idempotent no-op: an entry with `completed == completion_date`
    /// is already in the log. No file write. If the caller supplied a
    /// comment, it was discarded.
    AlreadyDone {
        existing: CompletionLogEntry,
        comment_discarded: bool,
    },
}

/// Render a successful `DoneResult` as its JSON envelope.
/// Shared by the MCP tool and the CLI JSON output path.
pub fn done_result_to_json(result: &DoneResult) -> serde_json::Value {
    use serde_json::json;
    let mut resp = json!({
        "path": result.path.display().to_string(),
    });
    match result.outcome {
        DoneOutcome::Recurring {
            next_check_in_date,
            next_due_date,
        } => {
            resp["terminal"] = json!(false);
            resp["next_check_in_date"] = json!(next_check_in_date.to_string());
            if let Some(d) = next_due_date {
                resp["next_due_date"] = json!(d.to_string());
            }
        }
        DoneOutcome::Completed {
            ref completion,
            terminal,
        } => {
            resp["terminal"] = json!(terminal);
            completion.set_json(&mut resp);
        }
        DoneOutcome::AlreadyDone {
            ref existing,
            comment_discarded,
        } => {
            resp["already_done"] = json!(true);
            let mut entry = json!({
                "completed": existing.completed.to_string(),
            });
            if let Some(occ) = existing.occurrence {
                entry["occurrence"] = json!(occ.to_string());
            }
            if let Some(ref c) = existing.comment {
                entry["comment"] = json!(c);
            }
            resp["existing_entry"] = entry;
            resp["comment_discarded"] = json!(comment_discarded);
            resp["message"] = json!(format!(
                "Task already marked done for {}; {}",
                existing.completed,
                if comment_discarded {
                    "your comment was not written. To add notes, edit the file directly."
                } else {
                    "no changes made."
                }
            ));
        }
    }
    resp
}

/// Shape of a task for `todo_done`, derived from `effective_rrule()`.
///
/// Bright line: presence of any `BY…` modifier
/// makes the rule calendar-anchored; bare `FREQ=` is slip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskShape {
    NonRecurring,
    Slip,
    Anchored,
}

fn classify_shape(rrule: Option<&str>) -> TaskShape {
    match rrule {
        None => TaskShape::NonRecurring,
        Some(r) if contains_by_modifier(r) => TaskShape::Anchored,
        Some(_) => TaskShape::Slip,
    }
}

/// Return true if `rrule` contains any `BY…` calendar-anchored modifier.
///
/// RFC 5545 rule-parts `BYSECOND`, `BYMINUTE`, `BYHOUR`, `BYDAY`,
/// `BYMONTHDAY`, `BYYEARDAY`, `BYWEEKNO`, `BYMONTH`, `BYSETPOS` all
/// anchor the series to the calendar; bare `FREQ=…[;INTERVAL=…]` does
/// not. Case-insensitive because RFC 5545 allows uppercase/lowercase.
fn contains_by_modifier(rrule: &str) -> bool {
    let upper = rrule.to_ascii_uppercase();
    // Split on ';' (rule-parts) and look for any part starting with "BY".
    upper
        .split(';')
        .any(|part| part.trim_start().starts_with("BY"))
}

/// Structured errors from `todo_done`. Each variant carries the payload
/// for its error case. The `to_json()` method renders the
/// `{error, reason, ...payload}` envelope the MCP layer returns as an
/// `Ok(Value)` (see the comment in `TodoDoneTool::call`).
#[derive(Debug)]
pub enum DoneError {
    /// Anchored rule: naive next anchor is still before `completion_date`
    /// and `skip_past` was not supplied.
    StaleAnchor {
        stored_anchor: NaiveDate,
        completion_date: NaiveDate,
        next_anchor_if_skip_past_false: NaiveDate,
        next_anchor_if_skip_past_true: NaiveDate,
    },
    /// Anchored rule: `skip_past` was supplied but the stale-anchor
    /// condition didn't fire.
    SkipPastUnnecessary {
        stored_anchor: NaiveDate,
        next_anchor: NaiveDate,
    },
    /// Slip rule: `skip_past` was supplied; meaningless here.
    SkipPastOnSlipRule,
    /// Non-recurring task: `skip_past` was supplied; meaningless here.
    SkipPastOnNonRecurring,
    /// Non-recurring: stored `on_date`/`end_date` differs from
    /// `completion_date` and no `replacing` was supplied (or the
    /// supplied value didn't match).
    AlreadyDoneOrNotDueYet {
        stored_on_date: NaiveDate,
        completion_date: NaiveDate,
    },
    /// `replacing` supplied on a recurring task (slip or anchored).
    /// `replacing` only makes sense on non-recurring tasks (overwriting
    /// a stored on_date/end_date). Recurring tasks advance on each
    /// `done`, so there is nothing to replace.
    ReplacingOnRecurring,
    /// Task already has a terminal status (`done` / `cancelled`). No
    /// further mutations are allowed.
    AlreadyTerminal { status: Status },
    /// The file's frontmatter or state is not eligible for `done`
    /// (lint errors, malformed input, etc.). Returned as plain text —
    /// not one of the design-named structured codes.
    Invalid(String),
    // --- pre-flight / MCP layer errors (not business logic) ---
    // TODO(done-error-grouping-comment): add doc-comment grouping markers
    // separating pre-flight/MCP-layer, business-logic, and infra variants as
    // DoneError grows.
    /// Path argument does not resolve in the repo (file not found,
    /// malformed slug, lint errors, etc.). Returned as a structured
    /// envelope — the LLM should check the path and retry.
    PathNotFound { path: String, reason: String },
    /// A date argument failed to parse or was missing when required.
    /// Returned as a structured envelope — the LLM should fix the value
    /// and retry.
    InvalidDate { reason: String },
    // --- infrastructure errors (never serialized as envelopes) ---
    /// Low-level infrastructure failure (DB open, file read, rrule
    /// parse). Always propagates up to the caller as an anyhow Err.
    Infra(anyhow::Error),
}

impl DoneError {
    pub fn code(&self) -> &'static str {
        match self {
            DoneError::StaleAnchor { .. } => "stale_anchor",
            DoneError::SkipPastUnnecessary { .. } => "skip_past_unnecessary",
            DoneError::SkipPastOnSlipRule => "skip_past_on_slip_rule",
            DoneError::SkipPastOnNonRecurring => "skip_past_on_non_recurring",
            DoneError::AlreadyDoneOrNotDueYet { .. } => "already_done_or_not_due_yet",
            DoneError::ReplacingOnRecurring => "replacing_on_recurring",
            DoneError::AlreadyTerminal { .. } => "already_terminal",
            DoneError::Invalid(_) => "invalid",
            DoneError::PathNotFound { .. } => "path_not_found",
            DoneError::InvalidDate { .. } => "invalid_date",
            DoneError::Infra(_) => "infra",
        }
    }

    pub fn reason(&self) -> String {
        match self {
            DoneError::StaleAnchor {
                stored_anchor,
                completion_date,
                next_anchor_if_skip_past_false,
                next_anchor_if_skip_past_true,
            } => format!(
                "Stored anchor ({stored_anchor}) is far enough behind that advancing one \
                 occurrence lands at {next_anchor_if_skip_past_false}, still before \
                 completion_date ({completion_date}). Retry with skip_past: true to resume \
                 on schedule (jumps to {next_anchor_if_skip_past_true}), or skip_past: false \
                 to credit the next occurrence and catch up one step."
            ),
            DoneError::SkipPastUnnecessary {
                stored_anchor,
                next_anchor,
            } => format!(
                "skip_past was supplied but is not needed: stored anchor ({stored_anchor}) \
                 advances naturally to {next_anchor}, which is not before completion_date."
            ),
            DoneError::SkipPastOnSlipRule => {
                "skip_past has no meaning for a slip (bare FREQ) recurrence: the series \
                 always advances from completion_date."
                    .to_string()
            }
            DoneError::SkipPastOnNonRecurring => {
                "skip_past has no meaning for a non-recurring task.".to_string()
            }
            DoneError::AlreadyDoneOrNotDueYet {
                stored_on_date,
                completion_date,
            } => format!(
                "Task already has a stored completion date ({stored_on_date}); \
                 completion_date={completion_date} does not match. Pass \
                 replacing={stored_on_date} with your chosen completion_date to overwrite, \
                 or pass completion_date={stored_on_date} to keep it."
            ),
            DoneError::ReplacingOnRecurring => {
                "`replacing` is only valid on non-recurring tasks. Recurring tasks advance on \
                 each `done` and have nothing to replace."
                    .to_string()
            }
            DoneError::AlreadyTerminal { status } => {
                format!("Task already has terminal status {status}; no further mutations allowed.")
            }
            DoneError::Invalid(msg) => msg.clone(),
            DoneError::PathNotFound { reason, .. } => reason.clone(),
            DoneError::InvalidDate { reason } => reason.clone(),
            DoneError::Infra(e) => format!("{e:#}"),
        }
    }

    /// Render the `{error, reason, ...payload}` JSON envelope.
    pub fn to_json(&self) -> serde_json::Value {
        use serde_json::json;
        let mut obj = json!({
            "error": self.code(),
            "reason": self.reason(),
        });
        match self {
            DoneError::StaleAnchor {
                stored_anchor,
                completion_date,
                next_anchor_if_skip_past_false,
                next_anchor_if_skip_past_true,
            } => {
                obj["stored_anchor"] = json!(stored_anchor.to_string());
                obj["completion_date"] = json!(completion_date.to_string());
                obj["next_anchor_if_skip_past_false"] =
                    json!(next_anchor_if_skip_past_false.to_string());
                obj["next_anchor_if_skip_past_true"] =
                    json!(next_anchor_if_skip_past_true.to_string());
            }
            DoneError::SkipPastUnnecessary {
                stored_anchor,
                next_anchor,
            } => {
                obj["stored_anchor"] = json!(stored_anchor.to_string());
                obj["next_anchor"] = json!(next_anchor.to_string());
            }
            DoneError::AlreadyDoneOrNotDueYet {
                stored_on_date,
                completion_date,
            } => {
                obj["stored_on_date"] = json!(stored_on_date.to_string());
                obj["completion_date"] = json!(completion_date.to_string());
            }
            DoneError::AlreadyTerminal { status } => {
                obj["status"] = json!(status.to_string());
            }
            DoneError::PathNotFound { path, .. } => {
                obj["path"] = json!(path);
            }
            DoneError::SkipPastOnSlipRule
            | DoneError::SkipPastOnNonRecurring
            | DoneError::ReplacingOnRecurring
            | DoneError::Invalid(_)
            | DoneError::InvalidDate { .. }
            | DoneError::Infra(_) => {}
        }
        obj
    }
}

impl fmt::Display for DoneError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code(), self.reason())
    }
}

impl std::error::Error for DoneError {}

impl From<anyhow::Error> for DoneError {
    fn from(e: anyhow::Error) -> Self {
        DoneError::Infra(e)
    }
}

pub fn todo_done(opts: &DoneOptions) -> Result<DoneResult, DoneError> {
    if !crate::is_md(&opts.path) {
        return Err(DoneError::Invalid(format!(
            "path must end in .md: {}",
            opts.path.display()
        )));
    }

    let sync = db::open_and_sync(&opts.repo_dir).map_err(DoneError::Infra)?;
    // Distinguish file-not-found from lint/parse/validation failures.
    // File-not-found → PathNotFound (LLM-actionable: wrong path).
    // Lint/parse failures → Infra (must fix the document first).
    let abs_path = opts.repo_dir.join(&opts.path);
    if !abs_path.exists() {
        return Err(DoneError::PathNotFound {
            path: opts.path.display().to_string(),
            reason: format!("no such file: {}", abs_path.display()),
        });
    }
    let (content, fm) = read_and_validate(&opts.repo_dir, &opts.path).map_err(DoneError::Infra)?;

    // Reject already-terminal items with a structured code.
    if let Some(status) = fm.status.as_ref()
        && status.is_terminal()
    {
        return Err(DoneError::AlreadyTerminal {
            status: status.clone(),
        });
    }

    // Only the `updated` audit stamp uses the wall clock now. Completion
    // logic flows entirely from `opts.completion_date`.
    let now = format_tz_timestamp(&Clock::from_env().now());
    let shape = classify_shape(fm.effective_rrule());

    // Up-front shape-gating for options that only apply on specific shapes.
    if opts.skip_past.is_some() {
        match shape {
            TaskShape::NonRecurring => return Err(DoneError::SkipPastOnNonRecurring),
            TaskShape::Slip => return Err(DoneError::SkipPastOnSlipRule),
            TaskShape::Anchored => {}
        }
    }
    // `replacing` is only meaningful on non-recurring tasks (overwriting
    // a stored on_date/end_date). Reject up front on recurring shapes —
    // otherwise the slip/anchored branches would silently ignore it and
    // advance the task as if `replacing` were absent.
    if opts.replacing.is_some() && shape != TaskShape::NonRecurring {
        return Err(DoneError::ReplacingOnRecurring);
    }

    match shape {
        TaskShape::NonRecurring => {
            done_non_recurring(opts, &sync, &content, &fm, &now).map(|outcome| DoneResult {
                path: opts.path.clone(),
                outcome,
            })
        }
        TaskShape::Slip => done_slip(opts, &sync, &content, &fm, &now).map(|outcome| DoneResult {
            path: opts.path.clone(),
            outcome,
        }),
        TaskShape::Anchored => {
            done_anchored(opts, &sync, &content, &fm, &now).map(|outcome| DoneResult {
                path: opts.path.clone(),
                outcome,
            })
        }
    }
}

/// Non-recurring branch: write `status=done`, the completion date,
/// clear scheduling fields, clear sort_order, append a log entry
/// (Phase 2 — Phase 1 deferred this).
fn done_non_recurring(
    opts: &DoneOptions,
    sync: &db::SyncResult,
    content: &str,
    fm: &frontmatter::Frontmatter,
    now: &str,
) -> Result<DoneOutcome, DoneError> {
    let cd = resolve_completion_date(fm, opts.completion_date, opts.replacing)?;
    let mut mutations = vec![FieldMutation::AppendCompletionLog {
        entry: CompletionLogEntry {
            completed: opts.completion_date,
            occurrence: None,
            comment: opts.comment.clone(),
        },
    }];
    // `terminal_task_mutations` clears any prior `sort_order` (terminal
    // tasks rank by `on_date` in the query layer, not by sort_order).
    mutations.extend(terminal_task_mutations(Status::Done, &cd, now));
    let new_fm = mutate_and_write(&opts.repo_dir, &opts.path, content, &mutations)
        .map_err(DoneError::Infra)?;
    sync.cache
        .upsert_doc(&opts.path, &new_fm, true)
        .map_err(DoneError::Infra)?;
    Ok(DoneOutcome::Completed {
        completion: cd,
        terminal: false,
    })
}

/// Slip branch: advance the anchor by `completion_date + period`, clear
/// tentative_date, clear sort_order, append a log entry. Same-day repeat
/// is a no-op (`AlreadyDone`).
fn done_slip(
    opts: &DoneOptions,
    sync: &db::SyncResult,
    content: &str,
    fm: &frontmatter::Frontmatter,
    now: &str,
) -> Result<DoneOutcome, DoneError> {
    // Idempotency scan: if any existing entry has `completed == completion_date`,
    // short-circuit. Comment is discarded on no-op.
    if let Some(existing) = fm
        .completion_log
        .iter()
        .find(|e| e.completed == opts.completion_date)
    {
        return Ok(DoneOutcome::AlreadyDone {
            existing: existing.clone(),
            comment_discarded: opts.comment.is_some(),
        });
    }

    // Use `completion_date` as DTSTART. That makes the "next occurrence
    // after DTSTART" helper yield `completion_date + period`, which is
    // exactly the slip semantics.
    let rrule_str = fm
        .effective_rrule()
        .expect("classified as Slip implies effective_rrule is Some");
    let Some(new_anchor) =
        compute_next_occurrence(rrule_str, opts.completion_date).map_err(DoneError::Infra)?
    else {
        // Slip rule with a terminator (`COUNT` / `UNTIL`) has exhausted.
        // Treat as terminal completion. NOTE the asymmetry with the anchored
        // exhaustion branch below: slip exhaustion re-uses
        // `terminal_task_mutations`, which strips `CheckInDate`, `DueDate`,
        // `Rrule`, and `Recurrence`. Anchored exhaustion preserves all of
        // those: the recurrence block is retained and due_date /
        // check_in_date freeze at the final occurrence. Slip series don't
        // have a calendar-anchored "final occurrence" to freeze at — each
        // occurrence derives from the previous completion date, so once the
        // series ends there's no meaningful schedule state to keep.
        let cd = resolve_completion_date(fm, opts.completion_date, opts.replacing)?;
        let mut mutations = vec![FieldMutation::AppendCompletionLog {
            entry: CompletionLogEntry {
                completed: opts.completion_date,
                occurrence: None,
                comment: opts.comment.clone(),
            },
        }];
        // `terminal_task_mutations` also clears `sort_order` (terminal
        // tasks rank by `on_date` in the query layer).
        mutations.extend(terminal_task_mutations(Status::Done, &cd, now));
        let new_fm = mutate_and_write(&opts.repo_dir, &opts.path, content, &mutations)
            .map_err(DoneError::Infra)?;
        sync.cache
            .upsert_doc(&opts.path, &new_fm, true)
            .map_err(DoneError::Infra)?;
        return Ok(DoneOutcome::Completed {
            completion: cd,
            terminal: true,
        });
    };

    let mutations = slip_or_anchored_advance_mutations(
        fm,
        now,
        new_anchor,
        CompletionLogEntry {
            completed: opts.completion_date,
            occurrence: None,
            comment: opts.comment.clone(),
        },
    );
    let new_fm = mutate_and_write(&opts.repo_dir, &opts.path, content, &mutations)
        .map_err(DoneError::Infra)?;
    sync.cache
        .upsert_doc(&opts.path, &new_fm, true)
        .map_err(DoneError::Infra)?;

    let (next_check_in_date, next_due_date) = next_dates(&new_fm);
    Ok(DoneOutcome::Recurring {
        next_check_in_date,
        next_due_date,
    })
}

/// Anchored branch: advance one occurrence from the previous anchor. On
/// exhaustion, mark terminal but preserve the `recurrence` block and
/// freeze dates.
fn done_anchored(
    opts: &DoneOptions,
    sync: &db::SyncResult,
    content: &str,
    fm: &frontmatter::Frontmatter,
    now: &str,
) -> Result<DoneOutcome, DoneError> {
    let rrule_str = fm
        .effective_rrule()
        .expect("classified as Anchored implies effective_rrule is Some");
    // validate() guarantees recurring tasks carry due_date or check_in_date.
    let previous_anchor = fm.due_date.or(fm.check_in_date).ok_or_else(|| {
        DoneError::Invalid(format!(
            "{}: recurring task is missing both due_date and check_in_date",
            opts.path.display()
        ))
    })?;

    let naive_next =
        compute_next_occurrence(rrule_str, previous_anchor).map_err(DoneError::Infra)?;

    let Some(naive_next) = naive_next else {
        // Rrule exhaustion past previous_anchor: terminal completion.
        return anchored_terminal_completion(opts, sync, content, fm, now, previous_anchor);
    };

    // Non-terminal. Compare naive_next to completion_date.
    let (new_anchor, occurrence_credited) = if naive_next >= opts.completion_date {
        if opts.skip_past.is_some() {
            return Err(DoneError::SkipPastUnnecessary {
                stored_anchor: previous_anchor,
                next_anchor: naive_next,
            });
        }
        (naive_next, previous_anchor)
    } else {
        // naive_next < completion_date → stale-anchor window. Check
        // whether the rrule is also exhausted past completion_date; if
        // so, there is no "skip_past: true" target and advancing one
        // more step would land before completion_date. Force terminal
        // completion instead of offering a nonsensical choice.
        //
        // Iterate from the real DTSTART (previous_anchor), not from
        // completion_date — a finite rrule whose UNTIL is before
        // completion_date would make the rrule crate error on a
        // DTSTART > UNTIL seed.
        let next_past_completion =
            compute_occurrence_after(rrule_str, previous_anchor, opts.completion_date)
                .map_err(DoneError::Infra)?;
        let Some(skip_next) = next_past_completion else {
            // Finite rule exhausted past completion_date. The series is
            // over; treat as forced terminal completion. `skip_past`
            // is moot — accept either value (or None) without
            // complaint because there is no meaningful choice to make.
            return anchored_terminal_completion(opts, sync, content, fm, now, previous_anchor);
        };
        match opts.skip_past {
            None => {
                return Err(DoneError::StaleAnchor {
                    stored_anchor: previous_anchor,
                    completion_date: opts.completion_date,
                    next_anchor_if_skip_past_false: naive_next,
                    next_anchor_if_skip_past_true: skip_next,
                });
            }
            Some(false) => (naive_next, previous_anchor),
            Some(true) => (skip_next, opts.completion_date),
        }
    };

    let entry = CompletionLogEntry {
        completed: opts.completion_date,
        occurrence: Some(occurrence_credited),
        comment: opts.comment.clone(),
    };
    let mutations = slip_or_anchored_advance_mutations(fm, now, new_anchor, entry);
    let new_fm = mutate_and_write(&opts.repo_dir, &opts.path, content, &mutations)
        .map_err(DoneError::Infra)?;
    sync.cache
        .upsert_doc(&opts.path, &new_fm, true)
        .map_err(DoneError::Infra)?;

    let (next_check_in_date, next_due_date) = next_dates(&new_fm);
    Ok(DoneOutcome::Recurring {
        next_check_in_date,
        next_due_date,
    })
}

/// Anchored terminal completion: rrule is exhausted (either past
/// `previous_anchor` or past `completion_date`). Writes `status=done`,
/// the completion date, preserves `recurrence` / `check_in_date` /
/// `due_date` (they freeze at the final occurrence), clears sort_order,
/// bumps `updated`, appends a log entry crediting `previous_anchor` as
/// the occurrence.
fn anchored_terminal_completion(
    opts: &DoneOptions,
    sync: &db::SyncResult,
    content: &str,
    fm: &frontmatter::Frontmatter,
    now: &str,
    previous_anchor: NaiveDate,
) -> Result<DoneOutcome, DoneError> {
    let cd = resolve_completion_date(fm, opts.completion_date, opts.replacing)?;
    let mutations = vec![
        FieldMutation::AppendCompletionLog {
            entry: CompletionLogEntry {
                completed: opts.completion_date,
                occurrence: Some(previous_anchor),
                comment: opts.comment.clone(),
            },
        },
        FieldMutation::Set {
            field: Field::Status,
            value: Status::Done.to_string(),
        },
        cd.to_mutation(),
        FieldMutation::Remove {
            field: Field::TentativeDate,
        },
        // Preserve: CheckInDate, DueDate, Recurrence (+ legacy Rrule).
        // Clear sort_order: terminal tasks rank by on_date in the query
        // layer, and the only writer of sort_order is `todo_reorder`.
        FieldMutation::Remove {
            field: Field::SortOrder,
        },
        FieldMutation::Set {
            field: Field::Updated,
            value: format!("'{now}'"),
        },
    ];
    let new_fm = mutate_and_write(&opts.repo_dir, &opts.path, content, &mutations)
        .map_err(DoneError::Infra)?;
    sync.cache
        .upsert_doc(&opts.path, &new_fm, true)
        .map_err(DoneError::Infra)?;
    Ok(DoneOutcome::Completed {
        completion: cd,
        terminal: true,
    })
}

/// Shared mutation set for slip and anchored non-terminal advances.
/// Both shapes: anchor field write, `check_in_date = due_date - lead_days`
/// when `due_date` is the anchor, clear `tentative_date`, clear
/// `sort_order` (rank in the new bucket falls through to `priority`),
/// append log entry, bump `updated`.
fn slip_or_anchored_advance_mutations(
    fm: &frontmatter::Frontmatter,
    now: &str,
    new_anchor: NaiveDate,
    entry: CompletionLogEntry,
) -> Vec<FieldMutation> {
    let lead_days = fm
        .recurrence
        .as_ref()
        .and_then(|r| r.lead_days)
        .unwrap_or(0);
    // Anchor field: due_date if present, else check_in_date.
    let due_is_anchor = fm.due_date.is_some();
    let (new_check_in, due_mutation): (NaiveDate, Option<FieldMutation>) = if due_is_anchor {
        let derived = new_anchor - chrono::Duration::days(lead_days as i64);
        (
            derived,
            Some(FieldMutation::Set {
                field: Field::DueDate,
                value: format!("'{new_anchor}'"),
            }),
        )
    } else {
        (new_anchor, None)
    };

    let mut mutations = Vec::with_capacity(7);
    mutations.push(FieldMutation::AppendCompletionLog { entry });
    if let Some(m) = due_mutation {
        mutations.push(m);
    }
    // Always set check_in_date (either derived from due_date, or equal to
    // the new anchor when check_in_date alone carried the schedule).
    mutations.push(FieldMutation::Set {
        field: Field::CheckInDate,
        value: format!("'{new_check_in}'"),
    });
    mutations.push(FieldMutation::Remove {
        field: Field::TentativeDate,
    });
    // Clear any prior manual `sort_order`. The user's placement in the
    // old date bucket does not transfer to the new bucket; rank falls
    // through `COALESCE(sort_order, priority, UNRANKED)` to `priority`.
    // The only writer of sort_order is `todo_reorder`.
    mutations.push(FieldMutation::Remove {
        field: Field::SortOrder,
    });
    mutations.push(FieldMutation::Set {
        field: Field::Updated,
        value: format!("'{now}'"),
    });
    mutations
}

/// Extract `(next_check_in_date, next_due_date)` from the post-write fm
/// for the MCP response.
fn next_dates(fm: &frontmatter::Frontmatter) -> (NaiveDate, Option<NaiveDate>) {
    // validate() guarantees check_in_date is present after an advance
    // (we just wrote it).
    let check_in = fm
        .check_in_date
        .expect("post-advance fm must have check_in_date");
    (check_in, fm.due_date)
}

/// Resolve which date field to write (`on_date` vs `end_date`) and enforce
/// the `replacing` interlock.
///
/// Shape-agnostic: used by the non-recurring and anchored-exhaustion branches
/// of `todo_done` and by `todo_cancel`. The caller supplies the completion
/// date it wants to write (typically `opts.completion_date`; `todo_cancel`
/// defaults to today since it has no user-supplied date yet).
///
/// Returns a structured `DoneError` on conflict. `todo_cancel` adapts at the
/// call site by prefixing the path into the anyhow error; the structured
/// variant names (`AlreadyDoneOrNotDueYet`, `Invalid`) are meaningful to the
/// done path, and cancel callers see them stringified via `Display`.
///
/// Schema-invariant reliance: assumes the frontmatter has already passed lint
/// (via `read_and_validate`). In particular, lint rejects `on_date + start_date`
/// and `on_date + end_date` as mutually exclusive, so the relevant stored
/// field is unambiguous.
fn resolve_completion_date(
    fm: &frontmatter::Frontmatter,
    completion: NaiveDate,
    replacing: Option<NaiveDate>,
) -> Result<CompletionDate, DoneError> {
    let use_end_date = fm.start_date.is_some();
    let stored = if use_end_date {
        fm.end_date
    } else {
        fm.on_date
    };

    match (stored, replacing) {
        // Nothing stored, nothing to replace.
        (None, Some(_)) => Err(DoneError::Invalid(format!(
            "`replacing` supplied but no stored on_date/end_date to replace ({replacing:?})"
        ))),
        (None, None) => Ok(CompletionDate::from_date(use_end_date, completion)),
        // Stored value already equals what we'd write.
        (Some(s), _) if completion == s => {
            if replacing.is_some() {
                return Err(DoneError::Invalid(format!(
                    "`replacing` supplied but completion_date={completion} equals stored value ({s}); nothing to overwrite"
                )));
            }
            Ok(CompletionDate::from_date(use_end_date, completion))
        }
        // Stored value differs — must acknowledge via `replacing`.
        (Some(s), None) => Err(DoneError::AlreadyDoneOrNotDueYet {
            stored_on_date: s,
            completion_date: completion,
        }),
        (Some(s), Some(r)) if r != s => Err(DoneError::AlreadyDoneOrNotDueYet {
            stored_on_date: s,
            completion_date: completion,
        }),
        (Some(_), Some(_)) => Ok(CompletionDate::from_date(use_end_date, completion)),
    }
}

// ---------------------------------------------------------------------------
// Cancel
// ---------------------------------------------------------------------------

/// Options for `todo_cancel`.
pub struct CancelOptions {
    pub repo_dir: PathBuf,
    pub path: PathBuf,
    /// Completion date to record. Defaults to `today` (see below).
    pub date: Option<NaiveDate>,
    /// Acknowledges that a stored completion date (on_date or end_date)
    /// will be replaced. Must equal the stored value. Required when the
    /// stored value differs from `date`; forbidden otherwise.
    pub replacing: Option<NaiveDate>,
    /// See [`DoneOptions::today`].
    pub today: Option<NaiveDate>,
}

/// Result of `todo_cancel`.
pub struct CancelResult {
    pub path: PathBuf,
    pub date: CompletionDate,
}

pub fn todo_cancel(opts: &CancelOptions) -> Result<CancelResult> {
    ensure!(
        crate::is_md(&opts.path),
        "path must end in .md: {}",
        opts.path.display()
    );

    let sync = db::open_and_sync(&opts.repo_dir)?;
    let (content, fm) = read_and_validate(&opts.repo_dir, &opts.path)?;

    // Reject already-terminal items
    if fm.status.as_ref().is_some_and(|s| s.is_terminal()) {
        bail!(
            "{} already has status {}",
            opts.path.display(),
            fm.status.unwrap()
        );
    }

    // Snapshot — see same rationale in todo_done.
    let clock = Clock::from_env();
    let now_dt = clock.now();
    let today = opts.today.expect("today is required");
    let now = format_tz_timestamp(&now_dt);

    let completion = opts.date.unwrap_or(today);
    let cd = resolve_completion_date(&fm, completion, opts.replacing)
        .map_err(|e| anyhow::anyhow!("{}: {}", opts.path.display(), e))?;
    let mutations = terminal_task_mutations(Status::Cancelled, &cd, &now);
    let new_fm = mutate_and_write(&opts.repo_dir, &opts.path, &content, &mutations)?;
    sync.cache.upsert_doc(&opts.path, &new_fm, true)?;

    Ok(CancelResult {
        path: opts.path.clone(),
        date: cd,
    })
}

// ---------------------------------------------------------------------------
// Schedule
// ---------------------------------------------------------------------------

/// Options for `todo_schedule`.
pub struct ScheduleOptions {
    pub repo_dir: PathBuf,
    pub path: PathBuf,
    pub date: NaiveDate,
}

/// Result of `todo_schedule`.
pub struct ScheduleResult {
    pub path: PathBuf,
    pub tentative_date: NaiveDate,
}

/// Structured errors from `todo_schedule`. Mirrors `DoneError`'s shape;
/// rendered by `to_json()` as `{error, reason, ...payload}`.
#[derive(Debug)]
pub enum ScheduleError {
    /// Task already has a terminal status (`done` / `cancelled`). No
    /// further scheduling is allowed.
    TerminalTask { status: Status, path: PathBuf },
    /// `date` failed parse / was missing.
    InvalidDate { reason: String },
    /// Path does not resolve in the repo (file not found, malformed,
    /// lint errors, etc.).
    PathNotFound { path: String, reason: String },
    /// Low-level infrastructure failure (DB open, file I/O, etc.).
    /// Always propagates up to the caller as an anyhow Err.
    Infra(anyhow::Error),
}

impl ScheduleError {
    pub fn code(&self) -> &'static str {
        match self {
            ScheduleError::TerminalTask { .. } => "terminal_task",
            ScheduleError::InvalidDate { .. } => "invalid_date",
            ScheduleError::PathNotFound { .. } => "path_not_found",
            ScheduleError::Infra(_) => "infra",
        }
    }

    pub fn reason(&self) -> String {
        match self {
            ScheduleError::TerminalTask { status, path } => {
                format!("cannot schedule {}: status is {status}", path.display())
            }
            ScheduleError::InvalidDate { reason } => reason.clone(),
            ScheduleError::PathNotFound { reason, .. } => reason.clone(),
            ScheduleError::Infra(e) => format!("{e:#}"),
        }
    }

    /// Render the `{error, reason, ...payload}` JSON envelope.
    pub fn to_json(&self) -> serde_json::Value {
        use serde_json::json;
        let mut obj = json!({
            "error": self.code(),
            "reason": self.reason(),
        });
        match self {
            ScheduleError::TerminalTask { status, path } => {
                obj["status"] = json!(status.to_string());
                obj["path"] = json!(path.display().to_string());
            }
            ScheduleError::PathNotFound { path, .. } => {
                obj["path"] = json!(path);
            }
            ScheduleError::InvalidDate { .. } | ScheduleError::Infra(_) => {}
        }
        obj
    }
}

impl fmt::Display for ScheduleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code(), self.reason())
    }
}

impl std::error::Error for ScheduleError {}

impl From<anyhow::Error> for ScheduleError {
    fn from(e: anyhow::Error) -> Self {
        ScheduleError::Infra(e)
    }
}

pub fn todo_schedule(opts: &ScheduleOptions) -> Result<ScheduleResult, ScheduleError> {
    if !crate::is_md(&opts.path) {
        return Err(ScheduleError::PathNotFound {
            path: opts.path.display().to_string(),
            reason: format!("path must end in .md: {}", opts.path.display()),
        });
    }

    let sync = db::open_and_sync(&opts.repo_dir).map_err(ScheduleError::Infra)?;
    let (content, fm) = match read_and_validate(&opts.repo_dir, &opts.path) {
        Ok(v) => v,
        Err(e) => {
            return Err(ScheduleError::PathNotFound {
                path: opts.path.display().to_string(),
                reason: format!("{e:#}"),
            });
        }
    };

    // Reject terminal items
    if let Some(status) = fm.status.as_ref()
        && status.is_terminal()
    {
        return Err(ScheduleError::TerminalTask {
            status: status.clone(),
            path: opts.path.clone(),
        });
    }

    let now = format_tz_timestamp(&Clock::from_env().now());

    // Reschedule clears any prior manual `sort_order`. The user's manual
    // ordering in the old date bucket does not transfer; rank in the new
    // bucket falls through `COALESCE(sort_order, priority, UNRANKED)` to
    // `priority`. Reorder afterwards if a specific position is needed.
    let mutations = vec![
        FieldMutation::Set {
            field: Field::TentativeDate,
            value: format!("'{}'", opts.date),
        },
        FieldMutation::Remove {
            field: Field::SortOrder,
        },
        FieldMutation::Set {
            field: Field::Updated,
            value: format!("'{now}'"),
        },
    ];

    let new_fm = mutate_and_write(&opts.repo_dir, &opts.path, &content, &mutations)
        .map_err(ScheduleError::Infra)?;
    sync.cache
        .upsert_doc(&opts.path, &new_fm, true)
        .map_err(ScheduleError::Infra)?;

    Ok(ScheduleResult {
        path: opts.path.clone(),
        tentative_date: opts.date,
    })
}

// ---------------------------------------------------------------------------
// Reorder
// ---------------------------------------------------------------------------

/// Pre-resolved anchor data for reorder. The CLI/MCP layer resolves
/// `slug:path` or bare path strings into this before calling `todo_reorder`.
pub struct ReorderAnchor {
    pub sort_order: Option<f64>,
    pub priority: Option<u8>,
    pub effective_date: Option<NaiveDate>,
}

impl ReorderAnchor {
    /// Effective rank: `COALESCE(sort_order, priority, UNRANKED)`.
    /// Matches the SQL `ORDER BY` and `cli::rank()`.
    fn rank(&self) -> f64 {
        self.sort_order
            .or(self.priority.map(|p| p as f64))
            .unwrap_or(crate::frontmatter::UNRANKED)
    }
}

/// Options for `todo_reorder`.
pub struct ReorderOptions {
    pub repo_dir: PathBuf,
    pub path: PathBuf,
    pub after: Option<ReorderAnchor>,
    pub before: Option<ReorderAnchor>,
}

/// Result of `todo_reorder`.
pub struct ReorderResult {
    pub path: PathBuf,
    pub sort_order: f64,
    pub tentative_date: Option<NaiveDate>,
}

pub fn todo_reorder(opts: &ReorderOptions) -> Result<ReorderResult> {
    ensure!(
        crate::is_md(&opts.path),
        "path must end in .md: {}",
        opts.path.display()
    );
    ensure!(
        opts.after.is_some() || opts.before.is_some(),
        "at least one of --after or --before is required"
    );

    let sync = db::open_and_sync(&opts.repo_dir)?;
    let (content, fm) = read_and_validate(&opts.repo_dir, &opts.path)?;

    if fm.status.as_ref().is_some_and(|s| s.is_terminal()) {
        bail!(
            "cannot reorder {}: status is {}",
            opts.path.display(),
            fm.status.unwrap()
        );
    }

    let sort_order = match (&opts.after, &opts.before) {
        (Some(a), Some(b)) => {
            ensure!(
                b.rank() > a.rank(),
                "before anchor (rank {}) must rank below after anchor (rank {})",
                b.rank(),
                a.rank()
            );
            (a.rank() + b.rank()) / 2.0
        }
        (Some(a), None) => a.rank() + 1.0,
        (None, Some(b)) => b.rank() - 1.0,
        (None, None) => unreachable!("validated above"),
    };

    // Date inference: target inherits --after's effective_date (or --before's
    // if --after is absent).  When crossing a date boundary, we join the group
    // above (--after's group).
    let inferred_date = match (&opts.after, &opts.before) {
        (Some(a), _) => a.effective_date,
        (None, Some(b)) => b.effective_date,
        (None, None) => unreachable!(),
    };

    let now = format_tz_timestamp(&Clock::from_env().now());

    let mut mutations = vec![
        FieldMutation::Set {
            field: Field::SortOrder,
            value: format_sort_order(sort_order),
        },
        FieldMutation::Set {
            field: Field::Updated,
            value: format!("'{now}'"),
        },
    ];

    // Update tentative_date from inferred date
    match inferred_date {
        Some(date) => {
            mutations.push(FieldMutation::Set {
                field: Field::TentativeDate,
                value: format!("'{date}'"),
            });
        }
        None => {
            // Neighbors are unscheduled — clear target's tentative_date too
            mutations.push(FieldMutation::Remove {
                field: Field::TentativeDate,
            });
        }
    }

    let new_fm = mutate_and_write(&opts.repo_dir, &opts.path, &content, &mutations)?;
    sync.cache.upsert_doc(&opts.path, &new_fm, true)?;

    Ok(ReorderResult {
        path: opts.path.clone(),
        sort_order,
        tentative_date: inferred_date,
    })
}

// ---------------------------------------------------------------------------
// Recurrence helpers
// ---------------------------------------------------------------------------

/// Compute the next occurrence date from an RFC 5545 RRULE string.
///
/// Uses `dtstart_date` as the DTSTART (the current occurrence's scheduled date)
/// and returns the first occurrence strictly after DTSTART, or None if the rule
/// is exhausted.
fn compute_next_occurrence(rrule_str: &str, dtstart_date: NaiveDate) -> Result<Option<NaiveDate>> {
    // The rrule crate needs DateTime<Tz>, not NaiveDate.
    // noon-UTC is deliberate: sidesteps DST seams in the rrule crate.
    let noon = NaiveTime::from_hms_opt(12, 0, 0).unwrap();
    let dtstart = Utc
        .from_utc_datetime(&dtstart_date.and_time(noon))
        .with_timezone(&rrule::Tz::UTC);

    let full_str = format!(
        "DTSTART:{}\nRRULE:{rrule_str}",
        dtstart.format("%Y%m%dT%H%M%SZ")
    );
    let rrset: RRuleSet = full_str
        .parse()
        .with_context(|| format!("parsing rrule: {rrule_str}"))?;

    // The rrule crate includes dtstart itself as the first occurrence,
    // so we need at most 2 to find the one strictly after dtstart.
    let mut iter = rrset.into_iter();
    for occ in [iter.next(), iter.next()].into_iter().flatten() {
        let occ_date = occ.date_naive();
        if occ_date > dtstart_date {
            return Ok(Some(occ_date));
        }
    }

    Ok(None)
}

/// Upper bound on rrule iteration inside `compute_occurrence_after`.
/// Chosen to shrug off pathological rules (e.g. `FREQ=SECONDLY`) while
/// staying well above the realistic stale-anchor window (tens to low
/// hundreds of occurrences).
const MAX_RRULE_ITERATIONS: usize = 10_000;

/// Find the first occurrence of `rrule_str` (anchored at `dtstart`)
/// that is strictly after `threshold`. Returns `None` when the rrule
/// is exhausted with no occurrence past the threshold — whether via
/// `COUNT` / `UNTIL` terminator or empty rrule.
///
/// Unlike `compute_next_occurrence(rrule_str, threshold)`, this does
/// NOT reseed DTSTART to `threshold`. That matters for finite rules
/// where `threshold > UNTIL` — re-seeding would make the rrule crate
/// error on `DTSTART > UNTIL`; iterating from the real DTSTART lets
/// the crate correctly report "no more occurrences".
fn compute_occurrence_after(
    rrule_str: &str,
    dtstart_date: NaiveDate,
    threshold: NaiveDate,
) -> Result<Option<NaiveDate>> {
    let noon = NaiveTime::from_hms_opt(12, 0, 0).unwrap();
    let dtstart = Utc
        .from_utc_datetime(&dtstart_date.and_time(noon))
        .with_timezone(&rrule::Tz::UTC);
    let full_str = format!(
        "DTSTART:{}\nRRULE:{rrule_str}",
        dtstart.format("%Y%m%dT%H%M%SZ")
    );
    let rrset: RRuleSet = full_str
        .parse()
        .with_context(|| format!("parsing rrule: {rrule_str}"))?;

    // Safety bound: stale-anchor windows realistically produce tens to low
    // hundreds; MAX_RRULE_ITERATIONS protects against pathological rrule
    // inputs without artificially capping valid ones.
    for occ in rrset.into_iter().take(MAX_RRULE_ITERATIONS) {
        let occ_date = occ.date_naive();
        if occ_date > threshold {
            return Ok(Some(occ_date));
        }
    }
    Ok(None)
}

// ---------------------------------------------------------------------------
// Shared mutation helper
// ---------------------------------------------------------------------------

/// Build frontmatter mutations for transitioning a task to a terminal status:
/// set status + on_date/end_date, remove scheduling fields + rrule/recurrence,
/// update timestamp. Removing an absent field is a no-op, so always removing
/// both rrule shapes is safe.
fn terminal_task_mutations(
    status: Status,
    completion: &CompletionDate,
    now: &str,
) -> Vec<FieldMutation> {
    vec![
        FieldMutation::Set {
            field: Field::Status,
            value: status.to_string(),
        },
        completion.to_mutation(),
        FieldMutation::Remove {
            field: Field::TentativeDate,
        },
        FieldMutation::Remove {
            field: Field::CheckInDate,
        },
        FieldMutation::Remove {
            field: Field::DueDate,
        },
        FieldMutation::Remove {
            field: Field::Rrule,
        },
        FieldMutation::Remove {
            field: Field::Recurrence,
        },
        // Clear any prior manual sort_order. Terminal tasks sort by on_date
        // in the query layer, not by sort_order, so the value is dead
        // weight; clearing it keeps the only writer of sort_order
        // `todo_reorder` per the data-model invariant.
        FieldMutation::Remove {
            field: Field::SortOrder,
        },
        FieldMutation::Set {
            field: Field::Updated,
            value: format!("'{now}'"),
        },
    ]
}

/// Format a `DateTime<Tz>` as an ISO 8601 timestamp with the user's
/// IANA zone offset for YAML frontmatter (`created` / `updated`).
///
/// `created` / `updated` are audit-trail metadata — the hour-level
/// details aren't load-bearing, but the offset must at least reflect
/// the user's configured zone so the timestamps read sensibly.
fn format_tz_timestamp(dt: &chrono::DateTime<chrono_tz::Tz>) -> String {
    dt.format("%Y-%m-%dT%H:%M:%S%:z").to_string()
}

/// Format a sort_order for YAML: integer when possible, decimal otherwise.
fn format_sort_order(v: f64) -> String {
    if v.fract() == 0.0 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

/// Read a file, parse and validate its frontmatter, then run lint rules.
/// Mutation commands use this to refuse to operate on invalid documents.
/// Returns the raw content and parsed Frontmatter for reuse by the caller.
fn read_and_validate(
    repo_dir: &Path,
    rel_path: &Path,
) -> Result<(String, frontmatter::Frontmatter)> {
    let abs_path = repo_dir.join(rel_path);
    let content =
        fs::read_to_string(&abs_path).with_context(|| format!("reading {}", rel_path.display()))?;
    let fm = frontmatter::parse_frontmatter(&content)
        .with_context(|| format!("parsing {}", rel_path.display()))?
        .with_context(|| format!("missing frontmatter in {}", rel_path.display()))?;
    fm.validate()
        .with_context(|| format!("invalid frontmatter in {}", rel_path.display()))?;

    // Lint-gate: refuse to mutate docs that fail any lint rule.
    let lint_errors = crate::lint::check_rules(rel_path, &fm);
    if !lint_errors.is_empty() {
        let messages: Vec<&str> = lint_errors.iter().map(|e| e.message.as_str()).collect();
        bail!(
            "{} has lint errors, fix before mutating: {}",
            rel_path.display(),
            messages.join("; ")
        );
    }

    Ok((content, fm))
}

/// Apply field mutations to file content, write it back, re-parse and
/// validate.  Takes the already-read content to avoid a redundant read.
fn mutate_and_write(
    repo_dir: &Path,
    rel_path: &Path,
    content: &str,
    mutations: &[FieldMutation],
) -> Result<frontmatter::Frontmatter> {
    let abs_path = repo_dir.join(rel_path);
    let new_content = frontmatter::rewrite_frontmatter(content, mutations)?;
    fs::write(&abs_path, &new_content)
        .with_context(|| format!("writing {}", rel_path.display()))?;
    // Re-parse from disk to confirm write landed correctly
    let fm = crate::frontmatter::parse_file(&abs_path)
        .with_context(|| format!("re-parsing {}", rel_path.display()))?
        .with_context(|| format!("missing frontmatter after rewrite: {}", rel_path.display()))?;
    fm.validate()
        .with_context(|| format!("validation failed after rewrite: {}", rel_path.display()))?;
    Ok(fm)
}

// ---------------------------------------------------------------------------
// Reindex
// ---------------------------------------------------------------------------

/// Result of a full reindex.
pub struct ReindexResult {
    pub files_indexed: usize,
    pub errors: Vec<FileError>,
}

pub fn reindex(repo_dir: &Path) -> Result<ReindexResult> {
    let sync = db::reindex(repo_dir)?;

    // Count indexed files (SELECT COUNT from the DB)
    let files_indexed: usize = sync.cache.doc_count()?;

    Ok(ReindexResult {
        files_indexed,
        errors: sync.errors,
    })
}

// ---------------------------------------------------------------------------
// Human-readable formatting
// ---------------------------------------------------------------------------

/// Format query results for TTY output, grouped by effective date.
///
/// When `multi_repo` is true, each task line is prefixed with the repo slug.
pub fn format_human(result: &TodoQueryResult, today: NaiveDate, multi_repo: bool) -> String {
    let mut out = String::new();
    let mut current_date: Option<NaiveDate> = None;

    for task in &result.tasks {
        let task_date = task.effective_date;
        if current_date != Some(task_date) {
            if current_date.is_some() {
                out.push('\n');
            }
            current_date = Some(task_date);
            let rel = format_relative_date(task_date, today);
            out.push_str(&format!("{task_date} ({rel})\n"));
        }

        let status_tag = match &task.status {
            Some(Status::Todo) => "todo",
            Some(Status::InProgress) => "ip",
            Some(Status::Done) => "done",
            Some(Status::Cancelled) => "canc",
            Some(Status::Reminder) => "rem",
            None => "?",
        };
        let priority = task
            .priority
            .map(|p| format!("P{p}"))
            .unwrap_or_else(|| "  ".to_string());

        if multi_repo {
            let repo_tag = task.repo.as_ref().map(|s| s.as_str()).unwrap_or("?");
            out.push_str(&format!(
                "  {repo_tag:<8}  [{status_tag:<4}]  {priority}  {:<40}  {}\n",
                task.tldr, task.path
            ));
        } else {
            out.push_str(&format!(
                "  [{status_tag:<4}]  {priority}  {:<40}  {}\n",
                task.tldr, task.path
            ));
        }
    }

    // Lint errors
    if !result.lint_errors.is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str("Lint errors:\n");
        for err in &result.lint_errors {
            if multi_repo {
                let repo_tag = err.repo.as_ref().map(|s| s.as_str()).unwrap_or("?");
                out.push_str(&format!("  {repo_tag}: {}: {}\n", err.path, err.message));
            } else {
                out.push_str(&format!("  {}: {}\n", err.path, err.message));
            }
        }
    }

    out
}

fn format_relative_date(date: NaiveDate, today: NaiveDate) -> String {
    let diff = (date - today).num_days();
    match diff {
        0 => "today".to_string(),
        1 => "tomorrow".to_string(),
        -1 => "yesterday".to_string(),
        d if d > 1 => format!("in {d} days"),
        d => format!("{} days ago", -d),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchor_rank_uses_sort_order_first() {
        let a = ReorderAnchor {
            sort_order: Some(7.0),
            priority: Some(2),
            effective_date: None,
        };
        assert_eq!(a.rank(), 7.0);
    }

    #[test]
    fn anchor_rank_falls_back_to_priority() {
        let a = ReorderAnchor {
            sort_order: None,
            priority: Some(3),
            effective_date: None,
        };
        assert_eq!(a.rank(), 3.0);
    }

    #[test]
    fn anchor_rank_falls_back_to_unranked() {
        let a = ReorderAnchor {
            sort_order: None,
            priority: None,
            effective_date: None,
        };
        assert_eq!(a.rank(), crate::frontmatter::UNRANKED);
    }

    #[test]
    fn format_relative_date_today() {
        let today = NaiveDate::from_ymd_opt(2026, 4, 11).unwrap();
        assert_eq!(format_relative_date(today, today), "today");
    }

    #[test]
    fn format_relative_date_tomorrow() {
        let today = NaiveDate::from_ymd_opt(2026, 4, 11).unwrap();
        let tomorrow = NaiveDate::from_ymd_opt(2026, 4, 12).unwrap();
        assert_eq!(format_relative_date(tomorrow, today), "tomorrow");
    }

    #[test]
    fn format_relative_date_yesterday() {
        let today = NaiveDate::from_ymd_opt(2026, 4, 11).unwrap();
        let yesterday = NaiveDate::from_ymd_opt(2026, 4, 10).unwrap();
        assert_eq!(format_relative_date(yesterday, today), "yesterday");
    }

    #[test]
    fn format_relative_date_past() {
        let today = NaiveDate::from_ymd_opt(2026, 4, 11).unwrap();
        let past = NaiveDate::from_ymd_opt(2026, 4, 8).unwrap();
        assert_eq!(format_relative_date(past, today), "3 days ago");
    }

    #[test]
    fn format_relative_date_future() {
        let today = NaiveDate::from_ymd_opt(2026, 4, 11).unwrap();
        let future = NaiveDate::from_ymd_opt(2026, 4, 15).unwrap();
        assert_eq!(format_relative_date(future, today), "in 4 days");
    }

    #[test]
    fn format_human_grouped_by_date() {
        let today = NaiveDate::from_ymd_opt(2026, 4, 11).unwrap();
        let later = NaiveDate::from_ymd_opt(2026, 4, 15).unwrap();
        let result = TodoQueryResult {
            domains: None,
            tasks: vec![
                TodoItem {
                    path: "todo/a.md".to_string(),
                    repo: None,
                    domain: None,
                    tldr: "Task A".to_string(),
                    status: Some(Status::Todo),
                    effective_date: today,
                    tentative_date: Some(today),
                    check_in_date: None,
                    due_date: None,
                    sort_order: Some(1.0),
                    priority: Some(2),
                    effort: None,
                    rrule: None,
                    labels: vec![],
                },
                TodoItem {
                    path: "todo/b.md".to_string(),
                    repo: None,
                    domain: None,
                    tldr: "Task B".to_string(),
                    status: Some(Status::InProgress),
                    effective_date: today,
                    tentative_date: None,
                    check_in_date: Some(today),
                    due_date: None,
                    sort_order: None,
                    priority: Some(1),
                    effort: None,
                    rrule: None,
                    labels: vec![],
                },
                TodoItem {
                    path: "todo/c.md".to_string(),
                    repo: None,
                    domain: None,
                    tldr: "Task C".to_string(),
                    status: Some(Status::Todo),
                    effective_date: later,
                    tentative_date: None,
                    check_in_date: Some(later),
                    due_date: None,
                    sort_order: None,
                    priority: Some(3),
                    effort: None,
                    rrule: None,
                    labels: vec![],
                },
            ],
            lint_errors: vec![],
        };

        let output = format_human(&result, today, false);
        assert!(output.contains("2026-04-11 (today)"));
        assert!(output.contains("2026-04-15 (in 4 days)"));
        assert!(output.contains("Task A"));
        assert!(output.contains("Task B"));
        assert!(output.contains("Task C"));
        // Today's tasks appear before later-dated tasks.
        let today_pos = output.find("(today)").unwrap();
        let later_pos = output.find("(in 4 days)").unwrap();
        assert!(today_pos < later_pos);
    }

    #[test]
    fn format_human_with_lint_errors() {
        let today = NaiveDate::from_ymd_opt(2026, 4, 11).unwrap();
        let result = TodoQueryResult {
            domains: None,
            tasks: vec![],
            lint_errors: vec![TodoError {
                path: "todo/bad.md".to_string(),
                repo: None,
                message: "done item has check_in_date set".to_string(),
            }],
        };
        let output = format_human(&result, today, false);
        assert!(output.contains("Lint errors:"));
        assert!(output.contains("todo/bad.md"));
    }

    #[test]
    fn effort_display() {
        assert_eq!(Effort::Trivial.to_string(), "trivial");
        assert_eq!(Effort::Small.to_string(), "small");
        assert_eq!(Effort::Medium.to_string(), "medium");
        assert_eq!(Effort::Large.to_string(), "large");
        assert_eq!(Effort::Epic.to_string(), "epic");
    }

    // -----------------------------------------------------------------------
    // Recurrence helper tests
    // -----------------------------------------------------------------------

    #[test]
    fn compute_next_monthly() {
        let dtstart = NaiveDate::from_ymd_opt(2026, 4, 1).unwrap();
        let next = compute_next_occurrence("FREQ=MONTHLY", dtstart)
            .unwrap()
            .unwrap();
        assert_eq!(next, NaiveDate::from_ymd_opt(2026, 5, 1).unwrap());
    }

    #[test]
    fn compute_next_weekly() {
        let dtstart = NaiveDate::from_ymd_opt(2026, 4, 6).unwrap(); // Monday
        let next = compute_next_occurrence("FREQ=WEEKLY", dtstart)
            .unwrap()
            .unwrap();
        assert_eq!(next, NaiveDate::from_ymd_opt(2026, 4, 13).unwrap());
    }

    #[test]
    fn compute_next_weekly_byday() {
        let dtstart = NaiveDate::from_ymd_opt(2026, 4, 11).unwrap(); // Saturday
        let next = compute_next_occurrence("FREQ=WEEKLY;BYDAY=SA", dtstart)
            .unwrap()
            .unwrap();
        assert_eq!(next, NaiveDate::from_ymd_opt(2026, 4, 18).unwrap());
    }

    #[test]
    fn compute_next_exhausted_count() {
        // COUNT=1 means only one occurrence (the DTSTART itself), so no next
        let dtstart = NaiveDate::from_ymd_opt(2026, 4, 1).unwrap();
        let next = compute_next_occurrence("FREQ=MONTHLY;COUNT=1", dtstart).unwrap();
        assert!(next.is_none());
    }

    #[test]
    fn compute_next_count_two() {
        // COUNT=2 means DTSTART + one more
        let dtstart = NaiveDate::from_ymd_opt(2026, 4, 1).unwrap();
        let next = compute_next_occurrence("FREQ=MONTHLY;COUNT=2", dtstart)
            .unwrap()
            .unwrap();
        assert_eq!(next, NaiveDate::from_ymd_opt(2026, 5, 1).unwrap());
    }

    #[test]
    fn compute_next_invalid_rrule() {
        let dtstart = NaiveDate::from_ymd_opt(2026, 4, 1).unwrap();
        let result = compute_next_occurrence("FREQ=BOGUS", dtstart);
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------------
    // todo_done: frontmatter-list completion log + recurring semantics
    // -----------------------------------------------------------------------

    use tempfile::TempDir;

    fn run_git(dir: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git invocation");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn setup_repo() -> TempDir {
        let dir = TempDir::new().unwrap();
        run_git(dir.path(), &["init", "-b", "main"]);
        run_git(dir.path(), &["config", "user.email", "test@test.com"]);
        run_git(dir.path(), &["config", "user.name", "Test"]);
        run_git(dir.path(), &["commit", "--allow-empty", "-m", "init"]);
        dir
    }

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    fn write_task(dir: &TempDir, rel: &str, content: &str) -> PathBuf {
        let path = PathBuf::from(rel);
        let abs = dir.path().join(&path);
        if let Some(p) = abs.parent() {
            fs::create_dir_all(p).unwrap();
        }
        fs::write(&abs, content).unwrap();
        path
    }

    fn do_done(
        dir: &TempDir,
        path: &Path,
        completion_date: NaiveDate,
    ) -> std::result::Result<DoneResult, DoneError> {
        todo_done(&DoneOptions {
            repo_dir: dir.path().to_path_buf(),
            path: path.to_path_buf(),
            completion_date,
            replacing: None,
            skip_past: None,
            comment: None,
        })
    }

    #[test]
    fn todo_done_slip_appends_frontmatter_log_entry() {
        let dir = setup_repo();
        let path = write_task(
            &dir,
            "todo/task.md",
            "---\ntldr: test\nstatus: todo\ncheck_in_date: 2026-04-18\nrecurrence:\n  rrule: \"FREQ=WEEKLY\"\ntentative_date: 2026-04-18\n---\nBody.\n",
        );

        let completion = d(2026, 4, 18);
        let result = do_done(&dir, &path, completion).unwrap();
        match result.outcome {
            DoneOutcome::Recurring {
                next_check_in_date,
                next_due_date,
            } => {
                // Slip: completion_date + 1 week.
                assert_eq!(next_check_in_date, d(2026, 4, 25));
                assert!(next_due_date.is_none());
            }
            _ => panic!("expected Recurring"),
        }

        let content = fs::read_to_string(dir.path().join(&path)).unwrap();
        assert!(content.contains("completion_log:"));
        assert!(content.contains(&format!("completed: {completion}")));
        assert!(!content.contains("tentative_date"));
        assert!(!content.contains("## Completion Log"));
    }

    #[test]
    fn todo_done_non_recurring_writes_a_log_entry() {
        let dir = setup_repo();
        let path = write_task(
            &dir,
            "todo/task.md",
            "---\ntldr: test\nstatus: todo\ncheck_in_date: 2026-04-18\n---\nBody.\n",
        );

        let completion = d(2026, 4, 18);
        do_done(&dir, &path, completion).unwrap();

        let content = fs::read_to_string(dir.path().join(&path)).unwrap();
        assert!(
            content.contains("completion_log:"),
            "Phase 2: non-recurring done writes a log entry; got:\n{content}"
        );
        assert!(content.contains(&format!("completed: {completion}")));
        assert!(content.contains("status: done"));
    }

    #[test]
    fn todo_cancel_strips_recurrence_and_rrule() {
        let dir = setup_repo();
        let path = write_task(
            &dir,
            "todo/task.md",
            "---\ntldr: test\nstatus: todo\ncheck_in_date: 2026-04-18\nrrule: \"FREQ=WEEKLY\"\nrecurrence:\n  rrule: \"FREQ=WEEKLY\"\n---\nBody.\n",
        );

        todo_cancel(&CancelOptions {
            repo_dir: dir.path().to_path_buf(),
            path: path.clone(),
            date: None,
            replacing: None,
            today: Some(d(2026, 4, 18)),
        })
        .unwrap();

        let content = fs::read_to_string(dir.path().join(&path)).unwrap();
        assert!(content.contains("status: cancelled"));
        assert!(
            !content.contains("rrule:"),
            "rrule not stripped:\n{content}"
        );
        assert!(
            !content.contains("recurrence:"),
            "recurrence not stripped:\n{content}"
        );
    }

    #[test]
    fn todo_add_with_recurrence_roundtrips() {
        let dir = setup_repo();
        let result = todo_add(&AddOptions {
            repo_dir: dir.path().to_path_buf(),
            path: PathBuf::from("todo/task.md"),
            title: "Test".into(),
            priority: 3,
            due_date: Some(NaiveDate::from_ymd_opt(2026, 4, 25).unwrap()),
            recurrence: Some(Recurrence {
                rrule: "FREQ=WEEKLY;BYDAY=SA".to_string(),
                lead_days: Some(7),
            }),
            ..Default::default()
        })
        .unwrap();
        let content = fs::read_to_string(dir.path().join(&result.path)).unwrap();

        // Structural round-trip check: parse + validate + inspect the fields.
        // Exact serialization shape (quoted vs bare scalar) is left to
        // `yaml_scalar`.
        let fm = crate::frontmatter::parse_frontmatter(&content)
            .unwrap()
            .unwrap();
        fm.validate().unwrap();
        assert_eq!(
            fm.recurrence.as_ref().unwrap().rrule,
            "FREQ=WEEKLY;BYDAY=SA"
        );
        assert_eq!(fm.recurrence.as_ref().unwrap().lead_days, Some(7));
    }

    #[test]
    fn todo_add_lead_days_without_due_date_errors() {
        let dir = setup_repo();
        let result = todo_add(&AddOptions {
            repo_dir: dir.path().to_path_buf(),
            path: PathBuf::from("todo/task.md"),
            title: "Test".into(),
            priority: 3,
            check_in_date: Some(NaiveDate::from_ymd_opt(2026, 4, 25).unwrap()),
            recurrence: Some(Recurrence {
                rrule: "FREQ=WEEKLY".to_string(),
                lead_days: Some(7),
            }),
            ..Default::default()
        });
        assert!(result.is_err(), "expected validate() rejection");
    }

    // -----------------------------------------------------------------------
    // AddOptions — new fields (body + extra frontmatter)
    // -----------------------------------------------------------------------

    /// Helper: create a minimal valid AddOptions with check_in_date set.
    fn minimal_add(dir: &TempDir, path: &str) -> AddOptions {
        AddOptions {
            repo_dir: dir.path().to_path_buf(),
            path: PathBuf::from(path),
            title: "Test task".into(),
            priority: 3,
            check_in_date: Some(d(2026, 5, 1)),
            ..Default::default()
        }
    }

    #[test]
    fn todo_add_with_body_strips_and_normalizes() {
        let dir = setup_repo();
        let opts = AddOptions {
            body: Some("  hello\n\n  ".into()),
            ..minimal_add(&dir, "todo/t.md")
        };
        let result = todo_add(&opts).unwrap();
        let content = fs::read_to_string(dir.path().join(&result.path)).unwrap();
        assert!(
            content.ends_with("---\nhello\n"),
            "expected body after frontmatter, got:\n{content}"
        );
    }

    #[test]
    fn todo_add_body_whitespace_only_omitted() {
        let dir = setup_repo();
        let opts = AddOptions {
            body: Some("   \n\n  ".into()),
            ..minimal_add(&dir, "todo/t.md")
        };
        let result = todo_add(&opts).unwrap();
        let content = fs::read_to_string(dir.path().join(&result.path)).unwrap();
        assert!(
            content.ends_with("---\n"),
            "whitespace-only body should produce no body section, got:\n{content}"
        );
    }

    #[test]
    fn todo_add_with_body_containing_dash_lines() {
        let dir = setup_repo();
        let body = "intro\n---\nstill body\n";
        let opts = AddOptions {
            body: Some(body.into()),
            ..minimal_add(&dir, "todo/t.md")
        };
        let result = todo_add(&opts).unwrap();
        let abs = dir.path().join(&result.path);
        let content = fs::read_to_string(&abs).unwrap();
        // Frontmatter must still parse correctly
        let fm = crate::frontmatter::parse_frontmatter(&content)
            .unwrap()
            .unwrap();
        fm.validate().unwrap();
        assert_eq!(fm.tldr, "Test task");
        // The body --- should be present in the file but after the frontmatter
        assert!(content.contains("---\nintro\n---\nstill body\n"));
    }

    #[test]
    fn todo_add_with_summary() {
        let dir = setup_repo();
        let opts = AddOptions {
            summary: Some("A brief summary".into()),
            ..minimal_add(&dir, "todo/t.md")
        };
        let result = todo_add(&opts).unwrap();
        let abs = dir.path().join(&result.path);
        let fm = crate::frontmatter::parse_file(&abs).unwrap().unwrap();
        fm.validate().unwrap();
        assert_eq!(fm.summary.as_deref(), Some("A brief summary"));
    }

    #[test]
    fn todo_add_with_context_link() {
        let dir = setup_repo();
        let opts = AddOptions {
            context_link: Some("projects/alpha.md".into()),
            ..minimal_add(&dir, "todo/t.md")
        };
        let result = todo_add(&opts).unwrap();
        let abs = dir.path().join(&result.path);
        let fm = crate::frontmatter::parse_file(&abs).unwrap().unwrap();
        fm.validate().unwrap();
        assert_eq!(fm.context_link.as_deref(), Some("projects/alpha.md"));
    }

    #[test]
    fn todo_add_with_links() {
        let dir = setup_repo();
        let opts = AddOptions {
            links: vec!["docs/a.md".into(), "docs/b.md".into()],
            ..minimal_add(&dir, "todo/t.md")
        };
        let result = todo_add(&opts).unwrap();
        let abs = dir.path().join(&result.path);
        let fm = crate::frontmatter::parse_file(&abs).unwrap().unwrap();
        fm.validate().unwrap();
        assert_eq!(fm.links, vec!["docs/a.md", "docs/b.md"]);
    }

    #[test]
    fn todo_add_with_blocked_by() {
        let dir = setup_repo();
        let opts = AddOptions {
            blocked_by: vec!["todo/blocker.md".into()],
            ..minimal_add(&dir, "todo/t.md")
        };
        let result = todo_add(&opts).unwrap();
        let abs = dir.path().join(&result.path);
        let fm = crate::frontmatter::parse_file(&abs).unwrap().unwrap();
        fm.validate().unwrap();
        assert_eq!(fm.blocked_by, vec!["todo/blocker.md"]);
    }

    #[test]
    fn todo_add_with_assigned_to() {
        let dir = setup_repo();
        let opts = AddOptions {
            assigned_to: Some("alice".into()),
            ..minimal_add(&dir, "todo/t.md")
        };
        let result = todo_add(&opts).unwrap();
        let abs = dir.path().join(&result.path);
        let fm = crate::frontmatter::parse_file(&abs).unwrap().unwrap();
        fm.validate().unwrap();
        assert_eq!(fm.assigned_to.as_deref(), Some("alice"));
    }

    #[test]
    fn todo_add_with_on_date() {
        let dir = setup_repo();
        let opts = AddOptions {
            on_date: Some(d(2026, 6, 15)),
            ..minimal_add(&dir, "todo/t.md")
        };
        let result = todo_add(&opts).unwrap();
        let abs = dir.path().join(&result.path);
        let fm = crate::frontmatter::parse_file(&abs).unwrap().unwrap();
        fm.validate().unwrap();
        assert_eq!(fm.on_date, Some(d(2026, 6, 15)));
    }

    #[test]
    fn todo_add_with_start_date() {
        let dir = setup_repo();
        let opts = AddOptions {
            start_date: Some(d(2026, 6, 1)),
            ..minimal_add(&dir, "todo/t.md")
        };
        let result = todo_add(&opts).unwrap();
        let abs = dir.path().join(&result.path);
        let fm = crate::frontmatter::parse_file(&abs).unwrap().unwrap();
        fm.validate().unwrap();
        assert_eq!(fm.start_date, Some(d(2026, 6, 1)));
    }

    #[test]
    fn todo_add_with_end_date() {
        let dir = setup_repo();
        let opts = AddOptions {
            end_date: Some(d(2026, 6, 30)),
            ..minimal_add(&dir, "todo/t.md")
        };
        let result = todo_add(&opts).unwrap();
        let abs = dir.path().join(&result.path);
        let fm = crate::frontmatter::parse_file(&abs).unwrap().unwrap();
        fm.validate().unwrap();
        assert_eq!(fm.end_date, Some(d(2026, 6, 30)));
    }

    #[test]
    fn todo_add_with_verified_date() {
        let dir = setup_repo();
        let opts = AddOptions {
            verified_date: Some(d(2026, 5, 20)),
            ..minimal_add(&dir, "todo/t.md")
        };
        let result = todo_add(&opts).unwrap();
        let abs = dir.path().join(&result.path);
        let fm = crate::frontmatter::parse_file(&abs).unwrap().unwrap();
        fm.validate().unwrap();
        assert_eq!(fm.verified_date, Some(d(2026, 5, 20)));
    }

    #[test]
    fn todo_add_with_tentative_date() {
        let dir = setup_repo();
        let opts = AddOptions {
            tentative_date: Some(d(2026, 7, 4)),
            ..minimal_add(&dir, "todo/t.md")
        };
        let result = todo_add(&opts).unwrap();
        let abs = dir.path().join(&result.path);
        let fm = crate::frontmatter::parse_file(&abs).unwrap().unwrap();
        fm.validate().unwrap();
        assert_eq!(fm.tentative_date, Some(d(2026, 7, 4)));
    }

    #[test]
    fn todo_add_default_body_byte_identical_format() {
        // When all new fields are omitted/default, the file content should match
        // the expected format byte-for-byte (modulo clock-derived created/updated).
        let dir = setup_repo();
        let opts = minimal_add(&dir, "todo/t.md");
        let result = todo_add(&opts).unwrap();
        let content = fs::read_to_string(dir.path().join(&result.path)).unwrap();
        // Extract the created/updated values from the written file so we can
        // substitute them into a fixed template.
        let created = content
            .lines()
            .find_map(|l| l.strip_prefix("created: '"))
            .and_then(|s| s.strip_suffix('\''))
            .expect("created line");
        let updated = content
            .lines()
            .find_map(|l| l.strip_prefix("updated: '"))
            .and_then(|s| s.strip_suffix('\''))
            .expect("updated line");
        let expected = format!(
            "---\ntldr: \"Test task\"\nstatus: todo\npriority: 3\ncheck_in_date: '2026-05-01'\ncreated: '{created}'\nupdated: '{updated}'\n---\n"
        );
        assert_eq!(
            content, expected,
            "byte-identical format check failed; got:\n{content}"
        );
    }

    #[test]
    fn todo_add_body_with_special_yaml_chars_in_strings() {
        let dir = setup_repo();
        let opts = AddOptions {
            summary: Some("x: y # z".into()),
            context_link: Some("http://example.com/a#b".into()),
            links: vec!["docs/x: y # z.md".into()],
            blocked_by: vec!["todo/dep: thing.md".into()],
            ..minimal_add(&dir, "todo/t.md")
        };
        let result = todo_add(&opts).unwrap();
        let abs = dir.path().join(&result.path);
        let fm = crate::frontmatter::parse_file(&abs).unwrap().unwrap();
        fm.validate().unwrap();
        assert_eq!(fm.summary.as_deref(), Some("x: y # z"));
        assert_eq!(fm.context_link.as_deref(), Some("http://example.com/a#b"));
        assert_eq!(fm.links, vec!["docs/x: y # z.md"]);
        assert_eq!(fm.blocked_by, vec!["todo/dep: thing.md"]);
    }

    #[test]
    fn todo_query_on_legacy_file_returns_data_and_leaves_file_unchanged() {
        // Once the cache is primed (schema matches), routine tool
        // use goes through `sync`, not `do_reindex`. A legacy-shape file
        // introduced after initial reindex must still be queryable through
        // `todo_query` (lenient parse) and must not be rewritten on disk.
        let dir = setup_repo();
        // First, prime the cache with a placeholder so the initial reindex
        // runs on a repo that needs no migration.
        write_task(
            &dir,
            "todo/placeholder.md",
            "---\ntldr: placeholder\nstatus: todo\ncheck_in_date: 2026-04-18\n---\n",
        );
        run_git(dir.path(), &["add", "-A"]);
        run_git(dir.path(), &["commit", "-m", "placeholder"]);
        todo_query(&QueryOptions {
            repo_dir: dir.path().to_path_buf(),
            repo_slug: None,
            repo_domain: None,
            horizon_days: None,
            include_done: false,
            statuses: vec![],
            labels: vec![],
            limit: None,
        })
        .unwrap();

        // Now drop in a legacy-shape file. The next `todo_query` goes
        // through `sync` (schema matches) and must not migrate.
        let legacy_content = "---\ntldr: Legacy Weekly\nstatus: todo\ncheck_in_date: 2026-04-18\nrrule: \"FREQ=WEEKLY\"\ntentative_date: 2026-04-18\n---\nBody.\n";
        let path = write_task(&dir, "todo/legacy.md", legacy_content);
        run_git(dir.path(), &["add", "-A"]);
        run_git(dir.path(), &["commit", "-m", "add legacy"]);
        let before = fs::read_to_string(dir.path().join(&path)).unwrap();

        let result = todo_query(&QueryOptions {
            repo_dir: dir.path().to_path_buf(),
            repo_slug: None,
            repo_domain: None,
            horizon_days: None,
            include_done: false,
            statuses: vec![],
            labels: vec![],
            limit: None,
        })
        .unwrap();

        let after = fs::read_to_string(dir.path().join(&path)).unwrap();
        assert_eq!(before, after, "sync must not migrate legacy-shape files");
        let task = result
            .tasks
            .iter()
            .find(|t| t.path.ends_with("legacy.md"))
            .expect("legacy task surfaced through lenient parse");
        // effective_rrule is populated from the flat rrule on disk.
        assert_eq!(task.rrule.as_deref(), Some("FREQ=WEEKLY"));
    }

    #[test]
    fn todo_schedule_on_recurring_with_tentative_date_overwrites() {
        // Schedule on a recurring task with tentative_date set
        // must overwrite cleanly (tentative_date is re-admitted on recurring
        // tasks in Phase 1).
        let dir = setup_repo();
        let path = write_task(
            &dir,
            "todo/recurring.md",
            "---\ntldr: test\nstatus: todo\ncheck_in_date: 2026-04-18\nrecurrence:\n  rrule: \"FREQ=WEEKLY\"\ntentative_date: 2026-04-20\n---\n",
        );
        let new_date = d(2026, 4, 21);
        let result = todo_schedule(&ScheduleOptions {
            repo_dir: dir.path().to_path_buf(),
            path: path.to_path_buf(),
            date: new_date,
        })
        .unwrap();
        assert_eq!(result.tentative_date, new_date);

        let content = fs::read_to_string(dir.path().join(&path)).unwrap();
        // Only one tentative_date line, set to the new date.
        assert_eq!(
            content.matches("tentative_date:").count(),
            1,
            "expected exactly one tentative_date line in:\n{content}"
        );
        let fm = frontmatter::parse_frontmatter(&content).unwrap().unwrap();
        assert_eq!(fm.tentative_date, Some(new_date));
        assert!(
            fm.recurrence.is_some(),
            "recurrence block must be preserved"
        );
    }

    #[test]
    fn todo_done_on_legacy_flat_rrule_recurs_and_appends_frontmatter_log() {
        let dir = setup_repo();
        let path = write_task(
            &dir,
            "todo/task.md",
            "---\ntldr: test\nstatus: todo\ncheck_in_date: 2026-04-18\nrrule: \"FREQ=WEEKLY\"\ntentative_date: 2026-04-18\n---\nBody.\n",
        );

        let completion = d(2026, 4, 18);
        let result = do_done(&dir, &path, completion).unwrap();
        match result.outcome {
            DoneOutcome::Recurring { .. } => {}
            _ => panic!("expected Recurring"),
        }

        let content = fs::read_to_string(dir.path().join(&path)).unwrap();
        assert!(content.contains("completion_log:"));
        assert!(content.contains(&format!("completed: {completion}")));
        // Phase 1 does not rewrite the flat rrule here; that's reindex's job.
        // The form on disk preserves whatever quoting the user had.
        assert!(
            content.contains("FREQ=WEEKLY"),
            "expected legacy rrule preserved in:\n{content}"
        );
    }

    // -----------------------------------------------------------------------
    // Phase 2: shape classifier
    // -----------------------------------------------------------------------

    #[test]
    fn classify_shape_none_is_non_recurring() {
        assert_eq!(classify_shape(None), TaskShape::NonRecurring);
    }

    #[test]
    fn classify_shape_bare_freq_is_slip() {
        assert_eq!(classify_shape(Some("FREQ=WEEKLY")), TaskShape::Slip);
        assert_eq!(classify_shape(Some("FREQ=MONTHLY")), TaskShape::Slip);
        assert_eq!(classify_shape(Some("FREQ=DAILY")), TaskShape::Slip);
        // INTERVAL is not a BY modifier.
        assert_eq!(
            classify_shape(Some("FREQ=WEEKLY;INTERVAL=2")),
            TaskShape::Slip
        );
    }

    #[test]
    fn classify_shape_by_modifiers_are_anchored() {
        for r in [
            "FREQ=WEEKLY;BYDAY=SA",
            "FREQ=MONTHLY;BYMONTHDAY=1",
            "FREQ=YEARLY;BYMONTH=4;BYMONTHDAY=1",
            "FREQ=WEEKLY;BYDAY=MO,WE,FR",
            "FREQ=MONTHLY;BYSETPOS=-1;BYDAY=FR",
            "FREQ=DAILY;BYHOUR=9",
        ] {
            assert_eq!(
                classify_shape(Some(r)),
                TaskShape::Anchored,
                "rule should be Anchored: {r}"
            );
        }
    }

    #[test]
    fn classify_shape_is_case_insensitive() {
        assert_eq!(
            classify_shape(Some("freq=weekly;byday=sa")),
            TaskShape::Anchored
        );
        assert_eq!(classify_shape(Some("freq=weekly")), TaskShape::Slip);
    }

    // -----------------------------------------------------------------------
    // Phase 2: non-recurring branch
    // -----------------------------------------------------------------------

    #[test]
    fn non_recurring_fresh_done_writes_on_date_and_one_log_entry() {
        let dir = setup_repo();
        let path = write_task(
            &dir,
            "todo/task.md",
            "---\ntldr: test\nstatus: todo\ncheck_in_date: 2026-04-18\n---\n",
        );
        let result = do_done(&dir, &path, d(2026, 4, 20)).unwrap();
        match result.outcome {
            DoneOutcome::Completed { terminal, .. } => assert!(!terminal),
            _ => panic!("expected Completed"),
        }
        let content = fs::read_to_string(dir.path().join(&path)).unwrap();
        assert!(content.contains("status: done"));
        assert!(content.contains("on_date: '2026-04-20'"));
        assert!(content.contains("completion_log:"));
        assert!(content.contains("completed: 2026-04-20"));
        // Done clears any prior sort_order — the only writer of
        // sort_order is `todo_reorder`. The fixture didn't carry one,
        // so the file has none after either.
        assert!(
            !content.contains("sort_order:"),
            "expected no sort_order on completed task; got:\n{content}"
        );
    }

    #[test]
    fn non_recurring_conflict_returns_already_done_or_not_due_yet() {
        let dir = setup_repo();
        let path = write_task(
            &dir,
            "todo/task.md",
            "---\ntldr: test\nstatus: todo\ncheck_in_date: 2026-04-18\non_date: 2026-04-15\n---\n",
        );
        let err = do_done(&dir, &path, d(2026, 4, 20)).unwrap_err();
        match err {
            DoneError::AlreadyDoneOrNotDueYet {
                stored_on_date,
                completion_date,
            } => {
                assert_eq!(stored_on_date, d(2026, 4, 15));
                assert_eq!(completion_date, d(2026, 4, 20));
            }
            other => panic!("expected AlreadyDoneOrNotDueYet, got {other:?}"),
        }
    }

    #[test]
    fn non_recurring_replacing_matching_stored_overwrites() {
        let dir = setup_repo();
        let path = write_task(
            &dir,
            "todo/task.md",
            "---\ntldr: test\nstatus: todo\ncheck_in_date: 2026-04-18\non_date: 2026-04-15\n---\n",
        );
        let result = todo_done(&DoneOptions {
            repo_dir: dir.path().to_path_buf(),
            path: path.clone(),
            completion_date: d(2026, 4, 20),
            replacing: Some(d(2026, 4, 15)),
            skip_past: None,
            comment: None,
        })
        .unwrap();
        match result.outcome {
            DoneOutcome::Completed {
                terminal: false, ..
            } => {}
            _ => panic!("expected non-terminal Completed"),
        }
        let content = fs::read_to_string(dir.path().join(&path)).unwrap();
        assert!(content.contains("on_date: '2026-04-20'"));
    }

    #[test]
    fn non_recurring_range_task_writes_end_date() {
        let dir = setup_repo();
        let path = write_task(
            &dir,
            "todo/task.md",
            "---\ntldr: test\nstatus: todo\ncheck_in_date: 2026-04-18\nstart_date: 2026-04-10\n---\n",
        );
        let result = do_done(&dir, &path, d(2026, 4, 20)).unwrap();
        match result.outcome {
            DoneOutcome::Completed {
                completion: CompletionDate::EndDate(d),
                terminal: false,
            } => assert_eq!(d, NaiveDate::from_ymd_opt(2026, 4, 20).unwrap()),
            _ => panic!("expected Completed/EndDate"),
        }
        let content = fs::read_to_string(dir.path().join(&path)).unwrap();
        assert!(content.contains("end_date: '2026-04-20'"));
    }

    #[test]
    fn skip_past_on_non_recurring_errors() {
        let dir = setup_repo();
        let path = write_task(
            &dir,
            "todo/task.md",
            "---\ntldr: test\nstatus: todo\ncheck_in_date: 2026-04-18\n---\n",
        );
        let err = todo_done(&DoneOptions {
            repo_dir: dir.path().to_path_buf(),
            path,
            completion_date: d(2026, 4, 20),
            replacing: None,
            skip_past: Some(true),
            comment: None,
        })
        .unwrap_err();
        matches!(err, DoneError::SkipPastOnNonRecurring);
    }

    // -----------------------------------------------------------------------
    // Phase 2: slip branch
    // -----------------------------------------------------------------------

    #[test]
    fn slip_same_day_redone_is_noop_with_existing_entry() {
        let dir = setup_repo();
        let path = write_task(
            &dir,
            "todo/task.md",
            "---\ntldr: test\nstatus: todo\ncheck_in_date: 2026-04-18\nrecurrence:\n  rrule: \"FREQ=WEEKLY\"\ncompletion_log:\n  - {completed: 2026-04-18}\n---\n",
        );
        let result = do_done(&dir, &path, d(2026, 4, 18)).unwrap();
        match result.outcome {
            DoneOutcome::AlreadyDone {
                existing,
                comment_discarded,
            } => {
                assert_eq!(existing.completed, d(2026, 4, 18));
                assert!(!comment_discarded);
            }
            _ => panic!("expected AlreadyDone"),
        }
        // File unchanged: still one entry.
        let content = fs::read_to_string(dir.path().join(&path)).unwrap();
        assert_eq!(content.matches("completed: 2026-04-18").count(), 1);
    }

    #[test]
    fn slip_same_day_with_comment_flags_discarded() {
        let dir = setup_repo();
        let path = write_task(
            &dir,
            "todo/task.md",
            "---\ntldr: test\nstatus: todo\ncheck_in_date: 2026-04-18\nrecurrence:\n  rrule: \"FREQ=WEEKLY\"\ncompletion_log:\n  - {completed: 2026-04-18}\n---\n",
        );
        let result = todo_done(&DoneOptions {
            repo_dir: dir.path().to_path_buf(),
            path,
            completion_date: d(2026, 4, 18),
            replacing: None,
            skip_past: None,
            comment: Some("extra note".into()),
        })
        .unwrap();
        match result.outcome {
            DoneOutcome::AlreadyDone {
                comment_discarded, ..
            } => assert!(comment_discarded),
            _ => panic!("expected AlreadyDone"),
        }
    }

    #[test]
    fn slip_distinct_day_advances_and_clears_tentative() {
        let dir = setup_repo();
        let path = write_task(
            &dir,
            "todo/task.md",
            "---\ntldr: test\nstatus: todo\ncheck_in_date: 2026-04-18\nrecurrence:\n  rrule: \"FREQ=WEEKLY\"\ntentative_date: 2026-04-18\n---\n",
        );
        let result = do_done(&dir, &path, d(2026, 4, 21)).unwrap();
        match result.outcome {
            DoneOutcome::Recurring {
                next_check_in_date,
                next_due_date,
            } => {
                assert_eq!(next_check_in_date, d(2026, 4, 28));
                assert!(next_due_date.is_none());
            }
            _ => panic!("expected Recurring"),
        }
        let content = fs::read_to_string(dir.path().join(&path)).unwrap();
        assert!(content.contains("check_in_date: '2026-04-28'"));
        assert!(!content.contains("tentative_date"));
        assert!(content.contains("completed: 2026-04-21"));
    }

    #[test]
    fn slip_with_due_date_respects_lead_days() {
        let dir = setup_repo();
        let path = write_task(
            &dir,
            "todo/task.md",
            "---\ntldr: test\nstatus: todo\ndue_date: 2026-04-25\ncheck_in_date: 2026-04-18\nrecurrence:\n  rrule: \"FREQ=WEEKLY\"\n  lead_days: 7\n---\n",
        );
        let result = do_done(&dir, &path, d(2026, 4, 25)).unwrap();
        match result.outcome {
            DoneOutcome::Recurring {
                next_check_in_date,
                next_due_date,
            } => {
                // due_date is the anchor: new due = completion_date + 1 week = 2026-05-02.
                assert_eq!(next_due_date, Some(d(2026, 5, 2)));
                // check_in_date = due_date - lead_days (7) = 2026-04-25.
                assert_eq!(next_check_in_date, d(2026, 4, 25));
            }
            _ => panic!("expected Recurring"),
        }
    }

    #[test]
    fn skip_past_on_slip_errors() {
        let dir = setup_repo();
        let path = write_task(
            &dir,
            "todo/task.md",
            "---\ntldr: test\nstatus: todo\ncheck_in_date: 2026-04-18\nrecurrence:\n  rrule: \"FREQ=WEEKLY\"\n---\n",
        );
        let err = todo_done(&DoneOptions {
            repo_dir: dir.path().to_path_buf(),
            path,
            completion_date: d(2026, 4, 20),
            replacing: None,
            skip_past: Some(false),
            comment: None,
        })
        .unwrap_err();
        matches!(err, DoneError::SkipPastOnSlipRule);
    }

    // -----------------------------------------------------------------------
    // Phase 2: anchored branch
    // -----------------------------------------------------------------------

    #[test]
    fn anchored_naive_advance_records_previous_as_occurrence() {
        let dir = setup_repo();
        let path = write_task(
            &dir,
            "todo/task.md",
            "---\ntldr: test\nstatus: todo\ndue_date: 2026-04-25\ncheck_in_date: 2026-04-18\nrecurrence:\n  rrule: \"FREQ=WEEKLY;BYDAY=SA\"\n  lead_days: 7\ntentative_date: 2026-04-20\n---\n",
        );
        // Completed Tue Apr 21, before next Sat Apr 25 → naive advance.
        let result = do_done(&dir, &path, d(2026, 4, 21)).unwrap();
        match result.outcome {
            DoneOutcome::Recurring {
                next_check_in_date,
                next_due_date,
            } => {
                // new due = next SA after 2026-04-25 = 2026-05-02.
                assert_eq!(next_due_date, Some(d(2026, 5, 2)));
                // new check-in = due - lead_days (7) = 2026-04-25.
                assert_eq!(next_check_in_date, d(2026, 4, 25));
            }
            _ => panic!("expected Recurring"),
        }
        let content = fs::read_to_string(dir.path().join(&path)).unwrap();
        assert!(content.contains("due_date: '2026-05-02'"));
        assert!(content.contains("check_in_date: '2026-04-25'"));
        assert!(!content.contains("tentative_date"));
        // Occurrence credited is the PREVIOUS anchor (2026-04-25).
        assert!(
            content.contains("occurrence: 2026-04-25"),
            "expected occurrence: 2026-04-25 in:\n{content}"
        );
        assert!(content.contains("completed: 2026-04-21"));
    }

    #[test]
    fn anchored_stale_anchor_without_skip_past_errors() {
        let dir = setup_repo();
        let path = write_task(
            &dir,
            "todo/task.md",
            "---\ntldr: test\nstatus: todo\ncheck_in_date: 2025-11-01\nrecurrence:\n  rrule: \"FREQ=MONTHLY;BYMONTHDAY=1\"\n---\n",
        );
        let err = do_done(&dir, &path, d(2026, 4, 20)).unwrap_err();
        match err {
            DoneError::StaleAnchor {
                stored_anchor,
                completion_date,
                next_anchor_if_skip_past_false,
                next_anchor_if_skip_past_true,
            } => {
                assert_eq!(stored_anchor, d(2025, 11, 1));
                assert_eq!(completion_date, d(2026, 4, 20));
                assert_eq!(next_anchor_if_skip_past_false, d(2025, 12, 1));
                assert_eq!(next_anchor_if_skip_past_true, d(2026, 5, 1));
            }
            other => panic!("expected StaleAnchor, got {other:?}"),
        }
    }

    #[test]
    fn anchored_stale_anchor_skip_past_false_catches_up_one_step() {
        let dir = setup_repo();
        let path = write_task(
            &dir,
            "todo/task.md",
            "---\ntldr: test\nstatus: todo\ncheck_in_date: 2025-11-01\nrecurrence:\n  rrule: \"FREQ=MONTHLY;BYMONTHDAY=1\"\n---\n",
        );
        let result = todo_done(&DoneOptions {
            repo_dir: dir.path().to_path_buf(),
            path: path.clone(),
            completion_date: d(2026, 4, 20),
            replacing: None,
            skip_past: Some(false),
            comment: None,
        })
        .unwrap();
        match result.outcome {
            DoneOutcome::Recurring {
                next_check_in_date, ..
            } => assert_eq!(next_check_in_date, d(2025, 12, 1)),
            _ => panic!("expected Recurring"),
        }
        let content = fs::read_to_string(dir.path().join(&path)).unwrap();
        assert!(content.contains("occurrence: 2025-11-01"));
        assert!(content.contains("completed: 2026-04-20"));
    }

    #[test]
    fn anchored_stale_anchor_skip_past_true_resumes_on_schedule() {
        let dir = setup_repo();
        let path = write_task(
            &dir,
            "todo/task.md",
            "---\ntldr: test\nstatus: todo\ncheck_in_date: 2025-11-01\nrecurrence:\n  rrule: \"FREQ=MONTHLY;BYMONTHDAY=1\"\n---\n",
        );
        let result = todo_done(&DoneOptions {
            repo_dir: dir.path().to_path_buf(),
            path: path.clone(),
            completion_date: d(2026, 4, 20),
            replacing: None,
            skip_past: Some(true),
            comment: None,
        })
        .unwrap();
        match result.outcome {
            DoneOutcome::Recurring {
                next_check_in_date, ..
            } => assert_eq!(next_check_in_date, d(2026, 5, 1)),
            _ => panic!("expected Recurring"),
        }
        let content = fs::read_to_string(dir.path().join(&path)).unwrap();
        // skip_past=true credits `occurrence: completion_date`.
        assert!(content.contains("occurrence: 2026-04-20"));
    }

    #[test]
    fn anchored_skip_past_unnecessary_errors_when_not_stale() {
        let dir = setup_repo();
        let path = write_task(
            &dir,
            "todo/task.md",
            "---\ntldr: test\nstatus: todo\ndue_date: 2026-04-25\ncheck_in_date: 2026-04-18\nrecurrence:\n  rrule: \"FREQ=WEEKLY;BYDAY=SA\"\n  lead_days: 7\n---\n",
        );
        let err = todo_done(&DoneOptions {
            repo_dir: dir.path().to_path_buf(),
            path,
            completion_date: d(2026, 4, 21),
            replacing: None,
            skip_past: Some(true),
            comment: None,
        })
        .unwrap_err();
        match err {
            DoneError::SkipPastUnnecessary {
                stored_anchor,
                next_anchor,
            } => {
                assert_eq!(stored_anchor, d(2026, 4, 25));
                assert_eq!(next_anchor, d(2026, 5, 2));
            }
            other => panic!("expected SkipPastUnnecessary, got {other:?}"),
        }
    }

    #[test]
    fn anchored_catch_up_walks_the_prd_worked_example() {
        // Worked example: monthly-on-1st, check-in 2025-11-01,
        // done on 2026-04-20. Five sequential skip_past:false calls then
        // one no-skip_past call.
        let dir = setup_repo();
        let path = write_task(
            &dir,
            "todo/task.md",
            "---\ntldr: test\nstatus: todo\ncheck_in_date: 2025-11-01\nrecurrence:\n  rrule: \"FREQ=MONTHLY;BYMONTHDAY=1\"\n---\n",
        );
        let completion = d(2026, 4, 20);
        let expected = [
            d(2025, 12, 1),
            d(2026, 1, 1),
            d(2026, 2, 1),
            d(2026, 3, 1),
            d(2026, 4, 1),
        ];
        for want in expected {
            let result = todo_done(&DoneOptions {
                repo_dir: dir.path().to_path_buf(),
                path: path.clone(),
                completion_date: completion,
                replacing: None,
                skip_past: Some(false),
                comment: None,
            })
            .unwrap();
            match result.outcome {
                DoneOutcome::Recurring {
                    next_check_in_date, ..
                } => assert_eq!(next_check_in_date, want),
                _ => panic!("expected Recurring"),
            }
        }
        // 6th call with no skip_past: stale-anchor no longer fires
        // (naive advance = 2026-05-01 >= completion 2026-04-20).
        let result = do_done(&dir, &path, completion).unwrap();
        match result.outcome {
            DoneOutcome::Recurring {
                next_check_in_date, ..
            } => assert_eq!(next_check_in_date, d(2026, 5, 1)),
            _ => panic!("expected Recurring"),
        }
    }

    #[test]
    fn anchored_exhaustion_keeps_recurrence_and_dates() {
        let dir = setup_repo();
        let path = write_task(
            &dir,
            "todo/task.md",
            "---\ntldr: test\nstatus: todo\ncheck_in_date: 2026-04-18\nrecurrence:\n  rrule: \"FREQ=WEEKLY;BYDAY=SA;COUNT=1\"\nsort_order: 99\n---\n",
        );
        // COUNT=1 means the dtstart itself is the last occurrence; next is exhausted.
        // Completion date 2026-04-18 (the final anchor).
        let result = do_done(&dir, &path, d(2026, 4, 18)).unwrap();
        match result.outcome {
            DoneOutcome::Completed {
                completion: CompletionDate::OnDate(d),
                terminal: true,
            } => assert_eq!(d, NaiveDate::from_ymd_opt(2026, 4, 18).unwrap()),
            _ => panic!("expected terminal Completed"),
        }
        let content = fs::read_to_string(dir.path().join(&path)).unwrap();
        assert!(content.contains("status: done"));
        assert!(content.contains("on_date: '2026-04-18'"));
        // Recurrence is retained; dates freeze at the
        // final occurrence; rrule intact.
        assert!(content.contains("recurrence:"));
        assert!(content.contains("FREQ=WEEKLY;BYDAY=SA;COUNT=1"));
        assert!(content.contains("check_in_date: 2026-04-18"));
        assert!(!content.contains("tentative_date"));
        // Occurrence credited on terminal entry = previous_anchor.
        assert!(content.contains("occurrence: 2026-04-18"));
        // anchored_terminal_completion must clear sort_order (it builds
        // its own mutation list, distinct from terminal_task_mutations).
        assert!(
            !content.contains("sort_order:"),
            "anchored exhaustion must clear sort_order; got:\n{content}"
        );
    }

    // -----------------------------------------------------------------------
    // Phase 2: invariants across all advances
    // -----------------------------------------------------------------------

    #[test]
    fn slip_advance_clears_sort_order() {
        // Slip-rule recurring advance must clear any prior `sort_order`
        // so the task ranks by `priority` in its new date bucket. The
        // only writer of `sort_order` is `todo_reorder`.
        let dir = setup_repo();
        let path = write_task(
            &dir,
            "todo/task.md",
            "---\ntldr: test\nstatus: todo\ncheck_in_date: 2026-04-18\nrecurrence:\n  rrule: \"FREQ=WEEKLY\"\nsort_order: 99\n---\n",
        );
        do_done(&dir, &path, d(2026, 4, 21)).unwrap();
        let content = fs::read_to_string(dir.path().join(&path)).unwrap();
        assert!(
            !content.contains("sort_order:"),
            "expected sort_order to be removed after slip advance; got:\n{content}"
        );
    }

    #[test]
    fn anchored_advance_clears_sort_order() {
        // Anchored-rule recurring advance must also clear `sort_order`.
        // Same helper (`slip_or_anchored_advance_mutations`) underneath,
        // but this test pins the anchored caller specifically — a
        // regression that re-introduces a Set just in the anchored path
        // (e.g., post-helper push) would not be caught by the slip test.
        let dir = setup_repo();
        let path = write_task(
            &dir,
            "todo/task.md",
            "---\ntldr: test\nstatus: todo\ndue_date: 2026-04-25\ncheck_in_date: 2026-04-18\nrecurrence:\n  rrule: \"FREQ=WEEKLY;BYDAY=SA\"\n  lead_days: 7\nsort_order: 99\n---\n",
        );
        // Completed Tue Apr 21, before next Sat Apr 25 → naive anchored advance.
        do_done(&dir, &path, d(2026, 4, 21)).unwrap();
        let content = fs::read_to_string(dir.path().join(&path)).unwrap();
        assert!(
            !content.contains("sort_order:"),
            "expected sort_order to be removed after anchored advance; got:\n{content}"
        );
    }

    #[test]
    fn done_error_to_json_stale_anchor_shape() {
        let err = DoneError::StaleAnchor {
            stored_anchor: d(2025, 11, 1),
            completion_date: d(2026, 4, 20),
            next_anchor_if_skip_past_false: d(2025, 12, 1),
            next_anchor_if_skip_past_true: d(2026, 5, 1),
        };
        let v = err.to_json();
        assert_eq!(v["error"], "stale_anchor");
        assert!(v["reason"].as_str().unwrap().contains("2026-04-20"));
        assert_eq!(v["stored_anchor"], "2025-11-01");
        assert_eq!(v["completion_date"], "2026-04-20");
        assert_eq!(v["next_anchor_if_skip_past_false"], "2025-12-01");
        assert_eq!(v["next_anchor_if_skip_past_true"], "2026-05-01");
    }

    #[test]
    fn done_error_path_not_found_to_json() {
        let err = DoneError::PathNotFound {
            path: "todo/missing.md".to_string(),
            reason: "file not found".to_string(),
        };
        let v = err.to_json();
        assert_eq!(v["error"], "path_not_found");
        assert_eq!(v["path"], "todo/missing.md");
        assert_eq!(v["reason"], "file not found");
    }

    #[test]
    fn done_error_invalid_date_to_json() {
        let err = DoneError::InvalidDate {
            reason: "missing required field: completion_date".to_string(),
        };
        let v = err.to_json();
        assert_eq!(v["error"], "invalid_date");
        assert_eq!(v["reason"], "missing required field: completion_date");
        assert!(v.get("path").is_none());
    }
}
