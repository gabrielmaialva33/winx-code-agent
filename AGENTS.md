# Repository Guidelines

## Project Structure & Module Organization

Winx is a Rust 2021 MCP server for shell and coding-agent workflows. The binary entry point is `src/main.rs`; library exports live in `src/lib.rs`. `src/server.rs` is the MCP-service facade, with catalog/schema, handler, principal isolation, session, Task, dispatch, and tests under `src/server/`; hardened HTTP transport lives in `src/http_server.rs`, and typed secret/config parsing in `src/config.rs`. Tool facades live under `src/tools/`; `BashCommand` and `FileWriteOrEdit` are split into focused submodules. Terminal and persistence logic belongs under `src/state/`, reusable parsing/path/repository/safety helpers under `src/utils/`, and audited libc calls exclusively under `src/os.rs` (the crate otherwise denies unsafe code). Integration tests live in `tests/`, Criterion benchmarks in `benches/`, cargo-fuzz targets in `fuzz/`, model assets in `assets/`, and pinned CI/release automation in `.github/workflows/`.

## Build, Test, and Development Commands

- `cargo check --all-features --locked` performs a fast compile check with the committed lockfile.
- `WINX_EMBEDDED=1 cargo run --release` starts an in-process development server without sibling daemons.
- `cargo build --release --locked --bins` produces the complete optimized Unix bundle; run `./target/release/winx-code-agent` afterward.
- `cargo fmt --all -- --check` verifies formatting; run `cargo fmt --all` to apply it.
- `cargo clippy --all-targets --all-features --locked -- -D warnings` runs the enforced lint suite.
- `cargo test --all-features --locked` runs the full normal test suite.
- `cargo test --features loom --lib loom_` runs the opt-in concurrency model checks.
- `cargo +1.88.0 check --all-features --locked` verifies the declared MSRV.
- `cargo package --locked` verifies the crates.io package.
- `cargo bench --bench performance --locked --no-run` compiles the benchmark suite.
- `cargo deny --all-features check` and `cargo audit --deny warnings` enforce dependency policy.
- `cargo +nightly fuzz build` compiles all cargo-fuzz targets.

## Coding Style & Naming Conventions

Use standard Rust naming: `snake_case` for modules, functions, and tests; `CamelCase` for types and traits; `SCREAMING_SNAKE_CASE` for constants. `rustfmt.toml` sets a 100-column width and reordered imports/modules. Preserve existing module boundaries, prefer typed errors and structured parsing, and avoid broad `#[allow(...)]` attributes. Comments should be concise, in English, and explain non-obvious behavior.

## Testing Guidelines

Place public-behavior and lifecycle tests in descriptively named `tests/*_test.rs` files; keep narrow unit tests beside their modules. Every bug fix should include a focused regression test. File-edit changes must exercise success and failure atomicity, shell changes must cover real PTY/TUI behavior, authentication changes must prove cross-principal isolation, and parser changes should update a fuzz target. Property-test regressions are tracked in `proptest-regressions/`; measurable hot-path changes should update `benches/performance.rs`. There is no stated numeric coverage target; meaningful behavioral coverage is required.

## Commit & Pull Request Guidelines

Follow the history’s concise Conventional Commit style, such as `feat:`, `fix:`, `test:`, `style:`, `chore:`, and scoped dependency commits like `cargo(deps):`. Keep each commit focused. Pull requests should describe behavior and security impact, link relevant issues, list exact validation commands, and update tests for changed behavior. Explicitly call out changes involving filesystem access, shell execution, mode restrictions, authentication, or persistence.

## Security & Configuration

Never commit tokens or machine-specific secrets. Treat remote HTTP transport as network-reachable command execution, and do not weaken path, command, thread-ID, or sandbox restrictions. Report vulnerabilities through `SECURITY.md`, not public issues.
