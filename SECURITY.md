# Security Policy

## Supported Versions

Security fixes are provided for the current `0.2.x` line.

| Version   | Supported |
|-----------|-----------|
| `0.2.x`   | Yes       |
| `< 0.2.0` | No        |

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

Winx supports local stdio and a primary network-reachable Streamable HTTP deployment. An authenticated MCP client can
ask it to:

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
- Keep HTTP on loopback; use `--allow-non-loopback` only as an explicit, reviewed exception.
- Store HTTP tokens in regular chmod-600 files, use one principal per remote client, and rotate leaked credentials.
- Prefer bearer headers. Query-string tokens are opt-in because they leak into URL logs and history.
- Keep HTTP concurrency/rate limits and the daemon guardian quota/tiered idle TTL enabled.
- Keep the default `--session-affinity workspace` for stateless clients. Use `thread` only when the client can retain stable
  IDs and operators accept responsibility for abandoned-session cleanup.
- Review redacted configuration with `winx-code-agent doctor`.
- Review durable sessions with `winx-code-agent list`; use `prune`, `kill <thread_id>`, or `kill --all` when cleanup is needed.
- Follow the complete [Streamable HTTP deployment guide](docs/streamable-http.md) before exposing remote access.

## Filesystem Safety

Winx validates workspace paths and tracks read-before-edit state. Security-sensitive changes should preserve:

- workspace path validation (default-on; widened only by the operator-set `WINX_ALLOW_PATHS`, read once at
  startup so no tool argument or agent-run command can relax it at runtime - `WINX_ALLOW_PATHS=/` disables
  containment for the file tools and should be treated as such);
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

## HTTP Authentication and Isolation

HTTP bearer tokens must be at least 32 bytes unless the operator explicitly enables weak local-development tokens.
`--principal-config` assigns each client an independent identity: external thread IDs and MCP Task IDs are scoped before
they reach the shared service registry, preventing accidental or deliberate cross-principal session reuse.

Remote first calls default to one durable session per `(principal, canonical workspace)`. This prevents stateless clients
and cosmetic model-generated ID changes from exhausting the guardian quota. It also means parallel conversations using
the same principal in the same repository intentionally share one shell, cwd, foreground-command lock, and output stream.
Use separate principals or `--session-affinity thread` when that sharing is unacceptable; thread affinity requires stable
client-owned IDs and explicit cleanup.

Authentication is not authorization inside a shell. A principal permitted to use Winx still receives the capabilities of
the selected Winx mode. Use `architect`, a constrained `code_writer`, containers, and OS-level isolation where needed.
Winx does not currently provide built-in TLS, OAuth/OIDC, mTLS, or per-principal tool policy; those controls belong at a
reviewed network edge when required. OAuth/OIDC well-known probes are intentionally ordinary `404 Not Found` responses;
only `/mcp` is protected by Winx bearer authentication.

## Guardian Lifecycle Safety

Protocol `1.3` guardians own their creation, activity, and last-command clocks. Files beside guardian sockets are a
control-plane cache, not the authoritative clock, because runtime directories may be tmpfs and recreated after reboot.
Compatibility logic for protocol `1.2` distinguishes passive metadata reconstruction from real adapter activity and uses
socket age for never-used legacy guardians.

The default retention policy is deliberately tiered:

- never-used guardians: 30 minutes (`WINX_UNUSED_SESSION_IDLE_TTL_SECS`);
- guardians that ran a command: 24 hours (`WINX_SESSION_IDLE_TTL_SECS`);
- active foreground or background commands: never removed by idle pruning.

Under hard quota pressure, Winx may reclaim only the oldest inactive guardian that has never run a command. Used or active
shells are not sacrificed to admit a new session. `prune --idle-seconds 0` is the explicit operator override for removing
all idle sessions while still preserving active commands.

## Dependency and Release Security

Dependency and workflow changes should pass:

```bash
cargo fmt --all -- --check
cargo check --all-features --locked
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-features --locked
cargo deny --all-features check
cargo audit --deny warnings
cargo package --locked
```

CI additionally checks the declared MSRV, real PTY/TUI behavior, fuzz-target compilation, pinned GitHub Actions, and the
release workflow contract. Releases include SHA-256 manifests, a CycloneDX SBOM, and GitHub artifact attestations.

## Disclosure

We follow coordinated disclosure. Public details should be shared after a fix or mitigation is available, unless immediate disclosure is necessary to protect users.

Last updated: August 2026
