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

## Observe several executions in one call

Use `observe` when a plugin client needs compact status and output together:

```json
{
  "thread_id": "<initialized-thread>",
  "workspace_root": "/absolute/initialized/workspace",
  "action": {
    "type": "observe",
    "items": [
      {"execution_id": "<build-id>", "page_bytes": 2048},
      {"execution_id": "<test-id>", "source": "output", "page_bytes": 4096},
      {"execution_id": "<test-id>", "source": "screen", "page_bytes": 2048}
    ],
    "timeout_seconds": 5
  }
}
```

The result's `data.items` follows request order. Each item has compact `execution`
metadata, an optional `page`, a finite `status`, and an `errors` array. Process
status, exit code, observed times, and output-pending state retain their existing
meaning. Use `get` or `explain` for command details, runtime identities, or recognition
evidence. A nonzero process exit is a successfully observed outcome, not a failed
observation.

Supply 1–32 items. Each page defaults to 2,048 bytes, accepts 4–65,536 bytes, and the
sum of all requested page sizes must not exceed 65,536 bytes. This limits raw page
text, not serialized response size or model tokens. Source defaults to `output`.
Duplicate execution/source pairs, malformed IDs, and invalid budgets reject the
whole request before session admission or history access.

Page cursors are independent and interchangeable with `read` for the same scope,
execution, and source. A missing execution, invalid cursor, refresh failure, or
unreadable payload affects its item; successful siblings and available metadata
remain. Inspect every item's `errors`, whose stages distinguish `refresh` from
`read`. Any item error produces `completed_with_issues` and the fixed
`execution_observation_issues` code. Retention gaps and snapshot changes retain
the normal successful resynchronization-page semantics.

The server refreshes each distinct eligible execution once, then reads all final
metadata and pages in one catalog transaction. A refresh may include a metadata
inspection and a full-output capture. The transaction also produces
`data.catalog_cursor`, usable as `wait.after`; it is separate from page cursors.
Runtime observations across different processes are not simultaneous. Unsupported
live observation still permits retained reads; timestamps, unknown state, and
`output_pending` expose the limits of retained information.

The optional operation deadline defaults to 5 seconds and accepts finite values
from 0 through 30; zero grants a 250 ms budget. One deadline covers session
admission, runtime observation, and catalog access. This is a single pass without
lifecycle waiting. An expired deadline or a catalog-wide failure returns an error
without a success cursor. A started blocking catalog operation may finish after
the response deadline under the existing bounded file-lock retry.

Output appears once in structured item pages, with normal response redaction and
without a concatenated top-level `output`. Neither output nor screen observation
consumes the legacy BashCommand delivery cursor. Usage reports classify this as
`Execution / observe / native`; the operation deadline does not add lifecycle
wait or prompt metrics.

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

## Workflow telemetry and latency diagnostics

Usage logs keep `usage_schema=1` and add `workflow_schema=1` for Agent/Execution
observations. `winx-code-agent report --log <usage-log> --last 10000 --since-minutes 0`
adds a `workflows` section grouped by tool, action and backend. The existing
`execution` section and HTTP correlation retain their meanings.

Workflow metrics distinguish catalog change/timeout/resynchronization, observed native
prompt receipts, and Herdr submission/uncertain-input metadata. A receipt read counts as
an observation, including when the same receipt is read again. These counters do not
estimate unique prompts delivered or tasks completed. Missing or unrecognized evidence
stays unknown; an explicit zero timeout is retained.

Auxiliary `tool_started` and `tool_phase` events share a random internal
`usage_call_id` with the terminal `tool_call`; HTTP correlation still uses the
existing request ID. Phases cover workspace validation, session admission, catalog work,
runtime inspection/capture, native prompt RPC, the Herdr adapter, deliberate wait sleep,
temporary-file audit, and response finalization. Logs contain finite classifications,
durations and counts, without adding command/prompt text, output, cursors or raw paths.

Each call emits at most 16 phase start/finish pairs and one truncation marker. Suppressed
phases still contribute to the terminal summary. Phase totals are inclusive: they can
repeat or overlap and must not be summed as exclusive portions of request duration.
Submillisecond observations can have zero milliseconds with a positive invocation count.
Discarded futures record observed abandonment; underlying blocking work may continue.

A start without a terminal means completion was not observed. Process loss, cancellation,
log rotation, and existing early error exits can all produce that pattern. It is not
evidence that a request is still running. The report ignores these new auxiliary event
types before applying `--last`, preserving terminal/HTTP windows and build attribution;
physical `linesSeen` still includes the added log lines.

