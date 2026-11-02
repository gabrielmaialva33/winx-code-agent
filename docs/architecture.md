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
5. `server::tool_dispatch` deserializes a registered tool, lowers `EditFiles` and hidden compatibility aliases into the
   unified mutation domain, and invokes the resulting operation under the correct logical session.
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

Shell recovery is evidence-bound in the adapter. One explicit reset is permitted, then a five-minute live-session
cooldown preserves the replacement PTY unless `BashCommand` returned a typed runtime/infrastructure failure. A redundant
request completes as `reset_skipped_healthy` rather than destroying useful cwd, terminal, or process context. The guard is
intentionally not persisted: losing adapter state is itself a new recovery boundary, and guardian attach remains
observational.

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

Mutations follow one typed plan/commit boundary. The public `EditFiles` wire and hidden legacy compatibility aliases all
lower into `tools::edit_files`; there is one source of truth for cardinality, mode authorization, canonical targets,
receipt identity, and commit reporting. Planning
validates path policy, mode policy, read coverage, expected revision, and edit syntax while computing the new bytes in
memory. Commit revalidates both the canonical path binding and original bytes immediately before an atomic replacement.
A failed plan writes nothing. A commit-stage failure reports the committed prefix and untouched suffix rather than
claiming cross-file rollback after a write already became durable.

`EditFiles` mode `line_patch` is the preferred compare-and-swap path when the model already has line coordinates. All
patches refer to one original revision, are ordered and non-overlapping, and may touch only visible ranges. A stale replay returns an
exact `ReadFiles` recovery action and cannot mutate the newer file. SEARCH/REPLACE remains available for text-anchored
work and invalidates its read permit after a conflict. Because `ReadFiles` already returns both coordinates and a
revision, `line_patch` is the default existing-file path. SEARCH conflict recovery performs the exact structured read
and switches the corrected retry to `line_patch` instead of escaping to an untracked shell edit.

Successful mutations receive bounded persisted receipts. Identical concurrent calls are single-flight, and a lost
response may be replayed without repeating the write only while target postconditions still match. Verification is a
separate receipt-bound operation: a failed check never tells the model to repeat a committed edit.

