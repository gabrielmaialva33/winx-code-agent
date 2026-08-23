# Streamable HTTP deployment

<p align="right">
  <b>English</b> • <a href="streamable-http.pt.md">Português</a> • <a href="streamable-http.zh.md">中文</a>
</p>

Winx exposes a configurable MCP toolset through an authenticated **Streamable HTTP** endpoint for ChatGPT, hosted
agents, remote automation, and clients that cannot launch a local stdio process. The endpoint is `/mcp`; the default
listener is `127.0.0.1:8000`, and the default tool profile remains `full`.

The endpoint grants real shell and filesystem capabilities. Winx therefore fails closed: it requires a strong credential,
refuses non-loopback binding unless explicitly acknowledged, bounds request cost, and isolates authenticated principals.

## At a glance

| Property                 | Default                         |
|--------------------------|---------------------------------|
| MCP endpoint             | `/mcp`                          |
| MCP protocol             | `2026-07-28`                    |
| Listener                 | `127.0.0.1:8000`                |
| Authentication           | `Authorization: Bearer <token>` |
| Minimum token length     | 32 bytes                        |
| Remote session affinity  | `workspace`                     |
| Request body limit       | 64 MiB                          |
| Request timeout          | 120 seconds                     |
| Global concurrency       | 32 requests                     |
| Per-source-IP rate limit | 120 requests per minute         |
| Invalid-auth delay       | 100 ms                          |
| Never-used guardian TTL  | 1,800 seconds (30 minutes)      |
| Used guardian TTL        | 86,400 seconds (24 hours)       |
| Live guardian quota      | 32                              |

Winx supports modern stateless MCP calls and the legacy HTTP initialization/session flow. The same tools, prompts,
resources, structured content, and optional MCP Tasks are available over HTTP and stdio.

## Architecture

```text
Remote MCP client
       │ HTTPS + bearer token
       ▼
Private tunnel / VPN / authenticated HTTPS reverse proxy
       │ loopback HTTP
       ▼
127.0.0.1:8000/mcp
       │
       ├─ Host and body validation
       ├─ Timeout, rate, and concurrency limits
       ├─ Principal authentication
       ├─ Session-affinity resolution
       └─ thread_id / MCP Task scoping
              │
              ▼
        shared WinxService
              │
              ▼
            winxd
              │
              └─ winx-guardian per logical session
                         │
                         └─ real PTY / bash or zsh / foreground and background work
```

On Linux, macOS, and WSL2, `winx-code-agent` is only the MCP adapter. `winxd` owns the control plane, while each
`winx-guardian` owns one PTY. Dropping an HTTP connection or restarting the adapter does not terminate that PTY.

Native Windows uses the embedded runtime, so sessions last only as long as the server process. WSL2 is recommended when
durable remote sessions are required.

## Quick start

Install the three Unix binaries:

```bash
cargo install winx-code-agent
```

Create a strong token in a user-only file:

```bash
mkdir -p ~/.config
install -m 600 /dev/null ~/.config/winx-http-token
openssl rand -hex 32 > ~/.config/winx-http-token
```

Start Winx on loopback:

```bash
winx-code-agent serve --http \
  --bind 127.0.0.1:8000 \
  --token-file ~/.config/winx-http-token
```

Configure the client with:

```text
URL: http://127.0.0.1:8000/mcp
Authorization: Bearer <contents of ~/.config/winx-http-token>
```

Cloud clients need a reachable HTTPS URL. Keep Winx on loopback and put a private tunnel, VPN, or authenticated HTTPS
reverse proxy in front. Winx does not terminate TLS itself.

`--token-file` is preferred over `--token`: command-line secrets may appear in process listings, shell history, and
automation logs. `WINX_HTTP_TOKEN` remains available as a single-principal environment fallback.

## Workspace session coherence

Remote `Initialize` returns two values that form one session binding:

```text
thread_id + workspace_root
```

Copy both values unchanged into every later stateful Winx call. Before selecting a PTY or performing any tool operation,
Winx verifies that the thread affinity, supplied canonical root, and initialized session agree. Missing or mixed bindings
return a structured `needs_initialize`/`conflict` tool result and do not reach the shell or filesystem.