## Recover an explicitly selected conversation

For these direct literal launches, Winx retains the UUID selected by the accepted command:

| Accepted launch | Recovery target |
|---|---|
| `claude --session-id <uuid>` | `claude --resume <uuid>` |
| `claude --resume <uuid>` | `claude --resume <uuid>` |
| `claude -r <uuid>` | `claude --resume <uuid>` |
| `codex resume <uuid>` | `codex resume <uuid>` |

The resulting advice is `explicit_target`, with `source=launch_argument` and
`current_conversation_verified=false`. It targets the recorded launch argument even if
another conversation later becomes most recent. Backend acceptance, startup success and
later in-TUI conversation switches are unverified.

Bare supported launches without a selector are `best_effort` and carry no executable
recovery recommendation. Unsupported or ambiguous launches are `unavailable`. Extra
flags, wrappers, command lists, substitutions, pipelines, conflicting selectors and
non-UUID names are not accepted for explicit recovery. No initial command or prompt is
replayed, and recovery does not automatically choose `--continue` or `--last`.

Records are scoped to the runtime's thread/workspace, bounded to 16 KiB and written
atomically with private files. Accepted foreground launches update the evidence before
the output wait; rejected, busy and background requests preserve the existing foreground
record. A surviving guardian is reattached through the existing path. Recovery advice
does not restart it or submit terminal input.

## Release managed temporary helpers

`EditFiles` supports explicit `release_temporary` requests for individually named helpers
in the current session's managed directory. Each request uses a fresh `ReadFiles` revision
and artifact generation; the runtime owner checks foreground/background activity before
releasing anything. The initial implementation requires Linux and a capable durable
guardian. Unsupported or unresolved ownership leaves the files in place.

See [Releasing managed temporary helpers](temporary-release.md) for the request schema,
revision and generation rules, retained-output protection, partial results and retry
semantics. Existing maintenance reports usage without automatically deleting old or
sibling-session helpers when runtime protection cannot be established.

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

## Usage metrics for plugin workflows

Usage logs contain aggregate metadata rather than command text, file contents,
execution cursors, or the file paths requested by these metrics.

For `Execution.observe`, `batch_items` and `observe_requested_items` count requested
entries. `observe_returned_items` counts response entries; `observe_items_with_issues`
counts entries with at least one error, so refresh and read errors on the same entry
count once. These are call observations, including repeated observations of the same
execution. They are not unique process counts or evidence that a process completed.
`observe_schema=1` identifies the new counters. Older `batch_items=0` values are
unverifiable for observe and must not be interpreted as measured empty batches.

For `ReadFiles`, `read_request_schema=1` accompanies three optional request counters:

| Field | Requested shape |
|---|---|
| `read_whole_file_requests` | No recognized line bounds |
| `read_bounded_range_requests` | An end bound, including `:10-20` and `:-20` |
| `read_open_ended_range_requests` | A start with no end, including `:20-` and `:20` |

Classification reuses the tool's parser and counts duplicate entries separately.
Invalid or empty requests have unknown shape counts. These fields describe requested
entries, not successful reads or unique files. Reading budgets, delivered contents,
revision receipts, and visible edit coverage are unchanged.

For ordinary wait-consuming `BashCommand` actions, `requested_wait_policy` and
`requested_wait_ms` retain the client's explicit input. Optional
`effective_wait_policy`, `effective_wait_ms`, and `effective_wait_mode` describe the
server-selected inline allowance after defaults and caps:

- `synchronous`: the allowance in the normalized synchronous request.
- `task_inline`: the initial adaptive allowance, retained if work is promoted to a Task.
- `task`: zero inline allowance for an immediate Task accepted by the server.

The effective allowance is configuration, not elapsed waiting time or proof a command
started. A structured runtime rejection can still report a selected allowance; failures
before routing or inline execution leave it unavailable. A failed promotion after an
inline execution retains that execution's allowance. `screen`, `inspect`,
`wait_for_turn`, and `prompt` have separate observation/timeout semantics and omit
these fields. Use `duration_ms` and phase timings to assess actual call latency.

The JSON usage report exposes `reads`, `observations`, and the
`execution.effectiveWait*` aggregates. Missing, malformed, and legacy metadata remain
unknown rather than becoming zero samples. Effective-wait counts use only BashCommand
calls; absence also includes actions for which this allowance is not applicable.
`observations.partialResults` counts partial calls separately from affected items.
Existing `errorResults` still counts nonempty error codes, including partial results.
Recorded response bytes help locate high-volume workflows; they do not establish
token billing or savings without a comparable workload.
