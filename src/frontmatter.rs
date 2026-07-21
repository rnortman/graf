use std::fmt;
use std::ops::Range;
use std::str::FromStr;

use anyhow::{Context, Result, ensure};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Valid values for the `status` frontmatter field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Todo,
    InProgress,
    Done,
    Cancelled,
    Reminder,
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Status::Todo => write!(f, "todo"),
            Status::InProgress => write!(f, "in_progress"),
            Status::Done => write!(f, "done"),
            Status::Cancelled => write!(f, "cancelled"),
            Status::Reminder => write!(f, "reminder"),
        }
    }
}

impl Status {
    /// All variants, in declaration order.
    pub const ALL: [Status; 5] = [
        Status::Todo,
        Status::InProgress,
        Status::Done,
        Status::Cancelled,
        Status::Reminder,
    ];

    /// Returns true for terminal statuses (Done, Cancelled).
    pub fn is_terminal(&self) -> bool {
        matches!(self, Status::Done | Status::Cancelled)
    }
}

impl FromStr for Status {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "todo" => Ok(Status::Todo),
            "in_progress" => Ok(Status::InProgress),
            "done" => Ok(Status::Done),
            "cancelled" => Ok(Status::Cancelled),
            "reminder" => Ok(Status::Reminder),
            _ => Err(format!(
                "invalid status: {s} (expected: todo, in_progress, done, cancelled, reminder)"
            )),
        }
    }
}

/// Valid values for the `effort` frontmatter field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effort {
    Trivial,
    Small,
    Medium,
    Large,
    Epic,
}

impl fmt::Display for Effort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Effort::Trivial => write!(f, "trivial"),
            Effort::Small => write!(f, "small"),
            Effort::Medium => write!(f, "medium"),
            Effort::Large => write!(f, "large"),
            Effort::Epic => write!(f, "epic"),
        }
    }
}

impl FromStr for Effort {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "trivial" => Ok(Effort::Trivial),
            "small" => Ok(Effort::Small),
            "medium" => Ok(Effort::Medium),
            "large" => Ok(Effort::Large),
            "epic" => Ok(Effort::Epic),
            _ => Err(format!(
                "invalid effort: {s} (expected: trivial, small, medium, large, epic)"
            )),
        }
    }
}

/// Structured recurrence block. Nested under `recurrence` in frontmatter;
/// `rrule` is required within, `lead_days` is optional (requires `due_date`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Recurrence {
    pub rrule: String,
    pub lead_days: Option<u32>,
}

/// One entry in `completion_log`. `completed` is required; `occurrence`
/// (anchored recurring instance) and `comment` are optional.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CompletionLogEntry {
    pub completed: NaiveDate,
    pub occurrence: Option<NaiveDate>,
    pub comment: Option<String>,
}

/// YAML frontmatter schema for graf documents.
///
/// All fields optional except `tldr`. Unknown fields are rejected.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Frontmatter {
    pub tldr: String,

    #[serde(default)]
    pub labels: Vec<String>,

    pub created: Option<String>,
    pub updated: Option<String>,

    pub summary: Option<String>,

    // Dates
    pub on_date: Option<NaiveDate>,
    pub start_date: Option<NaiveDate>,
    pub end_date: Option<NaiveDate>,
    pub due_date: Option<NaiveDate>,
    pub check_in_date: Option<NaiveDate>,
    pub verified_date: Option<NaiveDate>,

    // Links
    pub context_link: Option<String>,
    #[serde(default)]
    pub links: Vec<String>,

    // Attachments
    #[serde(default)]
    pub attachments: Vec<String>,

    // Todo-specific
    pub status: Option<Status>,
    pub priority: Option<u8>,
    pub effort: Option<Effort>,
    #[serde(default)]
    pub blocked_by: Vec<String>,
    // legacy-shape compat; kept indefinitely — cloned/restored repos keep
    // arriving with the old flat rrule. `recurrence` (below) is canonical;
    // read through `effective_rrule()`.
    // TODO(recurring-phase-2-remove-flat-rrule)
    pub rrule: Option<String>,
    pub recurrence: Option<Recurrence>,
    #[serde(default)]
    pub completion_log: Vec<CompletionLogEntry>,

    // Scheduling
    pub tentative_date: Option<NaiveDate>,
    pub sort_order: Option<f64>,
    pub assigned_to: Option<String>,
}

/// Rank used when both `sort_order` and `priority` are absent.
/// One past max priority (5), so unranked items sort after all prioritized ones.
pub const UNRANKED: f64 = 6.0;

/// Sort rank: `COALESCE(sort_order, priority, UNRANKED)`.
/// The single Rust encoding of the rank formula. The SQL encodings in
/// `db.rs` (ORDER BY and the `idx_active_effective_date` expression index)
/// must agree with this; integration tests in
/// `tests/multi_repo_integration.rs` cross-check the two engines.
pub fn rank(sort_order: Option<f64>, priority: Option<u8>) -> f64 {
    sort_order.or(priority.map(f64::from)).unwrap_or(UNRANKED)
}

impl Frontmatter {
    /// The effective rrule string, reading the canonical nested
    /// `recurrence.rrule` first, then falling back to the legacy flat
    /// `rrule` field.
    pub fn effective_rrule(&self) -> Option<&str> {
        self.recurrence
            .as_ref()
            .map(|r| r.rrule.as_str())
            .or(self.rrule.as_deref())
    }

    /// Validate field values that can't be checked by serde alone.
    pub fn validate(&self) -> Result<()> {
        if let Some(p) = self.priority {
            ensure!((1..=5).contains(&p), "priority must be 1-5, got {p}");
        }
        if let Some(s) = self.sort_order {
            ensure!(s.is_finite(), "sort_order must be finite, got {s}");
        }
        if let Some(ref ts) = self.created {
            parse_timestamp(ts).with_context(|| format!("invalid created timestamp: {ts}"))?;
        }
        if let Some(ref ts) = self.updated {
            parse_timestamp(ts).with_context(|| format!("invalid updated timestamp: {ts}"))?;
        }
        if let Some(ref r) = self.recurrence
            && r.lead_days.is_some()
            && self.due_date.is_none()
        {
            anyhow::bail!("recurrence.lead_days requires due_date");
        }
        Ok(())
    }
}

