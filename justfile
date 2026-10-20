# winx-code-agent task runner. `just --list` shows every recipe.
# Agents and contributors run `just check` before committing (AGENTS.md).

set shell := ["bash", "-euo", "pipefail", "-c"]
set windows-shell := ["powershell.exe", "-NoLogo", "-NoProfile", "-Command"]

windows_target := "x86_64-pc-windows-gnu"

# Default: list recipes
default:
    @just --list

# Fast compile check with the committed lockfile
check-fast:
    cargo check --all-features --locked

# Formatting check
fmt-check:
    cargo fmt --all -- --check

# Apply formatting
fmt:
    cargo fmt --all

# Enforced lint suite on the host target
clippy:
    cargo clippy --all-targets --all-features --locked -- -D warnings

# Lint the native Windows target from Linux/macOS (needs `just setup-windows-lint` once)
windows-lint:
    cargo clippy --target {{windows_target}} --all-targets --all-features --locked -- -D warnings

# Install the Windows target and MinGW linker prerequisites for `just windows-lint`
[unix]
setup-windows-lint:
    rustup target add {{windows_target}}
    @command -v x86_64-w64-mingw32-gcc >/dev/null || echo "install mingw-w64-gcc (pacman -S mingw-w64-gcc / apt install gcc-mingw-w64-x86-64)"

# Fast local lint: formatting + host clippy (what the pre-commit hook runs)
lint: fmt-check clippy

# Full normal test suite. The usage-log variables are cleared so a developer
# shell that exports WINX_USAGE_LOG does not get synthetic test events in its
# real telemetry.
[unix]
test:
    env -u WINX_USAGE_LOG -u WINX_USAGE_LOG_ROTATION cargo test --all-features --locked

[windows]
test:
    Remove-Item Env:\WINX_USAGE_LOG,Env:\WINX_USAGE_LOG_ROTATION -ErrorAction SilentlyContinue; cargo test --all-features --locked; exit $LASTEXITCODE

# Run one test filter, e.g. `just test-one status_check`
[unix]
test-one filter:
    env -u WINX_USAGE_LOG -u WINX_USAGE_LOG_ROTATION cargo test --all-features --locked {{filter}}

[windows]
test-one filter:
    Remove-Item Env:\WINX_USAGE_LOG,Env:\WINX_USAGE_LOG_ROTATION -ErrorAction SilentlyContinue; cargo test --all-features --locked {{filter}}; exit $LASTEXITCODE

# Opt-in loom concurrency model checks
test-loom:
    cargo test --features loom --lib loom_

# Verify the declared MSRV still compiles
msrv:
    cargo +1.90.0 check --all-features --locked

# Dependency policy and advisories (needs cargo-deny and cargo-audit)
deny:
    cargo deny --all-features check
    cargo audit --deny warnings

# Run one fuzz target for `seconds` (needs nightly and cargo-fuzz), e.g. `just fuzz terminal 120`
fuzz target seconds="60":
    cargo +nightly fuzz run {{target}} -- -max_total_time={{seconds}} -rss_limit_mb=2048 -timeout=20

# Verify the crates.io package and the benchmark build
package:
    cargo package --locked
    cargo bench --bench performance --locked --no-run

# Everything CI enforces on the host, plus the Windows target lint
check: lint windows-lint test msrv

# Release build of the three sibling binaries
build:
    cargo build --release --locked --bins

# Install repo-local git hooks (pre-commit lint, commit-msg conventional commits)
install-hooks:
    git config core.hooksPath .githooks
    @echo "installed git hooks from .githooks"
