# Multi-Repo Guide

Graf can manage multiple data repositories — personal, work, shared —
and query them together or individually.  This guide covers setup and
daily use.

## Quick Start

You have two repos: `~/grafdata/life` and `~/grafdata/work`.

### 1. Initialize each repo's identity

```sh
cd ~/grafdata/life
graf manifest init --id example.com/life --domain example.com/personal

cd ~/grafdata/work
graf manifest init --id acme.com/eng --domain acme.com/confidential
```

This creates `.graf/config.toml` in each repo with a global ID, a default
slug (derived from the last segment of the ID — `life`, `eng`), and an
optional sharing domain.

### 2. Register them in your manifest

```sh
graf manifest add ~/grafdata/life
graf manifest add ~/grafdata/work eng
```

The first command uses the repo's default slug (`life`).  The second
overrides it to `eng` (the default would be `eng` here too, but you
could choose `work` or anything else).

This creates `~/.config/graf/manifest.toml`:

```toml
[[repo]]
slug = "life"
path = "~/grafdata/life"
id = "example.com/life"

[[repo]]
slug = "eng"
path = "~/grafdata/work"
id = "acme.com/eng"
```

### 3. Validate

```sh
graf manifest check
```

Checks that paths exist, configs are present, and IDs match.

## Using `--repo`

The `--repo` flag accepts either a **slug** or a **filesystem path**:

```sh
graf todo --repo life --all          # by slug
graf todo --repo ~/grafdata/life     # by path (backward compatible)
```

Graf distinguishes them by syntax: if the value contains `/`, `~`, or
starts with `.`, it's a path.  Otherwise it's a slug.

## Querying Multiple Repos

When a manifest is active and you omit `--repo`, query commands
(`graf todo`, `graf lint`) operate on **all** repos:

```sh
graf todo --all
```

This returns tasks from every repo, each tagged with its repo slug and
sharing domain:

```json
{
  "domains": ["example.com/personal", "acme.com/confidential"],
  "tasks": [
    {
      "path": "todo/dentist.md",
      "repo": "life",
      "domain": "example.com/personal",
      "tldr": "Call dentist",
      ...
    },
    {
      "path": "todo/deploy.md",
      "repo": "eng",
      "domain": "acme.com/confidential",
      "tldr": "Fix deployment",
      ...
    }
  ]
}
```

The `path` is always bare (relative to its repo root).  The `repo` field
tells you which repo it belongs to.  To construct a qualified path for
display, combine them: `life:todo/dentist.md`.

### Human-readable output

When printing to a terminal, multi-repo output includes the repo slug:

```
2026-04-12 (today)
  life      [todo]  P2  Call dentist                               todo/dentist.md
  eng       [todo]  P1  Fix deployment                             todo/deploy.md
```

### Single-repo backward compatibility

Without a manifest, everything works exactly as before: `--repo` takes a
filesystem path (default: cwd), no slug or domain metadata in output.

## Mutations Require a Single Repo

Commands that modify files (`todo add`, `done`, `cancel`, `schedule`,
`reorder`, `fix`, `reindex`) must know which repo to operate on.  If the
manifest has multiple repos and you don't specify `--repo`, they error:

```sh
$ graf todo done todo/dentist.md
Error: multiple repos found in manifest — specify --repo for this command

$ graf todo done todo/dentist.md --repo life
Done: todo/dentist.md (completed 2026-04-12)
```

## JSON Output

Every command supports `--json`.  When stdout is not a terminal (e.g.,
piped or called by an LLM tool), JSON is the default.

| Command | JSON shape |
|---------|------------|
| `todo` (query) | `{ domains?, tasks: [...], lint_errors: [...] }` |
| `todo add` | `{ path }` |
| `todo done` | `{ path, on_date }` or `{ path, end_date }` or `{ path, next_date }` |
| `todo cancel` | `{ path, on_date }` or `{ path, end_date }` |
| `todo schedule` | `{ path, tentative_date, sort_order }` |
| `todo reorder` | `{ path, sort_order, tentative_date? }` |
| `lint` | `{ files_checked, files_fixed, errors }` |
| `reindex` | `{ files_indexed, errors }` |
| `manifest list` | `{ repos, domains }` |
| `manifest check` | `{ ok, errors, warnings, repos, domains }` |

## MCP Server

The MCP server (`graf mcp`) exposes all commands as tools.  Every
repo-aware tool accepts an optional `repo` parameter (slug or path),
matching the CLI behavior.

When a manifest is active, query tools (`graf_todo_query`, `graf_lint`)
query all repos by default.  Mutation tools require an explicit `repo`
parameter when the manifest has multiple repos.

Additional MCP-only tools:

- `graf_manifest_list` — returns the manifest contents
- `graf_manifest_check` — validates the manifest

## Sharing Domains

Each repo can declare a **sharing domain** — an information boundary that
answers "who should see derivatives of data from this repo?"

```toml
# .graf/config.toml
id = "example.com/life"
domain = "example.com/personal"
```

Domains appear in query results so that upstream consumers (like brenn)
can track information contamination across LLM conversations.  Graf
itself does not enforce domain policy — it provides the metadata.

## Manifest Reference

### Repo config (`.graf/config.toml`)

Created by `graf manifest init`.  Travels with the repo.

```toml
id = "example.com/life"          # globally unique identity
default_slug = "life"             # suggested slug for manifest add
domain = "example.com/personal"   # sharing domain (optional)
```

### Application manifest (`~/.config/graf/manifest.toml`)

Created/updated by `graf manifest add`.  Local to this machine.
Override location with `GRAF_MANIFEST` environment variable.

```toml
[[repo]]
slug = "life"
path = "~/grafdata/life"
id = "example.com/life"

[[repo]]
slug = "eng"
path = "~/grafdata/work"
id = "acme.com/eng"

[[domain]]
slug = "personal"
id = "example.com/personal"
```

### Validation rules

- All repo slugs must be unique
- All repo IDs must be unique
- All domain slugs must be unique
- All domain IDs must be unique
- Repo paths must exist and contain `.graf/config.toml`
- Manifest ID must match the repo's config ID

Run `graf manifest check` to verify all of this.

## Partial Failures

When querying multiple repos, a failure in one repo (broken cache,
missing directory) does not abort the whole command.  The error is
included in the results and remaining repos are still processed.  The
exit code is non-zero if any repo had errors.