This check does **not** confine tool targets to `workspace_root`. It identifies which project context owns the terminal,
cwd, read history, and edit state. Path authority remains a separate policy: for example, `WINX_ALLOW_PATHS=/` still lets
a coherent session read or edit supporting paths outside the project when its active mode permits. This separation keeps
real monorepo/cross-directory work possible without allowing a chat to silently inherit another project's terminal.

For a different project, call `Initialize` with `type="first_call"` and the new path, then use the returned pair. Remote
`user_asked_change_workspace` calls fail closed so a durable key is never repointed to a different project in place.

## Session affinity

### Workspace affinity (default)

The default is:

```bash
--session-affinity workspace
```

For each remote `Initialize(first_call)`, Winx derives the logical session from:

```text
(authenticated principal, canonical workspace)
```

The client-provided first-call `thread_id` is not trusted as the durable key. Variants such as:

```text
release_02333
release_0_2_333
```

resolve to the same internal guardian when they refer to the same principal and workspace. Winx returns a stable external
ID such as `ws_project_<hash>` and expects later tool calls to use that returned value.

Consequences:

- stateless reconnects attach to the existing session instead of creating another guardian;
- repeated first calls preserve the PTY, cwd, output journal, and running command, and return a compact response instead
  of regenerating unchanged guidelines, repository context, and orchestration instructions;
- two different principals still receive different namespaces;
- parallel conversations from the **same principal in the same workspace share one shell** and its foreground-command
  lock;
- first calls without a workspace share one scratch-session key per principal;
- task resumption without a workspace is keyed by the saved task ID.

Workspace affinity is the recommended mode for ChatGPT and other clients whose generated IDs are not guaranteed to be
stable.

### Conversation affinity (parallel web conversations)

Use:

```bash
--session-affinity conversation
```

when parallel conversations from one principal must work in the same repository without sharing a shell. Winx derives the
key from:

```text
(authenticated principal, conversation identity, canonical workspace)
```

Identity preference is:

1. `Mcp-Session-Id`, when the transport has a stable MCP session;
2. `X-Winx-Conversation-Id`, when a reviewed gateway injects a stable opaque value;
3. the supplied first-call `thread_id`, for modern stateless clients;
4. workspace affinity, when no conversation identity exists.

The identity is hashed into an external ID such as `cv_project_<hash>` and is never written raw to tool results or usage
logs. Repeating `Initialize(first_call)` with the same conversation identity reattaches even if the supplied model-generated
ID changes; another conversation identity gets a distinct guardian, cwd, command lock, and output journal.

The gateway header is a trust input. Only inject it at an authenticated edge, strip untrusted client copies, and keep its
value stable for the lifetime of the conversation.

### Thread affinity (explicit opt-out)

Use:

```bash
--session-affinity thread
```

when parallel conversations in one repository must own separate shells. In this mode the normalized external `thread_id`
is the durable key. The client is responsible for:

- generating a stable ID;
- reusing it after reconnects;
- avoiding cosmetic variants;
- pruning or killing abandoned sessions.

An empty first-call ID is still generated, so thread affinity should not be used with a client that cannot retain the
returned ID.

## Attach-or-create

Protocol `1.3` guardians implement attach-or-create for `FirstCall`:

1. a missing logical session creates a new PTY;
2. an existing logical session returns its authoritative snapshot;
3. the adapter updates its local state from that snapshot;
4. the guardian keeps its original PTY process, cwd, mode, journal, cursors, and running commands.

A deliberate shell replacement still uses reset, and mode changes remain explicit. A remote project change uses a new
`FirstCall` binding instead of mutating the old workspace identity. Repeating `FirstCall` is no longer an implicit reset.

The same adapter also refreshes an existing guardian through a non-destructive mode transition. This keeps local and
embedded runtimes from resetting a session on duplicate first calls.

## Multiple authenticated principals

Use one credential per client or automation:

```bash
mkdir -p ~/.config
install -m 600 /dev/null ~/.config/winx-chatgpt-token
install -m 600 /dev/null ~/.config/winx-automation-token
openssl rand -hex 32 > ~/.config/winx-chatgpt-token
openssl rand -hex 32 > ~/.config/winx-automation-token
```

Create a TOML file:

```toml
# ~/.config/winx-principals.toml
[[principals]]
name = "chatgpt"
token_file = "/home/alice/.config/winx-chatgpt-token"
tool_profile = "coding"

[[principals]]
name = "automation"
token_file = "/home/alice/.config/winx-automation-token"
tool_profile = "terminal"

[[principals]]
name = "ci"
token_env = "WINX_CI_MCP_TOKEN"
allowed_tools = ["Initialize", "BashCommand", "ReadFiles"]
```

