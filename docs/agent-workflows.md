# Execution history and agent workflows

Winx exposes two session-bound workflow tools: `Execution` observes accepted commands,
and `Agent` observes or prompts a native execution or an explicitly configured Herdr target.
Both require the exact `thread_id` and `workspace_root` returned together by `Initialize`.
HTTP history is also isolated by authenticated principal; an execution ID or cursor is
not authority to inspect another principal or workspace.

## Discovery and authority

| Profile | Execution | Agent |
|---|---|---|
| `full`, `coding`, `terminal` | Yes | Yes |
| `read-only` | Yes | No |

An explicit tool allowlist can expose `Agent` for observation without granting
`BashCommand`. Prompting additionally requires trusted `BashCommand` authority and
`wcgw` mode. Neither `architect` nor restricted `code_writer` mode can delegate
unrestricted input to an agent. `Execution` is advertised as read-only and closed-world;
`Agent` conservatively advertises destructive, open-world behavior because it can send input.

## List, inspect, and explain native executions

Start a command through `BashCommand` as usual. Use the returned execution identity, or
find recent accepted commands with `Execution`:

```json
{
  "thread_id": "<initialized-thread>",
  "workspace_root": "/absolute/initialized/workspace",
  "action": {"type": "list", "limit": 20}
}
```

For details, use `{"type":"get","execution_id":"<execution-id>"}` as the action.
Records distinguish process status, an observed exit code, working directory, observation
times, and retained-output availability. An unknown outcome is not a successful exit.
Unchanged polls leave `observed_at_unix_ms` unchanged: it records the latest observation
that produced a persisted change and should not be used as a liveness heartbeat.

An accepted command creates history under the thread and workspace supplied by the runtime
when it admitted that command. Later observations update an existing record with the same
execution identity; they cannot create a copy in a workspace selected after the launch.

Use `{"type":"explain","execution_id":"<execution-id>","recognizer":"auto"}` to
inspect turn recognition: recognizer, state, detection source, matching rule and priority,
fallback or unknown reason, and observation metadata. Detection uses the complete
recognition input, independently of presentation line limits. An explicit manifest
`Unknown` remains authoritative.

Observation does not consume the legacy `BashCommand` output or screen delivery cursor.
Monitoring and an existing status-check client can coexist.

## Read bounded output pages

```json
{
  "thread_id": "<initialized-thread>",
  "workspace_root": "/absolute/initialized/workspace",
  "action": {
    "type": "read",
    "execution_id": "<execution-id>",
    "source": "output",
    "page_bytes": 16384
  }
}
```

Sources are `output` (retained command output), `screen` (visible screen), and
`detection` (the detection snapshot). The default page size is 16 KiB and the maximum
is 64 KiB. Pages preserve UTF-8 boundaries.

Continue using the returned `next_cursor` as `cursor`, with the same execution and source.
Cursors are opaque and scoped; do not construct them, exchange them between executions,
or reuse a screen cursor for command output. Honor `has_more`, retention gaps and
`resync_required`. A changed snapshot or unavailable retained range requires resynchronizing
from the current data, rather than concatenating incompatible pages into one transcript.

`output_activity_seq` identifies the activity covered by the last full retained-output
capture. `output_pending` remains true when metadata advances without a complete capture,
including after exit. A supported runtime refresh makes at most one additional full-output
inspection against the same execution identity; later refreshes retry while capture remains
pending, even without new activity. Reads continue to expose the available retained text.

## Wait for changes across executions

```json
{
  "thread_id": "<initialized-thread>",
  "workspace_root": "/absolute/initialized/workspace",
  "action": {
    "type": "wait",
    "execution_ids": ["<execution-one>", "<execution-two>"],
    "after": "<cursor-from-prior-list-or-wait>",
    "timeout_seconds": 10
  }
}
```

A wait watches at most 32 explicit execution IDs for at most 30 seconds. Its timeout
includes session admission, baseline catalog I/O, and live refreshes. `timeout_seconds: 0`
performs one catalog poll with a total 250 ms admission/I/O budget and no lifecycle wait.
If the catalog cannot be observed within the budget, the call reports an error rather than
claiming no change. Native `Agent` waits without `prompt_id` use the same budget.

Optional `events` selects `state`, `output`, `exited`, `awaiting_input`,
`awaiting_approval`, `busy`, or `unknown`. Preserve the returned baseline cursor for
the next call. Changed records, timeout/no-change, and resynchronization are distinct
outcomes. Timeout does not imply that a process completed.

## Submit a native prompt once

```json
{
  "thread_id": "<initialized-thread>",
  "workspace_root": "/absolute/initialized/workspace",
  "backend": "native",
  "action": "prompt",
  "target": "<execution-id>",
  "prompt_id": "review-round-1",
  "text": "Review the current changes and report any correctness defects.",
  "recognizer": "auto",
  "timeout_seconds": 10
}
```

A native prompt requires an explicit execution ID and caller-supplied `prompt_id`.
Winx validates input readiness, binds delivery to the current runtime identity and
generation, and captures an activity baseline before sending the text and Enter.
Prompt text is limited to 16 KiB; inline waits are limited to 30 seconds. Receipts are
bounded to 64 per generation.

