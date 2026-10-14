use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::{Datelike, NaiveDate};
use rusqlite::params;
use rusqlite::types::ToSql;

use crate::errors::FileError;
use crate::frontmatter::{self, Effort, Frontmatter, Status};
use crate::git::{self, FileChange};
use crate::is_md;
use crate::lint;

// ---------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------

/// Bump on any schema change, and also on any change to `lint::check_rules`
/// (rules or message text): lint messages are persisted in `docs.lint_errors`
/// and deduped by exact text, so stale sets must be rebuilt.  The
/// unparseable-rrule rule embeds the `rrule` crate's own error text in its
/// message, so a `rrule`-crate upgrade that rewords that error also changes
/// persisted message text and requires a bump, even with `check_rules`
/// untouched.
pub(crate) const SCHEMA_VERSION: &str = "8";

/// The COALESCE expression for effective date.  Used in the schema index,
/// SELECT, WHERE, and ORDER BY.  Must be textually identical everywhere
/// for SQLite's expression index to match.
///
/// For active items the scheduling dates resolve first (lint requires at
/// least one of check_in_date / due_date).  Terminal items normally have
/// their scheduling dates stripped, so on_date/end_date is the fallback;
/// `on_date`/`end_date` precede `due_date` so those items sort by
/// completion date, not their old deadline.  A frozen series (terminal,
/// recurrence retained) is the exception: it keeps `check_in_date` frozen
/// at the final occurrence, which therefore wins the COALESCE over its
/// `on_date`.  TODO(frozen-series-effective-date)
const EFF_DATE: &str = "COALESCE(tentative_date, check_in_date, on_date, end_date, due_date)";

/// SQL tuple of terminal statuses. Must match Status::is_terminal() over
/// Status::ALL in declaration order — asserted by status_sql_literals_match_enum.
const TERMINAL_STATUSES_SQL: &str = "('done', 'cancelled')";

