# graf

Rust CLI and MCP server for managing a personal knowledge base and todo
system built on markdown files with YAML frontmatter.

This is like a lot of other markdown-based "graph knowledge base" systems,
with a few small twists:

- It's designed mostly to be useful for LLMs rather than humans directly, though
  it's fine for humans to read and author also. But there's no automatic
  keyword-based Wiki-style linking; all links are explicit paths or URIs.
- It integrates the knowledge base and a to-do/ticket-tracking system. Any doc
  can become a tracked to-do item or "ticket" simply by setting a check-in date
  (when the item needs attention next) or due date. A simple recurrence system
  is provided for recurring tasks, but LLMs can also step in to handle more
  complex schedules directly.
- It supports [multiple graf repos](docs/multi-repo.md) ("vaults" in Obsidian
  terminology) with a well-defined cross-repo linking system. It's suggested to
  make each graf repo also be its own git repo.
- It's written in Rust: Single binary to deploy, very few deps, very fast.

It includes an MCP server and a repo linter/tidier, though it's also usable by
LLMs directly just through normal filesystem read/write/search tools. The MCP
server handles structured tasks like marking a to-do item complete, or
snoozing/rescheduling it, or running the linter, which are mechanical tasks LLMs
are less reliable at (and more expensive).

## Installing

```bash
cargo install --path . --bin graf
```

This puts the `graf` binary in `~/.cargo/bin`.

## Building

For working on graf itself:

```bash
make check    # fmt + clippy + test
make build    # debug build
make release  # optimized release build
```

## Setting Up a New Graf Repo

A graf repo is a directory of `.md` files with YAML frontmatter, tracked by
git. Any directory works — graf doesn't require a particular layout.

### 1. Initialize the repo

```bash
mkdir ~/grafdata/myrepo && cd ~/grafdata/myrepo
git init
```

### 2. Add `.gitignore`

Graf creates a SQLite cache file (`.graf.db`) in the repo root. This is
derived data and must not be committed:

```
.graf.db
```

### 3. Set up the pre-commit hook

The pre-commit hook runs `graf lint` on changed files and blocks commits
with lint errors (missing frontmatter, invalid fields, etc.):

```bash
mkdir -p .githooks
cat > .githooks/pre-commit << 'EOF'
#!/usr/bin/env bash
exec graf lint --changed
EOF
chmod +x .githooks/pre-commit
git config core.hooksPath .githooks
git add .gitignore .githooks
git commit -m "Set up graf hooks"
```

graf needs the repo to have at least one commit before it will touch it, so
don't skip this step even if you have nothing else to commit yet.

### 4. Create your first document

```bash
graf todo add todo/my-first-task.md "Set up graf repo" --priority 2 --check-in 2026-07-21
```

The title is positional. At least one of `--check-in` or `--due` is required —
a task with no date isn't a task graf will ever surface to you.

Or manually create any `.md` file with frontmatter:

```markdown
---
tldr: My first document
created: '2026-04-11T12:00:00-04:00'
updated: '2026-04-11T12:00:00-04:00'
---

Document body here.
```

## Commands

| Command | Description |
|---------|-------------|
| `graf todo` | Query tasks (due today or earlier by default) |
| `graf todo add <path> <title>` | Create a new task |
| `graf todo done <path>` | Mark a task done |
| `graf todo cancel <path>` | Cancel a task |
| `graf todo schedule <path> <date>` | Set tentative date |
| `graf todo reorder <path> --after/--before <path>` | Reorder tasks |
| `graf lint` | Validate all files |
| `graf lint --changed` | Validate git-changed files only |
| `graf lint <files...>` | Validate specific files |
| `graf fix` | Auto-fix timestamps on changed files |
| `graf manifest init --id <id>` | Initialize a repo's `.graf/config.toml` |
| `graf manifest add` | Add a repo to the application manifest |
| `graf manifest list` | List all manifest entries |
| `graf manifest check` | Validate the manifest against repo configs |
| `graf reindex` | Rebuild the SQLite cache from scratch |
| `graf mcp` | Run as an MCP server (JSON-RPC over stdio) |

Paths accept `slug:path` (e.g. `life:todo/my-task.md`) to address a document
in another repo — see the [multi-repo guide](docs/multi-repo.md).

### Common flags

- `--json` — JSON output (auto-enabled when stdout is not a TTY)
- `--repo <slug|path>` — repo slug or path. Defaults to the current
  directory, except `graf todo` (query), which defaults to every repo in the
  manifest.

## MCP Server

`graf mcp` serves MCP over stdio, for use by Claude Code or other MCP
clients. The tool surface is close to the CLI but not identical: the
`graf_ref_*` tools are MCP-only, returning reference material an LLM would
otherwise have to guess at.

| Group | Tools |
|-------|-------|
| Validation | `graf_lint`, `graf_fix`, `graf_reindex` |
| Todos | `graf_todo_query`, `graf_todo_add`, `graf_todo_done`, `graf_todo_cancel`, `graf_todo_schedule`, `graf_todo_reorder` |
| Manifest | `graf_manifest_list`, `graf_manifest_check` |
| Reference (MCP-only) | `graf_ref_frontmatter_schema`, `graf_ref_attachments`, `graf_ref_links`, `graf_ref_context_link` |