/// Parse an ISO 8601 timestamp string, returning the parsed DateTime.
fn parse_timestamp(s: &str) -> Result<DateTime<Utc>> {
    // Try RFC 3339 first (most common: "2025-05-20T20:35:36.732Z")
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Ok(dt.with_timezone(&Utc));
    }
    // Try without fractional seconds: "2025-05-20T20:35:36Z"
    if let Ok(dt) = DateTime::parse_from_rfc3339(&format!("{s}.0")) {
        return Ok(dt.with_timezone(&Utc));
    }
    anyhow::bail!("cannot parse as ISO 8601 datetime");
}

// ---------------------------------------------------------------------------
// YAML field mutations
// ---------------------------------------------------------------------------

/// Typed frontmatter field identifier.
///
/// Every variant corresponds 1:1 with a field in [`Frontmatter`].
/// Using this instead of raw strings ensures invalid field names are caught
/// at compile time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Tldr,
    Labels,
    Created,
    Updated,
    Summary,
    OnDate,
    StartDate,
    EndDate,
    DueDate,
    CheckInDate,
    VerifiedDate,
    ContextLink,
    Links,
    Attachments,
    Status,
    Priority,
    Effort,
    BlockedBy,
    Rrule,
    Recurrence,
    CompletionLog,
    TentativeDate,
    SortOrder,
    AssignedTo,
}

impl Field {
    /// The YAML key name for this field.
    pub fn yaml_key(self) -> &'static str {
        match self {
            Field::Tldr => "tldr",
            Field::Labels => "labels",
            Field::Created => "created",
            Field::Updated => "updated",
            Field::Summary => "summary",
            Field::OnDate => "on_date",
            Field::StartDate => "start_date",
            Field::EndDate => "end_date",
            Field::DueDate => "due_date",
            Field::CheckInDate => "check_in_date",
            Field::VerifiedDate => "verified_date",
            Field::ContextLink => "context_link",
            Field::Links => "links",
            Field::Attachments => "attachments",
            Field::Status => "status",
            Field::Priority => "priority",
            Field::Effort => "effort",
            Field::BlockedBy => "blocked_by",
            Field::Rrule => "rrule",
            Field::Recurrence => "recurrence",
            Field::CompletionLog => "completion_log",
            Field::TentativeDate => "tentative_date",
            Field::SortOrder => "sort_order",
            Field::AssignedTo => "assigned_to",
        }
    }
}

/// A single field-level mutation to apply to YAML frontmatter.
#[derive(Debug, Clone)]
pub enum FieldMutation {
    /// Set a field to a value (upserts: replaces if exists, appends if not).
    Set { field: Field, value: String },
    /// Remove a field entirely.  No-op if the field doesn't exist.
    Remove { field: Field },
    /// Append a single flow-style list item to a sequence-valued key.
    /// If the key is absent, the entry is created under a new key block.
    AppendCompletionLog { entry: CompletionLogEntry },
}

/// Replace an existing YAML field's value, or insert it at the end if not present.
pub fn upsert_yaml_field(lines: &mut Vec<String>, field: Field, value: &str) {
    let key = field.yaml_key();
    let prefix = format!("{key}:");
    for line in lines.iter_mut() {
        if line.starts_with(&prefix) {
            *line = format!("{key}: {value}");
            return;
        }
    }
    // Not found — append at end
    lines.push(format!("{key}: {value}"));
}

/// Remove a YAML field from frontmatter lines.  No-op if not present.
///
/// Handles block-style fields (`recurrence:` with indented children,
/// `completion_log:` with `-` items) by also stripping the indented/list
/// lines that follow the key.
pub fn remove_yaml_field(lines: &mut Vec<String>, field: Field) {
    let prefix = format!("{}:", field.yaml_key());
    let mut i = 0;
    while i < lines.len() {
        if lines[i].starts_with(&prefix) {
            lines.remove(i);
            // Strip any immediately-following block children (lines starting
            // with whitespace or a list marker with leading whitespace).
            while i < lines.len() {
                let l = &lines[i];
                // Block children are indented: `  key:`, `  - item`, or a
                // tab. A bare `- foo` is a top-level sequence item, not a
                // child of the removed scalar/list — don't consume it.
                let is_child = l.starts_with("  ") || l.starts_with("\t");
                if is_child {
                    lines.remove(i);
                } else {
                    break;
                }
            }
        } else {
            i += 1;
        }
    }
}

/// Apply a set of field mutations to a file's frontmatter, preserving
/// the rest of the file content.  Returns the new file content.
///
/// Errors if the file has no frontmatter.
pub fn rewrite_frontmatter(content: &str, mutations: &[FieldMutation]) -> Result<String> {
    let range = frontmatter_yaml_range(content)?.context("file has no frontmatter to rewrite")?;

    let yaml = &content[range.clone()];
    let mut lines: Vec<String> = yaml.lines().map(|l| l.to_string()).collect();

    for mutation in mutations {
        match mutation {
            FieldMutation::Set { field, value } => {
                upsert_yaml_field(&mut lines, *field, value);
            }
            FieldMutation::Remove { field } => {
                remove_yaml_field(&mut lines, *field);
            }
            FieldMutation::AppendCompletionLog { entry } => {
                append_completion_log_entry(&mut lines, entry);
            }
        }
    }

    let new_yaml = lines.join("\n");
    let mut result = String::with_capacity(content.len() + 64);
    result.push_str(&content[..range.start]);
    result.push_str(&new_yaml);
    result.push_str(&content[range.end..]);

    Ok(result)
}

/// Append one completion_log entry. Creates the block if absent.
///
/// Emits flow style (single line, `- { completed: …, … }`) normally, and
/// switches to block style with a `comment: |` block scalar when the
/// comment contains a newline.
fn append_completion_log_entry(lines: &mut Vec<String>, entry: &CompletionLogEntry) {
    let item_lines = format_completion_log_entry(entry);
    let key = Field::CompletionLog.yaml_key();
    let prefix = format!("{key}:");

    let existing_idx = lines.iter().position(|l| l.starts_with(&prefix));

    let Some(start) = existing_idx else {
        // No existing block; append header + item.
        lines.push(format!("{key}:"));
        lines.extend(item_lines);
        return;
    };

    // Find end of the block: next line that is neither a list item nor a
    // continuation of the previous item.
    let mut end = start + 1;
    while end < lines.len() {
        let l = &lines[end];
        let is_item = l.starts_with("  -") || l.starts_with("- ");
        let is_continuation = l.starts_with("  ") && !l.starts_with("  -");
        if is_item || is_continuation {
            end += 1;
        } else {
            break;
        }
    }
    for (i, line) in item_lines.into_iter().enumerate() {
        lines.insert(end + i, line);
    }
}

