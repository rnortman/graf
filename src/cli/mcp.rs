use std::path::PathBuf;

use anyhow::{Context, Result};
use brenn_mcp::{Server, Tool};
use chrono::NaiveDate;
use clap::Args;
use serde_json::{Value, json};

use graf::frontmatter::{Effort, Recurrence, Status};
use graf::lint::{self, LintOptions};
use graf::manifest::AppManifest;
use graf::todo::{self, AddOptions, DoneOptions, ReorderOptions, ScheduleOptions};

use super::ResolvedRepo;

#[derive(Args)]
pub struct McpArgs {
    /// Repo slug or path (default: use manifest, or current directory)
    #[arg(long)]
    pub repo: Option<String>,
}

/// Shared context for all MCP tools: the default repo and manifest state.
#[derive(Clone)]
struct McpContext {
    /// Default repo from --repo flag at server startup.
    default_repo: Option<String>,
}

impl McpContext {
    /// Resolve the repo for a tool call. Uses the tool-level `repo` param
    /// if provided, otherwise falls back to the server-level default.
    fn resolve_single(&self, tool_repo: Option<&str>) -> Result<ResolvedRepo> {
        let repo_arg = tool_repo.or(self.default_repo.as_deref());
        super::resolve_single_repo(repo_arg)
    }

    /// Resolve repos for a query (may return multiple).
    fn resolve_all(&self, tool_repo: Option<&str>) -> Result<Vec<ResolvedRepo>> {
        let repo_arg = tool_repo.or(self.default_repo.as_deref());
        super::resolve_repos(repo_arg)
    }

    /// Resolve a target document path for mutation tools.
    /// Passes `tool_repo` for conflict detection, server default as fallback.
    fn resolve_target(
        &self,
        path_str: &str,
        tool_repo: Option<&str>,
    ) -> Result<(ResolvedRepo, PathBuf)> {
        super::resolve_target(path_str, tool_repo, self.default_repo.as_deref())
    }
}

const INSTRUCTIONS: &str = "\
graf is a knowledge base with todo and linking support, built on markdown files \
with YAML frontmatter. Each .md file is an entity. Prefer many small docs with \
cross-links over large monolithic docs.

Files with a status field are tasks. Files without status are knowledge-base \
entries. The line between \"task\" and \"document\" is blurry \u{2014} docs can appear \
in the task list if they have a check_in_date, even without status.

Tool parameters accept slug:path for cross-repo references (e.g. \
life:todo/my-task.md). Use graf_manifest_list to see available repos.

graf_todo_done takes `completion_date` (required) — the real date the \
user did the task, in their local timezone. Pass the user's true local \
date; do not guess. graf_todo_cancel requires `today` — the user's true \
local date.

Call graf_ref_* tools for detailed reference: graf_ref_frontmatter_schema, \
graf_ref_attachments, graf_ref_links, graf_ref_context_link.";

pub fn run(args: McpArgs) -> Result<()> {
    let ctx = McpContext {
        default_repo: args.repo,
    };

    let server = Server::new("graf", env!("CARGO_PKG_VERSION"))
        .with_instructions(INSTRUCTIONS)
        .with_tool(LintTool(ctx.clone()))
        .with_tool(FixTool(ctx.clone()))
        .with_tool(ReindexTool(ctx.clone()))
        .with_tool(TodoQueryTool(ctx.clone()))
        .with_tool(TodoAddTool(ctx.clone()))
        .with_tool(TodoDoneTool(ctx.clone()))
        .with_tool(TodoCancelTool(ctx.clone()))
        .with_tool(TodoScheduleTool(ctx.clone()))
        .with_tool(TodoReorderTool(ctx.clone()))
        .with_tool(ManifestListTool)
        .with_tool(ManifestCheckTool)
        .with_tool(RefFrontmatterSchemaTool)
        .with_tool(RefAttachmentsTool)
        .with_tool(RefLinksTool)
        .with_tool(RefContextLinkTool);

    server.run()
}

// ---------------------------------------------------------------------------
// Reference tools (detailed docs, callable on demand)
// ---------------------------------------------------------------------------

fn ref_schema() -> Value {
    json!({ "type": "object", "properties": {} })
}

struct RefFrontmatterSchemaTool;

impl Tool for RefFrontmatterSchemaTool {
    fn name(&self) -> &str {
        "graf_ref_frontmatter_schema"
    }

    fn description(&self) -> &str {
        "Get the complete frontmatter field reference for graf markdown files."
    }

    fn input_schema(&self) -> Value {
        ref_schema()
    }

