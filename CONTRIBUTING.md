# Contributing to Winx Code Agent

Thanks for taking the time to contribute. Winx is a Rust MCP server for local code-agent workflows, so changes should be grounded in real behavior, tested, and careful around filesystem and shell access.

## Code of Conduct

By participating, you agree to follow the [Code of Conduct](CODE_OF_CONDUCT.md).

## Development Setup

Requirements:

- Rust 1.88 or newer
- Cargo
- Git

Setup:

```bash
git clone https://github.com/YOUR_USERNAME/winx-code-agent.git
cd winx-code-agent
git remote add upstream https://github.com/gabrielmaialva33/winx-code-agent.git
cargo check --all-features --locked
```

## Project Structure

- `src/server.rs`: thin MCP-service facade; catalog, handler, principal isolation, session, Task, dispatch, and tests live in `src/server/`.
- `src/tools/`: MCP tool facades; `BashCommand` and `FileWriteOrEdit` internals are split into focused submodules.
- `src/config.rs`: typed environment/secret/principal loading.
- `src/os.rs`: the narrowly audited libc boundary; the rest of the crate denies unsafe code.
- `src/state/`: shell, PTY, persistence, and terminal state.
- `src/utils/`: shared file, path, mmap, repo, and command-safety helpers.
- `tests/`, `benches/`, `fuzz/`: integration tests, Criterion benchmarks, and cargo-fuzz targets.
- `.github/workflows/`: pinned CI and release automation.

## Local Checks

Run these before opening a pull request:

```bash
cargo fmt --all -- --check
cargo check --all-features --locked
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-features --locked
cargo +1.88.0 check --all-features --locked
cargo package --locked
cargo bench --bench performance --locked --no-run
cargo deny --all-features check
cargo audit --deny warnings
cargo +nightly fuzz build
```

Use `cargo fmt --all` to format changes.

## Code Guidelines

- Follow existing Rust module boundaries and naming.
- Keep tool behavior explicit and covered by tests.
- Prefer structured parsing and typed errors over ad hoc string handling.
- Do not weaken filesystem, thread-id, mode, or command restrictions.
- Avoid broad `#[allow(...)]` additions. Refactor instead when practical.
- Keep comments in English and only add them when they clarify non-obvious logic.

## Testing Guidance

- Add focused tests for bug fixes.
- Add integration tests when changing tool behavior or MCP-facing schemas.
- For file edits, verify both success behavior and failure atomicity.
- For shell behavior, run the real PTY/TUI tests serially; CI has a dedicated Linux job for them.
- Add a benchmark when changing a claimed hot path, and add or update a fuzz target for parsers exposed to arbitrary input.
- Authentication changes must test cross-principal thread and Task isolation.

## Commit Style

Use short, descriptive commit subjects. Conventional prefixes are welcome:

- `feat:` new behavior
- `fix:` bug fix
- `docs:` documentation only
- `test:` test changes
- `refactor:` internal restructuring
- `chore:` maintenance

## Pull Requests

Before opening a PR:

- Rebase or merge the latest `main`.
- Keep the change focused.
- Explain behavior changes and security impact.
- Include the exact commands you ran.
- Link related issues when applicable.

Security-sensitive changes should call out filesystem access, shell execution, mode restrictions, and persistence behavior explicitly.

## Reporting Issues

Use the GitHub issue templates for bugs and feature requests. Do not file public issues for security vulnerabilities; follow [SECURITY.md](SECURITY.md) instead.
