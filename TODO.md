# TODOs

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

## `xdg-empty-unset`

`graf_config_dir_from()` (`src/config.rs`) uses an empty-but-set
`$XDG_CONFIG_HOME` verbatim, resolving config/manifest relative to the process
CWD. The XDG Base Directory spec requires an empty variable to be treated as
unset (fall back to `$HOME/.config`). The behavior is pre-existing (the old
`env::var` block did the same) and was deliberately preserved as a pure
extraction — the `config_dir_honors_empty_xdg_verbatim` test pins it so a future
fix is a deliberate change. Fixing it (`xdg_config_home.filter(|s|
!s.is_empty())`, invert the test) is a behavior change with CI/systemd/container
blast radius, so it is deferred to a design decision rather than folded into a
refactor.

## `clippy-gate-skips-tests`

The `make check` clippy gate (`Makefile`, `clippy:` target) runs `cargo clippy
-- -D warnings` without `--all-targets`, so test code is ungated. `cargo clippy
--all-targets` currently emits ~25 warnings in test modules (useless `vec!`,
redundant closures, etc.). CLAUDE.md's "clippy must pass with no warnings" thus
holds only for lib/bin code while test-code lint rot accumulates silently. The
warnings predate this work. Fixing means adding `--all-targets` to the clippy
target and burning down the existing test-code warnings — a project-wide
lint-gate policy change, separate from any feature work.

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

## `git-env-leaks-into-test-repos`

Every git spawn in graf is a bare `Command::new("git")` that inherits the
caller's environment: production `src/git.rs` (all four functions) and the
tests in `src/db.rs` (`run_git` plus five direct spawns that bypass it),
`src/todo.rs`, `src/cli/mcp.rs`, `tests/multi_repo_integration.rs`,
`tests/todo_integration.rs`, `tests/mcp_integration.rs` and
`tests/lint_integration.rs`. Each file carries one
`TODO(git-env-leaks-into-test-repos)` marker; the work covers every spawn in
the file, not only the marked line.

Under a pre-commit hook, git exports `GIT_INDEX_FILE` (absolute under
`git commit -a` or `git commit <pathspec>`, and from a linked worktree in
every form, where `GIT_DIR` is exported too) plus `GIT_AUTHOR_*` and
`GIT_PREFIX` unconditionally. The test suite then operates on the outer
repo's pending index: parallel tests collide on `index.lock.lock`, foreign
blobs appear as `invalid object … for '<file>'`, and the outer commit's index
is rewritten by the tests (git re-reads it after the hook), so a passing hook
could silently commit a temp repo's paths. Until this is fixed, graf commits
are made with `git add <paths>` and a bare `git commit` from the main
worktree.

Work: route every git spawn through one helper that scrubs `GIT_*` and pins
config sources (`GIT_CONFIG_GLOBAL` / `GIT_CONFIG_SYSTEM` = `/dev/null`,
`GIT_CEILING_DIRECTORIES` at the temp root), with a test that sets a bogus
`GIT_INDEX_FILE` and proves the helper ignores it. Open questions needing a
design decision: whether production `src/git.rs` scrubs as well (graf run
from inside someone else's hook has the same exposure), and whether to take
brenn's `git-fixture` as a dev-dependency or write a graf-local helper.
Deferred because it spans eight files and 42 call sites plus that mechanism
decision.