    fn call(&self, _args: Value) -> Result<Value> {
        Ok(json!("\
Every .md file must have YAML frontmatter. Unknown fields are rejected.

Only required field:
- tldr: string

Metadata:
- labels: [string]
- created: string \u{2014} RFC 3339 timestamp (managed by graf_fix)
- updated: string \u{2014} [same]
- summary: string

Date fields (YYYY-MM-DD):
- on_date \u{2014} single-day event; mutually exclusive with start_date/end_date
- start_date, end_date \u{2014} date range; end_date requires start_date; \
mutually exclusive with on_date; start without end implies ongoing
- due_date
- check_in_date
- verified_date \u{2014} When information was last verified
- tentative_date \u{2014} set by snooze/schedule/reorder tools (rarely set manually)

Task fields:
- status: todo | in_progress | done | cancelled | reminder
- priority: integer 1-5 (1 is highest priority); omit if unknown
- effort: trivial | small | medium | large | epic
- assigned_to: string
- sort_order: float \u{2014} managed by tools
- recurrence: { rrule: string (RFC 5545), lead_days?: int \u{2265}0 (requires due_date) }
- completion_log: list of { completed: date, occurrence?: date, comment?: markdown } \
\u{2014} entries written by graf_todo_done; direct-edit to add comments to existing entries

Link fields (repo-root-relative):
- context_link: string \u{2014} the single most important related document (see graf_ref_context_link)
- links: [string]
- blocked_by: [string]
- attachments: [string] \u{2014} (see graf_ref_attachments)"))
    }
}

struct RefAttachmentsTool;

impl Tool for RefAttachmentsTool {
    fn name(&self) -> &str {
        "graf_ref_attachments"
    }

    fn description(&self) -> &str {
        "Get reference docs for how non-markdown file attachments work in graf."
    }

    fn input_schema(&self) -> Value {
        ref_schema()
    }

    fn call(&self, _args: Value) -> Result<Value> {
        Ok(json!(
            "\
Non-markdown files are stored as attachments to a markdown file.

Convention:
- The attachment shares the same basename as its .md sidecar.
  Example: report.md + report.pdf
- The .md file lists the attachment's extension in its frontmatter:
  attachments: [.pdf]"
        ))
    }
}

struct RefLinksTool;

impl Tool for RefLinksTool {
    fn name(&self) -> &str {
        "graf_ref_links"
    }

    fn description(&self) -> &str {
        "Get reference docs for how linking between documents works in graf."
    }

    fn input_schema(&self) -> Value {
        ref_schema()
    }

    fn call(&self, _args: Value) -> Result<Value> {
        Ok(json!(
            "\
graf is a graph of small, atomic documents connected by cross-links.
Links can be in frontmatter fields or the markdown body.
All link paths are repo-relative paths to .md files.

Link fields:
- context_link: string \u{2014} the single most important related doc (see graf_ref_context_link)
- links: [string] \u{2014} other related documents, looser coupling
- blocked_by: [string] \u{2014} tasks that block this one (task-specific)

Cross-repo references (in tool parameters only):
Tool parameters accept slug:path syntax for cross-repo references.
  Example: life:todo/call-dentist.md
Slugs are defined in the application manifest. Use graf_manifest_list
to see available repos and their slugs.

Nexus pattern:
A nexus is a small document that serves as a link *target* for a cluster
of related documents. Other docs set context_link or links pointing to
the nexus. To explore a topic, start at the nexus and traverse outward.
Don't overload a nexus with outgoing links \u{2014} it's a hub, not an index."
        ))
    }
}

struct RefContextLinkTool;

impl Tool for RefContextLinkTool {
    fn name(&self) -> &str {
        "graf_ref_context_link"
    }

    fn description(&self) -> &str {
        "Get reference docs for what context_link is and when to use it."
    }

    fn input_schema(&self) -> Value {
        ref_schema()
    }

    fn call(&self, _args: Value) -> Result<Value> {
        Ok(json!(
            "\
context_link is a single string field (not an array) that points to the
one most important related document. It answers: \"What is the primary
context for understanding this document?\"

When to set context_link:

- Sub-tasks point to their parent task.
- Journal entries point to the change or topic they document.
- Knowledge articles point to their primary topic nexus.
- Any document that \"belongs to\" or \"is about\" another document.

context_link vs links:
- context_link is singular and hierarchical \u{2014} it's the parent/primary.
- links is plural and peer-to-peer."
        ))
    }
}

// ---------------------------------------------------------------------------
// Repo parameter schema (shared by all repo-aware tools)
// ---------------------------------------------------------------------------

fn repo_property() -> Value {
    json!({
        "type": "string",
        "description": "Repo slug or filesystem path. Default: server startup --repo, or all manifest repos for queries."
    })
}

/// The `today` override, shared across mutators that use "today" as a
/// default. See graf-user-tz.md.
fn today_property() -> Value {
    json!({
        "type": "string",
        "description":
            "The calendar date to treat as 'today' for this operation, in the user's local zone (ISO 8601, e.g. 2026-04-16). Required. Pass the user's true local date."
    })
}

// ---------------------------------------------------------------------------
// Lint tool
// ---------------------------------------------------------------------------

struct LintTool(McpContext);

impl Tool for LintTool {
    fn name(&self) -> &str {
        "graf_lint"
    }

    fn description(&self) -> &str {
        "Validate frontmatter in markdown files. By default checks only git-changed files. \
         Use changed=false to check all files. Use paths to check specific files."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "repo": repo_property(),
                "changed": {
                    "type": "boolean",
                    "description": "Only check git-changed files (default: true, ignored if paths is set)"
                },
                "paths": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Specific files to lint (relative to repo root). Takes precedence over changed."
                }
            }
        })
    }

    fn call(&self, args: Value) -> Result<Value> {
        let repo_arg = args["repo"].as_str();
        let paths = parse_string_array(&args["paths"])?;
        let changed = args["changed"].as_bool().unwrap_or(true);

        let repos = if !paths.is_empty() {
            vec![self.0.resolve_single(repo_arg)?]
        } else {
            self.0.resolve_all(repo_arg)?
        };

        let combined = super::multi_repo_lint(
            &repos,
            &super::MultiRepoLintOpts {
                fix: false,
                changed,
                files: paths.iter().map(PathBuf::from).collect(),
            },
        );

        Ok(lint::lint_result_to_json(&combined))
    }
}