const SCHEMA_DDL: &str = "
CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS docs (
    path            TEXT PRIMARY KEY,
    tldr            TEXT NOT NULL,
    status          TEXT,
    priority        INTEGER,
    effort          TEXT,
    tentative_date  TEXT,
    check_in_date   TEXT,
    due_date        TEXT,
    on_date         TEXT,
    end_date        TEXT,
    sort_order      REAL,
    labels          TEXT,
    assigned_to     TEXT,
    context_link    TEXT,
    rrule           TEXT,
    blocked_by      TEXT,
    lint_errors     TEXT NOT NULL DEFAULT '[]',
    dirty           INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_active_effective_date ON docs(
    COALESCE(tentative_date, check_in_date, on_date, end_date, due_date),
    COALESCE(sort_order, priority, 6)  -- must match frontmatter::rank(); 6 = frontmatter::UNRANKED
) WHERE status IN ('todo', 'in_progress', 'reminder');  -- active statuses; asserted by status_sql_literals_match_enum

CREATE INDEX IF NOT EXISTS idx_status ON docs(status, on_date, end_date);
";

// ---------------------------------------------------------------------------
// Status/Effort string conversion for DB layer
// ---------------------------------------------------------------------------

fn str_to_status(s: &str) -> Result<Status> {
    s.parse::<Status>()
        .map_err(|_| anyhow::anyhow!("corrupt status value in cache DB: {s:?}"))
}

fn str_to_effort(s: &str) -> Result<Effort> {
    s.parse::<Effort>()
        .map_err(|_| anyhow::anyhow!("corrupt effort value in cache DB: {s:?}"))
}

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

pub struct Cache {
    conn: rusqlite::Connection,
    repo_dir: PathBuf,
}

pub struct SyncResult {
    pub cache: Cache,
    /// Parse failures + lint-rule violations found during sync.
    pub errors: Vec<FileError>,
}

pub struct TaskFilter {
    /// Symmetric date window: active items with
    /// effective_date <= reference_date + horizon_days, and terminal
    /// (done/cancelled) items with effective_date >= reference_date -
    /// horizon_days, i.e. completed within the last `horizon_days` days.
    /// None means no date filter at all: every item passes.
    pub horizon: Option<HorizonFilter>,
    /// Statuses to include.  Default: \[Todo, InProgress, Reminder\].
    /// When `include_done` is true, Done and Cancelled are appended.
    pub statuses: Vec<Status>,
    /// Append Done and Cancelled to `statuses`.  Default false.
    pub include_done: bool,
    /// Label filter (AND semantics).
    pub labels: Vec<String>,
    /// Max results.  None = unlimited.
    pub limit: Option<usize>,
}

impl Default for TaskFilter {
    fn default() -> Self {
        Self {
            horizon: None,
            statuses: vec![Status::Todo, Status::InProgress, Status::Reminder],
            include_done: false,
            labels: Vec::new(),
            limit: None,
        }
    }
}

pub struct HorizonFilter {
    pub reference_date: NaiveDate,
    pub days: u32,
}

#[derive(Debug, Clone)]
pub struct TaskRow {
    pub path: PathBuf,
    pub tldr: String,
    pub status: Option<Status>,
    pub priority: Option<u8>,
    pub effort: Option<Effort>,
    pub tentative_date: Option<NaiveDate>,
    pub check_in_date: Option<NaiveDate>,
    pub due_date: Option<NaiveDate>,
    pub on_date: Option<NaiveDate>,
    pub end_date: Option<NaiveDate>,
    pub sort_order: Option<f64>,
    pub labels: Vec<String>,
    pub assigned_to: Option<String>,
    pub context_link: Option<String>,
    pub rrule: Option<String>,
    pub blocked_by: Vec<String>,
    /// Computed: COALESCE(tentative_date, check_in_date, due_date)
    pub effective_date: Option<NaiveDate>,
}

// ---------------------------------------------------------------------------
// DB path helper
// ---------------------------------------------------------------------------

pub fn db_path(repo_dir: &Path) -> PathBuf {
    repo_dir.join(".graf.db")
}

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// Opens (or creates) the cache DB, runs the sync protocol, and returns a
/// handle ready for queries.
pub fn open_and_sync(repo_dir: &Path) -> Result<SyncResult> {
    let mut cache = open_db(repo_dir)?;
    // A never-synced DB has the `meta` table (open_db just created it) but no
    // `last_indexed_commit` row.  Any other read failure is a real fault:
    // reindexing on it would drop both tables, destroying the evidence and
    // reporting the wrong cause.
    let is_new = match cache.conn.query_row(
        "SELECT 1 FROM meta WHERE key = 'last_indexed_commit'",
        [],
        |_| Ok(()),
    ) {
        Ok(()) => false,
        Err(rusqlite::Error::QueryReturnedNoRows) => true,
        Err(e) => return Err(e).context("failed to read last_indexed_commit from cache DB"),
    };

    if is_new || schema_version_mismatch(&cache.conn)? {
        let errors = do_reindex(&mut cache)?;
        return Ok(SyncResult { cache, errors });
    }

    let errors = sync(&mut cache)?;
    Ok(SyncResult { cache, errors })
}

/// Drop all data and rebuild from a full filesystem scan.
pub fn reindex(repo_dir: &Path) -> Result<SyncResult> {
    let mut cache = open_db(repo_dir)?;
    let errors = do_reindex(&mut cache)?;
    Ok(SyncResult { cache, errors })
}

/// Open the cache DB without running the sync protocol. Useful for
/// lookups against a repo's cache (e.g., cross-repo anchor resolution)
/// when a full sync isn't needed. Fails if the DB doesn't exist.
pub fn open_cache_no_sync(repo_dir: &Path) -> Result<Cache> {
    let path = db_path(repo_dir);
    if !path.exists() {
        bail!(
            "no cache DB at {} — query this repo at least once first",
            path.display()
        );
    }
    let cache = open_db(repo_dir)?;
    if schema_version_mismatch(&cache.conn)? {
        bail!(
            "cache DB at {} has stale schema — query this repo to rebuild it",
            path.display()
        );
    }
    Ok(cache)
}

fn open_db(repo_dir: &Path) -> Result<Cache> {
    let path = db_path(repo_dir);
    let conn = rusqlite::Connection::open(&path)
        .with_context(|| format!("failed to open cache DB at {}", path.display()))?;
    conn.pragma_update(None, "journal_mode", "wal")?;
    conn.execute_batch(SCHEMA_DDL)
        .context("failed to create cache schema")?;
    Ok(Cache {
        conn,
        repo_dir: repo_dir.to_path_buf(),
    })
}

// ---------------------------------------------------------------------------
// Cache methods
// ---------------------------------------------------------------------------

impl Cache {
    /// Upsert a single document's metadata into the cache.
    pub fn upsert_doc(&self, rel_path: &Path, fm: &Frontmatter, dirty: bool) -> Result<()> {
        execute_upsert(&self.conn, rel_path, fm, dirty)
    }

    /// Remove a document from the cache.
    pub fn remove_doc(&self, rel_path: &Path) -> Result<()> {
        self.conn.execute(
            "DELETE FROM docs WHERE path = ?1",
            params![rel_path.to_str().context("path is not UTF-8")?],
        )?;
        Ok(())
    }

    /// Returns the number of documents in the cache.
    pub fn doc_count(&self) -> Result<usize> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM docs", [], |row| row.get(0))?;
        Ok(count as usize)
    }

    /// Query tasks with filtering and sorting.
    pub fn query_tasks(&self, filter: &TaskFilter) -> Result<Vec<TaskRow>> {
        let mut qb = QueryBuilder::new();

        qb.push_str(&format!(
            "SELECT path, tldr, status, priority, effort,
                    tentative_date, check_in_date, due_date, on_date, end_date,
                    sort_order, labels, assigned_to, context_link, rrule, blocked_by,
                    {EFF_DATE} AS effective_date
             FROM docs WHERE "
        ));

        // Status filter
        let mut statuses: Vec<String> = filter.statuses.iter().map(|s| s.to_string()).collect();
        if filter.include_done {
            for s in Status::ALL.iter().filter(|s| s.is_terminal()) {
                let s = s.to_string();
                if !statuses.contains(&s) {
                    statuses.push(s);
                }
            }
        }
        if statuses.is_empty() {
            // No statuses means no results
            qb.push_str("0");
        } else {
            qb.push_str("status IN (");
            for (i, s) in statuses.iter().enumerate() {
                if i > 0 {
                    qb.push_str(", ");
                }
                let p = qb.push_param(s.to_string());
                qb.push_str(&p);
            }
            qb.push_str(")");
        }

        // Date filter: a horizon is a symmetric window around the reference
        // date.  Active items pass with an effective date at or before
        // reference + days; active items without a date are excluded (they are
        // invalid per lint rules but may exist in unchecked data).  Terminal
        // items (done/cancelled) pass with an effective date at or after
        // reference - days: their scheduling dates are stripped on completion,
        // so the effective date is the completion date.  The terminal branch
        // also admits a NULL effective date: such a row is a lint violation,
        // and sync does not re-lint unchanged files, so letting it through is
        // what surfaces it as a lint error rather than silently dropping it.
        if let Some(ref horizon) = filter.horizon {
            let upper = horizon
                .reference_date
                .checked_add_signed(chrono::Duration::days(i64::from(horizon.days)))
                .context("date overflow computing horizon cutoff")?;
            let lower = horizon
                .reference_date
                .checked_sub_signed(chrono::Duration::days(i64::from(horizon.days)))
                .context("date underflow computing terminal horizon lower bound")?;

            // Both bounds are compared against stored dates as text, so they
            // must render in the same zero-padded four-digit-year form.
            // Outside years 0000-9999 chrono emits a sign-prefixed extended
            // year, which breaks the byte-wise ordering the predicate relies
            // on; reject such a horizon rather than return wrong rows.
            if upper.year() > 9999 || lower.year() < 0 {
                bail!(
                    "horizon of {} days is out of range: window {lower}..{upper} \
                     falls outside years 0000-9999",
                    horizon.days
                );
            }

            let p_upper = qb.push_param(upper.to_string());
            let p_lower = qb.push_param(lower.to_string());
            qb.push_str(&format!(
                " AND ((status IN {TERMINAL_STATUSES_SQL} \
                 AND ({EFF_DATE} IS NULL OR {EFF_DATE} >= {p_lower})) \
                 OR (status NOT IN {TERMINAL_STATUSES_SQL} \
                 AND {EFF_DATE} IS NOT NULL AND {EFF_DATE} <= {p_upper}))"
            ));
        }

        // Label filter (AND semantics)
        for label in &filter.labels {
            let p = qb.push_param(label.clone());
            qb.push_str(&format!(
                " AND EXISTS (SELECT 1 FROM json_each(docs.labels) WHERE json_each.value = {p})"
            ));
        }

        // This must match todo_sort_cmp() in cli/mod.rs; the rank term is the
        // canonical formula frontmatter::rank(). Guarded by the multi-repo sort
        // integration tests.
        // ORDER BY: active items first, then by effective date, rank, title.
        qb.push_str(&format!(
            " ORDER BY status IN {TERMINAL_STATUSES_SQL}, \
             {EFF_DATE} IS NULL, {EFF_DATE}, \
             COALESCE(sort_order, priority, {unranked}), tldr",
            unranked = frontmatter::UNRANKED as i64
        ));

        // LIMIT
        if let Some(limit) = filter.limit {
            let p = qb.push_param(limit as i64);
            qb.push_str(&format!(" LIMIT {p}"));
        }

        let params_refs: Vec<&dyn ToSql> = qb.params.iter().map(|p| p.as_ref()).collect();
        let mut stmt = self.conn.prepare(&qb.sql)?;
        let mut tasks = Vec::new();
        let mut rows = stmt.query(params_refs.as_slice())?;
        while let Some(row) = rows.next()? {
            tasks.push(row_to_task(row)?);
        }
        drop(rows);

        Ok(tasks)
    }

    /// All persisted lint errors for every cached doc, repo-wide and
    /// independent of any task filter.  A separate scan rather than a
    /// `TaskRow` field: `query_tasks` filters by status and horizon, so
    /// reading lint state off returned rows alone would keep terminal-item
    /// and horizon-excluded violations invisible.
    pub fn all_lint_errors(&self) -> Result<Vec<FileError>> {
        let mut stmt = self.conn.prepare(
            "SELECT path, lint_errors FROM docs WHERE lint_errors != '[]' ORDER BY path",
        )?;
        let mut rows = stmt.query([])?;
        let mut errors = Vec::new();
        while let Some(row) = rows.next()? {
            let path = PathBuf::from(row.get::<_, String>(0)?);
            for message in parse_json_vec(row.get::<_, Option<String>>(1)?)? {
                errors.push(FileError::new(&path, message));
            }
        }
        Ok(errors)
    }

    /// Look up a single document by path.  Returns None if not in the cache.
    pub fn get_doc(&self, rel_path: &Path) -> Result<Option<TaskRow>> {
        let path_str = rel_path.to_str().context("path is not UTF-8")?;
        let sql = format!(
            "SELECT path, tldr, status, priority, effort,
                    tentative_date, check_in_date, due_date, on_date, end_date,
                    sort_order, labels, assigned_to, context_link, rrule, blocked_by,
                    {EFF_DATE} AS effective_date
             FROM docs WHERE path = ?1"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query(params![path_str])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        Ok(Some(row_to_task(row)?))
    }
}

// ---------------------------------------------------------------------------
// Internal: row mapping
// ---------------------------------------------------------------------------

/// Map a SQLite row to a TaskRow.  Assumes the SELECT column order matches
/// the column list used in query_tasks and get_doc.
fn row_to_task(row: &rusqlite::Row<'_>) -> Result<TaskRow> {
    let status_str = row.get::<_, Option<String>>(2)?;
    let effort_str = row.get::<_, Option<String>>(4)?;
    Ok(TaskRow {
        path: PathBuf::from(row.get::<_, String>(0)?),
        tldr: row.get(1)?,
        status: status_str.map(|s| str_to_status(&s)).transpose()?,
        priority: row.get::<_, Option<u8>>(3)?,
        effort: effort_str.map(|s| str_to_effort(&s)).transpose()?,
        tentative_date: parse_date(row.get::<_, Option<String>>(5)?)?,
        check_in_date: parse_date(row.get::<_, Option<String>>(6)?)?,
        due_date: parse_date(row.get::<_, Option<String>>(7)?)?,
        on_date: parse_date(row.get::<_, Option<String>>(8)?)?,
        end_date: parse_date(row.get::<_, Option<String>>(9)?)?,
        sort_order: row.get(10)?,
        labels: parse_json_vec(row.get::<_, Option<String>>(11)?)?,
        assigned_to: row.get(12)?,
        context_link: row.get(13)?,
        rrule: row.get(14)?,
        blocked_by: parse_json_vec(row.get::<_, Option<String>>(15)?)?,
        effective_date: parse_date(row.get::<_, Option<String>>(16)?)?,
    })
}

// ---------------------------------------------------------------------------
// Internal: JSON deserialization
// ---------------------------------------------------------------------------

fn parse_json_vec(val: Option<String>) -> Result<Vec<String>> {
    match val {
        Some(s) if !s.is_empty() => {
            serde_json::from_str::<Vec<String>>(&s).context("corrupt JSON array in cache DB")
        }
        _ => Ok(Vec::new()),
    }
}

fn parse_date(val: Option<String>) -> Result<Option<NaiveDate>> {
    match val {
        Some(s) => {
            let d = NaiveDate::parse_from_str(&s, "%Y-%m-%d")
                .with_context(|| format!("corrupt date in cache DB: {s:?}"))?;
            Ok(Some(d))
        }
        None => Ok(None),
    }
}

// ---------------------------------------------------------------------------
// Internal: QueryBuilder
// ---------------------------------------------------------------------------

struct QueryBuilder {
    sql: String,
    params: Vec<Box<dyn ToSql>>,
}

impl QueryBuilder {
    fn new() -> Self {
        Self {
            sql: String::with_capacity(512),
            params: Vec::new(),
        }
    }

    fn push_str(&mut self, s: &str) {
        self.sql.push_str(s);
    }

    fn push_param(&mut self, val: impl ToSql + 'static) -> String {
        self.params.push(Box::new(val));
        format!("?{}", self.params.len())
    }
}

// ---------------------------------------------------------------------------
// Internal: schema version check
// ---------------------------------------------------------------------------

/// Returns true if the DB has a schema version that doesn't match the current
/// one.  A missing row counts as a mismatch: pre-versioning caches predate the
/// current column set and must be rebuilt rather than queried.  Genuinely new
/// DBs are caught by the `is_new` check before this runs.
///
/// Any other SQLite failure (corruption, I/O) propagates rather than being
/// read as staleness — a reindex would destroy the evidence and report the
/// wrong cause.
fn schema_version_mismatch(conn: &rusqlite::Connection) -> Result<bool> {
    let version: Option<String> = match conn.query_row(
        "SELECT value FROM meta WHERE key = 'schema_version'",
        [],
        |row| row.get(0),
    ) {
        Ok(v) => Some(v),
        Err(rusqlite::Error::QueryReturnedNoRows) => None,
        Err(e) => return Err(e).context("failed to read schema_version from cache DB"),
    };

    Ok(version.as_deref() != Some(SCHEMA_VERSION))
}

// ---------------------------------------------------------------------------
// Internal: full reindex
// ---------------------------------------------------------------------------

fn do_reindex(cache: &mut Cache) -> Result<Vec<FileError>> {
    let mut errors = Vec::new();
    let repo_dir = cache.repo_dir.clone();

    let tx = cache.conn.transaction()?;

    // Drop and recreate tables to pick up schema changes (new/removed columns).
    // `CREATE TABLE IF NOT EXISTS` won't alter an existing table, so we must
    // drop first to ensure the schema matches SCHEMA_DDL.
    tx.execute_batch("DROP TABLE IF EXISTS docs; DROP TABLE IF EXISTS meta;")?;
    tx.execute_batch(SCHEMA_DDL)
        .context("failed to recreate schema during reindex")?;

    // Walk the repo and index all .md files
    let all_files = crate::collect_repo_files(&repo_dir)?;
    for rel in all_files.iter().filter(|p| is_md(p)) {
        let abs = repo_dir.join(rel);
        match parse_validate_and_migrate(&abs, rel) {
            Ok((fm, mut file_errors)) => {
                execute_upsert(&tx, rel, &fm, false)?;
                errors.append(&mut file_errors);
            }
            Err(e) => {
                errors.push(FileError::new(rel, format!("{e:#}")));
            }
        }
    }

    // Mark working-tree dirty files so the next sync knows to re-check them.
    // Without this, untracked files get indexed as dirty=0 and if later deleted,
    // neither git status nor reconcile_dirty would catch the stale row.
    let changed = git::changed_files(&repo_dir)?;
    let dirty_paths: HashSet<PathBuf> = changed
        .iter()
        .filter(|f| f.change != FileChange::Deleted && is_md(&f.path))
        .map(|f| f.path.clone())
        .collect();
    for path in &dirty_paths {
        let path_str = path.to_str().context("path is not UTF-8")?;
        tx.execute(
            "UPDATE docs SET dirty = 1 WHERE path = ?1",
            params![path_str],
        )?;
    }

    // Store metadata
    let head = git::rev_parse_head(&repo_dir)?;
    tx.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES ('last_indexed_commit', ?1)",
        params![head],
    )?;
    tx.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES ('schema_version', ?1)",
        params![SCHEMA_VERSION],
    )?;

    tx.commit()?;
    Ok(errors)
}

// ---------------------------------------------------------------------------
// Internal: sync protocol
// ---------------------------------------------------------------------------

fn sync(cache: &mut Cache) -> Result<Vec<FileError>> {
    let mut errors = Vec::new();
    let repo_dir = cache.repo_dir.clone();

    // Get current working-tree state (used by steps 2 and 5)
    let working_changes = git::changed_files(&repo_dir)?;
    let currently_dirty: HashSet<PathBuf> = working_changes
        .iter()
        .filter(|f| f.change != FileChange::Deleted)
        .map(|f| f.path.clone())
        .collect();

    let tx = cache.conn.transaction()?;

    // Step 2: Reconcile previously-dirty rows
    reconcile_dirty(&tx, &repo_dir, &currently_dirty, &mut errors)?;

    // Step 3: Check HEAD against last indexed commit
    let head = git::rev_parse_head(&repo_dir)?;
    let last_commit: Option<String> = tx
        .query_row(
            "SELECT value FROM meta WHERE key = 'last_indexed_commit'",
            [],
            |row| row.get(0),
        )
        .ok();

    match &last_commit {
        None => {
            // No last_indexed_commit — full reindex
            tx.rollback()?;
            return do_reindex(cache);
        }
        Some(old) if old == &head => {
            // HEAD unchanged — skip to step 5
        }
        Some(old) => {
            if git::is_ancestor(&repo_dir, old, &head)? {
                // Step 4: Fast-forward — incremental commit diff
                apply_changed_files(
                    &tx,
                    &repo_dir,
                    &git::diff_between(&repo_dir, old, &head)?,
                    false,
                    &mut errors,
                )?;
            } else {
                // Not a fast-forward — full reindex
                tx.rollback()?;
                return do_reindex(cache);
            }
        }
    }

    // Update last_indexed_commit
    tx.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES ('last_indexed_commit', ?1)",
        params![head],
    )?;

    // Step 5: Working-tree changes
    apply_changed_files(&tx, &repo_dir, &working_changes, true, &mut errors)?;

    // Ensure schema_version is set (in case it was missing)
    tx.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES ('schema_version', ?1)",
        params![SCHEMA_VERSION],
    )?;

    tx.commit()?;
    Ok(errors)
}

