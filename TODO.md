# TODOs

## `done-error-taxonomy-cleanup`

`DoneError::Invalid(String)` and `DoneError::Infra(anyhow::Error)` (src/todo.rs) are two catch-all variants that every caller must special-case: the MCP layer routes `Infra` to `Err(e)` while emitting the rest as `Ok(envelope)`; CLI JSON mode does the same; CLI human mode stringifies everything. Consider splitting `Invalid` into a small set of named variants (path-not-md, replacing-with-no-stored, replacing-redundant, missing-anchor) so frontends can branch on named codes, and keeping `Infra` as the sole catch-all with a `DoneError::is_infra()` predicate so callers don't pattern-match on the specific variant.

## `done-error-derive-serialize`

`DoneError::to_json` (src/todo.rs) hand-rolls the `{error, reason, ...payload}` envelope by mutating a `serde_json::Value`. A `#[derive(Serialize)]` with `#[serde(tag = "error", rename_all = "snake_case")]` would replace the 80-line `code()` / `to_json()` pair with framework-driven serialization; the `reason` string can stay computed in a `reason()` accessor. Phase 3 added `ScheduleError` with the same hand-rolled idiom (code/reason/to_json/Display/Error/From<anyhow>) — the pattern is now load-bearing for two error types, with a likely third (`ReorderError`, `CancelError`) each time a mutator grows structured errors. Sweep both (all) enums together; the refactor is purely mechanical, no semantic change.

## `done-anchored-double-rrule-parse`

`done_anchored` in src/todo.rs parses and iterates the rrule twice on the stale-anchor branch — once via `compute_next_occurrence` for the naive next anchor, once via `compute_occurrence_after` for the skip-past target. A single helper that returns both values in one iteration would halve the parse+iterate work. Once per user action, so the impact is negligible; cleanup only.

## `workspace-handle`

Multiple callers carry `repo_dir` in their Options structs and independently call `open_and_sync`. Consider a higher-level `Workspace` handle that wraps `repo_dir` + `Cache`, so callers share a single entry point. See `db.rs`.

## `cross-repo-ref-validation`

`graf manifest check` validates manifest-level consistency (slugs unique, IDs match, paths exist) but does not validate cross-repo refs. A repo's `.graf/config.toml` `refs` section maps slugs to global IDs — `check` could verify those IDs exist in the application manifest and that the slugs don't conflict. Deferred per design doc: "Cross-repo ref validation (do slugs in refs resolve?) is deferred."

## `sort-key-duplication`

The SQL `ORDER BY` in `db.rs` and the Rust `todo_sort_cmp()` in `cli/mod.rs` encode the same sort logic independently. The Rust comparator is used for k-way merge of multi-repo results; the SQL ORDER BY handles single-repo. They must agree. The integration test `test_multi_repo_sort_matches_single_repo_sql_order` catches divergence. Consider generating the sort key from a shared definition to make divergence impossible.

## `mcp-config-caching`

`AppConfig::load_default()` is called on every MCP `graf_todo_query` tool call, re-reading and parsing the config file from disk each time. The MCP server is long-lived and config won't change mid-session. Load the config once at server startup and store it in `McpContext`, then reference it from tool `call()` methods without per-request I/O.

## `xdg-path-duplication`

`AppConfig::default_path()` and `AppManifest::default_path()` both implement the same XDG config directory logic (`XDG_CONFIG_HOME` → fallback `~/.config`). Extract a shared `graf_config_dir()` helper when a third caller appears.

## `done-horizon`

`--include-done --horizon N` exempts all terminal items from the horizon filter, returning every done/cancelled task regardless of when it was completed. For a user with many historical completed items this could produce a large result set. Consider adding a separate horizon for terminal items — e.g., only include items completed within the last N days — or filtering terminal items by `on_date`/`end_date` relative to the horizon cutoff. See the horizon exemption in `db.rs` `query_tasks()`.

## `sql-terminal-status-strings`