/// Render an entry as one or more YAML lines. Flow form (single line) for
/// entries without a multi-line comment; block form (multi-line, with
/// `comment: |` block scalar) when the comment carries a newline.
fn format_completion_log_entry(entry: &CompletionLogEntry) -> Vec<String> {
    let multiline_comment = entry.comment.as_deref().is_some_and(|c| c.contains('\n'));
    if multiline_comment {
        format_completion_log_entry_block(entry)
    } else {
        vec![format_completion_log_entry_flow(entry)]
    }
}

fn format_completion_log_entry_flow(entry: &CompletionLogEntry) -> String {
    let mut out = format!("  - {{ completed: {}", entry.completed);
    if let Some(occ) = entry.occurrence {
        out.push_str(&format!(", occurrence: {occ}"));
    }
    if let Some(ref comment) = entry.comment {
        out.push_str(&format!(", comment: {}", yaml_scalar(comment)));
    }
    out.push_str(" }");
    out
}

/// Block form, used when the comment contains a newline. The block-scalar
/// `comment: |` literal preserves interior newlines and keeps the entry
/// diff-friendly. Each line is right-trimmed.
fn format_completion_log_entry_block(entry: &CompletionLogEntry) -> Vec<String> {
    let mut out = vec![format!("  - completed: {}", entry.completed)];
    if let Some(occ) = entry.occurrence {
        out.push(format!("    occurrence: {occ}"));
    }
    // Guaranteed Some by caller (`multiline_comment` gate).
    let comment = entry
        .comment
        .as_deref()
        .expect("block form only reached when comment is multi-line");
    out.push("    comment: |".to_string());
    // `split('\n')` rather than `.lines()` so a trailing newline becomes an
    // empty line in the output — preserving interior newline count.
    // Trim trailing blank lines (no point emitting `      ` for them).
    let mut content_lines: Vec<&str> = comment.split('\n').collect();
    while content_lines.last().is_some_and(|l| l.trim().is_empty()) {
        content_lines.pop();
    }
    for line in content_lines {
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            out.push(String::new());
        } else {
            out.push(format!("      {trimmed}"));
        }
    }
    out
}