Then start:

```bash
chmod 600 ~/.config/winx-principals.toml
winx-code-agent serve --http \
  --principal-config ~/.config/winx-principals.toml
```

Principal rules:

- names may contain ASCII letters, digits, `_`, and `-`;
- each entry sets exactly one of `token_file` or `token_env`;
- names, derived IDs, and tokens must be unique;
- token files must be regular non-symlink files;
- Unix token-file permissions must exclude group and other access;
- tokens must contain at least 32 bytes unless `--allow-weak-token` is explicitly enabled for local testing.
- `tool_profile` defaults to `full`; `allowed_tools`, when present, is an exact replacement and cannot be empty;
- allowlist names are case-sensitive and unknown tools prevent startup.

Thread IDs and MCP Task IDs are scoped before they reach the shared service. Normal results, structured content, Task
results, and errors are translated back before leaving the server. A principal cannot get, update, or cancel another
principal's Task.

### Tool catalog profiles

Profiles reduce the `tools/list` schema payload for clients that do not need every capability. Winx also checks the same
policy before dispatch, so a client cannot call a hidden tool by name.

| Profile     | Advertised tools                                                                                      |
|-------------|-------------------------------------------------------------------------------------------------------|
| `full`      | All nine tools (backward-compatible default)                                                          |
| `coding`    | `Initialize`, `BashCommand`, `ReadFiles`, both edit tools, `UndoEdit`, and `CodeMap`                  |
| `read-only` | `Initialize`, `ReadFiles`, `ReadImage`, and `CodeMap`                                                 |
| `terminal`  | `Initialize` and `BashCommand`                                                                        |

For a single-token server, select a profile on the command line:

```bash
winx-code-agent serve --http --token-file ~/.config/winx-http-token \
  --tool-profile coding
```

Or construct an exact catalog by repeating `--allow-tool`; explicit names replace `--tool-profile`:

```bash
winx-code-agent serve --http --token-file ~/.config/winx-http-token \
  --allow-tool Initialize --allow-tool BashCommand --allow-tool ReadFiles
```

Catalog policy is not a shell sandbox. Any profile containing `BashCommand` still grants the command capabilities of the
initialized Winx mode and operating-system user.

## LLM orchestration contract

The MCP handshake leads with a deterministic sequencing contract: initialize once, preserve the returned `thread_id`, use
`CodeMap` before broad reads, batch `ReadFiles`, read before editing, compose related fail-fast checks with `&&`, and never
repeat a rejected call unchanged. `Initialize` also returns a bounded `<workspace_root>/.winx/tmp/session-…/` directory
for independently useful derived helpers. Those helpers remain non-canonical, preserve source-path/line provenance, and
never encode payload in filesystem names or pollute the project root with `.winx-*`/`.winx_tmp` artifacts. A finite post-edit
check can be supplied as `verify_command` on either edit tool, saving one network/model round trip. Extra
`WINX_SERVER_INSTRUCTIONS` are appended after those stable rules and are also included in the `Initialize` response for
clients that do not expose handshake instructions to the model.

Every tool advertises an `outputSchema` and returns a common `structuredContent` envelope. Existing text and image content
remain unchanged for older clients. The main fields are:

```json
{
  "status": "needs_read",
  "tool": "FileWriteOrEdit",
  "message": "FileWriteOrEdit failed: ...",
  "errorCode": "read_required",
  "retryable": true,
  "retrySameCall": false,
  "nextAction": {
    "tool": "ReadFiles",
    "instruction": "Perform every required read before retrying the edit.",
    "arguments": {
      "file_paths": ["/workspace/README.md:231-301"],
      "thread_id": "ws_project_hash"
    }
  },
  "requiredReads": [
    { "path": "/workspace/README.md", "ranges": ["231-301"] }
  ]
}
```

