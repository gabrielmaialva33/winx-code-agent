# Architecture and invariants

Winx is a long-lived command-execution service, not a stateless collection of MCP functions. Its architecture keeps
transport, project identity, shell ownership, file evidence, and model-facing recovery separate so each boundary can
fail closed without losing useful sessions.

## Process model

On Unix, three executable roles cooperate:

```text
MCP client
    |
    v
winx-code-agent  adapter: HTTP/stdio, auth, schemas, session routing
    |
    v
winxd            control plane: quotas, discovery, guardian lifecycle
    |
    v
winx-guardian    one durable PTY owner per logical project session
```

The adapter may restart without killing a guardian. The control plane may perform a planned restart while existing
guardians keep their PTYs. Every negotiated process reports a typed role and an exact build identity; capability checks
come from the effective guardian, never from the newer adapter or control plane by association.

All launch paths resolve the control socket through `daemon::socket`. An explicit `WINX_SOCKET` wins, then a private
`XDG_RUNTIME_DIR`, then `/run/user/<uid>`, and only then a private `/tmp` fallback. `doctor` inspects every candidate and
reports split-brain sockets, incompatible control builds, and mixed guardian builds without exposing credentials.

## MCP request path

The server path is deliberately layered:

1. `http_server` applies network limits, host validation, authentication, readiness, and request correlation.
2. `server::principal` scopes caller-controlled thread and Task identifiers to the authenticated principal.
3. `server::coherence` verifies the immutable conversation/workspace binding. Allowed external paths do not change the
   project identity, even with `WINX_ALLOW_PATHS=/`.
4. `server::handler` owns protocol negotiation, Task routing, single-flight mutation coordination, and telemetry.
5. `server::tool_dispatch` deserializes a registered tool, lowers every edit facade into the unified mutation domain,
   and invokes the resulting operation under the correct logical session.
6. `server::outcomes` converts typed domain results into the shared MCP orchestration envelope.

`tool_registry::ToolKind` is the stable source of truth for tool names, discovery order, policy bits, annotations,
session contract, output contract, and exhaustive dispatch. Additions are appended so persisted policy bits retain their
meaning. Profiles in `tool_policy` are derived from those typed identifiers rather than parallel strings.

## Session and project coherence

`thread_id` and `workspace_root` form one project-session identity. A remote conversation cannot silently rebind that
identity to another project. A tool may intentionally access an allowed absolute path outside the root, but doing so
does not mutate the binding or make a different project's PTY current.

The adapter registry is bounded and pins in-flight sessions against eviction. The guardian owns the authoritative PTY,
cwd, command generation, output journal, activity clock, and runtime state. Model-controlled terminal text is never
parsed to infer whether a command is running, awaiting input, or completed.

Task cancellation is bound to that exact runtime identity. A cancellation that races a launch waits for either the
published generation token or proof that launch finished without a process. If the handshake cannot settle within its
bounded deadline, Winx terminates the affected session before acknowledging cancellation. It never lets a stale,
generation-less interrupt escape and target the next command.

## File evidence and mutation protocol

File reads use bounded, stable snapshots through safe standard-library APIs. A descriptor is read, metadata is checked
again, and a concurrent replacement retries from a fresh descriptor. There is no memory mapping or shared mutable view
of a live file.

`ReadFiles` returns both visible text and a machine-readable receipt for every successful file:

- canonical path;
- opaque SHA-256 revision;
- total line count;
- exact complete line ranges visible to the model;
- truncation and continuation metadata.

Token truncation retreats to a complete newline. Only complete visible lines become edit evidence, so a large-file read
cannot accidentally authorize an unseen overwrite.