// ---------------------------------------------------------------------------
// Fix tool
// ---------------------------------------------------------------------------

struct FixTool(McpContext);

impl Tool for FixTool {
    fn name(&self) -> &str {
        "graf_fix"
    }

    fn description(&self) -> &str {
        "Auto-fix frontmatter issues (timestamps, etc.) in git-changed files. \
         Only fixes auto-fixable issues; lint errors that require human judgment are reported."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "repo": repo_property()
            }
        })
    }

    fn call(&self, args: Value) -> Result<Value> {
        let repo = self.0.resolve_single(args["repo"].as_str())?;
        let options = LintOptions {
            fix: true,
            repo_dir: repo.path,
        };
        let result = lint::lint_changed(&options)?;
        Ok(lint::lint_result_to_json(&result))
    }
}

// ---------------------------------------------------------------------------
// Reindex tool
// ---------------------------------------------------------------------------

struct ReindexTool(McpContext);

impl Tool for ReindexTool {
    fn name(&self) -> &str {
        "graf_reindex"
    }

    fn description(&self) -> &str {
        "Rebuild the SQLite metadata cache from scratch. Use after suspected cache corruption \
         or when the incremental sync may have drifted. Requires --repo when manifest is active."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "repo": repo_property()
            }
        })
    }

    fn call(&self, args: Value) -> Result<Value> {
        let repo = self.0.resolve_single(args["repo"].as_str())?;
        let result = todo::reindex(&repo.path)?;
        Ok(json!({
            "files_indexed": result.files_indexed,
            "errors": result.errors.iter()
                .map(|e| json!({
                    "path": e.path.display().to_string(),
                    "message": e.message,
                }))
                .collect::<Vec<Value>>(),
        }))
    }
}

// ---------------------------------------------------------------------------
// Todo query tool
// ---------------------------------------------------------------------------

struct TodoQueryTool(McpContext);

impl Tool for TodoQueryTool {
    fn name(&self) -> &str {
        "graf_todo_query"
    }

    fn description(&self) -> &str {
        "Query tasks from the todo system. Returns tasks sorted by effective date, \
         along with any lint errors found during sync. By default returns all active \
         tasks (todo, in_progress, reminder) up to the configured default limit. \
         Use horizon_days to narrow to a time window. \
         When a manifest is active and no repo specified, queries all repos."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "repo": repo_property(),
                "horizon_days": {
                    "type": "integer",
                    "description": "Only include items up to N days in the future"
                },
                "include_done": {
                    "type": "boolean",
                    "description": "Include done/cancelled items (default: false)"
                },
                "statuses": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Filter by status (default: [todo, in_progress, reminder])"
                },
                "labels": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Filter by label (AND semantics)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Max results (overrides config default_limit)"
                }
            }
        })
    }

    fn call(&self, args: Value) -> Result<Value> {
        let repos = self.0.resolve_all(args["repo"].as_str())?;
        // TODO(mcp-config-caching)
        let config = graf::config::AppConfig::load_default()?;
        let explicit_limit = args["limit"].as_u64().map(|v| v as usize);
        let limit = explicit_limit.or(config.todo.effective_limit());

        let result = super::multi_repo_todo_query(
            &repos,
            &super::MultiRepoQueryOpts {
                horizon_days: args["horizon_days"].as_u64().map(|v| v as u32),
                include_done: args["include_done"].as_bool().unwrap_or(false),
                statuses: parse_status_array(&args["statuses"])?,
                labels: parse_string_array(&args["labels"])?,
                limit,
            },
        );
        Ok(serde_json::to_value(&result)?)
    }
}

// ---------------------------------------------------------------------------
// Todo add tool
// ---------------------------------------------------------------------------

struct TodoAddTool(McpContext);

impl Tool for TodoAddTool {
    fn name(&self) -> &str {
        "graf_todo_add"
    }