/// Reconcile rows marked dirty in the DB against the current working-tree state.
/// Rows that were dirty but are no longer in `currently_dirty` are refreshed
/// from disk (or removed if the file no longer exists).
fn reconcile_dirty(
    tx: &rusqlite::Transaction<'_>,
    repo_dir: &Path,
    currently_dirty: &HashSet<PathBuf>,
    errors: &mut Vec<FileError>,
) -> Result<()> {
    let mut stmt = tx.prepare("SELECT path FROM docs WHERE dirty = 1")?;
    let prev_dirty: Vec<String> = stmt
        .query_map([], |row| row.get(0))?
        .collect::<Result<_, _>>()?;

    for path_str in &prev_dirty {
        let path = PathBuf::from(path_str);
        if currently_dirty.contains(&path) {
            // Still dirty — step 5 will handle it
            continue;
        }
        // Was dirty, now clean — refresh from disk
        let abs = repo_dir.join(&path);
        if !abs.exists() {
            tx.execute("DELETE FROM docs WHERE path = ?1", params![path_str])?;
            continue;
        }
        if !is_md(&path) {
            continue;
        }
        match parse_and_validate(&abs, &path) {
            Ok((fm, mut file_errors)) => {
                execute_upsert(tx, &path, &fm, false)?;
                errors.append(&mut file_errors);
            }
            Err(e) => {
                // TODO(persist-parse-errors)
                tx.execute("DELETE FROM docs WHERE path = ?1", params![path_str])?;
                errors.push(FileError::new(&path, format!("{e:#}")));
            }
        }
    }
    Ok(())
}