The SQL strings `status IN ('done', 'cancelled')` appear in the ORDER BY and horizon WHERE clauses of `db.rs`. These are hardcoded string literals that must stay in sync with `Status::Done` and `Status::Cancelled` serialization. If a new terminal status were added, these would need manual updates found only by text search. Consider building the SQL fragment from `Status::is_terminal()` variants or a shared constant.

## `resolve-target-env-isolation`

`resolve_target` unit tests call the real `AppManifest::load_default()`, so results depend on whether the user's `~/.config/graf/manifest.toml` exists and what slugs it contains. The function should accept a manifest (or manifest loader) as a parameter instead of reading the global config implicitly. This would also make `resolve_target` more testable in general. See `cli/mod.rs` `resolve_target()` and its tests.

## `mcp-preflight-shared-error-type`

`DoneError::PathNotFound`/`InvalidDate` (`src/todo.rs`) and `ScheduleError::PathNotFound`/`InvalidDate` (`src/todo.rs`) are structurally identical variants with identical `code()`, `reason()`, and `to_json()` arms. When `cancel-mcp-envelope` lands a third copy will appear. Extract a shared `McpPreflightError` enum (in `src/cli/mcp.rs` or a new `src/cli/mcp_errors.rs`) and embed it via a variant in each tool-specific error enum. Best tackled alongside `done-error-derive-serialize`. See `TODO(mcp-preflight-shared-error-type)` comment at `src/todo.rs`.

## `recurring-phase-2-remove-flat-rrule`

`Frontmatter.rrule: Option<String>` is retained as a legacy-shape compat field (`src/frontmatter.rs`). Reindex eagerly migrates old files into the nested `recurrence` block, and no write path emits flat `rrule`. Once we're confident no repos still carry legacy-shape files (or are willing to surface them as parse errors for manual fix-up), remove the field and the associated `Field::Rrule` variant, plus the legacy-compat branches in `maybe_migrate_legacy` and `effective_rrule`.

## `migration-rrule-syntax-validation`

`maybe_migrate_legacy` in `src/migrate.rs` rewrites legacy frontmatter without validating that the `rrule` string is parseable. A file with a broken `rrule` (e.g. `FREQ=BOGUS`) migrates into the nested shape still broken; runtime breakage surfaces only when `compute_next_occurrence` is invoked. The intended behavior is that a `FREQ=BOGUS` rrule records a `FileError` and leaves the file untouched, but neither lint nor parse validates rrule syntax elsewhere, so migration isn't the natural place to add enforcement. Consider adding an rrule-syntax lint rule that applies uniformly; migration would then surface the issue via the post-rewrite lint pass.

## `ephemeral-lint-errors`

`TodoQueryResult.lint_errors` is populated from the current sync pass's `sync.errors`, which only contains errors for files the sync actually re-linted (step 5 working-tree changes, step 4 HEAD diff, step 2 reconcile-dirty). A file with a lint violation that was committed long ago and hasn't been touched since gets linted once when indexed; on subsequent queries its lint error is not re-emitted — the error message is never persisted in the cache.

Null-`effective_date` rows are exempt from this silence because `todo_query` partitions them from `tasks` into `lint_errors` at the response-building layer on every call (see the invariant on `TodoQueryResult` and the test `test_null_effective_date_partitioned_across_queries`). But other lint rules (e.g. terminal item with `check_in_date` set, `end_date` without `start_date`) still exhibit the silence.

`graf lint` runs a full-repo scan and is the authoritative way to surface all current violations. `graf todo`'s `lint_errors` is therefore a "what changed in this sync pass" channel plus the null-date partition, not a comprehensive "what's wrong in the repo" report.

Fix options: (1) persist lint error messages in the cache (column on `docs` or a side table), refresh the set on each file re-lint; (2) re-run `check_rules` at query time against every cache row (likely too expensive for large repos); (3) document the current semantics and treat `graf lint` as the authoritative surface. Option (1) has the best cost/value ratio and would let `graf todo`'s `lint_errors` become trustworthy in steady state.