    fn description(&self) -> &str {
        "Create a new todo file with frontmatter. The caller chooses the file path. \
         At least one of check_in_date or due_date must be provided. \
         Requires specifying a repo when manifest is active. \
         Optionally supply body (markdown written verbatim after the frontmatter) \
         and any of the additional frontmatter fields; all new parameters are optional."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "repo": repo_property(),
                "path": {
                    "type": "string",
                    "description": "Relative path for the new file, or slug:path (e.g. life:todo/my-task.md)"
                },
                "title": {
                    "type": "string",
                    "description": "The tldr / title for the task"
                },
                "priority": {
                    "type": "integer",
                    "description": "Priority 1-5 (default: 3)"
                },
                "effort": {
                    "type": "string",
                    "description": "Effort level: trivial, small, medium, large, epic"
                },
                "check_in_date": {
                    "type": "string",
                    "description": "Check-in date (ISO 8601, e.g. 2026-04-15). At least one of check_in_date or due_date is required."
                },
                "due_date": {
                    "type": "string",
                    "description": "Due date (ISO 8601, e.g. 2026-04-15). At least one of check_in_date or due_date is required."
                },
                "labels": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Labels for the task"
                },
                "recurrence": {
                    "type": "object",
                    "description": "Optional recurrence block. Required `rrule` (RFC 5545), optional `lead_days` (non-negative int, requires due_date).",
                    "properties": {
                        "rrule": {
                            "type": "string",
                            "description": "RFC 5545 rrule string (e.g. \"FREQ=WEEKLY;BYDAY=SA\")"
                        },
                        "lead_days": {
                            "type": "integer",
                            "description": "Non-negative integer. Surface check-in this many days before due_date. Requires due_date."
                        }
                    },
                    "required": ["rrule"]
                },
                "body": {
                    "type": "string",
                    "description": "Optional markdown body written after the closing frontmatter ---. May contain any UTF-8 including blank lines and additional --- lines."
                },
                "summary": {
                    "type": "string",
                    "description": "Optional summary for the task (distinct from tldr)."
                },
                "context_link": {
                    "type": "string",
                    "description": "Optional hierarchical context link (e.g. parent doc path)."
                },
                "links": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional list of related document paths."
                },
                "blocked_by": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional list of blocking document paths."
                },
                "assigned_to": {
                    "type": "string",
                    "description": "Optional assignee identifier."
                },
                "on_date": {
                    "type": "string",
                    "description": "ISO 8601 date (YYYY-MM-DD). Mutually exclusive with start_date/end_date."
                },
                "start_date": {
                    "type": "string",
                    "description": "ISO 8601 date (YYYY-MM-DD). Start of a date range."
                },
                "end_date": {
                    "type": "string",
                    "description": "ISO 8601 date (YYYY-MM-DD). End of a date range."
                },
                "verified_date": {
                    "type": "string",
                    "description": "ISO 8601 date (YYYY-MM-DD). Date the task was verified."
                },
                "tentative_date": {
                    "type": "string",
                    "description": "ISO 8601 date (YYYY-MM-DD). Tentative scheduling date."
                }
            },
            "required": ["path", "title"]
        })
    }

    fn call(&self, args: Value) -> Result<Value> {
        let path_str = args["path"]
            .as_str()
            .context("missing required field: path")?;
        let (repo, path) = self.0.resolve_target(path_str, args["repo"].as_str())?;
        let title = args["title"]
            .as_str()
            .context("missing required field: title")?;
        let priority = match args["priority"].as_u64() {
            Some(v) if v <= u8::MAX as u64 => v as u8,
            Some(v) => anyhow::bail!("priority out of range: {v}"),
            None => 3,
        };
        let effort = match args["effort"].as_str() {
            Some(s) => Some(s.parse::<Effort>().map_err(|e| anyhow::anyhow!("{e}"))?),
            None => None,
        };
        let check_in_date = parse_date_field(&args["check_in_date"])?;
        let due_date = parse_date_field(&args["due_date"])?;
        let labels = parse_string_array(&args["labels"])?;
        let recurrence = parse_recurrence_field(&args["recurrence"])?;
        let body = args["body"].as_str().map(String::from);
        let summary = args["summary"].as_str().map(String::from);
        let context_link = args["context_link"].as_str().map(String::from);
        let links = parse_string_array(&args["links"])?;
        let blocked_by = parse_string_array(&args["blocked_by"])?;
        let assigned_to = args["assigned_to"].as_str().map(String::from);
        let on_date = parse_date_field(&args["on_date"])?;
        let start_date = parse_date_field(&args["start_date"])?;
        let end_date = parse_date_field(&args["end_date"])?;
        let verified_date = parse_date_field(&args["verified_date"])?;
        let tentative_date = parse_date_field(&args["tentative_date"])?;

        let opts = AddOptions {
            repo_dir: repo.path,
            path,
            title: title.to_string(),
            priority,
            effort,
            check_in_date,
            due_date,
            labels,
            recurrence,
            body,
            summary,
            context_link,
            links,
            blocked_by,
            assigned_to,
            on_date,
            start_date,
            end_date,
            verified_date,
            tentative_date,
        };

        let result = todo::todo_add(&opts)?;
        Ok(json!({ "path": result.path.display().to_string() }))
    }
}

// ---------------------------------------------------------------------------
// Todo done tool
// ---------------------------------------------------------------------------

struct TodoDoneTool(McpContext);

impl Tool for TodoDoneTool {
    fn name(&self) -> &str {
        "graf_todo_done"
    }

