# Agent workflow observability and control

## Intent and authorization
Gabriel approved implementing all six proposals on 2026-10-27: execution history/results, state explanation, reliable prompt delivery, paginated output, waiting for changes across executions, and an optional Herdr adapter.
The implementation builds on Winx's existing Rust runtime, typed execution identity, guardian ownership, workspace binding, MCP policies, and bounded output.
Implementation details below are engineering decisions, not additional user requirements.

## Architecture and alternatives
Use two public tools: Execution (read-only list/get/read/explain/wait) and Agent (native or explicitly configured Herdr operations). Preserve existing tools and behavior.
An adapter-only implementation would not help ordinary Winx commands; a new terminal multiplexer would duplicate the existing runtime. Use a native execution catalog plus an optional Herdr adapter.
The catalog observes authoritative runtime state. It never infers process completion or identity from terminal text.
Native inspection must be non-consuming: observation/history must not advance existing status_check or screen delivery cursors.
Native prompts execute beside the PTY with generation/epoch checks. Old guardians must reject unsupported new actions before any input is written.
Herdr subprocess execution uses argument vectors and bounded pipes, never a shell.

## Public behavior
Execution list/get identifies recent accepted commands by an opaque session-scoped ID, with process state, exit code, cwd, observation times, retention and output availability.
History is bounded and persisted independently of the five-minute MCP Task result cache. Keep at most 128 records per session, a 24-hour terminal-record TTL, 1 MiB retained text per execution, and 16 MiB total retained text per session. Active records are not silently evicted.
Read defaults to 16 KiB per page and caps at 64 KiB. UTF-8 boundaries are preserved. Cursors bind execution, source and retained range. Gaps/expiry, snapshot changes, unknown outcomes and missing capabilities are explicit.
Sources distinguish retained command output, current visible screen and the full detection snapshot. Screen recognition always uses the complete detection input; presentation line limits cannot change classification.
Wait accepts at most 32 execution IDs, a prior sequence/cursor and a timeout bounded to 30 seconds. Return changed records or an explicit timed-out/no-change/resync result, never fabricate completion.
Explain exposes recognizer, state, source, matching rule/priority, fallback/unknown reason and observation metadata. Explicit manifest Unknown remains authoritative.
Native Agent prompt requires an explicit execution ID and a caller-supplied prompt ID, validates current input-ready state, captures a pre-send baseline, writes text and Enter to the same generation, and returns a receipt distinguishing delivery, observed activity and condition reached.
Strict prompt activity requires observed work or approval after submission; elapsed warmup alone cannot satisfy it. Reusing a prompt ID with the same payload must not resend; changed payload must fail. Bound per-generation receipts to 64, prompt text to 16 KiB, and inline waits to 30 seconds. Uncertain delivery never produces an automatic resend nextAction.
Keep existing unbound generic wait_for_turn behavior compatible; new strict semantics are opt-in through the new action/tool.

## Herdr adapter
Disabled unless an operator-owned configuration names an executable, a connection and bindings from principal plus canonical workspace to stable terminal IDs/aliases.
Expose list/get/explain/read/prompt/wait only. List enumerates configured targets; never leak a global agent listing. No implicit focused/current target, server startup, agent start, attach or raw keys.
Prompt requires BashCommand-equivalent authority and wcgw mode; Architect and restricted CodeWriter cannot use an external agent to escape restrictions.
Use the actual local CLI contract: envelopes for list/get/prompt/wait, bare JSON for explain --json, raw text for agent read. Always supply finite wait timeouts. Bound subprocess runtime, stdout, stderr and argv. A prompt timeout is an uncertain outcome.
Literal prompt metacharacters must remain literal arguments. Do not assume an unimplemented stdin API.

## Constraints
Rust 2021; MSRV 1.90; no new dependency unless existing facilities cannot satisfy a concrete requirement.
Preserve thread/workspace/principal/path/mode isolation and existing policy bit values.
No automatic replay of commands or prompts during recovery.
Retain redaction and private on-disk permissions; do not put command/prompt text into usage telemetry.
Old clients continue existing behavior. New daemon functionality is capability-negotiated against the effective guardian.
No live Herdr prompt, release installation, service restart, shared-branch merge or push is part of implementation validation.

## Acceptance
Real PTY tests cover stale idle, post-send activity, generation replacement and non-consuming inspection.
Catalog tests cover accepted/completed/nonzero outcomes, restart/readback, isolation, retention, UTF-8 paging, source-bound cursors, gaps and wait-any.
Herdr tests cover actual output shapes, literal argv, disabled config, principal/workspace/target denial, mode denial, output limits and uncertain prompt timeout without connecting to active agents.
Run the repository's full just check gate and independent review before committing.