Workspace-local derived helpers live only in `.winx/tmp/session-…`. Hard byte/file limits remain fail-closed. A separate
high-water policy controls long-lived active sessions: at 75% of either session limit, Winx may unlink only managed
artifacts whose metadata has been inactive for the full 24-hour TTL, oldest first, until 50% headroom is restored. The
bounded walk never follows symlinks; an unreadable or excessive tree is retained. Fresh helpers and canonical project
files are never candidates, and any remaining hard-limit overage still requires explicit cleanup.

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
- `src/tools/`: model-facing capabilities, compatibility adapters, and shared domain engines; all file mutations converge in `tools::edit_files`,
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
cargo +1.90.0 check --all-features --locked
cargo package --locked
cargo deny --all-features check
cargo audit --deny warnings
```

Changes to parser inputs should update fuzz targets. Hot-path claims should compile and, when meaningful, update the
Criterion benchmarks. File changes need success and failure-atomicity coverage; daemon changes need multi-adapter and
cancellation races; MCP changes need schema plus HTTP integration tests.

## Optional Eulix call graph navigation

`CodeMap` retains its existing `outline` and `references` implementations. On Linux,
`callers` and `callees` query direct resolved static call edges; `impact` follows reverse
edges to at most depth 8, 10,000 visited symbols and 1,000 results. The default result
limit is 100. `name` accepts an exact short name or full Eulix ID. Ambiguous names and
conditional-compilation variants include all matching definitions. `path` selects the
starting definitions (a file or directory inside the initialized workspace); neighbors
can live elsewhere in that workspace. Derived helpers and allowlisted external roots
use the structured `references` fallback. Exact source remains canonical; Eulix is an
approximate resolver, not a compiler or a rename/absence oracle.

Build/install the user's compatible Eulix fork separately:

```sh
cd /path/to/eulix
cargo install --path eulix-parser --locked
# Set this in the Winx server's operator environment, then restart that server:
export WINX_EULIX_PARSER=/absolute/path/to/eulix_parser
```

The backend is opt-in: only an absolute `WINX_EULIX_PARSER` enables it; a parser that
is merely on `PATH` is ignored, so installing Eulix never starts background parsing by
itself. Client parameters and
workspace configuration cannot select an executable; an executable resolving inside
the workspace is refused. Missing binaries return `index_status: unavailable` and a
concrete `next_action` invoking `CodeMap` with `operation: references`. Non-Linux hosts
return the same fallback with `index_platform_unsupported`; syntax navigation remains
portable.

Every response reports `index_status` (`fresh`, `stale`, `building`, or `unavailable`),
`index_age_seconds` (null before the first complete build), `snapshot_hash`,
`parser_version`, and `index_error` when applicable. `graph_hits` carries definition
file/line/end-line plus caller-owned `call_site_file` / `call_site_line` and traversal
depth. A building response may carry the previous generation. Freshness is checked in
the background at most every five seconds; it reflects the last completed check, not a
transactional snapshot of files being edited. Poll a graph operation after the build
finishes, or use the supplied references action in the meantime.

Winx spawns the parser directly without a shell, clears its inherited environment,
closes stdin and discards stdout/stderr. It probes `--version` with a two-second limit;
parsing has a 120-second limit, four Rayon workers, and a four-CPU child affinity mask
that also bounds the current fork's independent discovery pool. Only one parser build
runs per Winx process. A directory advisory lock prevents duplicate builds of the same
workspace across processes. Requests schedule the work and return the available
cached generation; they never wait for a parser. Rebuild attempts are debounced for
two seconds. A changed source snapshot during parsing prevents publication. Merge or
rebase markers defer rebuilding.

This backend executes an operator-trusted program and writes private artifacts under
`.winx/index/eulix/`, including in read-only navigation modes. Ignore `.winx/` in Git.
Linux directory-relative, no-follow opens pin storage against symlink replacement;
a complete checksummed generation is atomically renamed to `index.wcg`. Parser KB,
sidecars and spill files live in a private per-build directory and are removed after
success or failure. Existing path/session/tool policies still apply. The parser's
current CLI disables gitignore; Winx explicitly excludes `.winx` and `.git`, and filters
returned provenance through its own current gitignore-aware workspace inventory.
The trusted parser may read additional files inside the workspace; do not configure an
untrusted executable. The backend does not provide a new OS sandbox for that program.

Conversion streams the KB and graph sidecar with Serde visitors, discarding unused AST
fields instead of building a JSON DOM. The Winx format has interned strings, u32 symbol
IDs, file ranges, name lookups and bidirectional CSR adjacency. Its safe decoder checks
version, checksum, lengths, IDs, paths, sorted rows and reverse-edge consistency.
Limits are 512 MiB per JSON artifact, 128 MiB compact files, 64 MiB interned strings,
1,000,000 symbols and 4,000,000 edges. The per-process decoded cache uses an estimated
64 MiB budget and at most 16 workspace entries, keyed by canonical workspace plus
file size/mtime and format version. Freshness includes Git HEAD, dirty/untracked file
content hashes and parser identity; non-Git directories use file size/mtime. Git dirty
hashing is capped at 20,000 files / 256 MiB. Oversize, corrupt, busy, or unsafe input
returns a structured reason and fallback. Limits are refusals, not silent partial
indexes. The batch CLI has no incremental call-graph mode; refreshes currently rebuild
the full graph. See `examples/call_graph_metrics.rs` and the `call_graph` Criterion group
for reproducible conversion/load/query measurements.

The backend inherits any Landlock domain already applied to the Winx process; it
never disables it or adds filesystem grants. Parser and Git subprocesses get their
own process group; timeout handling kills and reaps the leader and signals the whole
group. Graph query workers have a 75 ms observation deadline and return a structured
references fallback when that budget is exhausted.
