# Repository Guidelines

## Project Structure & Module Organization

Winx is a Rust 2021 MCP server for shell and coding-agent workflows. The binary entry point is `src/main.rs`; library exports live in `src/lib.rs`. MCP registration and transports are organized in `src/server.rs` and `src/http_server.rs`. Add tool implementations under `src/tools/`, terminal and persistence logic under `src/state/`, and reusable parsing, path, repository, and safety helpers under `src/utils/`. Integration and lifecycle coverage belongs in `tests/`. Tokenizer and path-ranking model data lives in `assets/`; CI and release automation lives in `.github/workflows/`.

## Build, Test, and Development Commands

- `cargo check --tests` performs a fast compile check for library and test code.
- `cargo run --release` starts the MCP server locally without installing it.
- `cargo build --release --locked` produces the optimized binary using the committed lockfile.
- `cargo fmt --all -- --check` verifies formatting; run `cargo fmt --all` to apply it.
- `cargo clippy --all-targets --all-features` runs the configured lint suite.
- `cargo test --all-features` runs the full normal test suite.
- `cargo test --features loom --lib loom_` runs the opt-in concurrency model checks.

## Coding Style & Naming Conventions

Use standard Rust naming: `snake_case` for modules, functions, and tests; `CamelCase` for types and traits; `SCREAMING_SNAKE_CASE` for constants. `rustfmt.toml` sets a 100-column width and reordered imports/modules. Preserve existing module boundaries, prefer typed errors and structured parsing, and avoid broad `#[allow(...)]` attributes. Comments should be concise, in English, and explain non-obvious behavior.

## Testing Guidelines

Place public-behavior and lifecycle tests in descriptively named `tests/*_test.rs` files; keep narrow unit tests beside their modules. Every bug fix should include a focused regression test. File-edit changes must exercise success and failure atomicity, while shell changes should account for PTY-sensitive behavior. Property-test regressions are tracked in `proptest-regressions/`. There is no stated numeric coverage target; meaningful behavioral coverage is required.

## Commit & Pull Request Guidelines

Follow the history’s concise Conventional Commit style, such as `feat:`, `fix:`, `test:`, `style:`, `chore:`, and scoped dependency commits like `cargo(deps):`. Keep each commit focused. Pull requests should describe behavior and security impact, link relevant issues, list exact validation commands, and update tests for changed behavior. Explicitly call out changes involving filesystem access, shell execution, mode restrictions, authentication, or persistence.

## Security & Configuration

Never commit tokens or machine-specific secrets. Treat remote HTTP transport as network-reachable command execution, and do not weaken path, command, thread-ID, or sandbox restrictions. Report vulnerabilities through `SECURITY.md`, not public issues.