Recoverable execution failures use HTTP/JSON-RPC success with MCP `isError: true`; malformed requests and failures that
prevent a valid tool result from being serialized remain JSON-RPC errors. `BashCommand` reports `running` with
`retryAfterMs` and a `status_check` next action, while interactive turns report `awaiting_input` or `awaiting_approval`.
The shell runtime produces a separate `BashCommandState` containing process status, cwd, exit code, background ID, elapsed
time, and optional interactive turn state. The MCP adapter and MCP Tasks consume only that typed value; rendered output,
child output, and unescaped-looking command metadata are presentation data and cannot spoof orchestration.
`BashCommand.wait_policy` selects generic execution behavior: `adaptive` (default) keeps short calls inline and promotes
an already-running foreground command only when Tasks and generation-bound runtime actions were negotiated; `until_complete` is accepted only for a foreground Command and creates a Task immediately or
uses a 60-second bounded synchronous fallback; `return_early` always stays inline and caps `wait_for_seconds` at 5 seconds. `wait_for_seconds` is bounded by the chosen policy. Task routing never depends on client
identity. A client that advertises the `io.winx/compact-bash-output` extension receives the runtime's trailer-free payload;
without that explicit capability, the historical text output is unchanged.

`FileWriteOrEdit` and `MultiFileEdit` accept optional `verify_command` and `verify_wait_for_seconds` (default `15`, maximum
`60`). Verification runs only after a successful commit, as a foreground command under the same mode allowlist as
`BashCommand`. Exit code zero completes the combined result. A non-zero exit returns `isError: true` with
`errorCode: verification_failed` and `data.edit_applied: true`; Winx never claims the edit was rolled back. A check still
running at the bounded wait returns the normal `BashCommand` `status_check` next action. A principal must permit both the
edit tool and `BashCommand` to use this option.

Generation-bound routing is negotiated with the effective per-session guardian, not inferred from the control daemon's
version. The adapter keeps that guardian negotiation on an epoch-bound session channel, so ordinary daemon calls add no
repeated control hello round trips; generation-bound actions perform a final guardian check before relay. A closed channel forces renegotiation before another Task-bound launch.

A `ReadFiles` batch containing one or more failed paths returns `isError: true`; content from successful paths remains in the
same response, with `successful_files` and `failed_files` counts. `MultiFileEdit` preserves planning failure semantics:
unread and stale files return `needs_read`, while missing or ambiguous SEARCH blocks return `conflict`, all without writing
any file and with a concrete `ReadFiles` recovery action. Batched reads run filesystem and tokenization work in a bounded
parallel pool (`WINX_READ_PARALLELISM`, default `4`, maximum `32`) but publish results and read coverage in request order.
MCP Task results retain the same envelope.

## Guardian lifecycle

`winxd` enforces lifecycle across every adapter connected to the same control socket.

### Authoritative activity clock

Protocol `1.3` guardians report:

- guardian creation time;
- last session activity;
- last command time;
- whether a command has ever been attempted.

The control-plane JSON next to each guardian socket is a cache for ownership and migration; it is not the source of truth.
This matters because runtime directories such as `/run/user/<uid>` are usually tmpfs and can lose metadata on reboot or
control-plane recreation.

For protocol-1.2 guardians without activity fields:

- used sessions conservatively use real control-plane request observations;
- never-used sessions prefer the guardian socket birth time;
- passive metadata reconstruction is distinguished from a real adapter request;
- recreating every metadata file at one instant does not make every old guardian appear newly active.

### Tiered TTL

Two independent defaults avoid treating disposable shells and valuable builds alike:

```text
WINX_UNUSED_SESSION_IDLE_TTL_SECS=1800   # never ran a command
WINX_SESSION_IDLE_TTL_SECS=86400         # ran a command
```

Set either value to `0` to disable that automatic tier. A foreground command or live background command is never removed,
even when its wall-clock TTL has elapsed. Output produced by a running command refreshes guardian activity, so retention
starts from meaningful terminal activity rather than only command submission.

### Quota pressure

Before refusing a new guardian, `winxd`:

1. removes stale sockets and expired sessions;
2. counts all live guardians;
3. if still full, reclaims the oldest inactive guardian that has never run a command;
4. repeats until one slot is available or no disposable guardian remains.

Used sessions and active foreground/background commands are never sacrificed to admit another session. If the quota is
still full, the error points to the effective manual command:

```bash
winx-code-agent prune --idle-seconds 0
```

That command removes every **idle** session while preserving active commands.

## Operational commands

