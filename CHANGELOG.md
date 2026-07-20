# Changelog

Notable changes to graf. Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