Mutations follow one typed plan/commit boundary. The stable public facades (`FileWriteOrEdit`, `MultiFileEdit`,
`ApplyPatch`, and `UndoEdit`) and the unadvertised migration wire all lower into `tools::edit_files`; there is one source
of truth for cardinality, mode authorization, canonical targets, receipt identity, and commit reporting. Planning
validates path policy, mode policy, read coverage, expected revision, and edit syntax while computing the new bytes in
memory. Commit revalidates both the canonical path binding and original bytes immediately before an atomic replacement.
A failed plan writes nothing. A commit-stage failure reports the committed prefix and untouched suffix rather than
claiming cross-file rollback after a write already became durable.

`ApplyPatch` is the preferred compare-and-swap path when the model already has line coordinates. All patches refer to
one original revision, are ordered and non-overlapping, and may touch only visible ranges. A stale replay returns an
exact `ReadFiles` recovery action and cannot mutate the newer file. SEARCH/REPLACE remains available for text-anchored
work and invalidates its read permit after a conflict.

Successful mutations receive bounded persisted receipts. Identical concurrent calls are single-flight, and a lost
response may be replayed without repeating the write only while target postconditions still match. Verification is a
separate receipt-bound operation: a failed check never tells the model to repeat a committed edit.

## Errors and agent recovery

Domain code returns typed `WinxError` variants carrying paths, reasons, ranges, and revisions. `server::recovery`
classifies those variants directly; it never searches human-readable error strings. Expected correction flows are MCP
tool results with `isError: true`, `retrySameCall: false`, and an exact `nextAction` when one is safe to infer. Only
serialization, poisoned state, and comparable server failures become JSON-RPC internal errors.

This contract is part of the agent interface. Descriptions lead with the recovery rules, schemas avoid client-rejected
constructs, and trace fixtures assert that retries, fresh reads, committed edits, and workspace rejection remain safe.

## Observability and deployment

Operational logs and privacy-safe usage events are separate. Usage telemetry records fixed command categories, hashes
for correlation, result/recovery metadata, latency, response size, workspace coherence, and exact build identity; it
never records shell text, file contents, arguments, tokens, or raw credentials.

`winx-code-agent report` aggregates rotated JSONL offline, correlates tool and HTTP events, summarizes latency/bytes, and
audits retry invariants. Usage events carry a schema version so older logs with missing recovery fields are reported as
unverifiable instead of becoming false violations. `healthz` reports liveness; `readyz` turns unavailable during graceful drain. HTTP shutdown
stops admission before waiting for active work.

The versioned installer builds all three binaries together into an immutable bundle, smoke-tests the adapter, then
atomically switches symlinks. Existing processes retain their old inode; new launches cannot observe a partially
installed trio. The maximally optimized `dist` profile is reserved for published/versioned artifacts so normal release
development remains responsive.

## Module ownership

- `src/server/`: MCP lifecycle, catalog, policies at the request boundary, session routing, recovery, mutations, Tasks,
  usage events, and response shaping.
- `src/tools/`: model-facing facades plus shared domain engines; all file mutations converge in `tools::edit_files`,
  while larger capabilities keep focused parser/planner/commit/report submodules.
- `src/state/`: PTY, terminal rendering, persistent shell state, bounded journals, and edit/read evidence.
- `src/daemon/`: wire protocol, control plane, guardian client/server, lifecycle, and socket resolution.
- `src/runtime/`: one runtime abstraction over embedded and daemon-backed shell execution.
- `src/utils/`: safe snapshots and reusable path, repository, parsing, redaction, and policy helpers.
- `src/os.rs`: the only audited libc/unsafe boundary; the rest of the crate denies unsafe code.

Avoid moving policy into generic utilities or transport details into tools. A module should own one state transition and
return typed data to the next layer.

## Validation expectations

The normal gate is:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-features --locked
cargo test --features loom --lib loom_
cargo +1.88.0 check --all-features --locked
cargo package --locked
cargo deny --all-features check
cargo audit --deny warnings
```

Changes to parser inputs should update fuzz targets. Hot-path claims should compile and, when meaningful, update the
Criterion benchmarks. File changes need success and failure-atomicity coverage; daemon changes need multi-adapter and
cancellation races; MCP changes need schema plus HTTP integration tests.