```bash
# Inspect sessions; protocol 1.3 includes activity timestamps and ever_ran_command
winx-code-agent list

# Follow a guardian journal with an independent cursor
winx-code-agent attach <thread_id> --follow

# Apply the configured 30-minute / 24-hour tiers
winx-code-agent prune

# Override both tiers for one prune operation
winx-code-agent prune --idle-seconds 3600

# Remove every idle session; active commands remain
winx-code-agent prune --idle-seconds 0

# Explicit cleanup
winx-code-agent kill <thread_id>
winx-code-agent kill --all

# Replace only the control plane; guardians and PTYs survive
winx-code-agent restart-daemon

# Redacted runtime report
winx-code-agent doctor
```

Changing guardian limits or TTL environment variables requires `restart-daemon` because `winxd` reads them at startup.

## Persistent usage telemetry

Set a path to write only structured `winx::usage` events through a non-blocking JSONL writer:

```bash
WINX_USAGE_LOG="$HOME/.local/state/winx/usage.jsonl" \
WINX_USAGE_LOG_ROTATION=daily \
WINX_USAGE_LOG_KEEP_DAYS=7 \
winx-code-agent serve --http --token-file ~/.config/winx-http-token
```

Rotation accepts `daily` (default), `hourly`, or `never`; retention applies to daily/hourly files, and `0` disables pruning.
On Unix, every initial and rotated file is created/opened with `O_NOFOLLOW` and mode `0600`; an existing broader mode is
reduced to `0600`, and newly created log directories use `0700`. Each tool event contains the tool/action, principal, scoped
thread, hashed request and MCP-session correlation, client name and version, negotiated protocol, outcome, result status,
duration, response size, batch item count, and the configured worker limit (`0` for non-batched tools). HTTP events contain
peer, method, status, and duration. Initialize events additionally record the transition (`created`, `attached_existing`,
or an explicit change), whether it reused a session, compact/full response mode, generated context/guideline sizes,
initial-file count, and effective code-writer policy strength. Command text, file contents, tool
output, bearer tokens, and raw conversation identities are never written to this sink. Ordinary warnings and diagnostics
continue to stderr according to `RUST_LOG` and `WINX_LOG_FORMAT`.

For a quick per-tool latency view across the active and rotated files:

```bash
jq -s '
  def pct($p): sort | .[((length - 1) * $p | floor)];
  [.[] | select(.fields.event == "tool_call") | .fields]
  | group_by(.tool)
  | map(. as $calls | {
      tool: $calls[0].tool,
      calls: ($calls | length),
      p50_ms: ([$calls[].duration_ms] | pct(0.50)),
      p95_ms: ([$calls[].duration_ms] | pct(0.95)),
      avg_response_bytes: (([$calls[].response_bytes] | add) / ($calls | length))
    })
' ~/.local/state/winx/usage.jsonl*
```

Use `request_id` to correlate a `tool_call` with its `http_request`; the difference between their durations approximates
transport/adapter overhead without exposing payloads.

Landlock is applied before the non-blocking logging worker is created, so the writer inherits the same domain as Tokio and
PTY children. With `WINX_SANDBOX=1`, the usage-log path must therefore be under the startup workspace, `/tmp`, or an
explicit `WINX_SANDBOX_RW_PATHS` root; startup fails rather than silently creating an unconstrained writer outside that
policy.

`winx-code-agent doctor` reports whether a usage log is configured and whether file tools are in `workspace`, `extended`,
or `unconfined` containment mode.

## Network exposure

### Preferred

Keep the default loopback listener and place one of these in front:

- a private VPN such as WireGuard or Tailscale;
- an outbound-only/private MCP tunnel;
- an authenticated HTTPS reverse proxy;
- a tunnel operating inside the same trust boundary.

When a proxy forwards a public `Host`, add the exact authority:

```bash
winx-code-agent serve --http \
  --token-file ~/.config/winx-http-token \
  --allowed-host mcp.example.com
```

`--allowed-host` extends host validation; it does not create DNS, TLS, authentication, or a tunnel.

### Direct non-loopback binding

Winx refuses wildcard, LAN, and public listeners by default. A reviewed deployment can acknowledge the risk:

```bash
winx-code-agent serve --http \
  --bind 192.168.1.20:8000 \
  --allow-non-loopback \
  --token-file ~/.config/winx-http-token
```

This flag only permits the listener. It is not a substitute for HTTPS or firewall policy.

## Authentication modes

Winx accepts exactly one credential configuration:

1. `--principal-config` for named principals; or
2. `--token-file` for one file-backed principal; or
3. `--token` for one command-line principal; or
4. `WINX_HTTP_TOKEN` when no CLI credential source is supplied.