    fn description(&self) -> &str {
        "Mark a task done. `completion_date` = the date the user actually did it, in their \
         local timezone — pass the real date, not a guess. For slip recurrence (bare \
         `FREQ=…`, no `BY…`), `completion_date` becomes the new anchor; a second same-day \
         `done` is a no-op and discards any supplied comment. For anchored recurrence \
         (any `BY…`), each `done` advances one occurrence — calling twice advances two, \
         correct for \"I did two weeks of watering today.\" If the stored anchor is far \
         enough back that the next rrule occurrence is still before `completion_date`, \
         the server rejects with `stale_anchor` — ask whether to catch up one step \
         (`skip_past: false`) or resume on schedule (`skip_past: true`), then retry. \
         Don't supply `skip_past` when not needed. Optional `comment` attaches a markdown \
         note to the log entry. \
         \
         Structured error codes in the response `error` field: `stale_anchor`, \
         `skip_past_unnecessary`, `skip_past_on_slip_rule`, `skip_past_on_non_recurring`, \
         `already_done_or_not_due_yet`, `replacing_on_recurring` (`replacing` is \
         non-recurring only), `already_terminal` (task is already done/cancelled), \
         `invalid` (catch-all for malformed input / lint errors — read the `reason`), \
         `path_not_found` (path does not resolve in the repo), \
         `invalid_date` (malformed or missing date argument)."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "repo": repo_property(),
                "path": {
                    "type": "string",
                    "description": "Relative path to the task file, or slug:path (e.g. life:todo/my-task.md)"
                },
                "completion_date": {
                    "type": "string",
                    "description": "REQUIRED. The date the user actually did the task, ISO 8601 (YYYY-MM-DD), in their local timezone. Pass the real date, not a guess."
                },
                "replacing": {
                    "type": "string",
                    "description": "Non-recurring only. Required ONLY when overwriting a stored on_date/end_date with a different completion_date. Must equal the current stored value. Forces you to read current frontmatter before overwriting."
                },
                "skip_past": {
                    "type": "boolean",
                    "description": "Anchored rules only (any BY… modifier). Only supply in response to a stale_anchor error: true = resume on schedule; false = catch up one step. Rejected on slip or non-recurring tasks."
                },
                "comment": {
                    "type": "string",
                    "description": "Optional markdown note attached to the log entry. Discarded silently would be bad — if the call becomes a slip no-op or non-recurring conflict, the response flags comment_discarded: true."
                },
            },
            "required": ["path", "completion_date"]
        })
    }

    // The brenn-mcp framework stringifies `Err(anyhow!(...))` via
    // `format!("Error: {e}")`, so structured error JSON cannot travel
    // through the `Err` path. Return LLM-actionable errors as
    // `Ok(envelope_json)` so the LLM sees structured fields directly.
    // Infra errors (DB open, file I/O) still propagate as `Err` since
    // they're genuinely unexpected and there's nothing the LLM can do.
    fn call(&self, args: Value) -> Result<Value> {
        let path_str = args["path"]
            .as_str()
            .context("missing required field: path")?;
        let (repo, path) = match self.0.resolve_target(path_str, args["repo"].as_str()) {
            Ok(v) => v,
            Err(e) => {
                return Ok(todo::DoneError::PathNotFound {
                    path: path_str.to_string(),
                    reason: format!("{e:#}"),
                }
                .to_json());
            }
        };
        let completion_date = match parse_date_field(&args["completion_date"]) {
            Ok(Some(d)) => d,
            Ok(None) => {
                return Ok(todo::DoneError::InvalidDate {
                    reason: "missing required field: completion_date".to_string(),
                }
                .to_json());
            }
            Err(e) => {
                return Ok(todo::DoneError::InvalidDate {
                    reason: format!("{e:#}"),
                }
                .to_json());
            }
        };
        let replacing = match parse_date_field(&args["replacing"]) {
            Ok(d) => d,
            Err(e) => {
                return Ok(todo::DoneError::InvalidDate {
                    reason: format!("{e:#}"),
                }
                .to_json());
            }
        };
        let skip_past = match &args["skip_past"] {
            v if v.is_null() => None,
            v => Some(v.as_bool().context("skip_past must be a boolean")?),
        };
        let comment = args["comment"].as_str().map(String::from);

        match todo::todo_done(&DoneOptions {
            repo_dir: repo.path,
            path,
            completion_date,
            replacing,
            skip_past,
            comment,
        }) {
            Ok(result) => Ok(todo::done_result_to_json(&result)),
            Err(todo::DoneError::Infra(e)) => Err(e),
            Err(err) => Ok(err.to_json()),
        }
    }
}

// ---------------------------------------------------------------------------
// Todo cancel tool
// ---------------------------------------------------------------------------

struct TodoCancelTool(McpContext);

impl Tool for TodoCancelTool {
    fn name(&self) -> &str {
        "graf_todo_cancel"
    }