The receipt distinguishes delivery, observed post-submission activity, and reaching the
requested wait condition. Strict activity requires observed work or approval after the
baseline; stale idle state, terminal echo, or elapsed warmup alone cannot satisfy it.
The existing generic `BashCommand wait_for_turn` behavior remains compatible.

Reusing a native prompt ID with the same payload retrieves its receipt without another
delivery; changing that payload is rejected. An uncertain delivery or timeout never
authorizes an automatic resend with a new ID. Inspect the receipt and execution before
deciding what to do next. A missing capability on an older guardian is explicit; Winx
does not silently fall back to unprotected raw input.

To observe the same receipt without submitting text, use:

```json
{
  "thread_id": "<initialized-thread>",
  "workspace_root": "/absolute/initialized/workspace",
  "action": "wait",
  "target": "<execution-id>",
  "prompt_id": "review-round-1",
  "timeout_seconds": 5
}
```

Recognition remains heuristic. If the submitted text itself resembles a work or approval
marker, that receipt conservatively leaves activity unconfirmed for its lifetime. This
also covers text echoed after Enter or reflowed across lines; later real work may remain
unconfirmed. The execution's authoritative exit is still reported independently. A ready
terminal is evidence about its lifecycle, not proof that the requested task succeeded.

Other `Agent` actions are `list`, `get`, `read`, `explain`, and `wait`.
The backend defaults to `native`. Native reads use the same sources and paging semantics
as `Execution`.

## Retention and recovery

History is persisted independently of the five-minute MCP Task result cache. Defaults
allow 128 records per scoped session, 1 MiB of command output and 64 KiB of screen text per
execution, and 16 MiB of combined output and screen text per session. Output may be pruned
while execution metadata remains; active records are not silently evicted.

Exited and `unknown` records have a 24-hour retention deadline, starting when that terminal
or unknown outcome was first recorded. Expired records are pruned lazily during catalog
operations, and terminal or unknown records may be removed earlier to make room. Treat
output as bounded retained coverage, not a permanent complete audit log.

Recovery reconciles records against their full runtime execution identity. If identity
or completion cannot be established, the result stays explicit about the unknown
outcome. Recovery never replays a command or prompt. Persistence uses private files;
normal output redaction still applies.

## Optional Herdr backend

Herdr is disabled unless the operator configures an executable, a connection, and target
bindings for an authenticated principal and canonical workspace. Agents cannot use the
adapter to select a globally focused terminal. Listing reveals only configured targets.

Select `"backend":"herdr"` and an explicit configured target alias for
`get`, `read`, `explain`, `prompt`, or `wait`. The adapter exposes no server
startup, agent start, attach, or raw-key operation. It invokes the configured executable
with literal argument vectors, bounded output, and finite deadlines.

Herdr `read` returns recent text and accepts a `lines` limit; it does not promise the
native catalog's retained-output pagination. A successful Herdr prompt acknowledges
submission; acceptance alone does not establish task completion. Use `get`, `explain`,
or `wait` to observe the subsequent lifecycle.

Herdr does not provide native prompt-ID deduplication, so `prompt_id` is rejected for this
backend. A prompt subprocess timeout is an uncertain outcome, not evidence that no input
was delivered, and must not trigger an automatic resend.

### Operator configuration

Set `WINX_HERDR_CONFIG` to an operator-owned JSON configuration file. On Unix the file
must be private (mode `0600`); it is limited to 256 KiB and rejects unknown fields.

```json
{
  "executable": "/absolute/path/to/herdr",
  "socket_path": "/absolute/path/to/herdr.sock",
  "deadline_ms": 5000,
  "bindings": [
    {
      "principal_key": "local",
      "workspace_root": "/canonical/workspace",
      "targets": {
        "writer": "stable-terminal-id"
      }
    }
  ]
}
```

The executable must resolve to an existing executable regular file. The workspace must
be its canonical absolute path, and the socket path must be absolute. The local principal
key is `local`; HTTP bindings use `http:<principal-id>`, where the ID is the configured
`HttpPrincipal.id()` value, not the bearer token or a client-supplied thread ID.

`deadline_ms` defaults to 5000 and must be between 1 and 30000. This operation deadline
also caps Herdr waits; a larger `timeout_seconds` does not extend it. Each subprocess is
limited to 64 KiB stdout and 16 KiB stderr; listing is capped at 32 configured targets and
64 KiB total. Read limits default to 100 lines and cap at 1000. The adapter removes inherited
`HERDR_*` environment variables and sets `HERDR_SOCKET_PATH` from trusted configuration.
It validates target identity using the configured binding and the CLI response.

For example, observe the configured target with:

```json
{
  "thread_id": "<initialized-thread>",
  "workspace_root": "/canonical/workspace",
  "backend": "herdr",
  "action": "read",
  "target": "writer",
  "lines": 100
}
```

A Herdr wait can supply `until` with lifecycle states `idle`, `working`, `blocked`,
`done`, or `unknown`, and a finite `timeout_seconds`. A prompt supplies `text` and
omits `prompt_id`. Configuration only grants access to existing targets; it does not
start Herdr or attach a terminal.