The preferred request header is:

```http
Authorization: Bearer <token>
```

Query tokens are disabled by default. Compatibility mode is explicit:

```bash
winx-code-agent serve --http \
  --token-file ~/.config/winx-http-token \
  --allow-query-token
```

Then a URL-only client can use `https://host/mcp?token=<token>`. Query strings often appear in browser, proxy, tunnel,
trace, and monitoring logs; prefer the bearer header.

Winx does not implement OAuth/OIDC discovery. Requests to `/.well-known/oauth-protected-resource`,
`/.well-known/openid-configuration`, and unrelated routes return ordinary `404 Not Found` responses without being forced
through bearer authentication. `/mcp` remains protected and returns `401` for a missing or invalid token.

## Resource limits and responses

| Condition                                       | Response                  | Notes                        |
|-------------------------------------------------|---------------------------|------------------------------|
| Missing or invalid token                        | `401 Unauthorized`        | Delayed by 100 ms            |
| Source IP exceeds the window                    | `429 Too Many Requests`   | Includes `Retry-After: 1`    |
| Global concurrency exhausted                    | `503 Service Unavailable` | Includes `Retry-After: 1`    |
| Request exceeds 120 seconds                     | `408 Request Timeout`     | Request is terminated        |
| Body exceeds 64 MiB                             | `413 Payload Too Large`   | Rejected before MCP dispatch |
| Public bind without acknowledgement             | Startup failure           | Prefer a private edge        |
| Missing, weak, duplicate, or unsafe credentials | Startup failure           | Fix the credential source    |

The rate limiter uses the TCP peer address seen by Winx. Several users behind one reverse proxy may share one window.
Winx intentionally does not trust forwarded-IP headers by default.

## CLI reference

| Option                        | Purpose                                            |
|-------------------------------|----------------------------------------------------|
| `serve --http`                | Select Streamable HTTP instead of stdio            |
| `--bind <IP:PORT>`            | Listener, default `127.0.0.1:8000`                 |
| `--token-file <PATH>`         | Preferred single-principal credential source       |
| `--principal-config <PATH>`   | Multi-principal TOML configuration                 |
| `--token <VALUE>`             | Compatibility source; visible in process arguments |
| `--tool-profile <PROFILE>`    | Single-principal catalog: `full`, `coding`, `read-only`, or `terminal` |
| `--allow-tool <NAME>`         | Build an exact single-principal catalog; repeatable |
| `--session-affinity <MODE>`   | Select `workspace`, `conversation`, or caller-owned `thread` IDs |
| `--allow-weak-token`          | Permit a short token for local tests only          |
| `--allow-non-loopback`        | Explicitly permit a non-loopback listener          |
| `--allowed-host <HOST>`       | Extend accepted Host authorities; repeatable       |
| `--allow-query-token`         | Accept `?token=...`; disabled by default           |
| `--max-concurrency <N>`       | Global simultaneous-request cap, default `32`      |
| `--requests-per-minute <N>`   | Per-source-IP limit, default `120`                 |

## MCP request requirements

Modern clients normally send:

```http
Content-Type: application/json
Accept: application/json, text/event-stream
MCP-Protocol-Version: 2026-07-28
Authorization: Bearer <token>
```

Winx also supports the legacy MCP HTTP initialization/session flow and returns `Mcp-Session-Id` for that lifecycle.
Remote Roots bootstrap is disabled on the shared HTTP service; the explicit `Initialize` tool selects the workspace.

## Upgrade notes for 0.2.333 and later

- Protocol `1.5` adds an optional trailer-free runtime payload and generation-bound actions. Protocol-1.4 guardians
  remain usable through bounded synchronous wait-policy fallback; adapters use legacy output when the compact field is
  absent. A newer control plane never substitutes its own capabilities for those of an older session guardian.
- Protocol `1.4` adds the `typed_action_result` capability and transports runtime-owned `BashCommandState` separately from
  terminal text. The control plane upgrades automatically when possible. Existing older guardians remain listable and
  attachable, but execution fails closed until the affected durable session is removed and initialized again; no
  text-parser fallback is used.