    fn description(&self) -> &str {
        "Cancel a task. Default: writes today's date to `end_date` (if \
         `start_date` is present) or `on_date`, clears scheduling fields and \
         rrule. If the task already has a stored on_date/end_date, the call \
         FAILS with the stored value in the error message — pass `date` \
         equal to the stored value to keep it, or `replacing` equal to the \
         stored value plus a chosen `date` to overwrite. Rejects \
         already-done/cancelled items."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "repo": repo_property(),
                "path": {
                    "type": "string",
                    "description": "Relative path to the task file, or slug:path (e.g. life:todo/my-task.md)"
                },
                "date": {
                    "type": "string",
                    "description": "Completion date (ISO 8601, e.g. 2026-04-16). Default: today. Written to end_date if start_date is present, otherwise on_date."
                },
                "replacing": {
                    "type": "string",
                    "description": "Required ONLY when overwriting a stored on_date/end_date with a different value. Must equal the current stored value. Forbidden in all other cases. Forces you to read the current frontmatter rather than guess."
                },
                "today": today_property(),
            },
            "required": ["path", "today"]
        })
    }

    // The brenn-mcp framework stringifies `Err(anyhow!(...))` via
    // `format!("Error: {e}")`, so structured error JSON cannot travel
    // through the `Err` path. Return LLM-actionable pre-flight errors as
    // `Ok(envelope_json)` so the LLM sees structured fields directly.
    // Infra errors (DB open, file I/O) and business-logic errors from
    // `todo_cancel` still propagate as `Err`.
    fn call(&self, args: Value) -> Result<Value> {
        let path_str = args["path"]
            .as_str()
            .context("missing required field: path")?;
        let (repo, path) = match self.0.resolve_target(path_str, args["repo"].as_str()) {
            Ok(v) => v,
            Err(e) => {
                return Ok(todo::DoneError::PathNotFound {
                    path: path_str.to_string(),
                    reason: format!("{e:#}"),
                }
                .to_json());
            }
        };
        let date = match parse_date_field(&args["date"]) {
            Ok(d) => d,
            Err(e) => {
                return Ok(todo::DoneError::InvalidDate {
                    reason: format!("{e:#}"),
                }
                .to_json());
            }
        };
        let replacing = match parse_date_field(&args["replacing"]) {
            Ok(d) => d,
            Err(e) => {
                return Ok(todo::DoneError::InvalidDate {
                    reason: format!("{e:#}"),
                }
                .to_json());
            }
        };
        let today = match parse_date_field(&args["today"])? {
            Some(d) => d,
            None => {
                return Ok(todo::DoneError::InvalidDate {
                    reason: "missing required field: today".to_string(),
                }
                .to_json());
            }
        };
        let result = todo::todo_cancel(&todo::CancelOptions {
            repo_dir: repo.path,
            path,
            date,
            replacing,
            today: Some(today),
        })?;
        let mut resp = json!({
            "path": result.path.display().to_string(),
        });
        result.date.set_json(&mut resp);
        Ok(resp)
    }
}

// ---------------------------------------------------------------------------
// Todo schedule tool
// ---------------------------------------------------------------------------

struct TodoScheduleTool(McpContext);

impl Tool for TodoScheduleTool {
    fn name(&self) -> &str {
        "graf_todo_schedule"
    }

    fn description(&self) -> &str {
        "Set a task's tentative date. `date` is absolute (ISO 8601) — the caller \
         resolves any \"today\"/offsets before calling. Writes only `tentative_date`; \
         `check_in_date`, `due_date`, `recurrence`, and `status` are untouched. \
         Any prior manual `sort_order` is cleared so the rescheduled task ranks by \
         `priority` in its new date bucket (reorder afterwards if a specific position \
         matters). Also serves the snooze use case — callers compute \
         `max(today, effective_date) + N` and pass the resulting absolute date. \
         \
         Structured error codes in the response `error` field: `terminal_task` \
         (task is already done/cancelled), `invalid_date` (malformed `date`), \
         `path_not_found` (path does not resolve in the repo)."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "repo": repo_property(),
                "path": {
                    "type": "string",
                    "description": "Relative path to the task file, or slug:path (e.g. life:todo/my-task.md)"
                },
                "date": {
                    "type": "string",
                    "description": "Target date (ISO 8601, e.g. 2026-04-15). Absolute — the caller resolves any \"today\"/offsets before calling."
                }
            },
            "required": ["path", "date"]
        })
    }

    // The brenn-mcp framework stringifies `Err(anyhow!(...))` via
    // `format!("Error: {e}")`, so structured error JSON cannot travel
    // through the `Err` path. Return errors as `Ok(envelope_json)` so
    // the LLM sees structured fields directly. Infra errors still
    // propagate as `Err` since they're genuinely unexpected and there's
    // nothing the LLM can do.
    fn call(&self, args: Value) -> Result<Value> {
        let path_str = args["path"]
            .as_str()
            .context("missing required field: path")?;
        let (repo, path) = match self.0.resolve_target(path_str, args["repo"].as_str()) {
            Ok(v) => v,
            Err(e) => {
                return Ok(todo::ScheduleError::PathNotFound {
                    path: path_str.to_string(),
                    reason: format!("{e:#}"),
                }
                .to_json());
            }
        };
        let date = match parse_date_field(&args["date"]) {
            Ok(Some(d)) => d,
            Ok(None) => {
                return Ok(todo::ScheduleError::InvalidDate {
                    reason: "missing required field: date".to_string(),
                }
                .to_json());
            }
            Err(e) => {
                return Ok(todo::ScheduleError::InvalidDate {
                    reason: format!("{e:#}"),
                }
                .to_json());
            }
        };
        match todo::todo_schedule(&ScheduleOptions {
            repo_dir: repo.path,
            path,
            date,
        }) {
            Ok(result) => Ok(json!({
                "path": result.path.display().to_string(),
                "tentative_date": result.tentative_date.to_string(),
            })),
            Err(todo::ScheduleError::Infra(e)) => Err(e),
            Err(err) => Ok(err.to_json()),
        }
    }
}

// ---------------------------------------------------------------------------
// Todo reorder tool
// ---------------------------------------------------------------------------

struct TodoReorderTool(McpContext);

impl Tool for TodoReorderTool {
    fn name(&self) -> &str {
        "graf_todo_reorder"
    }