/// Quote a YAML scalar conservatively: double-quoted if the value contains
/// any character that's ambiguous in either block- or flow-context
/// (superset of both contexts' trigger sets); bare otherwise. Valid for
/// use as a block-scalar value (`key: VALUE`) and as a flow-mapping value
/// (`{ key: VALUE }`).
pub fn yaml_scalar(s: &str) -> String {
    // Force quoting when the value contains characters that YAML parsers treat
    // as special, including \r (YAML 1.2 line break) and indicator chars that
    // change scalar interpretation when they appear at the start of a value.
    let starts_with_indicator = s
        .starts_with(['&', '*', '!', '|', '>', '?', '[', ']', '%', '@', '`', '\''])
        || s.starts_with("- ");
    let needs_quote = s.contains([':', '#', '{', '}', ',', '"', '\n', '\r'])
        || s.starts_with(' ')
        || s.ends_with(' ')
        || s.ends_with(':')
        || s.is_empty()
        || starts_with_indicator;
    if needs_quote {
        let escaped = s
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
            .replace('\r', "\\r");
        format!("\"{escaped}\"")
    } else {
        s.to_string()
    }
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// Returns the byte range of the YAML content between the `---` delimiters.
///
/// The range covers only the YAML text itself, not the delimiters.
/// Returns `None` if the file has no frontmatter.
pub fn frontmatter_yaml_range(content: &str) -> Result<Option<Range<usize>>> {
    let trimmed = content.trim_start();
    let leading_ws = content.len() - trimmed.len();

    if !trimmed.starts_with("---") {
        return Ok(None);
    }

    let after_opening_offset = leading_ws + 3;
    // Skip past the newline after opening ---
    let newline_pos = content[after_opening_offset..]
        .find('\n')
        .context("frontmatter has opening `---` but no newline after it")?;
    let yaml_start = after_opening_offset + newline_pos + 1;

    let closing = content[yaml_start..]
        .find("\n---")
        .context("frontmatter has opening `---` but no closing `---`")?;
    let yaml_end = yaml_start + closing;

    Ok(Some(yaml_start..yaml_end))
}

/// Extract YAML frontmatter from a markdown file's contents.
///
/// Returns `None` if the file has no frontmatter (doesn't start with `---`).
pub fn parse_frontmatter(content: &str) -> Result<Option<Frontmatter>> {
    let range = frontmatter_yaml_range(content)?;
    let Some(range) = range else {
        return Ok(None);
    };

    let yaml_str = &content[range];
    let fm: Frontmatter =
        serde_yaml::from_str(yaml_str).context("failed to parse YAML frontmatter")?;

    Ok(Some(fm))
}

/// Parse frontmatter from a file path.
pub fn parse_file(path: &Path) -> Result<Option<Frontmatter>> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    parse_frontmatter(&content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_statuses_exhaustive() {
        // Exhaustive match: adding a Status variant makes this non-exhaustive,
        // a compile error prompting an update to Status::ALL. This is a nudge,
        // not a proof of ALL's completeness.
        for s in Status::ALL {
            match s {
                Status::Todo
                | Status::InProgress
                | Status::Done
                | Status::Cancelled
                | Status::Reminder => {}
            }
        }

        // Serializations must be pairwise distinct (duplicate-entry guard).
        let serialized: Vec<String> = Status::ALL.iter().map(|s| s.to_string()).collect();
        for i in 0..serialized.len() {
            for j in (i + 1)..serialized.len() {
                assert_ne!(
                    serialized[i], serialized[j],
                    "duplicate status serialization in Status::ALL"
                );
            }
        }
    }

    #[test]
    fn test_parse_basic_frontmatter() {
        let content = r#"---
tldr: "Test document"
labels: [test, example]
check_in_date: 2025-10-01
status: todo
priority: 2
---

Some body content here.
"#;
        let fm = parse_frontmatter(content).unwrap().unwrap();
        assert_eq!(fm.tldr, "Test document");
        assert_eq!(fm.labels, vec!["test", "example"]);
        assert_eq!(
            fm.check_in_date,
            Some(NaiveDate::from_ymd_opt(2025, 10, 1).unwrap())
        );
        assert_eq!(fm.status, Some(Status::Todo));
        assert_eq!(fm.priority, Some(2));
    }

    #[test]
    fn test_no_frontmatter() {
        let content = "Just some markdown content.";
        assert!(parse_frontmatter(content).unwrap().is_none());
    }

    #[test]
    fn test_unclosed_frontmatter() {
        let content = "---\ntldr: oops\nno closing delimiter";
        assert!(parse_frontmatter(content).is_err());
    }

    #[test]
    fn test_minimal_frontmatter() {
        let content = "---\ntldr: minimal\n---\n";
        let fm = parse_frontmatter(content).unwrap().unwrap();
        assert_eq!(fm.tldr, "minimal");
        assert!(fm.labels.is_empty());
        assert!(fm.check_in_date.is_none());
        assert!(fm.status.is_none());
    }

    #[test]
    fn test_all_date_fields() {
        let content = r#"---
tldr: "Dates test"
on_date: 2025-01-01
start_date: 2025-01-01
end_date: 2025-12-31
due_date: 2025-06-15
check_in_date: 2025-03-01
---
"#;
        let fm = parse_frontmatter(content).unwrap().unwrap();
        assert!(fm.on_date.is_some());
        assert!(fm.start_date.is_some());
        assert!(fm.end_date.is_some());
        assert!(fm.due_date.is_some());
        assert!(fm.check_in_date.is_some());
    }

    #[test]
    fn test_links_and_attachments() {
        let content = r#"---
tldr: "Links test"
context_link: life/kb/finance/tips.md
links:
  - life/kb/finance/bonds.md
  - life/kb/finance/treasury.md
attachments: [.pdf, .csv]
---
"#;
        let fm = parse_frontmatter(content).unwrap().unwrap();
        assert_eq!(fm.context_link, Some("life/kb/finance/tips.md".to_string()));
        assert_eq!(fm.links.len(), 2);
        assert_eq!(fm.attachments, vec![".pdf", ".csv"]);
    }

    #[test]
    fn test_summary_field() {
        let content = r#"---
tldr: "Short summary"
summary: "This is a longer description that provides more context."
---
"#;
        let fm = parse_frontmatter(content).unwrap().unwrap();
        assert_eq!(
            fm.summary,
            Some("This is a longer description that provides more context.".to_string())
        );
    }

    #[test]
    fn test_deny_unknown_fields() {
        let content = "---\ntldr: test\nbogus_field: true\n---\n";
        let err = parse_frontmatter(content).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("bogus_field"),
            "error should mention the unknown field: {msg}"
        );
    }

    #[test]
    fn test_invalid_status() {
        let content = "---\ntldr: test\nstatus: bogus\n---\n";
        assert!(parse_frontmatter(content).is_err());
    }

    #[test]
    fn test_invalid_effort() {
        let content = "---\ntldr: test\neffort: gigantic\n---\n";
        assert!(parse_frontmatter(content).is_err());
    }

    #[test]
    fn test_all_status_values() {
        // Frozen on-disk spellings: the wire format written to markdown files.
        // Pinned independently of Display so a coordinated rename across
        // Display/FromStr/serde still trips here rather than silently changing
        // the format under existing files.
        assert_eq!(
            Status::ALL.map(|s| s.to_string()),
            ["todo", "in_progress", "done", "cancelled", "reminder"]
        );

        for s in Status::ALL {
            // Display / FromStr roundtrip.
            let rendered = s.to_string();
            assert_eq!(rendered.parse::<Status>(), Ok(s.clone()));
            // serde (YAML) roundtrip: the DB stores Display strings, so YAML
            // parsing must map each Display spelling back to its variant.
            let content = format!("---\ntldr: test\nstatus: {rendered}\n---\n");
            let fm = parse_frontmatter(&content).unwrap().unwrap();
            assert_eq!(fm.status, Some(s));
        }
    }

    #[test]
    fn test_status_is_terminal() {
        assert!(!Status::Todo.is_terminal());
        assert!(!Status::InProgress.is_terminal());
        assert!(Status::Done.is_terminal());
        assert!(Status::Cancelled.is_terminal());
        assert!(!Status::Reminder.is_terminal());
    }

    #[test]
    fn test_all_effort_values() {
        for (yaml, expected) in [
            ("trivial", Effort::Trivial),
            ("small", Effort::Small),
            ("medium", Effort::Medium),
            ("large", Effort::Large),
            ("epic", Effort::Epic),
        ] {
            let content = format!("---\ntldr: test\neffort: {yaml}\n---\n");
            let fm = parse_frontmatter(&content).unwrap().unwrap();
            assert_eq!(fm.effort, Some(expected));
        }
    }

    #[test]
    fn test_priority_valid_range() {
        for p in 1..=5 {
            let content = format!("---\ntldr: test\npriority: {p}\n---\n");
            let fm = parse_frontmatter(&content).unwrap().unwrap();
            fm.validate().unwrap();
        }
    }

    #[test]
    fn test_priority_zero_invalid() {
        let content = "---\ntldr: test\npriority: 0\n---\n";
        let fm = parse_frontmatter(content).unwrap().unwrap();
        assert!(fm.validate().is_err());
    }

    #[test]
    fn test_priority_six_invalid() {
        let content = "---\ntldr: test\npriority: 6\n---\n";
        let fm = parse_frontmatter(content).unwrap().unwrap();
        assert!(fm.validate().is_err());
    }

    #[test]
    fn test_valid_timestamps() {
        let content = r#"---
tldr: test
created: '2025-05-20T20:35:36.732Z'
updated: '2025-06-01T15:30:52.778Z'
---
"#;
        let fm = parse_frontmatter(content).unwrap().unwrap();
        fm.validate().unwrap();
    }

    #[test]
    fn test_timestamp_without_fractional_seconds() {
        let content = r#"---
tldr: test
created: '2025-05-20T20:35:36Z'
updated: '2026-03-16T00:00:00Z'
---
"#;
        let fm = parse_frontmatter(content).unwrap().unwrap();
        fm.validate().unwrap();
    }

    #[test]
    fn test_invalid_timestamp() {
        let content = "---\ntldr: test\ncreated: 'not-a-timestamp'\n---\n";
        let fm = parse_frontmatter(content).unwrap().unwrap();
        assert!(fm.validate().is_err());
    }

    #[test]
    fn test_frontmatter_yaml_range() {
        let content = "---\ntldr: test\nlabels: [a]\n---\nBody text.";
        let range = frontmatter_yaml_range(content).unwrap().unwrap();
        assert_eq!(&content[range], "tldr: test\nlabels: [a]");
    }

    #[test]
    fn test_frontmatter_yaml_range_no_frontmatter() {
        let content = "Just text.";
        assert!(frontmatter_yaml_range(content).unwrap().is_none());
    }

    #[test]
    fn test_verified_date() {
        let content = "---\ntldr: test\nverified_date: 2025-04-03\n---\n";
        let fm = parse_frontmatter(content).unwrap().unwrap();
        assert_eq!(
            fm.verified_date,
            Some(NaiveDate::from_ymd_opt(2025, 4, 3).unwrap())
        );
    }

    #[test]
    fn test_todo_specific_fields() {
        let content = r#"---
tldr: "Task test"
status: in_progress
priority: 3
effort: medium
blocked_by:
  - life/todo/other-task.md
rrule: "FREQ=MONTHLY"
---
"#;
        let fm = parse_frontmatter(content).unwrap().unwrap();
        assert_eq!(fm.status, Some(Status::InProgress));
        assert_eq!(fm.priority, Some(3));
        assert_eq!(fm.effort, Some(Effort::Medium));
        assert_eq!(fm.blocked_by, vec!["life/todo/other-task.md"]);
        assert_eq!(fm.rrule, Some("FREQ=MONTHLY".to_string()));
        fm.validate().unwrap();
    }

    #[test]
    fn test_tentative_date() {
        let content = "---\ntldr: test\ntentative_date: 2026-04-10\n---\n";
        let fm = parse_frontmatter(content).unwrap().unwrap();
        assert_eq!(
            fm.tentative_date,
            Some(NaiveDate::from_ymd_opt(2026, 4, 10).unwrap())
        );
    }

    #[test]
    fn test_sort_order() {
        let content = "---\ntldr: test\nsort_order: 2.5\n---\n";
        let fm = parse_frontmatter(content).unwrap().unwrap();
        assert_eq!(fm.sort_order, Some(2.5));
        fm.validate().unwrap();
    }

    #[test]
    fn test_sort_order_nan_rejected() {
        let content = "---\ntldr: test\nsort_order: .nan\n---\n";
        let fm = parse_frontmatter(content).unwrap().unwrap();
        assert!(fm.validate().is_err());
    }

    #[test]
    fn test_sort_order_inf_rejected() {
        let content = "---\ntldr: test\nsort_order: .inf\n---\n";
        let fm = parse_frontmatter(content).unwrap().unwrap();
        assert!(fm.validate().is_err());
    }

    #[test]
    fn test_sort_order_neg_inf_rejected() {
        let content = "---\ntldr: test\nsort_order: -.inf\n---\n";
        let fm = parse_frontmatter(content).unwrap().unwrap();
        assert!(fm.validate().is_err());
    }

    #[test]
    fn test_sort_order_integer_parses_as_f64() {
        let content = "---\ntldr: test\nsort_order: 3\n---\n";
        let fm = parse_frontmatter(content).unwrap().unwrap();
        assert_eq!(fm.sort_order, Some(3.0));
        fm.validate().unwrap();
    }

    #[test]
    fn test_assigned_to() {
        let content = "---\ntldr: test\nassigned_to: alice\n---\n";
        let fm = parse_frontmatter(content).unwrap().unwrap();
        assert_eq!(fm.assigned_to, Some("alice".to_string()));
    }

    // -----------------------------------------------------------------------
    // YAML field mutation tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_upsert_yaml_field_existing() {
        let mut lines = vec![
            "tldr: test".to_string(),
            "status: todo".to_string(),
            "labels: [a]".to_string(),
        ];
        upsert_yaml_field(&mut lines, Field::Status, "done");
        assert_eq!(lines[1], "status: done");
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn test_upsert_yaml_field_missing() {
        let mut lines = vec!["tldr: test".to_string()];
        upsert_yaml_field(&mut lines, Field::Status, "todo");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1], "status: todo");
    }

    #[test]
    fn test_remove_yaml_field_existing() {
        let mut lines = vec![
            "tldr: test".to_string(),
            "tentative_date: '2026-04-11'".to_string(),
            "labels: [a]".to_string(),
        ];
        remove_yaml_field(&mut lines, Field::TentativeDate);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "tldr: test");
        assert_eq!(lines[1], "labels: [a]");
    }

    #[test]
    fn test_remove_yaml_field_missing_is_noop() {
        let mut lines = vec!["tldr: test".to_string(), "labels: [a]".to_string()];
        remove_yaml_field(&mut lines, Field::TentativeDate);
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn test_rewrite_frontmatter_set() {
        let content = "---\ntldr: test\nstatus: todo\n---\nBody text.\n";
        let result = rewrite_frontmatter(
            content,
            &[FieldMutation::Set {
                field: Field::Status,
                value: "done".to_string(),
            }],
        )
        .unwrap();
        assert!(result.contains("status: done"));
        assert!(result.contains("Body text."));
        assert!(result.contains("tldr: test"));
    }

    #[test]
    fn test_rewrite_frontmatter_remove() {
        let content = "---\ntldr: test\ntentative_date: '2026-04-11'\n---\nBody.\n";
        let result = rewrite_frontmatter(
            content,
            &[FieldMutation::Remove {
                field: Field::TentativeDate,
            }],
        )
        .unwrap();
        assert!(!result.contains("tentative_date"));
        assert!(result.contains("tldr: test"));
        assert!(result.contains("Body."));
    }

    #[test]
    fn test_rewrite_frontmatter_combined() {
        let content =
            "---\ntldr: test\nstatus: todo\ntentative_date: '2026-04-11'\n---\n# Heading\n";
        let result = rewrite_frontmatter(
            content,
            &[
                FieldMutation::Set {
                    field: Field::Status,
                    value: "done".to_string(),
                },
                FieldMutation::Set {
                    field: Field::OnDate,
                    value: "'2026-04-11'".to_string(),
                },
                FieldMutation::Remove {
                    field: Field::TentativeDate,
                },
            ],
        )
        .unwrap();
        assert!(result.contains("status: done"));
        assert!(result.contains("on_date: '2026-04-11'"));
        assert!(!result.contains("tentative_date"));
        assert!(result.contains("# Heading"));
    }

    #[test]
    fn test_rewrite_frontmatter_no_frontmatter_errors() {
        let content = "Just plain text.";
        let result = rewrite_frontmatter(content, &[]);
        assert!(result.is_err());
    }

    #[test]
    fn test_rewrite_frontmatter_preserves_body_formatting() {
        let content = "---\ntldr: test\n---\n\n# Title\n\nParagraph 1.\n\nParagraph 2.\n";
        let result = rewrite_frontmatter(
            content,
            &[FieldMutation::Set {
                field: Field::Status,
                value: "todo".to_string(),
            }],
        )
        .unwrap();
        assert!(result.contains("\n\n# Title\n\nParagraph 1.\n\nParagraph 2.\n"));
    }

    // -----------------------------------------------------------------------
    // Field enum tests
    // -----------------------------------------------------------------------

    /// All Field variants. Kept in sync manually — if a variant is added to
    /// Field but not here, `test_field_enum_covers_all_frontmatter_fields`
    /// will catch it (the parsed YAML will have a key with no matching variant).
    const ALL_FIELDS: &[Field] = &[
        Field::Tldr,
        Field::Labels,
        Field::Created,
        Field::Updated,
        Field::Summary,
        Field::OnDate,
        Field::StartDate,
        Field::EndDate,
        Field::DueDate,
        Field::CheckInDate,
        Field::VerifiedDate,
        Field::ContextLink,
        Field::Links,
        Field::Attachments,
        Field::Status,
        Field::Priority,
        Field::Effort,
        Field::BlockedBy,
        Field::Rrule,
        Field::Recurrence,
        Field::CompletionLog,
        Field::TentativeDate,
        Field::SortOrder,
        Field::AssignedTo,
    ];

    #[test]
    fn test_field_yaml_key_mapping() {
        // Verify every Field variant maps to the expected YAML key.
        let expected: &[(Field, &str)] = &[
            (Field::Tldr, "tldr"),
            (Field::Labels, "labels"),
            (Field::Created, "created"),
            (Field::Updated, "updated"),
            (Field::Summary, "summary"),
            (Field::OnDate, "on_date"),
            (Field::StartDate, "start_date"),
            (Field::EndDate, "end_date"),
            (Field::DueDate, "due_date"),
            (Field::CheckInDate, "check_in_date"),
            (Field::VerifiedDate, "verified_date"),
            (Field::ContextLink, "context_link"),
            (Field::Links, "links"),
            (Field::Attachments, "attachments"),
            (Field::Status, "status"),
            (Field::Priority, "priority"),
            (Field::Effort, "effort"),
            (Field::BlockedBy, "blocked_by"),
            (Field::Rrule, "rrule"),
            (Field::Recurrence, "recurrence"),
            (Field::CompletionLog, "completion_log"),
            (Field::TentativeDate, "tentative_date"),
            (Field::SortOrder, "sort_order"),
            (Field::AssignedTo, "assigned_to"),
        ];
        for (field, key) in expected {
            assert_eq!(
                field.yaml_key(),
                *key,
                "Field::{field:?} maps to wrong YAML key"
            );
        }
        assert_eq!(
            expected.len(),
            ALL_FIELDS.len(),
            "expected mapping table and ALL_FIELDS are out of sync"
        );
    }

    // -----------------------------------------------------------------------
    // Recurrence / completion_log: parser, accessor, and validation
    // -----------------------------------------------------------------------

    #[test]
    fn test_legacy_flat_rrule_parses_and_effective_rrule_returns_it() {
        let content = "---\ntldr: test\nstatus: todo\ncheck_in_date: 2026-05-01\nrrule: \"FREQ=WEEKLY\"\n---\n";
        let fm = parse_frontmatter(content).unwrap().unwrap();
        fm.validate().unwrap();
        assert_eq!(fm.effective_rrule(), Some("FREQ=WEEKLY"));
        assert!(fm.recurrence.is_none());
        assert!(fm.completion_log.is_empty());
    }

    #[test]
    fn test_nested_recurrence_parses_and_wins_over_flat() {
        let content = r#"---
tldr: test
status: todo
check_in_date: 2026-05-01
rrule: "FREQ=WEEKLY"
recurrence:
  rrule: "FREQ=MONTHLY"
completion_log:
  - completed: 2026-04-01
  - completed: 2026-04-08
    comment: did it early
---
"#;
        let fm = parse_frontmatter(content).unwrap().unwrap();
        fm.validate().unwrap();
        assert_eq!(fm.effective_rrule(), Some("FREQ=MONTHLY"));
        assert_eq!(fm.completion_log.len(), 2);
        assert_eq!(
            fm.completion_log[1].comment.as_deref(),
            Some("did it early")
        );
    }

    #[test]
    fn test_lead_days_without_due_date_rejected() {
        let content = r#"---
tldr: test
status: todo
check_in_date: 2026-05-01
recurrence:
  rrule: "FREQ=WEEKLY"
  lead_days: 7
---
"#;
        let fm = parse_frontmatter(content).unwrap().unwrap();
        let err = fm.validate().unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("lead_days"), "err: {msg}");
    }

    #[test]
    fn test_lead_days_with_due_date_accepted() {
        let content = r#"---
tldr: test
status: todo
due_date: 2026-05-08
recurrence:
  rrule: "FREQ=WEEKLY"
  lead_days: 7
---
"#;
        let fm = parse_frontmatter(content).unwrap().unwrap();
        fm.validate().unwrap();
        assert_eq!(fm.recurrence.as_ref().unwrap().lead_days, Some(7));
    }

    #[test]
    fn test_completion_log_unknown_field_rejected() {
        let content = r#"---
tldr: test
completion_log:
  - completed: 2026-04-01
    bogus: true
---
"#;
        let err = parse_frontmatter(content).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("bogus"), "err: {msg}");
    }

    #[test]
    fn test_recurrence_unknown_field_rejected() {
        let content = r#"---
tldr: test
recurrence:
  rrule: "FREQ=WEEKLY"
  bogus: true
---
"#;
        let err = parse_frontmatter(content).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("bogus"), "err: {msg}");
    }

    #[test]
    fn test_effective_rrule_none_when_unset() {
        let content = "---\ntldr: test\n---\n";
        let fm = parse_frontmatter(content).unwrap().unwrap();
        assert!(fm.effective_rrule().is_none());
    }

    #[test]
    fn test_remove_yaml_field_does_not_consume_top_level_list_item() {
        // If a top-level block-style sequence (e.g. `labels:` followed by
        // `- a`) appears immediately after the field we're removing, those
        // `- a` entries must NOT be swept up as block children of the
        // removed scalar. Block children are indented; top-level list
        // items are not.
        let mut lines = vec![
            "rrule: FREQ=WEEKLY".to_string(),
            "- a".to_string(),
            "- b".to_string(),
            "status: todo".to_string(),
        ];
        remove_yaml_field(&mut lines, Field::Rrule);
        assert_eq!(lines, vec!["- a", "- b", "status: todo"]);
    }

    #[test]
    fn test_remove_recurrence_strips_nested_block() {
        let mut lines = vec![
            "tldr: test".to_string(),
            "recurrence:".to_string(),
            "  rrule: FREQ=WEEKLY".to_string(),
            "  lead_days: 3".to_string(),
            "status: todo".to_string(),
        ];
        remove_yaml_field(&mut lines, Field::Recurrence);
        assert_eq!(lines, vec!["tldr: test", "status: todo"]);
    }

    #[test]
    fn test_append_completion_log_creates_block() {
        let content = "---\ntldr: test\n---\nBody.\n";
        let result = rewrite_frontmatter(
            content,
            &[FieldMutation::AppendCompletionLog {
                entry: CompletionLogEntry {
                    completed: NaiveDate::from_ymd_opt(2026, 4, 22).unwrap(),
                    occurrence: None,
                    comment: None,
                },
            }],
        )
        .unwrap();
        assert!(result.contains("completion_log:"));
        assert!(result.contains("- { completed: 2026-04-22 }"));
        assert!(result.contains("Body."));
        // Round-trips.
        let fm = parse_frontmatter(&result).unwrap().unwrap();
        fm.validate().unwrap();
        assert_eq!(fm.completion_log.len(), 1);
    }

    #[test]
    fn test_append_completion_log_extends_existing_block() {
        let content = r#"---
tldr: test
completion_log:
  - { completed: 2026-03-01 }
---
"#;
        let result = rewrite_frontmatter(
            content,
            &[FieldMutation::AppendCompletionLog {
                entry: CompletionLogEntry {
                    completed: NaiveDate::from_ymd_opt(2026, 4, 22).unwrap(),
                    occurrence: None,
                    comment: None,
                },
            }],
        )
        .unwrap();
        let fm = parse_frontmatter(&result).unwrap().unwrap();
        assert_eq!(fm.completion_log.len(), 2);
    }

    #[test]
    fn test_append_completion_log_stops_at_next_top_level_key() {
        // Covers the `append_completion_log_entry` end-of-block scan when a
        // top-level key (e.g. `status`) follows the list. The new entry must
        // land before `status:`, not after.
        let content = r#"---
tldr: test
completion_log:
  - { completed: 2026-03-01 }
status: todo
---
"#;
        let result = rewrite_frontmatter(
            content,
            &[FieldMutation::AppendCompletionLog {
                entry: CompletionLogEntry {
                    completed: NaiveDate::from_ymd_opt(2026, 4, 22).unwrap(),
                    occurrence: None,
                    comment: None,
                },
            }],
        )
        .unwrap();
        let fm = parse_frontmatter(&result).unwrap().unwrap();
        assert_eq!(fm.completion_log.len(), 2);
        assert_eq!(fm.status, Some(Status::Todo));
        // Order: both log entries before `status:`.
        let log_pos = result.find("completion_log:").unwrap();
        let status_pos = result.find("status:").unwrap();
        assert!(log_pos < status_pos);
        let apr_pos = result.find("2026-04-22").unwrap();
        assert!(
            apr_pos < status_pos,
            "new entry must not land after status:"
        );
    }

    #[test]
    fn test_append_completion_log_continuation_lines_preserved() {
        // Entries with a multi-line `comment` end up spread over a
        // continuation line (indented `  ` without a leading `-`). The
        // end-of-block scan must treat those as part of the current entry,
        // not as the end of the list.
        let mut lines = vec![
            "tldr: test".to_string(),
            "completion_log:".to_string(),
            "  - completed: 2026-03-01".to_string(),
            "    comment: |".to_string(),
            "      multi-line body".to_string(),
            "status: todo".to_string(),
        ];
        let entry = CompletionLogEntry {
            completed: NaiveDate::from_ymd_opt(2026, 4, 22).unwrap(),
            occurrence: None,
            comment: None,
        };
        append_completion_log_entry(&mut lines, &entry);
        // New entry must land after the continuation lines, before `status:`.
        let new_idx = lines
            .iter()
            .position(|l| l.contains("2026-04-22"))
            .expect("appended entry present");
        let status_idx = lines
            .iter()
            .position(|l| l == "status: todo")
            .expect("status preserved");
        assert!(new_idx < status_idx);
        assert!(new_idx >= 4, "new entry must follow continuation lines");
    }

    #[test]
    fn test_append_completion_log_all_three_fields_roundtrip() {
        // Writing an entry with `occurrence` + `comment` in flow form must
        // produce YAML that reparses to the same CompletionLogEntry.
        let mut lines = vec!["tldr: test".to_string(), "status: todo".to_string()];
        let entry = CompletionLogEntry {
            completed: NaiveDate::from_ymd_opt(2026, 4, 22).unwrap(),
            occurrence: Some(NaiveDate::from_ymd_opt(2026, 4, 25).unwrap()),
            comment: Some("did it early".to_string()),
        };
        append_completion_log_entry(&mut lines, &entry);
        let yaml = lines.join("\n");
        let parsed: crate::frontmatter::Frontmatter =
            serde_yaml::from_str(&yaml).expect("reparse round-tripped YAML");
        assert_eq!(parsed.completion_log.len(), 1);
        assert_eq!(parsed.completion_log[0], entry);
    }

    #[test]
    fn test_append_completion_log_multiline_comment_emits_block_scalar() {
        // Multi-line comments switch the entry from flow to block
        // form, using `comment: |` to preserve interior newlines.
        let mut lines = vec!["tldr: test".to_string(), "status: todo".to_string()];
        let entry = CompletionLogEntry {
            completed: NaiveDate::from_ymd_opt(2026, 4, 22).unwrap(),
            occurrence: None,
            comment: Some("first line\nsecond line".to_string()),
        };
        append_completion_log_entry(&mut lines, &entry);
        let yaml = lines.join("\n");

        // Assert block-scalar shape: no flow braces on this entry, and `|`
        // appears before the indented content lines.
        assert!(
            yaml.contains("  - completed: 2026-04-22"),
            "block form uses `- completed:` line, not flow mapping: {yaml}",
        );
        assert!(yaml.contains("    comment: |"));
        assert!(yaml.contains("      first line"));
        assert!(yaml.contains("      second line"));
        // And — no `\n` escape from the old flow-form path.
        assert!(
            !yaml.contains("\\n"),
            "block form must not escape newlines: {yaml}",
        );

        // Round-trip: the emitted YAML reparses to the original entry.
        let parsed: crate::frontmatter::Frontmatter =
            serde_yaml::from_str(&yaml).expect("reparse round-tripped YAML");
        assert_eq!(parsed.completion_log.len(), 1);
        assert_eq!(parsed.completion_log[0], entry);
    }

    #[test]
    fn test_remove_rrule_preserves_nested_recurrence_block() {
        // Regression guard: `Remove { Field::Rrule }` must strip only the
        // flat top-level `rrule:` key. The nested `recurrence.rrule` line
        // is indented and must not be consumed — it's a block child of
        // `recurrence:`, not of the removed scalar.
        let mut lines = vec![
            "tldr: test".to_string(),
            "rrule: FREQ=WEEKLY".to_string(),
            "recurrence:".to_string(),
            "  rrule: FREQ=MONTHLY".to_string(),
            "status: todo".to_string(),
        ];
        remove_yaml_field(&mut lines, Field::Rrule);
        assert_eq!(
            lines,
            vec![
                "tldr: test",
                "recurrence:",
                "  rrule: FREQ=MONTHLY",
                "status: todo",
            ]
        );
    }

    #[test]
    fn test_remove_completion_log_strips_list_items() {
        // `remove_yaml_field` must consume the `  - { ... }` children of a
        // `completion_log:` key, not leave them behind as orphans.
        let mut lines = vec![
            "tldr: test".to_string(),
            "completion_log:".to_string(),
            "  - { completed: 2026-03-01 }".to_string(),
            "  - { completed: 2026-04-01 }".to_string(),
            "status: todo".to_string(),
        ];
        remove_yaml_field(&mut lines, Field::CompletionLog);
        assert_eq!(lines, vec!["tldr: test", "status: todo"]);
    }

    #[test]
    fn test_field_enum_covers_all_frontmatter_fields() {
        // Parse a YAML doc with every Frontmatter field set, then verify
        // every key in that YAML has a matching Field variant.  This catches
        // a new field added to Frontmatter without a corresponding Field variant.
        let content = r#"---
tldr: "test"
labels: [a]
created: '2025-01-01T00:00:00.000Z'
updated: '2025-01-01T00:00:00.000Z'
summary: "s"
on_date: 2025-01-01
start_date: 2025-01-01
end_date: 2025-01-01
due_date: 2025-01-01
check_in_date: 2025-01-01
verified_date: 2025-01-01
context_link: foo.md
links: [bar.md]
attachments: [baz.pdf]
status: todo
priority: 3
effort: small
blocked_by: [x.md]
rrule: "FREQ=WEEKLY"
recurrence:
  rrule: "FREQ=WEEKLY"
  lead_days: 3
completion_log:
  - completed: 2025-01-01
tentative_date: 2025-01-01
sort_order: 1.0
assigned_to: test
---
"#;
        // Verify it parses (all fields recognized by Frontmatter)
        let fm = parse_frontmatter(content).unwrap().unwrap();
        fm.validate().unwrap();

        // Extract YAML keys from the raw text
        let range = frontmatter_yaml_range(content).unwrap().unwrap();
        let yaml = &content[range];
        let yaml_keys: std::collections::HashSet<&str> = yaml
            .lines()
            .filter_map(|line| line.split(':').next())
            .filter(|k| !k.starts_with(' ') && !k.starts_with('-'))
            .collect();

        // Every YAML key must have a Field variant
        let field_keys: std::collections::HashSet<&str> =
            ALL_FIELDS.iter().map(|f| f.yaml_key()).collect();

        let missing_from_enum: Vec<&&str> = yaml_keys.difference(&field_keys).collect();
        assert!(
            missing_from_enum.is_empty(),
            "Frontmatter fields without Field variants: {missing_from_enum:?}"
        );

        // Every Field variant must correspond to a real Frontmatter field
        let extra_in_enum: Vec<&&str> = field_keys.difference(&yaml_keys).collect();
        assert!(
            extra_in_enum.is_empty(),
            "Field variants without Frontmatter fields: {extra_in_enum:?}"
        );
    }

    #[test]
    fn rank_sort_order_wins_over_priority() {
        assert_eq!(rank(Some(2.5), Some(1)), 2.5);
    }

    #[test]
    fn rank_falls_back_to_priority() {
        assert_eq!(rank(None, Some(3)), 3.0);
    }

    #[test]
    fn rank_falls_back_to_unranked() {
        assert_eq!(rank(None, None), UNRANKED);
    }

    #[test]
    fn rank_fractional_sort_order_below_priority_one() {
        assert!(rank(Some(0.5), None) < rank(None, Some(1)));
    }
}
