# TODOs

## `cross-repo-ref-validation`

`graf manifest check` validates manifest-level consistency (slugs unique, IDs match, paths exist) but does not validate cross-repo refs. A repo's `.graf/config.toml` `refs` section maps slugs to global IDs — `check` could verify those IDs exist in the application manifest and that the slugs don't conflict. Deferred per design doc: "Cross-repo ref validation (do slugs in refs resolve?) is deferred."

## `sort-key-duplication`

The SQL `ORDER BY` in `db.rs` and the Rust `todo_sort_cmp()` in `cli/mod.rs` encode the same sort logic independently. The Rust comparator is used for k-way merge of multi-repo results; the SQL ORDER BY handles single-repo. They must agree. The integration test `test_multi_repo_sort_matches_single_repo_sql_order` catches divergence. Consider generating the sort key from a shared definition to make divergence impossible.

## `mcp-config-caching`

`AppConfig::load_default()` is called on every MCP `graf_todo_query` tool call, re-reading and parsing the config file from disk each time. The MCP server is long-lived and config won't change mid-session. Load the config once at server startup and store it in `McpContext`, then reference it from tool `call()` methods without per-request I/O.

## `xdg-path-duplication`

`AppConfig::default_path()` and `AppManifest::default_path()` both implement the same XDG config directory logic (`XDG_CONFIG_HOME` → fallback `~/.config`). Extract a shared `graf_config_dir()` helper when a third caller appears.

## `sql-terminal-status-strings`

The SQL strings `status IN ('done', 'cancelled')` appear in the ORDER BY and horizon WHERE clauses of `db.rs`. These are hardcoded string literals that must stay in sync with `Status::Done` and `Status::Cancelled` serialization. If a new terminal status were added, these would need manual updates found only by text search. Consider building the SQL fragment from `Status::is_terminal()` variants or a shared constant.

## `recurring-phase-2-remove-flat-rrule` — BLOCKED as of 2026-07-20

`Frontmatter.rrule: Option<String>` is retained as a legacy-shape compat field (`src/frontmatter.rs`). Reindex eagerly migrates old files into the nested `recurrence` block, and no write path emits flat `rrule`. Once we're confident no repos still carry legacy-shape files (or are willing to surface them as parse errors for manual fix-up), remove the field and the associated `Field::Rrule` variant, plus the legacy-compat branches in `maybe_migrate_legacy` and `effective_rrule`.

## `persist-parse-errors`

Lint-rule violations are persisted per-doc in `docs.lint_errors`, so `graf todo` reports them on every query. Parse and validation failures are not: a file that fails to parse has its cache row deleted (`reconcile_dirty` and `apply_changed_files` in `src/db.rs`), so its error is reported only on the sync pass that touched it — the same ephemerality that persisted lint state fixed for lint rules.

Persisting these means keeping rows for files with no valid frontmatter, which have no `tldr` and no dates: either a nullable-everything doc row or a separate errors table. That is a schema question of its own, and was out of scope for the lint-persistence work. Until then, `graf lint` remains the way to surface standing parse failures.


## `resolve-slug-arm-duplication`

`resolve_repos`' slug arm (`src/cli/mod.rs`) and `resolve_target`'s
`DocRef::Qualified` arm independently perform the same sequence:
`find_repo_by_slug` → error on miss → `AppManifest::expand_path` →
`load_repo_config_required` → construct `ResolvedRepo`. The "unknown repo slug"
error wording has already diverged slightly between the two arms, and any future
per-repo metadata added to `ResolvedRepo` must be plumbed twice. Now that both
arms share a `manifest: Option<&AppManifest>` parameter, extract
`fn resolve_manifest_slug(manifest: &AppManifest, slug: &Slug) -> Result<ResolvedRepo>`
and call it from both. Deferred: the resolver-threading design deliberately
scoped to "plain data parameter, no restructuring," so this cleanup is a
separate change.

## `frozen-series-effective-date`

`EFF_DATE` in `src/db.rs` resolves an item's effective date as
`COALESCE(tentative_date, check_in_date, on_date, end_date, due_date)`, on the
premise that terminal items have their scheduling dates stripped and so sort
and report by completion date. A frozen series breaks that premise: anchored
exhaustion (`anchored_terminal_completion` in `src/todo.rs`) preserves
`check_in_date` frozen at the final occurrence, so an exhausted series reports
and sorts by that occurrence rather than by its `on_date` completion date —
off by however long completion lagged the last occurrence.

Which date such an item should show is a product question against the phase-2
freeze semantics (PRD-data-model §2), not a local bug fix: either the terminal
branch of the effective-date expression prefers `on_date`/`end_date` (a schema
bump plus an index review, since `EFF_DATE` must stay textually identical
across the schema index, SELECT, WHERE and ORDER BY), or the final-occurrence
ordering is declared intended and documented as such.
