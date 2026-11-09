# Changelog

Notable changes to graf. Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- YAML frontmatter is now parsed by `serde-saphyr` instead of the deprecated
  `serde_yaml`, whose underlying `unsafe-libyaml` library is unmaintained
  (security advisory RUSTSEC-2024-0449). This clears the advisory from
  graf's dependency tree. Most files parse exactly as before; the visible
  differences are:
  - frontmatter parse errors are a single line ending in
    `at line L, column C`;
  - quoted numbers (`priority: "3"`) are accepted for numeric fields;
  - YAML `<<` merge keys are expanded;
  - a `comment: |` block that ends the frontmatter reads back with a
    trailing newline.
- The `migrate_completed_date` tool now rejects a date key that is present
  but empty or null, instead of treating it as absent.

### Fixed

- Values spelled `null`, `Null`, `NULL` or `~` are written quoted, so a label,
  link or other text field holding that word reads back as text instead of
  failing to parse.
- Multi-line `completion_log` comments are written as `comment: |-`, so they
  round-trip without gaining a trailing newline wherever the entry sits in
  the frontmatter.

### Security

- The bundled SQLite moves from 3.51.3 to 3.54.1 (`rusqlite` 0.41.1,
  `libsqlite3-sys` 0.39.1), which carries the fix for CVE-2026-119375
  (RUSTSEC-2026-0459): a database opened together with a crafted `-wal` file
  of a different page size could return uninitialized memory.

## [0.5.0] — 2026-07-20

First public release. Versions before 0.5.0 were developed privately and are
not part of this repository's history.

### Added

- **Todo management** — `graf todo` queries tasks (due today or earlier by
  default), with `add`, `done`, `cancel`, `schedule`, and `reorder`
  subcommands. Any document becomes a tracked task by carrying a check-in or
  due date; no separate task store.
- **Recurring tasks** — RFC 5545 `rrule` recurrence in a frontmatter
  `recurrence` block, with a completion log and automatic advancement to the
  next occurrence on completion.
- **Linting** — `graf lint` validates frontmatter across the repo, over
  git-changed files (`--changed`), or over named files. `graf fix` repairs
  timestamps on changed files.
- **MCP server** — `graf mcp` serves the Model Context Protocol over stdio for
  use by Claude Code and other MCP clients. Fifteen tools spanning validation,
  todo mutation, and manifest inspection, plus four `graf_ref_*` tools that
  return frontmatter, link, and attachment reference material on demand.
- **Multi-repo support** — an application manifest maps slugs to repos, so
  documents address each other across repo boundaries as `slug:path`. Queries
  span every manifest repo by default; mutations require a single target. See
  [docs/multi-repo.md](docs/multi-repo.md).
- **SQLite metadata cache** — incremental sync against git state, rebuildable
  from scratch with `graf reindex`.
- Apache-2.0 license.
