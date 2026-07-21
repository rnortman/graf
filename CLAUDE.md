# graf

Rust CLI tool for managing a personal knowledge base + todo system built on
markdown files with YAML frontmatter. Claude Code is the primary user-facing
interface; `graf` provides structured queries that Claude Code can't do
efficiently with grep alone.

## Building

```
make check    — full pre-commit suite (fmt + clippy + test)
make build    — debug build
make release  — release build
```

**Use `cargo add` for adding dependencies** (not manual Cargo.toml edits).

## Principles

**Fail fast.** No fallbacks. No swallowed errors. No "keep calm and carry on." Crash beats silent corruption.

**Resist disabling lint checks.** We use Rust to force correct code. Fix clippy warnings, don't suppress them.

**Reuse before reimplementing.** Search for existing functions before writing new ones. Make new code reusable where natural.

**Always write unit tests.**

Our motto: **Take your time. We are not in a hurry. Do it right.**

## TODOs

Track deferred work in [TODO.md](TODO.md). Each entry gets a kebab-case slug as its heading (e.g., `## \`my-slug\``). Reference it from code with `// TODO(my-slug)` comments. When adding a TODO:

1. Add a `## \`slug\`` section to TODO.md with a short description of the problem and why it's deferred.
2. Add `// TODO(slug)` at the relevant code site(s).

When completing a TODO, remove both the TODO.md entry and all `// TODO(slug)` comments.

## Code Standards

- Rust edition 2024, stable toolchain.
- `cargo clippy` must pass with no warnings. `cargo test` must pass.
- Pre-commit hook runs `make check`.
- Default branch: `main`.
