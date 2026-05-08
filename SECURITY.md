# Security Policy

## Supported Versions

Security fixes are provided for the current `0.2.x` line.

| Version | Supported |
| --- | --- |
| `0.2.x` | Yes |
| `< 0.2.0` | No |

## Reporting a Vulnerability

Do not report security vulnerabilities through public issues, discussions, or pull requests.

Preferred reporting path:

1. Use GitHub private vulnerability reporting for this repository, if available.
2. If private reporting is unavailable, contact the maintainer privately through GitHub.

Include:

- affected version or commit;
- operating system and MCP client;
- exact reproduction steps;
- expected and actual impact;
- proof of concept, logs, or screenshots when useful;
- whether the issue involves command execution, path traversal, symlinks, persistence, or file edits.

We aim to acknowledge reports within 48 hours and provide an assessment within 7 days. Fix timing depends on severity and complexity.

## Threat Model

Winx is a local MCP server. A connected MCP client can ask it to:

- read files;
- write or edit files;
- execute shell commands;
- interact with foreground and background PTY sessions;
- persist and reload shell state;
- save context snapshots.

This is powerful by design. Only connect Winx to MCP clients and agent workflows you trust.

## Operational Guidance

- Run Winx in the smallest workspace that is practical for the task.
- Prefer `architect` mode for read-oriented work.
- Use `code_writer` mode with explicit `allowed_globs` and `allowed_commands` for constrained write sessions.
- Review tool calls from untrusted or experimental agents.
- Keep secrets out of project files that agents can read.
- Avoid running Winx with elevated privileges.
- Treat shell access as equivalent to local user access.

## Filesystem Safety

Winx validates workspace paths and tracks read-before-edit state. Security-sensitive changes should preserve:

- workspace path validation;
- symlink and path traversal protections;
- read-before-edit enforcement;
- hash/range based overwrite tracking;
- mode checks for file writes and edits.

## Shell Safety

`BashCommand` runs local commands. Security-sensitive changes should preserve:

- thread-id validation;
- one foreground command at a time;
- mode checks for allowed commands;
- background command identifiers;
- clear behavior for interrupts and input forwarding.

## Dependency Security

Use standard Rust tooling and GitHub dependency alerts when reviewing dependency changes. Dependency updates should pass:

```bash
cargo fmt --all -- --check
cargo check --tests
cargo clippy --all-targets --all-features
cargo test --all-features
```

## Disclosure

We follow coordinated disclosure. Public details should be shared after a fix or mitigation is available, unless immediate disclosure is necessary to protect users.

Last updated: May 2026