    fn description(&self) -> &str {
        "Reorder a task by placing it between two neighbor items. Computes sort_order \
         as the midpoint and infers tentative_date from neighbors. At least one of \
         'after' or 'before' is required."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "repo": repo_property(),
                "path": {
                    "type": "string",
                    "description": "Relative path to the task to reorder, or slug:path (e.g. life:todo/my-task.md)"
                },
                "after": {
                    "type": "string",
                    "description": "Place after this item (relative path, or slug:path for cross-repo)"
                },
                "before": {
                    "type": "string",
                    "description": "Place before this item (relative path, or slug:path for cross-repo)"
                }
            },
            "required": ["path"]
        })
    }

    fn call(&self, args: Value) -> Result<Value> {
        let path_str = args["path"]
            .as_str()
            .context("missing required field: path")?;
        let (repo, path) = self.0.resolve_target(path_str, args["repo"].as_str())?;
        let manifest = AppManifest::load_default()?;
        let target_sync = graf::db::open_and_sync(&repo.path)?;

        let after = args["after"]
            .as_str()
            .map(|s| super::resolve_anchor(s, &target_sync.cache, manifest.as_ref()))
            .transpose()?;
        let before = args["before"]
            .as_str()
            .map(|s| super::resolve_anchor(s, &target_sync.cache, manifest.as_ref()))
            .transpose()?;
        drop(target_sync);

        let result = todo::todo_reorder(&ReorderOptions {
            repo_dir: repo.path,
            path,
            after,
            before,
        })?;
        let mut resp = json!({
            "path": result.path.display().to_string(),
            "sort_order": result.sort_order,
        });
        if let Some(date) = result.tentative_date {
            resp["tentative_date"] = json!(date.to_string());
        }
        Ok(resp)
    }
}

// ---------------------------------------------------------------------------
// Manifest list tool
// ---------------------------------------------------------------------------

struct ManifestListTool;

impl Tool for ManifestListTool {
    fn name(&self) -> &str {
        "graf_manifest_list"
    }

    fn description(&self) -> &str {
        "List all repos and domains in the application manifest."
    }

    fn input_schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }

    fn call(&self, _args: Value) -> Result<Value> {
        let manifest =
            AppManifest::load_default()?.ok_or_else(|| anyhow::anyhow!("no manifest found"))?;
        Ok(super::manifest_list_json(&manifest))
    }
}

// ---------------------------------------------------------------------------
// Manifest check tool
// ---------------------------------------------------------------------------

struct ManifestCheckTool;

impl Tool for ManifestCheckTool {
    fn name(&self) -> &str {
        "graf_manifest_check"
    }

    fn description(&self) -> &str {
        "Validate the application manifest against repo configs. \
         Returns validation errors and warnings."
    }

    fn input_schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }

    fn call(&self, _args: Value) -> Result<Value> {
        let manifest =
            AppManifest::load_default()?.ok_or_else(|| anyhow::anyhow!("no manifest found"))?;
        Ok(super::manifest_check_json(&manifest))
    }
}

// ---------------------------------------------------------------------------
// MCP helpers
// ---------------------------------------------------------------------------

fn parse_status_array(val: &Value) -> Result<Vec<Status>> {
    let Some(arr) = val.as_array() else {
        return Ok(Vec::new());
    };
    arr.iter()
        .map(|v| {
            let s = v.as_str().context("status must be a string")?;
            s.parse::<Status>().map_err(|e| anyhow::anyhow!("{e}"))
        })
        .collect()
}

fn parse_string_array(val: &Value) -> Result<Vec<String>> {
    let Some(arr) = val.as_array() else {
        return Ok(Vec::new());
    };
    arr.iter()
        .map(|v| {
            v.as_str()
                .map(String::from)
                .context("array element must be a string")
        })
        .collect()
}

fn parse_date_field(val: &Value) -> Result<Option<NaiveDate>> {
    match val.as_str() {
        Some(s) => {
            let d = NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .with_context(|| format!("invalid date: {s}"))?;
            Ok(Some(d))
        }
        None => Ok(None),
    }
}