/// Apply a list of changed files to the cache.  Used by both the incremental
/// commit-diff (step 4, dirty=false) and working-tree (step 5, dirty=true) passes.
fn apply_changed_files(
    tx: &rusqlite::Transaction<'_>,
    repo_dir: &Path,
    changes: &[git::ChangedFile],
    dirty: bool,
    errors: &mut Vec<FileError>,
) -> Result<()> {
    for cf in changes {
        if !is_md(&cf.path) {
            continue;
        }
        match cf.change {
            FileChange::Deleted => {
                let path_str = cf.path.to_str().context("path is not UTF-8")?;
                tx.execute("DELETE FROM docs WHERE path = ?1", params![path_str])?;
            }
            FileChange::New | FileChange::Modified => {
                let abs = repo_dir.join(&cf.path);
                if !abs.exists() {
                    continue;
                }
                match parse_and_validate(&abs, &cf.path) {
                    Ok((fm, mut file_errors)) => {
                        execute_upsert(tx, &cf.path, &fm, dirty)?;
                        errors.append(&mut file_errors);
                    }
                    Err(e) => {
                        // TODO(persist-parse-errors)
                        let path_str = cf.path.to_str().context("path is not UTF-8")?;
                        tx.execute("DELETE FROM docs WHERE path = ?1", params![path_str])?;
                        errors.push(FileError::new(&cf.path, format!("{e:#}")));
                    }
                }
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Internal: helpers
// ---------------------------------------------------------------------------

/// Parse frontmatter and run lint rules.  Returns the parsed Frontmatter
/// and any lint errors (non-fatal).  Returns Err only for parse/validation
/// failures.
fn parse_and_validate(abs_path: &Path, rel_path: &Path) -> Result<(Frontmatter, Vec<FileError>)> {
    let content = std::fs::read_to_string(abs_path)
        .with_context(|| format!("reading {}", rel_path.display()))?;
    let (fm, errors) = parse_and_validate_content(rel_path, &content)?;
    Ok((fm, errors))
}

/// Like `parse_and_validate`, but operates on already-read content. Returns
/// the parsed Frontmatter and any lint errors. Used by the reindex path
/// which also needs the raw content for migration.
fn parse_and_validate_content(
    rel_path: &Path,
    content: &str,
) -> Result<(Frontmatter, Vec<FileError>)> {
    let fm = frontmatter::parse_frontmatter(content)
        .with_context(|| format!("parsing {}", rel_path.display()))?
        .with_context(|| format!("missing frontmatter in {}", rel_path.display()))?;
    fm.validate()
        .with_context(|| format!("validating {}", rel_path.display()))?;
    let lint_errors = lint::check_rules(rel_path, &fm);
    Ok((fm, lint_errors))
}

/// Like `parse_and_validate`, but also eagerly rewrites legacy-shape files
/// (flat `rrule`, `## Completion Log` body section) to the new shape.
/// Used only during full reindex. Failures in the migration step are
/// propagated as `Err`; the caller records a per-file error and continues.
fn parse_validate_and_migrate(
    abs_path: &Path,
    rel_path: &Path,
) -> Result<(Frontmatter, Vec<FileError>)> {
    let content = std::fs::read_to_string(abs_path)
        .with_context(|| format!("reading {}", rel_path.display()))?;
    let (fm, lint_errors) = parse_and_validate_content(rel_path, &content)?;

    // If migration rewrote the file, re-lint against the new frontmatter.
    // Otherwise reuse the lint pass we just did.
    match crate::migrate::maybe_migrate_legacy(abs_path, rel_path, &content, &fm)? {
        Some(new_fm) => {
            let new_lint = lint::check_rules(rel_path, &new_fm);
            Ok((new_fm, new_lint))
        }
        None => Ok((fm, lint_errors)),
    }
}

/// Upsert a doc's frontmatter into the cache.  Works on both `Connection`
/// and `Transaction` (which derefs to `Connection`).  Only caches the subset
/// of frontmatter fields relevant to scheduling, filtering, and sorting.
///
/// Lint state is computed here rather than passed in, so that stored lint
/// messages always match the stored frontmatter no matter which call site
/// wrote the row — the mutation paths in `todo.rs` have no lint plumbing of
/// their own and would otherwise clear the state.  The sync call sites run
/// `check_rules` a second time as a result; it is a pure in-memory check
/// (field comparisons plus an rrule string parse for docs carrying one).
fn execute_upsert(
    conn: &rusqlite::Connection,
    rel_path: &Path,
    fm: &Frontmatter,
    dirty: bool,
) -> Result<()> {
    let lint_messages: Vec<String> = lint::check_rules(rel_path, fm)
        .into_iter()
        .map(|e| e.message)
        .collect();
    conn.execute(
        "INSERT INTO docs (
            path, tldr, status, priority, effort,
            tentative_date, check_in_date, due_date, on_date, end_date,
            sort_order, labels, assigned_to, context_link, rrule,
            blocked_by, lint_errors, dirty
        ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
            ?16, ?17, ?18
        ) ON CONFLICT(path) DO UPDATE SET
            tldr=excluded.tldr, status=excluded.status,
            priority=excluded.priority, effort=excluded.effort,
            tentative_date=excluded.tentative_date,
            check_in_date=excluded.check_in_date,
            due_date=excluded.due_date,
            on_date=excluded.on_date,
            end_date=excluded.end_date,
            sort_order=excluded.sort_order, labels=excluded.labels,
            assigned_to=excluded.assigned_to,
            context_link=excluded.context_link,
            rrule=excluded.rrule, blocked_by=excluded.blocked_by,
            lint_errors=excluded.lint_errors,
            dirty=excluded.dirty",
        params![
            rel_path.to_str().context("path is not UTF-8")?,
            fm.tldr,
            fm.status.as_ref().map(|s| s.to_string()),
            fm.priority,
            fm.effort.as_ref().map(|e| e.to_string()),
            fm.tentative_date.map(|d| d.to_string()),
            fm.check_in_date.map(|d| d.to_string()),
            fm.due_date.map(|d| d.to_string()),
            fm.on_date.map(|d| d.to_string()),
            fm.end_date.map(|d| d.to_string()),
            fm.sort_order,
            serde_json::to_string(&fm.labels)?,
            fm.assigned_to.as_deref(),
            fm.context_link.as_deref(),
            fm.effective_rrule(),
            serde_json::to_string(&fm.blocked_by)?,
            serde_json::to_string(&lint_messages)?,
            dirty,
        ],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use tempfile::TempDir;

    #[test]
    fn status_sql_literals_match_enum() {
        // Derive the terminal and active tuples from the enum, preserving
        // Status::ALL declaration order.
        let render = |statuses: &[String]| format!("('{}')", statuses.join("', '"));
        let terminal: Vec<String> = Status::ALL
            .iter()
            .filter(|s| s.is_terminal())
            .map(|s| s.to_string())
            .collect();
        let active: Vec<String> = Status::ALL
            .iter()
            .filter(|s| !s.is_terminal())
            .map(|s| s.to_string())
            .collect();
        let derived_terminal = render(&terminal);
        let derived_active = render(&active);

        // Covers the two runtime format! call sites that consume the constant.
        assert_eq!(TERMINAL_STATUSES_SQL, derived_terminal);
        // Covers the schema partial-index predicate.
        assert!(
            SCHEMA_DDL.contains(&format!("WHERE status IN {derived_active}")),
            "SCHEMA_DDL active-status predicate does not match enum-derived {derived_active}"
        );

        // SQL-quoting safety of the generated tuples.
        for s in Status::ALL.iter().map(|s| s.to_string()) {
            assert!(!s.contains('\''), "status {s:?} contains a single quote");
        }
    }

    // -- Test helpers --

    // TODO(git-env-leaks-into-test-repos): every git spawn in this file inherits
    // the caller's GIT_* environment.
    fn run_git(dir: &Path, args: &[&str]) {
        let output = Command::new("git")
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

    fn create_test_repo() -> TempDir {
        let dir = TempDir::new().unwrap();
        run_git(dir.path(), &["init", "-b", "main"]);
        run_git(dir.path(), &["config", "user.email", "test@test.com"]);
        run_git(dir.path(), &["config", "user.name", "Test"]);
        dir
    }

    fn write_md(repo: &Path, rel_path: &str, frontmatter_yaml: &str) {
        let abs = repo.join(rel_path);
        if let Some(parent) = abs.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&abs, format!("---\n{frontmatter_yaml}\n---\n")).unwrap();
    }

    fn commit_all(repo: &Path) {
        run_git(repo, &["add", "-A"]);
        run_git(repo, &["commit", "-m", "test commit", "--allow-empty"]);
    }

    fn count_docs(cache: &Cache) -> i64 {
        cache
            .conn
            .query_row("SELECT COUNT(*) FROM docs", [], |row| row.get(0))
            .unwrap()
    }

    fn get_dirty_flag(cache: &Cache, path: &str) -> bool {
        cache
            .conn
            .query_row(
                "SELECT dirty FROM docs WHERE path = ?1",
                params![path],
                |row| row.get::<_, bool>(0),
            )
            .unwrap()
    }

    // -- Tests --

    #[test]
    fn test_schema_creation() {
        let dir = create_test_repo();
        write_md(dir.path(), "test.md", "tldr: hello");
        commit_all(dir.path());

        let result = open_and_sync(dir.path()).unwrap();
        // Verify tables exist by querying them
        let doc_count: i64 = result
            .cache
            .conn
            .query_row("SELECT COUNT(*) FROM docs", [], |row| row.get(0))
            .unwrap();
        assert!(doc_count >= 0);
        let meta_count: i64 = result
            .cache
            .conn
            .query_row("SELECT COUNT(*) FROM meta", [], |row| row.get(0))
            .unwrap();
        assert!(meta_count > 0);
    }

    #[test]
    fn test_upsert_and_query() {
        let dir = create_test_repo();
        write_md(
            dir.path(),
            "todo/task-a.md",
            "tldr: Task A\nstatus: todo\npriority: 2\ntentative_date: 2026-04-10",
        );
        write_md(
            dir.path(),
            "todo/task-b.md",
            "tldr: Task B\nstatus: todo\npriority: 1\ntentative_date: 2026-04-11",
        );
        write_md(
            dir.path(),
            "todo/task-c.md",
            "tldr: Task C\nstatus: in_progress\npriority: 3\ntentative_date: 2026-04-10",
        );
        commit_all(dir.path());

        let result = open_and_sync(dir.path()).unwrap();
        let tasks = result.cache.query_tasks(&TaskFilter::default()).unwrap();

        assert_eq!(tasks.len(), 3);
        // Sorted by effective_date, then COALESCE(sort_order, priority)
        // 2026-04-10: Task A (priority 2), Task C (priority 3)
        // 2026-04-11: Task B (priority 1)
        assert_eq!(tasks[0].tldr, "Task A");
        assert_eq!(tasks[1].tldr, "Task C");
        assert_eq!(tasks[2].tldr, "Task B");
    }

    #[test]
    fn test_remove() {
        let dir = create_test_repo();
        write_md(dir.path(), "test.md", "tldr: hello\nstatus: todo");
        commit_all(dir.path());

        let result = open_and_sync(dir.path()).unwrap();
        assert_eq!(count_docs(&result.cache), 1);

        result.cache.remove_doc(Path::new("test.md")).unwrap();
        assert_eq!(count_docs(&result.cache), 0);
    }

    #[test]
    fn test_reindex() {
        let dir = create_test_repo();
        write_md(dir.path(), "a.md", "tldr: A\nstatus: todo");
        write_md(dir.path(), "b.md", "tldr: B\nstatus: todo");
        write_md(dir.path(), "sub/c.md", "tldr: C\nstatus: reminder");
        commit_all(dir.path());

        let result = reindex(dir.path()).unwrap();
        assert_eq!(count_docs(&result.cache), 3);
    }

    #[test]
    fn test_sync_clean_repo() {
        let dir = create_test_repo();
        write_md(
            dir.path(),
            "task.md",
            "tldr: My Task\nstatus: todo\npriority: 3",
        );
        commit_all(dir.path());

        let result = open_and_sync(dir.path()).unwrap();
        let tasks = result.cache.query_tasks(&TaskFilter::default()).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].tldr, "My Task");
        assert_eq!(tasks[0].priority, Some(3));
    }

    #[test]
    fn test_sync_incremental() {
        let dir = create_test_repo();
        write_md(dir.path(), "a.md", "tldr: A\nstatus: todo");
        commit_all(dir.path());

        // First sync
        let _result1 = open_and_sync(dir.path()).unwrap();

        // Add a new file and commit
        write_md(dir.path(), "b.md", "tldr: B\nstatus: todo");
        commit_all(dir.path());

        // Second sync should pick up the new file
        let result2 = open_and_sync(dir.path()).unwrap();
        assert_eq!(count_docs(&result2.cache), 2);
    }

    #[test]
    fn test_sync_dirty_files() {
        let dir = create_test_repo();
        write_md(dir.path(), "task.md", "tldr: Original\nstatus: todo");
        commit_all(dir.path());

        // Modify without committing
        write_md(dir.path(), "task.md", "tldr: Modified\nstatus: todo");

        let result = open_and_sync(dir.path()).unwrap();
        let tasks = result.cache.query_tasks(&TaskFilter::default()).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].tldr, "Modified");
        assert!(get_dirty_flag(&result.cache, "task.md"));
    }

    #[test]
    fn test_sync_deleted_file() {
        let dir = create_test_repo();
        write_md(dir.path(), "a.md", "tldr: A\nstatus: todo");
        write_md(dir.path(), "b.md", "tldr: B\nstatus: todo");
        commit_all(dir.path());

        // First sync
        let _result1 = open_and_sync(dir.path()).unwrap();

        // Delete one file and commit
        fs::remove_file(dir.path().join("b.md")).unwrap();
        commit_all(dir.path());

        let result2 = open_and_sync(dir.path()).unwrap();
        assert_eq!(count_docs(&result2.cache), 1);
    }

    #[test]
    fn test_sync_rebase_detection() {
        let dir = create_test_repo();
        write_md(dir.path(), "a.md", "tldr: A\nstatus: todo");
        commit_all(dir.path());

        // Create a side branch and commit there
        Command::new("git")
            .args(["checkout", "-b", "side"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        write_md(dir.path(), "side.md", "tldr: Side\nstatus: todo");
        commit_all(dir.path());

        // Sync while on the side branch — indexes side.md + a.md
        let result1 = open_and_sync(dir.path()).unwrap();
        assert_eq!(count_docs(&result1.cache), 2);
        drop(result1);

        // Go back to main — this is NOT a descendant of side's HEAD
        Command::new("git")
            .args(["checkout", "main"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        // Now sync — HEAD (main) is not a descendant of the indexed commit
        // (side), so it should do a full reindex
        let result2 = open_and_sync(dir.path()).unwrap();
        // Should only have a.md (main branch), side.md doesn't exist on disk
        assert_eq!(count_docs(&result2.cache), 1);
    }

    #[test]
    fn test_sync_dirty_revert() {
        let dir = create_test_repo();
        write_md(
            dir.path(),
            "task.md",
            "tldr: Original\nstatus: todo\npriority: 3",
        );
        commit_all(dir.path());

        // First sync — file is clean
        let _result1 = open_and_sync(dir.path()).unwrap();

        // Modify without committing
        write_md(
            dir.path(),
            "task.md",
            "tldr: Modified\nstatus: todo\npriority: 1",
        );

        // Second sync — picks up dirty version
        let result2 = open_and_sync(dir.path()).unwrap();
        let tasks2 = result2.cache.query_tasks(&TaskFilter::default()).unwrap();
        assert_eq!(tasks2[0].tldr, "Modified");
        assert_eq!(tasks2[0].priority, Some(1));
        assert!(get_dirty_flag(&result2.cache, "task.md"));

        // Revert the file
        Command::new("git")
            .args(["checkout", "--", "task.md"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        // Third sync — should reconcile back to committed version
        let result3 = open_and_sync(dir.path()).unwrap();
        let tasks3 = result3.cache.query_tasks(&TaskFilter::default()).unwrap();
        assert_eq!(tasks3[0].tldr, "Original");
        assert_eq!(tasks3[0].priority, Some(3));
        assert!(!get_dirty_flag(&result3.cache, "task.md"));
    }

    #[test]
    fn test_parse_failure_during_sync() {
        let dir = create_test_repo();
        write_md(dir.path(), "good.md", "tldr: Good\nstatus: todo");
        // Write a file with invalid YAML
        let bad_path = dir.path().join("bad.md");
        fs::write(&bad_path, "---\nbogus_field: true\n---\n").unwrap();
        commit_all(dir.path());

        let result = open_and_sync(dir.path()).unwrap();
        // good.md should be in cache, bad.md should not
        assert_eq!(count_docs(&result.cache), 1);
        // Should have at least one error for bad.md
        assert!(!result.errors.is_empty());
        assert!(
            result
                .errors
                .iter()
                .any(|e| e.path.to_str().unwrap().contains("bad"))
        );
    }

    #[test]
    fn test_schema_version_mismatch_auto_reindexes() {
        let dir = create_test_repo();
        write_md(dir.path(), "test.md", "tldr: test\nstatus: todo");
        commit_all(dir.path());

        // First sync to create DB
        let _result = open_and_sync(dir.path()).unwrap();

        // Poke schema version to something wrong
        let db = db_path(dir.path());
        let conn = rusqlite::Connection::open(db).unwrap();
        conn.execute(
            "UPDATE meta SET value = '99' WHERE key = 'schema_version'",
            [],
        )
        .unwrap();
        drop(conn);

        // Should auto-reindex, not fail
        let result = open_and_sync(dir.path()).unwrap();
        assert_eq!(count_docs(&result.cache), 1);
    }

    #[test]
    fn test_label_filtering() {
        let dir = create_test_repo();
        write_md(
            dir.path(),
            "a.md",
            "tldr: A\nstatus: todo\nlabels: [house, recurring]",
        );
        write_md(dir.path(), "b.md", "tldr: B\nstatus: todo\nlabels: [house]");
        write_md(dir.path(), "c.md", "tldr: C\nstatus: todo\nlabels: [work]");
        commit_all(dir.path());

        let result = open_and_sync(dir.path()).unwrap();

        // Filter by 'house' label
        let mut filter = TaskFilter::default();
        filter.labels = vec!["house".to_string()];
        let tasks = result.cache.query_tasks(&filter).unwrap();
        assert_eq!(tasks.len(), 2);

        // Filter by both 'house' AND 'recurring'
        filter.labels = vec!["house".to_string(), "recurring".to_string()];
        let tasks = result.cache.query_tasks(&filter).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].tldr, "A");

        // Filter by 'work'
        filter.labels = vec!["work".to_string()];
        let tasks = result.cache.query_tasks(&filter).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].tldr, "C");
    }

    #[test]
    fn test_date_horizon() {
        let dir = create_test_repo();
        write_md(
            dir.path(),
            "past.md",
            "tldr: Past\nstatus: todo\ntentative_date: 2026-04-01",
        );
        write_md(
            dir.path(),
            "today.md",
            "tldr: Today\nstatus: todo\ntentative_date: 2026-04-11",
        );
        write_md(
            dir.path(),
            "future.md",
            "tldr: Future\nstatus: todo\ntentative_date: 2026-04-20",
        );
        write_md(
            dir.path(),
            "unscheduled.md",
            "tldr: Unscheduled\nstatus: todo",
        );
        commit_all(dir.path());

        let result = open_and_sync(dir.path()).unwrap();

        // Horizon = today (0 days into future)
        let filter = TaskFilter {
            horizon: Some(HorizonFilter {
                reference_date: NaiveDate::from_ymd_opt(2026, 4, 11).unwrap(),
                days: 0,
            }),
            statuses: vec![Status::Todo],
            include_done: false,
            labels: Vec::new(),
            limit: None,
        };
        let tasks = result.cache.query_tasks(&filter).unwrap();
        assert_eq!(tasks.len(), 2); // past + today
        assert!(
            tasks
                .iter()
                .all(|t| t.tldr != "Future" && t.tldr != "Unscheduled")
        );

        // Horizon = today + 10 days
        let filter2 = TaskFilter {
            horizon: Some(HorizonFilter {
                reference_date: NaiveDate::from_ymd_opt(2026, 4, 11).unwrap(),
                days: 10,
            }),
            statuses: vec![Status::Todo],
            include_done: false,
            labels: Vec::new(),
            limit: None,
        };
        let tasks2 = result.cache.query_tasks(&filter2).unwrap();
        assert_eq!(tasks2.len(), 3); // past + today + future

        // No horizon = all items
        let filter3 = TaskFilter {
            horizon: None,
            statuses: vec![Status::Todo],
            include_done: false,
            labels: Vec::new(),
            limit: None,
        };
        let tasks3 = result.cache.query_tasks(&filter3).unwrap();
        assert_eq!(tasks3.len(), 4); // past + today + future + unscheduled
    }

    /// Query with a horizon and the default active statuses plus done/cancelled.
    fn horizon_filter(reference: (i32, u32, u32), days: u32) -> TaskFilter {
        TaskFilter {
            horizon: Some(HorizonFilter {
                reference_date: NaiveDate::from_ymd_opt(reference.0, reference.1, reference.2)
                    .unwrap(),
                days,
            }),
            include_done: true,
            ..TaskFilter::default()
        }
    }

    fn tldrs(tasks: &[TaskRow]) -> Vec<&str> {
        tasks.iter().map(|t| t.tldr.as_str()).collect()
    }

    #[test]
    fn test_horizon_bounds_terminal_items() {
        let dir = create_test_repo();
        // Active item within horizon
        write_md(
            dir.path(),
            "active.md",
            "tldr: Active\nstatus: todo\ntentative_date: 2026-04-11",
        );
        // Active item beyond horizon (should be excluded)
        write_md(
            dir.path(),
            "future.md",
            "tldr: Future\nstatus: todo\ntentative_date: 2026-12-01",
        );
        // Done item — scheduling dates stripped, on_date set — completed
        // inside the lookback window.
        write_md(
            dir.path(),
            "done.md",
            "tldr: Done\nstatus: done\non_date: 2026-04-10",
        );
        // Cancelled item — terminated long before the lookback window.
        write_md(
            dir.path(),
            "cancelled.md",
            "tldr: Cancelled\nstatus: cancelled\non_date: 2026-03-01",
        );
        // Cancelled item inside the lookback window: the terminal branch
        // admits both terminal statuses, not just done.
        write_md(
            dir.path(),
            "cancelled_recent.md",
            "tldr: CancelledRecent\nstatus: cancelled\non_date: 2026-04-09",
        );
        // Anomalous future-dated completion: the terminal branch has a lower
        // bound only, so it stays visible rather than being silently dropped.
        write_md(
            dir.path(),
            "future_done.md",
            "tldr: FutureDone\nstatus: done\non_date: 2026-05-20",
        );
        commit_all(dir.path());

        let result = open_and_sync(dir.path()).unwrap();

        // Window 2026-04-06 .. 2026-04-16
        let filter = horizon_filter((2026, 4, 11), 5);
        let tasks = result.cache.query_tasks(&filter).unwrap();
        let tldrs = tldrs(&tasks);

        assert!(
            tldrs.contains(&"Active"),
            "active within horizon: {tldrs:?}"
        );
        assert!(
            !tldrs.contains(&"Future"),
            "active beyond horizon excluded: {tldrs:?}"
        );
        assert!(
            tldrs.contains(&"Done"),
            "terminal item completed within lookback: {tldrs:?}"
        );
        assert!(
            !tldrs.contains(&"Cancelled"),
            "terminal item completed before lookback excluded: {tldrs:?}"
        );
        assert!(
            tldrs.contains(&"CancelledRecent"),
            "cancelled item terminated within lookback: {tldrs:?}"
        );
        assert!(
            tldrs.contains(&"FutureDone"),
            "future-dated completion stays visible: {tldrs:?}"
        );
        assert_eq!(tldrs.len(), 4);
    }

    #[test]
    fn test_horizon_extreme_days_errors() {
        let dir = create_test_repo();
        write_md(
            dir.path(),
            "active.md",
            "tldr: Active\nstatus: todo\ntentative_date: 2026-04-11",
        );
        commit_all(dir.path());

        let result = open_and_sync(dir.path()).unwrap();

        // Past the representable date range: the upper bound overflows first.
        let err = result
            .cache
            .query_tasks(&horizon_filter((2026, 4, 11), u32::MAX))
            .unwrap_err();
        let rendered = format!("{err:#}");
        assert!(
            rendered.contains("date overflow computing horizon cutoff"),
            "attributable overflow error: {rendered}"
        );
    }

    #[test]
    fn test_horizon_out_of_range_year_errors() {
        let dir = create_test_repo();
        write_md(
            dir.path(),
            "active.md",
            "tldr: Active\nstatus: todo\ntentative_date: 2026-04-11",
        );
        commit_all(dir.path());

        let result = open_and_sync(dir.path()).unwrap();

        // Representable, but the upper bound lands past year 9999, where the
        // date no longer renders comparably against stored dates.  Returning
        // zero active items instead of all of them would be a silent wrong
        // answer, so this must fail.
        let err = result
            .cache
            .query_tasks(&horizon_filter((2026, 4, 11), 3_000_000))
            .unwrap_err();
        let rendered = format!("{err:#}");
        assert!(
            rendered.contains("out of range"),
            "attributable range error: {rendered}"
        );

        // Mirror condition on the lower bound: a lookback past year 0.
        let err = result
            .cache
            .query_tasks(&horizon_filter((2026, 4, 11), 1_000_000))
            .unwrap_err();
        let rendered = format!("{err:#}");
        assert!(
            rendered.contains("out of range"),
            "attributable range error: {rendered}"
        );
    }

    #[test]
    fn test_horizon_terminal_lower_bound_is_inclusive() {
        let dir = create_test_repo();
        write_md(
            dir.path(),
            "at_bound.md",
            "tldr: AtBound\nstatus: done\non_date: 2026-04-06",
        );
        write_md(
            dir.path(),
            "before_bound.md",
            "tldr: BeforeBound\nstatus: done\non_date: 2026-04-05",
        );
        commit_all(dir.path());

        let result = open_and_sync(dir.path()).unwrap();

        // Window lower bound = 2026-04-11 - 5 days = 2026-04-06
        let tasks = result
            .cache
            .query_tasks(&horizon_filter((2026, 4, 11), 5))
            .unwrap();
        assert_eq!(tldrs(&tasks), vec!["AtBound"]);
    }

    #[test]
    fn test_horizon_zero_days_terminal_window() {
        let dir = create_test_repo();
        write_md(
            dir.path(),
            "today.md",
            "tldr: Today\nstatus: done\non_date: 2026-04-11",
        );
        write_md(
            dir.path(),
            "yesterday.md",
            "tldr: Yesterday\nstatus: done\non_date: 2026-04-10",
        );
        commit_all(dir.path());

        let result = open_and_sync(dir.path()).unwrap();

        // days: 0 — the window collapses to the reference date itself.
        let tasks = result
            .cache
            .query_tasks(&horizon_filter((2026, 4, 11), 0))
            .unwrap();
        assert_eq!(tldrs(&tasks), vec!["Today"]);
    }

    #[test]
    fn test_horizon_windows_range_completion_by_end_date() {
        let dir = create_test_repo();
        write_md(
            dir.path(),
            "recent.md",
            "tldr: Recent\nstatus: done\nstart_date: 2026-04-01\nend_date: 2026-04-09",
        );
        write_md(
            dir.path(),
            "old.md",
            "tldr: Old\nstatus: done\nstart_date: 2026-02-01\nend_date: 2026-02-20",
        );
        commit_all(dir.path());

        let result = open_and_sync(dir.path()).unwrap();

        let tasks = result
            .cache
            .query_tasks(&horizon_filter((2026, 4, 11), 5))
            .unwrap();
        assert_eq!(tldrs(&tasks), vec!["Recent"]);
    }

    #[test]
    fn test_horizon_applies_to_explicit_done_status() {
        let dir = create_test_repo();
        write_md(
            dir.path(),
            "recent.md",
            "tldr: Recent\nstatus: done\non_date: 2026-04-10",
        );
        write_md(
            dir.path(),
            "old.md",
            "tldr: Old\nstatus: done\non_date: 2026-01-10",
        );
        commit_all(dir.path());

        let result = open_and_sync(dir.path()).unwrap();

        // Explicit statuses, not include_done: the window still applies.
        let filter = TaskFilter {
            horizon: Some(HorizonFilter {
                reference_date: NaiveDate::from_ymd_opt(2026, 4, 11).unwrap(),
                days: 5,
            }),
            statuses: vec![Status::Done],
            ..TaskFilter::default()
        };
        let tasks = result.cache.query_tasks(&filter).unwrap();
        assert_eq!(tldrs(&tasks), vec!["Recent"]);
    }

    #[test]
    fn test_horizon_admits_null_date_terminal_row() {
        let dir = create_test_repo();
        // Lint violation: terminal with no completion date.  It must still
        // pass the SQL filter so the layer above can report it.
        write_md(dir.path(), "nodate.md", "tldr: NoDate\nstatus: done");
        write_md(
            dir.path(),
            "old.md",
            "tldr: Old\nstatus: done\non_date: 2026-01-10",
        );
        commit_all(dir.path());

        let result = open_and_sync(dir.path()).unwrap();

        let tasks = result
            .cache
            .query_tasks(&horizon_filter((2026, 4, 11), 5))
            .unwrap();
        assert_eq!(tldrs(&tasks), vec!["NoDate"]);
        assert!(tasks[0].effective_date.is_none());
    }

    #[test]
    fn test_horizon_windows_terminal_item_by_tentative_date() {
        let dir = create_test_repo();
        // Lint tolerates a retained tentative_date on a terminal item; it wins
        // the effective-date COALESCE, so it is what the window applies to.
        write_md(
            dir.path(),
            "old_completion.md",
            "tldr: OldCompletion\nstatus: done\non_date: 2026-01-10\ntentative_date: 2026-04-09",
        );
        write_md(
            dir.path(),
            "recent_completion.md",
            "tldr: RecentCompletion\nstatus: done\non_date: 2026-04-10\ntentative_date: 2026-01-09",
        );
        commit_all(dir.path());

        let result = open_and_sync(dir.path()).unwrap();

        let tasks = result
            .cache
            .query_tasks(&horizon_filter((2026, 4, 11), 5))
            .unwrap();
        assert_eq!(tldrs(&tasks), vec!["OldCompletion"]);
    }

    #[test]
    fn test_sort_order() {
        let dir = create_test_repo();
        // Same date, different sort_orders
        write_md(
            dir.path(),
            "c.md",
            "tldr: C\nstatus: todo\ntentative_date: 2026-04-10\nsort_order: 3.0",
        );
        write_md(
            dir.path(),
            "a.md",
            "tldr: A\nstatus: todo\ntentative_date: 2026-04-10\nsort_order: 1.0",
        );
        write_md(
            dir.path(),
            "b.md",
            "tldr: B\nstatus: todo\ntentative_date: 2026-04-10\nsort_order: 2.0",
        );
        // Earlier date should come first
        write_md(
            dir.path(),
            "z.md",
            "tldr: Z\nstatus: todo\ntentative_date: 2026-04-09\nsort_order: 99.0",
        );
        // No date — should come last
        write_md(dir.path(), "u.md", "tldr: U\nstatus: todo");
        commit_all(dir.path());

        let result = open_and_sync(dir.path()).unwrap();
        let tasks = result.cache.query_tasks(&TaskFilter::default()).unwrap();

        assert_eq!(tasks.len(), 5);
        assert_eq!(tasks[0].tldr, "Z"); // 04-09
        assert_eq!(tasks[1].tldr, "A"); // 04-10, sort 1.0
        assert_eq!(tasks[2].tldr, "B"); // 04-10, sort 2.0
        assert_eq!(tasks[3].tldr, "C"); // 04-10, sort 3.0
        assert_eq!(tasks[4].tldr, "U"); // unscheduled, last
    }

    #[test]
    fn test_include_done() {
        let dir = create_test_repo();
        write_md(dir.path(), "active.md", "tldr: Active\nstatus: todo");
        write_md(
            dir.path(),
            "done.md",
            "tldr: Done\nstatus: done\non_date: 2026-04-10",
        );
        write_md(
            dir.path(),
            "cancelled.md",
            "tldr: Cancelled\nstatus: cancelled\non_date: 2026-04-05",
        );
        commit_all(dir.path());

        let result = open_and_sync(dir.path()).unwrap();

        // Without include_done
        let tasks = result.cache.query_tasks(&TaskFilter::default()).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].tldr, "Active");

        // With include_done
        let mut filter = TaskFilter::default();
        filter.include_done = true;
        let tasks = result.cache.query_tasks(&filter).unwrap();
        assert_eq!(tasks.len(), 3);

        // Verify effective_date uses on_date for terminal items
        let done = tasks.iter().find(|t| t.tldr == "Done").unwrap();
        assert_eq!(
            done.effective_date,
            Some(NaiveDate::from_ymd_opt(2026, 4, 10).unwrap()),
            "done item effective_date should be on_date"
        );
        let cancelled = tasks.iter().find(|t| t.tldr == "Cancelled").unwrap();
        assert_eq!(
            cancelled.effective_date,
            Some(NaiveDate::from_ymd_opt(2026, 4, 5).unwrap()),
            "cancelled item effective_date should be on_date"
        );
    }

    #[test]
    fn test_effective_date_uses_end_date_for_range_completion() {
        let dir = create_test_repo();
        // Terminal item with start_date + end_date (range completion)
        write_md(
            dir.path(),
            "range-done.md",
            "tldr: Range Done\nstatus: done\nstart_date: 2026-04-01\nend_date: 2026-04-10",
        );
        // Terminal item with on_date only (point-in-time completion)
        write_md(
            dir.path(),
            "point-done.md",
            "tldr: Point Done\nstatus: done\non_date: 2026-04-05",
        );
        commit_all(dir.path());

        let result = open_and_sync(dir.path()).unwrap();
        let mut filter = TaskFilter::default();
        filter.include_done = true;
        let tasks = result.cache.query_tasks(&filter).unwrap();

        let range = tasks.iter().find(|t| t.tldr == "Range Done").unwrap();
        assert_eq!(
            range.effective_date,
            Some(NaiveDate::from_ymd_opt(2026, 4, 10).unwrap()),
            "range completion effective_date should be end_date"
        );
        assert_eq!(
            range.end_date,
            Some(NaiveDate::from_ymd_opt(2026, 4, 10).unwrap())
        );
        assert!(range.on_date.is_none());

        let point = tasks.iter().find(|t| t.tldr == "Point Done").unwrap();
        assert_eq!(
            point.effective_date,
            Some(NaiveDate::from_ymd_opt(2026, 4, 5).unwrap()),
            "point completion effective_date should be on_date"
        );
        assert_eq!(
            point.on_date,
            Some(NaiveDate::from_ymd_opt(2026, 4, 5).unwrap())
        );
        assert!(point.end_date.is_none());
    }

    #[test]
    fn test_lint_errors_during_sync() {
        let dir = create_test_repo();
        // This file has a lint error: done with check_in_date
        write_md(
            dir.path(),
            "bad.md",
            "tldr: Bad\nstatus: done\ncheck_in_date: 2026-05-01\non_date: 2026-04-10",
        );
        write_md(dir.path(), "good.md", "tldr: Good\nstatus: todo");
        commit_all(dir.path());

        let result = open_and_sync(dir.path()).unwrap();
        // Both should be in the cache (lint errors don't prevent caching)
        assert_eq!(count_docs(&result.cache), 2);
        // But we should get a lint error
        assert!(
            result
                .errors
                .iter()
                .any(|e| e.message.contains("check_in_date"))
        );
    }

    #[test]
    fn test_file_becomes_unparseable() {
        let dir = create_test_repo();
        write_md(dir.path(), "task.md", "tldr: Good\nstatus: todo");
        commit_all(dir.path());

        // First sync — file is cached
        let result1 = open_and_sync(dir.path()).unwrap();
        assert_eq!(count_docs(&result1.cache), 1);
        drop(result1);

        // Break the file (uncommitted)
        fs::write(dir.path().join("task.md"), "---\nbogus_field: oops\n---\n").unwrap();

        // Second sync — should remove stale cached data
        let result2 = open_and_sync(dir.path()).unwrap();
        assert_eq!(count_docs(&result2.cache), 0);
        assert!(!result2.errors.is_empty());
    }

    #[test]
    fn test_untracked_new_file() {
        let dir = create_test_repo();
        write_md(dir.path(), "existing.md", "tldr: Existing\nstatus: todo");
        commit_all(dir.path());

        // First sync
        let _result1 = open_and_sync(dir.path()).unwrap();

        // Add a brand-new untracked file (never committed)
        write_md(dir.path(), "brand-new.md", "tldr: Brand New\nstatus: todo");

        // Second sync should pick it up via git status
        let result2 = open_and_sync(dir.path()).unwrap();
        assert_eq!(count_docs(&result2.cache), 2);
        let tasks = result2.cache.query_tasks(&TaskFilter::default()).unwrap();
        assert!(tasks.iter().any(|t| t.tldr == "Brand New"));
    }

    #[test]
    fn test_query_with_limit() {
        let dir = create_test_repo();
        write_md(dir.path(), "a.md", "tldr: A\nstatus: todo");
        write_md(dir.path(), "b.md", "tldr: B\nstatus: todo");
        write_md(dir.path(), "c.md", "tldr: C\nstatus: todo");
        commit_all(dir.path());

        let result = open_and_sync(dir.path()).unwrap();
        let mut filter = TaskFilter::default();
        filter.limit = Some(2);
        let tasks = result.cache.query_tasks(&filter).unwrap();
        assert_eq!(tasks.len(), 2);
    }

    #[test]
    fn test_file_without_status() {
        let dir = create_test_repo();
        // File has tldr but no status field — status is NULL in the DB
        write_md(dir.path(), "note.md", "tldr: Just a note");
        write_md(dir.path(), "task.md", "tldr: A task\nstatus: todo");
        commit_all(dir.path());

        let result = open_and_sync(dir.path()).unwrap();
        // Default filter includes todo/in_progress/reminder — note.md has
        // NULL status, so it should be excluded
        let tasks = result.cache.query_tasks(&TaskFilter::default()).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].tldr, "A task");
    }

    #[test]
    fn test_rename_via_commit_diff() {
        let dir = create_test_repo();
        write_md(
            dir.path(),
            "old-name.md",
            "tldr: Renamed Task\nstatus: todo\npriority: 2",
        );
        commit_all(dir.path());

        // First sync — indexes under old name
        let result1 = open_and_sync(dir.path()).unwrap();
        assert_eq!(count_docs(&result1.cache), 1);
        drop(result1);

        // Rename via git mv and commit
        Command::new("git")
            .args(["mv", "old-name.md", "new-name.md"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        commit_all(dir.path());

        // Second sync — incremental diff should remove old, add new
        let result2 = open_and_sync(dir.path()).unwrap();
        assert_eq!(count_docs(&result2.cache), 1);
        let tasks = result2.cache.query_tasks(&TaskFilter::default()).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].path, PathBuf::from("new-name.md"));
        assert_eq!(tasks[0].tldr, "Renamed Task");
    }

    #[test]
    fn test_rename_in_working_tree() {
        let dir = create_test_repo();
        write_md(dir.path(), "original.md", "tldr: Moving Task\nstatus: todo");
        commit_all(dir.path());

        // First sync
        let _result1 = open_and_sync(dir.path()).unwrap();

        // Rename without committing (git mv stages it)
        Command::new("git")
            .args(["mv", "original.md", "moved.md"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        // Second sync — working-tree rename
        let result2 = open_and_sync(dir.path()).unwrap();
        let tasks = result2.cache.query_tasks(&TaskFilter::default()).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].path, PathBuf::from("moved.md"));
        assert_eq!(tasks[0].tldr, "Moving Task");
    }

    #[test]
    fn test_effective_date_coalesce() {
        let dir = create_test_repo();
        // tentative_date wins when all three are set
        write_md(
            dir.path(),
            "all-dates.md",
            "tldr: All Dates\nstatus: todo\ntentative_date: 2026-04-05\ncheck_in_date: 2026-04-10\ndue_date: 2026-04-15",
        );
        // Falls back to check_in_date when no tentative
        write_md(
            dir.path(),
            "check-in.md",
            "tldr: Check In\nstatus: todo\ncheck_in_date: 2026-04-08\ndue_date: 2026-04-20",
        );
        // Falls back to due_date when no tentative or check_in
        write_md(
            dir.path(),
            "due-only.md",
            "tldr: Due Only\nstatus: todo\ndue_date: 2026-04-12",
        );
        // No dates at all — effective_date should be None
        write_md(dir.path(), "no-dates.md", "tldr: No Dates\nstatus: todo");
        commit_all(dir.path());

        let result = open_and_sync(dir.path()).unwrap();
        let tasks = result.cache.query_tasks(&TaskFilter::default()).unwrap();

        // Sorted by effective_date: 04-05, 04-08, 04-12, None (last)
        assert_eq!(tasks.len(), 4);
        assert_eq!(tasks[0].tldr, "All Dates");
        assert_eq!(
            tasks[0].effective_date,
            Some(NaiveDate::from_ymd_opt(2026, 4, 5).unwrap())
        );
        assert_eq!(tasks[1].tldr, "Check In");
        assert_eq!(
            tasks[1].effective_date,
            Some(NaiveDate::from_ymd_opt(2026, 4, 8).unwrap())
        );
        assert_eq!(tasks[2].tldr, "Due Only");
        assert_eq!(
            tasks[2].effective_date,
            Some(NaiveDate::from_ymd_opt(2026, 4, 12).unwrap())
        );
        assert_eq!(tasks[3].tldr, "No Dates");
        assert!(tasks[3].effective_date.is_none());
    }

    #[test]
    fn test_sort_order_priority_fallback() {
        let dir = create_test_repo();
        // Same effective date; mix of sort_order and priority-only
        let date = "2026-04-10";
        // Has sort_order 5.0 — should sort by that
        write_md(
            dir.path(),
            "with-sort.md",
            &format!("tldr: With Sort\nstatus: todo\ntentative_date: {date}\nsort_order: 5.0"),
        );
        // No sort_order, priority 1 — COALESCE(NULL, 1) = 1, sorts first
        write_md(
            dir.path(),
            "prio-1.md",
            &format!("tldr: Prio 1\nstatus: todo\ntentative_date: {date}\npriority: 1"),
        );
        // No sort_order, priority 3 — COALESCE(NULL, 3) = 3
        write_md(
            dir.path(),
            "prio-3.md",
            &format!("tldr: Prio 3\nstatus: todo\ntentative_date: {date}\npriority: 3"),
        );
        commit_all(dir.path());

        let result = open_and_sync(dir.path()).unwrap();
        let tasks = result.cache.query_tasks(&TaskFilter::default()).unwrap();

        assert_eq!(tasks.len(), 3);
        // COALESCE(sort_order, priority): 1, 3, 5.0
        assert_eq!(tasks[0].tldr, "Prio 1");
        assert_eq!(tasks[1].tldr, "Prio 3");
        assert_eq!(tasks[2].tldr, "With Sort");
    }

    #[test]
    fn test_sync_incremental_modify_and_delete() {
        let dir = create_test_repo();
        write_md(
            dir.path(),
            "modify-me.md",
            "tldr: Original\nstatus: todo\npriority: 5",
        );
        write_md(dir.path(), "delete-me.md", "tldr: Doomed\nstatus: todo");
        write_md(dir.path(), "untouched.md", "tldr: Untouched\nstatus: todo");
        commit_all(dir.path());

        // First sync
        let _result1 = open_and_sync(dir.path()).unwrap();

        // Modify one, delete another, commit
        write_md(
            dir.path(),
            "modify-me.md",
            "tldr: Updated\nstatus: in_progress\npriority: 1",
        );
        fs::remove_file(dir.path().join("delete-me.md")).unwrap();
        commit_all(dir.path());

        // Incremental sync
        let result2 = open_and_sync(dir.path()).unwrap();
        assert_eq!(count_docs(&result2.cache), 2);

        let tasks = result2.cache.query_tasks(&TaskFilter::default()).unwrap();
        assert!(
            tasks
                .iter()
                .any(|t| t.tldr == "Updated" && t.priority == Some(1))
        );
        assert!(tasks.iter().any(|t| t.tldr == "Untouched"));
        assert!(!tasks.iter().any(|t| t.tldr == "Doomed"));
    }

    #[test]
    fn test_sync_noop_fast_path() {
        let dir = create_test_repo();
        write_md(dir.path(), "task.md", "tldr: Stable\nstatus: todo");
        commit_all(dir.path());

        // First sync
        let _result1 = open_and_sync(dir.path()).unwrap();

        // Second sync — nothing changed (HEAD matches, nothing dirty)
        let result2 = open_and_sync(dir.path()).unwrap();
        assert!(result2.errors.is_empty());
        let tasks = result2.cache.query_tasks(&TaskFilter::default()).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].tldr, "Stable");
    }

    #[test]
    fn test_reindex_preserves_field_values() {
        let dir = create_test_repo();
        write_md(
            dir.path(),
            "task.md",
            "tldr: Full Task\n\
             status: in_progress\n\
             priority: 2\n\
             effort: medium\n\
             tentative_date: 2026-04-10\n\
             check_in_date: 2026-05-01\n\
             due_date: 2026-06-15\n\
             sort_order: 3.5\n\
             labels: [house, recurring]\n\
             assigned_to: alice\n\
             context_link: life/kb/related.md\n\
             rrule: FREQ=MONTHLY\n\
             blocked_by: [life/todo/other.md]",
        );
        commit_all(dir.path());

        let result = reindex(dir.path()).unwrap();
        let tasks = result
            .cache
            .query_tasks(&TaskFilter {
                horizon: None,
                statuses: vec![Status::InProgress],
                include_done: false,
                labels: Vec::new(),
                limit: None,
            })
            .unwrap();

        assert_eq!(tasks.len(), 1);
        let t = &tasks[0];
        assert_eq!(t.tldr, "Full Task");
        assert_eq!(t.status, Some(Status::InProgress));
        assert_eq!(t.priority, Some(2));
        assert_eq!(t.effort, Some(Effort::Medium));
        assert_eq!(
            t.tentative_date,
            Some(NaiveDate::from_ymd_opt(2026, 4, 10).unwrap())
        );
        assert_eq!(
            t.check_in_date,
            Some(NaiveDate::from_ymd_opt(2026, 5, 1).unwrap())
        );
        assert_eq!(
            t.due_date,
            Some(NaiveDate::from_ymd_opt(2026, 6, 15).unwrap())
        );
        assert_eq!(t.sort_order, Some(3.5));
        assert_eq!(t.labels, vec!["house", "recurring"]);
        assert_eq!(t.assigned_to.as_deref(), Some("alice"));
        assert_eq!(t.context_link.as_deref(), Some("life/kb/related.md"));
        assert_eq!(t.rrule.as_deref(), Some("FREQ=MONTHLY"));
        assert_eq!(t.blocked_by, vec!["life/todo/other.md"]);
        // effective_date = tentative_date (first in COALESCE chain)
        assert_eq!(
            t.effective_date,
            Some(NaiveDate::from_ymd_opt(2026, 4, 10).unwrap())
        );
    }

    #[test]
    fn test_reindex_marks_dirty_files() {
        let dir = create_test_repo();
        write_md(dir.path(), "clean.md", "tldr: Clean\nstatus: todo");
        write_md(
            dir.path(),
            "will-be-dirty.md",
            "tldr: Dirty V1\nstatus: todo",
        );
        commit_all(dir.path());

        // Modify one file without committing
        write_md(
            dir.path(),
            "will-be-dirty.md",
            "tldr: Dirty V2\nstatus: todo",
        );

        let result = reindex(dir.path()).unwrap();
        assert_eq!(count_docs(&result.cache), 2);
        assert!(!get_dirty_flag(&result.cache, "clean.md"));
        assert!(get_dirty_flag(&result.cache, "will-be-dirty.md"));
    }

    // -----------------------------------------------------------------------
    // Phase 1 migration: lenient parse + reindex write-back
    // -----------------------------------------------------------------------

    #[test]
    fn test_reindex_migrates_flat_rrule_and_body_log() {
        let dir = create_test_repo();
        let file = "todo/recurring.md";
        let content = r#"---
tldr: Legacy Recurring
status: todo
check_in_date: 2026-04-18
rrule: "FREQ=WEEKLY"
---

Body text.

## Completion Log

### 2026-03-01
did it early

### 2026-04-01
"#;
        let abs = dir.path().join(file);
        fs::create_dir_all(abs.parent().unwrap()).unwrap();
        fs::write(&abs, content).unwrap();
        commit_all(dir.path());

        let result = reindex(dir.path()).unwrap();
        assert_eq!(count_docs(&result.cache), 1);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

        let rewritten = fs::read_to_string(&abs).unwrap();
        assert!(
            rewritten.contains("recurrence:"),
            "no recurrence: block:\n{rewritten}"
        );
        assert!(!rewritten.contains("\nrrule:"));
        assert!(!rewritten.contains("## Completion Log"));
        assert!(rewritten.contains("completion_log:"));
        assert!(rewritten.contains("completed: 2026-03-01"));
        assert!(rewritten.contains("completed: 2026-04-01"));
        assert!(rewritten.contains("did it early"));
        assert!(rewritten.contains("Body text."));

        // rrule column should carry the effective value so queries still work.
        let row = result.cache.get_doc(Path::new(file)).unwrap().expect("row");
        assert_eq!(row.rrule.as_deref(), Some("FREQ=WEEKLY"));
    }

    #[test]
    fn test_reindex_migrates_broken_flat_rrule_and_reports_it() {
        // Migration is shape-only, so a legacy file with an unparseable rrule
        // still migrates to the nested shape.  The post-rewrite re-lint then
        // reports the bad rule, which is where the user finds out.
        let dir = create_test_repo();
        let file = "todo/recurring.md";
        let content = r#"---
tldr: Legacy Broken Recurring
status: todo
check_in_date: 2026-04-18
rrule: "FREQ=BOGUS"
---

Body text.
"#;
        let abs = dir.path().join(file);
        fs::create_dir_all(abs.parent().unwrap()).unwrap();
        fs::write(&abs, content).unwrap();
        commit_all(dir.path());

        let result = reindex(dir.path()).unwrap();

        // The doc still indexes — lint errors are data, not aborts.
        assert_eq!(count_docs(&result.cache), 1);

        let rewritten = fs::read_to_string(&abs).unwrap();
        assert!(
            rewritten.contains("recurrence:"),
            "no recurrence: block:\n{rewritten}"
        );
        assert!(!rewritten.contains("\nrrule:"));

        assert!(
            result.errors.iter().any(
                |e| e.message.contains("unparseable rrule") && e.message.contains("FREQ=BOGUS")
            ),
            "errors: {:?}",
            result.errors
        );
    }

    #[test]
    fn test_reindex_noop_on_new_shape_file() {
        let dir = create_test_repo();
        let file = "todo/recurring.md";
        let content = r#"---
tldr: Already new shape
status: todo
check_in_date: 2026-04-18
recurrence:
  rrule: "FREQ=MONTHLY"
completion_log:
  - completed: 2026-04-01
---

Body.
"#;
        let abs = dir.path().join(file);
        fs::create_dir_all(abs.parent().unwrap()).unwrap();
        fs::write(&abs, content).unwrap();
        commit_all(dir.path());

        let result = reindex(dir.path()).unwrap();
        assert_eq!(count_docs(&result.cache), 1);
        assert!(result.errors.is_empty());

        let after = fs::read_to_string(&abs).unwrap();
        assert_eq!(after, content, "file should not be rewritten");
    }

    #[test]
    fn test_reindex_reports_error_on_malformed_completion_log_header() {
        let dir = create_test_repo();
        let good = "todo/good.md";
        let bad = "todo/bad.md";

        fs::create_dir_all(dir.path().join("todo")).unwrap();
        fs::write(
            dir.path().join(good),
            "---\ntldr: Good\nstatus: todo\ncheck_in_date: 2026-04-18\n---\n",
        )
        .unwrap();
        fs::write(
            dir.path().join(bad),
            "---\ntldr: Bad\nstatus: todo\ncheck_in_date: 2026-04-18\n---\n\n## Completion Log\n\n### not-a-date\n",
        )
        .unwrap();
        commit_all(dir.path());

        let result = reindex(dir.path()).unwrap();
        // Good file indexes; bad file reports an error but reindex continues.
        assert!(result.errors.iter().any(|e| e.path == Path::new(bad)));
        assert!(!result.errors.iter().any(|e| e.path == Path::new(good)));

        // Bad file untouched on disk.
        let bad_content = fs::read_to_string(dir.path().join(bad)).unwrap();
        assert!(bad_content.contains("## Completion Log"));
        assert!(bad_content.contains("### not-a-date"));
    }

    #[test]
    fn test_sync_does_not_migrate_legacy_files() {
        // Sync (not reindex) is read-only w.r.t. file shape. A subsequent
        // sync after the initial reindex migration should not touch files
        // that were new-shape from day one — and the sync path alone on a
        // dirty legacy file must also not rewrite.
        let dir = create_test_repo();
        let file = "todo/legacy.md";
        let legacy_content = "---\ntldr: Legacy\nstatus: todo\ncheck_in_date: 2026-04-18\nrrule: \"FREQ=WEEKLY\"\n---\n";
        fs::create_dir_all(dir.path().join("todo")).unwrap();
        fs::write(dir.path().join(file), legacy_content).unwrap();
        commit_all(dir.path());

        // First pass: initial reindex migrates the legacy file.
        let _ = open_and_sync(dir.path()).unwrap();
        let migrated = fs::read_to_string(dir.path().join(file)).unwrap();
        assert!(migrated.contains("recurrence:"));

        // Hand-place a legacy file again (simulates a fresh clone writing
        // the old shape). A *sync* pass (schema already at current version)
        // must NOT rewrite it — only reindex does that.
        fs::write(dir.path().join(file), legacy_content).unwrap();
        let before = fs::read_to_string(dir.path().join(file)).unwrap();
        let _ = open_and_sync(dir.path()).unwrap();
        let after = fs::read_to_string(dir.path().join(file)).unwrap();
        assert_eq!(before, after, "sync should not rewrite legacy files");
    }

    #[test]
    fn test_reindex_rewrites_flat_rrule_only() {
        let dir = create_test_repo();
        let file = "todo/recurring.md";
        let content = "---\ntldr: Weekly\nstatus: todo\ncheck_in_date: 2026-04-18\nrrule: \"FREQ=WEEKLY\"\n---\n";
        let abs = dir.path().join(file);
        fs::create_dir_all(abs.parent().unwrap()).unwrap();
        fs::write(&abs, content).unwrap();
        commit_all(dir.path());

        let result = reindex(dir.path()).unwrap();
        assert!(result.errors.is_empty());
        let after = fs::read_to_string(&abs).unwrap();
        assert!(after.contains("recurrence:"));
        assert!(!after.contains("\nrrule:"));
        assert!(!after.contains("completion_log:"));
    }

    // -- Persisted lint state --

    /// Raw `lint_errors` column for a path, as stored.
    fn lint_errors_col(cache: &Cache, path: &str) -> String {
        cache
            .conn
            .query_row(
                "SELECT lint_errors FROM docs WHERE path = ?1",
                params![path],
                |row| row.get(0),
            )
            .unwrap()
    }

    fn parse_fm(yaml: &str) -> Frontmatter {
        frontmatter::parse_frontmatter(&format!("---\n{yaml}\n---\n"))
            .unwrap()
            .unwrap()
    }

    #[test]
    fn test_lint_errors_persist_across_quiescent_sync() {
        let dir = create_test_repo();
        write_md(
            dir.path(),
            "bad.md",
            "tldr: Bad\nstatus: done\ncheck_in_date: 2026-05-01\non_date: 2026-04-10",
        );
        commit_all(dir.path());

        // First sync: reindex lints the file and reports it.
        let first = open_and_sync(dir.path()).unwrap();
        assert!(
            first
                .errors
                .iter()
                .any(|e| e.message.contains("check_in_date"))
        );
        drop(first);

        // Second sync: HEAD unchanged, clean tree — sync re-lints nothing,
        // but the persisted state still carries the violation.
        let second = open_and_sync(dir.path()).unwrap();
        assert!(
            second.errors.is_empty(),
            "quiescent sync should re-lint nothing: {:?}",
            second.errors
        );
        let persisted = second.cache.all_lint_errors().unwrap();
        assert_eq!(persisted.len(), 1);
        assert_eq!(persisted[0].path, PathBuf::from("bad.md"));
        assert!(persisted[0].message.contains("check_in_date"));
    }

    #[test]
    fn test_lint_errors_cleared_when_file_fixed() {
        let dir = create_test_repo();
        write_md(
            dir.path(),
            "bad.md",
            "tldr: Bad\nstatus: done\ncheck_in_date: 2026-05-01\non_date: 2026-04-10",
        );
        commit_all(dir.path());
        drop(open_and_sync(dir.path()).unwrap());

        // Drop the offending field and commit the fix.
        write_md(
            dir.path(),
            "bad.md",
            "tldr: Bad\nstatus: done\non_date: 2026-04-10",
        );
        commit_all(dir.path());

        let result = open_and_sync(dir.path()).unwrap();
        assert!(result.errors.is_empty());
        assert!(result.cache.all_lint_errors().unwrap().is_empty());
        assert_eq!(lint_errors_col(&result.cache, "bad.md"), "[]");
    }

    #[test]
    fn test_upsert_doc_refreshes_lint_state() {
        let dir = create_test_repo();
        write_md(dir.path(), "seed.md", "tldr: Seed");
        commit_all(dir.path());
        let cache = open_and_sync(dir.path()).unwrap().cache;

        let path = Path::new("task.md");
        let bad =
            parse_fm("tldr: Task\nstatus: done\ncheck_in_date: 2026-05-01\non_date: 2026-04-10");
        cache.upsert_doc(path, &bad, false).unwrap();
        let errors = cache.all_lint_errors().unwrap();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("check_in_date"));

        // Upserting a clean version over it clears the stored state.
        let good = parse_fm("tldr: Task\nstatus: done\non_date: 2026-04-10");
        cache.upsert_doc(path, &good, false).unwrap();
        assert!(cache.all_lint_errors().unwrap().is_empty());
        assert_eq!(lint_errors_col(&cache, "task.md"), "[]");
    }

    #[test]
    fn test_all_lint_errors_fails_on_corrupt_json() {
        let dir = create_test_repo();
        write_md(
            dir.path(),
            "bad.md",
            "tldr: Bad\nstatus: done\ncheck_in_date: 2026-05-01\non_date: 2026-04-10",
        );
        commit_all(dir.path());
        let cache = open_and_sync(dir.path()).unwrap().cache;

        cache
            .conn
            .execute(
                "UPDATE docs SET lint_errors = '{not json' WHERE path = 'bad.md'",
                [],
            )
            .unwrap();

        let err = cache.all_lint_errors().unwrap_err();
        let chain = format!("{err:#}");
        assert!(
            chain.contains("corrupt JSON array in cache DB"),
            "expected fail-fast on corrupt lint_errors, got: {chain}"
        );
    }

    #[test]
    fn test_schema_version_query_error_propagates() {
        let dir = create_test_repo();
        write_md(dir.path(), "a.md", "tldr: A");
        commit_all(dir.path());
        let cache = open_and_sync(dir.path()).unwrap().cache;

        // A BLOB survives the column's TEXT affinity, so reading it as a
        // String fails — a stand-in for a corrupt/unreadable meta table.
        cache
            .conn
            .execute(
                "INSERT OR REPLACE INTO meta (key, value) VALUES ('schema_version', x'00ff')",
                [],
            )
            .unwrap();

        let err = schema_version_mismatch(&cache.conn).unwrap_err();
        assert!(
            format!("{err:#}").contains("failed to read schema_version"),
            "expected the DB error to propagate, got: {err:#}"
        );
    }

    /// A failed `last_indexed_commit` probe must not read as "fresh DB": the
    /// resulting reindex would drop both tables and hide the real fault.
    #[test]
    fn test_last_indexed_commit_query_error_propagates() {
        let dir = create_test_repo();
        write_md(dir.path(), "a.md", "tldr: A");
        commit_all(dir.path());
        {
            let cache = open_and_sync(dir.path()).unwrap().cache;
            // Replace `meta` with a table of the same name but the wrong
            // shape: `CREATE TABLE IF NOT EXISTS` leaves it alone and the
            // probe fails with "no such column: key" — a fault, not a
            // never-synced DB.
            cache
                .conn
                .execute_batch(
                    "DROP TABLE meta; CREATE TABLE meta (wrong_column TEXT PRIMARY KEY);",
                )
                .unwrap();
        }

        let Err(err) = open_and_sync(dir.path()) else {
            panic!("expected the probe failure to propagate, got a successful sync");
        };
        assert!(
            format!("{err:#}").contains("failed to read last_indexed_commit"),
            "expected the DB error to propagate, got: {err:#}"
        );
    }

    #[test]
    fn test_all_lint_errors_expands_multiple_messages() {
        let dir = create_test_repo();
        write_md(dir.path(), "seed.md", "tldr: Seed");
        commit_all(dir.path());
        let cache = open_and_sync(dir.path()).unwrap().cache;

        // Trips both the terminal-check_in_date rule and the terminal-rrule rule.
        let fm = parse_fm(
            "tldr: Task\nstatus: done\non_date: 2026-04-10\ncheck_in_date: 2026-05-01\nrecurrence:\n  rrule: \"FREQ=WEEKLY\"",
        );
        cache.upsert_doc(Path::new("task.md"), &fm, false).unwrap();

        let errors = cache.all_lint_errors().unwrap();
        assert_eq!(errors.len(), 2, "got: {errors:?}");
        assert!(errors.iter().all(|e| e.path == PathBuf::from("task.md")));
        assert!(errors.iter().any(|e| e.message.contains("check_in_date")));
        assert!(errors.iter().any(|e| e.message.contains("rrule")));
    }

    #[test]
    fn test_unparseable_file_drops_persisted_lint_state() {
        let dir = create_test_repo();
        write_md(
            dir.path(),
            "task.md",
            "tldr: Bad\nstatus: done\ncheck_in_date: 2026-05-01\non_date: 2026-04-10",
        );
        commit_all(dir.path());

        let first = open_and_sync(dir.path()).unwrap();
        assert_eq!(first.cache.all_lint_errors().unwrap().len(), 1);
        drop(first);

        // Break the file (uncommitted): the row goes, and its lint state with it.
        fs::write(dir.path().join("task.md"), "---\nbogus_field: oops\n---\n").unwrap();

        let second = open_and_sync(dir.path()).unwrap();
        assert_eq!(count_docs(&second.cache), 0);
        assert!(second.cache.all_lint_errors().unwrap().is_empty());
        // The parse failure itself is still reported for this pass.
        assert_eq!(second.errors.len(), 1);
        assert!(second.errors[0].path.to_str().unwrap().contains("task.md"));
    }

    #[test]
    fn test_stale_schema_version_reindex_populates_lint_state() {
        let dir = create_test_repo();
        write_md(
            dir.path(),
            "bad.md",
            "tldr: Bad\nstatus: done\ncheck_in_date: 2026-05-01\non_date: 2026-04-10",
        );
        commit_all(dir.path());
        drop(open_and_sync(dir.path()).unwrap());

        let conn = rusqlite::Connection::open(db_path(dir.path())).unwrap();
        conn.execute(
            "UPDATE meta SET value = '99' WHERE key = 'schema_version'",
            [],
        )
        .unwrap();
        drop(conn);

        let result = open_and_sync(dir.path()).unwrap();
        assert_eq!(count_docs(&result.cache), 1);
        let errors = result.cache.all_lint_errors().unwrap();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("check_in_date"));
    }

    #[test]
    fn test_missing_schema_version_row_triggers_reindex() {
        let dir = create_test_repo();
        write_md(
            dir.path(),
            "bad.md",
            "tldr: Bad\nstatus: done\ncheck_in_date: 2026-05-01\non_date: 2026-04-10",
        );
        commit_all(dir.path());
        drop(open_and_sync(dir.path()).unwrap());

        // A pre-versioning cache: has last_indexed_commit but no schema_version.
        let conn = rusqlite::Connection::open(db_path(dir.path())).unwrap();
        conn.execute("DELETE FROM meta WHERE key = 'schema_version'", [])
            .unwrap();
        drop(conn);

        let result = open_and_sync(dir.path()).unwrap();
        assert_eq!(count_docs(&result.cache), 1);
        let errors = result.cache.all_lint_errors().unwrap();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("check_in_date"));
    }

    #[test]
    fn test_open_cache_no_sync_rejects_missing_schema_version() {
        let dir = create_test_repo();
        write_md(dir.path(), "task.md", "tldr: Task");
        commit_all(dir.path());
        drop(open_and_sync(dir.path()).unwrap());

        let conn = rusqlite::Connection::open(db_path(dir.path())).unwrap();
        conn.execute("DELETE FROM meta WHERE key = 'schema_version'", [])
            .unwrap();
        drop(conn);

        let Err(err) = open_cache_no_sync(dir.path()) else {
            panic!("expected open_cache_no_sync to reject a pre-versioning cache");
        };
        assert!(err.to_string().contains("stale schema"), "got: {err:#}");
    }
}
