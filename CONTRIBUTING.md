# Contributing to Winx Code Agent

Thanks for taking the time to contribute. Winx is a Rust MCP server for local code-agent workflows, so changes should be grounded in real behavior, tested, and careful around filesystem and shell access.

## Code of Conduct

By participating, you agree to follow the [Code of Conduct](CODE_OF_CONDUCT.md).

## Development Setup

Requirements:

- Rust 1.75 or newer
- Cargo
- Git

Setup:

```bash
git clone https://github.com/YOUR_USERNAME/winx-code-agent.git
cd winx-code-agent
git remote add upstream https://github.com/gabrielmaialva33/winx-code-agent.git
cargo check --tests
```

## Project Structure

- `src/server.rs`: MCP server wiring, tool registration, resource handlers.
- `src/tools/`: MCP tool implementations.
- `src/types.rs`: tool schemas and input deserialization.
- `src/state/`: shell, PTY, persistence, and terminal state.
- `src/utils/`: shared file, path, mmap, repo, and command-safety helpers.
- `tests/`: integration and lifecycle tests.
- `.github/workflows/`: CI, release, and publish workflows.

## Local Checks

Run these before opening a pull request:

```bash
cargo fmt --all -- --check
cargo check --tests
cargo clippy --all-targets --all-features
cargo test --all-features
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
- For shell behavior, account for PTY-sensitive tests that may be ignored in CI.

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