fn parse_recurrence_field(val: &Value) -> Result<Option<Recurrence>> {
    if val.is_null() {
        return Ok(None);
    }
    let Some(obj) = val.as_object() else {
        return Ok(None);
    };
    let rrule = obj
        .get("rrule")
        .and_then(|v| v.as_str())
        .context("recurrence requires `rrule` (string, RFC 5545)")?
        .to_string();
    let lead_days = match obj.get("lead_days") {
        Some(v) if v.is_null() => None,
        Some(v) => {
            let n = v
                .as_u64()
                .context("recurrence.lead_days must be a non-negative integer")?;
            if n > u32::MAX as u64 {
                anyhow::bail!("recurrence.lead_days out of range: {n}");
            }
            Some(n as u32)
        }
        None => None,
    };
    Ok(Some(Recurrence { rrule, lead_days }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    fn cancel_tool_ctx(dir: &TempDir) -> TodoCancelTool {
        TodoCancelTool(McpContext {
            default_repo: Some(dir.path().to_str().unwrap().to_string()),
        })
    }

    fn run_git(dir: &std::path::Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git invocation");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn setup_cancel_repo() -> TempDir {
        let dir = TempDir::new().unwrap();
        run_git(dir.path(), &["init", "-b", "main"]);
        run_git(dir.path(), &["config", "user.email", "test@test.com"]);
        run_git(dir.path(), &["config", "user.name", "Test"]);
        run_git(dir.path(), &["commit", "--allow-empty", "-m", "init"]);
        dir
    }

    #[test]
    fn parse_recurrence_field_null_is_none() {
        assert!(parse_recurrence_field(&Value::Null).unwrap().is_none());
    }

    #[test]
    fn parse_recurrence_field_non_object_is_none() {
        // Defensive: a scalar passed where an object is expected is silently
        // ignored (caller never sees a Recurrence). This matches the existing
        // `parse_date_field` / `parse_string_array` tolerance pattern.
        assert!(
            parse_recurrence_field(&json!("FREQ=WEEKLY"))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn parse_recurrence_field_missing_rrule_errors() {
        let err = parse_recurrence_field(&json!({})).unwrap_err();
        assert!(format!("{err:#}").contains("rrule"));
    }

    #[test]
    fn parse_recurrence_field_non_string_rrule_errors() {
        let err = parse_recurrence_field(&json!({ "rrule": 7 })).unwrap_err();
        assert!(format!("{err:#}").contains("rrule"));
    }

    #[test]
    fn parse_recurrence_field_accepts_rrule_only() {
        let rec = parse_recurrence_field(&json!({ "rrule": "FREQ=WEEKLY" }))
            .unwrap()
            .expect("Some");
        assert_eq!(rec.rrule, "FREQ=WEEKLY");
        assert_eq!(rec.lead_days, None);
    }

    #[test]
    fn parse_recurrence_field_accepts_lead_days() {
        let rec = parse_recurrence_field(&json!({ "rrule": "FREQ=WEEKLY", "lead_days": 3 }))
            .unwrap()
            .expect("Some");
        assert_eq!(rec.lead_days, Some(3));
    }

    #[test]
    fn parse_recurrence_field_negative_lead_days_errors() {
        let err = parse_recurrence_field(&json!({ "rrule": "FREQ=WEEKLY", "lead_days": -1 }))
            .unwrap_err();
        assert!(format!("{err:#}").contains("lead_days"));
    }

    #[test]
    fn parse_recurrence_field_non_integer_lead_days_errors() {
        let err = parse_recurrence_field(&json!({ "rrule": "FREQ=WEEKLY", "lead_days": "7" }))
            .unwrap_err();
        assert!(format!("{err:#}").contains("lead_days"));
    }

    #[test]
    fn parse_recurrence_field_overflow_lead_days_errors() {
        let overflow: u64 = u32::MAX as u64 + 1;
        let err = parse_recurrence_field(&json!({ "rrule": "FREQ=WEEKLY", "lead_days": overflow }))
            .unwrap_err();
        assert!(format!("{err:#}").contains("out of range"));
    }

    #[test]
    fn parse_recurrence_field_null_lead_days_accepted() {
        let rec = parse_recurrence_field(&json!({
            "rrule": "FREQ=WEEKLY",
            "lead_days": serde_json::Value::Null,
        }))
        .unwrap()
        .expect("Some");
        assert_eq!(rec.lead_days, None);
    }

    #[test]
    fn cancel_schema_requires_today() {
        let tool = TodoCancelTool(McpContext { default_repo: None });
        let schema = tool.input_schema();
        let required = schema["required"].as_array().expect("required is array");
        let required_strs: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();
        assert!(
            required_strs.contains(&"path"),
            "schema required missing 'path': {schema}"
        );
        assert!(
            required_strs.contains(&"today"),
            "schema required missing 'today': {schema}"
        );
    }

    #[test]
    fn cancel_call_missing_today_returns_structured_error() {
        let dir = TempDir::new().unwrap();
        // Create a valid task file so the guard is tested against a path
        // that would otherwise succeed, not one rejected for being missing.
        let todo_dir = dir.path().join("todo");
        std::fs::create_dir_all(&todo_dir).unwrap();
        std::fs::write(
            todo_dir.join("task.md"),
            "---\ntldr: test\nstatus: todo\ncheck_in_date: 2026-04-18\n---\nBody.\n",
        )
        .unwrap();
        let tool = cancel_tool_ctx(&dir);
        let result = tool
            .call(json!({
                "repo": dir.path().to_str().unwrap(),
                "path": "todo/task.md"
            }))
            .expect("call returned Err");
        assert_eq!(
            result["error"], "invalid_date",
            "expected error=invalid_date, got: {result}"
        );
        let reason = result["reason"].as_str().unwrap_or("");
        assert!(
            reason.contains("today"),
            "expected reason mentioning 'today', got: {reason}"
        );
    }

    #[test]
    fn cancel_call_valid_args_succeeds() {
        let dir = setup_cancel_repo();
        let todo_dir = dir.path().join("todo");
        std::fs::create_dir_all(&todo_dir).unwrap();
        std::fs::write(
            todo_dir.join("task.md"),
            "---\ntldr: test\nstatus: todo\ncheck_in_date: 2026-04-18\n---\nBody.\n",
        )
        .unwrap();
        let tool = cancel_tool_ctx(&dir);
        let result = tool
            .call(json!({
                "repo": dir.path().to_str().unwrap(),
                "path": "todo/task.md",
                "today": "2026-04-18"
            }))
            .expect("call returned Err");
        assert!(
            result.get("error").is_none(),
            "unexpected error in response: {result}"
        );
        assert!(
            result["path"].as_str().is_some(),
            "expected 'path' field in response: {result}"
        );
    }
}