- Streamable HTTP now defaults to workspace affinity.
- Repeated first calls attach rather than reset protocol-1.3 guardians.
- `winxd` automatically upgrades from a control plane that supports planned restart; existing guardians keep running.
- Existing protocol-1.2 guardians remain readable through compatible `SessionInfo` defaults.
- Old never-used guardians are aged from their socket/activity evidence instead of bulk-recreated metadata timestamps.
- A protocol-1.2 guardian cannot return the new authoritative snapshot fields; workspace affinity normally creates a new
  canonical session while old sessions remain available for explicit cleanup.
- The no-argument `prune` command now applies the two TTL tiers rather than only the 24-hour used-session TTL.

## Troubleshooting

### Guardian quota reached

Inspect the fleet:

```bash
winx-code-agent list | jq '.[] | {
  thread_id,
  running,
  background_command_ids,
  ever_ran_command,
  created_at_unix_ms,
  last_activity_unix_ms
}'
```

Apply normal retention:

```bash
winx-code-agent prune
```

Force all idle sessions out while preserving active work:

```bash
winx-code-agent prune --idle-seconds 0
```

Raising `WINX_MAX_GUARDIANS` only changes capacity. It does not solve unstable session keys; use workspace affinity or a
stable thread-ID policy.

### Session unexpectedly shared

Workspace affinity deliberately shares one shell across conversations from the same principal and canonical workspace.
Use a distinct principal or start with:

```bash
--session-affinity thread
```

and provide stable unique IDs.

### Session unexpectedly disappeared

Run:

```bash
winx-code-agent doctor
winx-code-agent list
```

Check the selected runtime, the unused/used TTLs, explicit prune/kill operations, guardian quota pressure, and whether the
server is native Windows/embedded rather than Unix daemon mode.

### Token file rejected

On Unix, verify that it is a regular file, not a symlink, and has no group/other permissions:

```bash
chmod 600 ~/.config/winx-http-token
ls -l ~/.config/winx-http-token
```

### `401 Unauthorized`

Verify the exact bearer token, remove surrounding quotes, and ensure the credential belongs to the intended principal.
OAuth discovery probes are intentionally 404; Winx uses static bearer credentials unless an external edge adds another
identity system.

### `429` or `503`

Lower polling/parallelism first. Raise `--requests-per-minute` or `--max-concurrency` only after measuring machine and
client behavior. Remember that a reverse proxy may collapse many users into one source IP.

## Security boundary

A valid principal receives the capabilities of the selected Winx mode as the operating-system user running the server.
Authentication separates clients; it does not make arbitrary shell execution harmless.

- `wcgw` is full access and `BashCommand` is not workspace-confined;
- `architect` is intended for read-oriented exploration;
- `code_writer` constrains commands and writable globs;
- secret redaction is enabled by default;
- `WINX_SANDBOX=1` adds an experimental Landlock filesystem boundary on Linux;
- Winx should not run as root or another elevated account;
- each external client should receive its own principal and token;
- leaked tokens should be rotated immediately;
- remote access should be stopped when no longer needed.

Winx does not currently provide built-in TLS, OAuth/OIDC, or mTLS. Those controls belong at a reviewed network edge when
required. Per-principal tool policy narrows MCP discovery and dispatch but is not a substitute for transport security or
an operating-system sandbox. Read [SECURITY.md](../SECURITY.md) before exposing a shell-capable principal.

## Verification coverage

Automated coverage includes:

- single-token and multi-principal HTTP startup;
- bearer and opt-in query-token authentication;
- public well-known probes returning 404 while `/mcp` remains authenticated;
- rejection of non-loopback binding without acknowledgement;
- rate limiting and duplicate-principal rejection;
- per-principal tool discovery and dispatch enforcement;
- modern stateless discovery and tool listing;
- legacy HTTP session initialization;
- MCP Task completion over HTTP;
- two principals using the same external ID without crossing workspaces;
- one principal/workspace reusing a canonical session despite unstable first-call IDs;
- guardian quota reclaim for never-used sessions;
- active guardian preservation under quota pressure;
- tiered default pruning;
- guardian-owned activity clocks;
- attach-or-create preserving shell PID and cwd;
- legacy metadata fallback that prefers socket age over passive tmpfs reseeding.

The implementation lives primarily in:

```text
src/http_server.rs
src/config.rs
src/server/principal.rs
src/server/handler.rs
src/daemon/lifecycle.rs
src/daemon/server.rs
tests/mcp_2026_discovery_test.rs
tests/daemon_lifecycle_test.rs
```
