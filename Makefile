.PHONY: check build release release-musl fmt clippy test clean setup-hooks scrub-tree

# Full pre-commit check suite.
check: fmt clippy test

setup-hooks:
	git config core.hooksPath .githooks
	@rm -f .git/hooks/pre-commit
	@command -v brenn-scrub >/dev/null 2>&1 || echo "brenn-scrub not on PATH: cargo install --path ~/src/brenn/scrub"
	@command -v gitleaks >/dev/null 2>&1 || echo "gitleaks not on PATH: install the release pinned in brenn/scrub/src/gitleaks.rs"
	@echo "setup-hooks: done."

scrub-tree:
	brenn-scrub tree

# Build the graf binary.
build:
	cargo build

# Production release build.
release:
	cargo build --release

# Static musl release build (for CI/deployment).
release-musl:
	cargo build --release --target x86_64-unknown-linux-musl

fmt:
	cargo fmt -- --check

clippy:
	cargo clippy -- -D warnings

test:
	cargo test

clean:
	cargo clean
