# Agent Workflow Improvements Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans. Steps use checkbox syntax for tracking.

**Goal:** Deliver all six approved improvements as compatible, tested Winx capabilities.
**Architecture:** Native runtime observations and prompt receipts feed a bounded durable execution catalog. Two scoped MCP tools expose the catalog and native/optional Herdr agent operations.
**Tech Stack:** Existing Rust 2021, Tokio, serde, rmcp, portable-pty; MSRV 1.90.
**Spec:** docs/superpowers/specs/2026-10-27-agent-workflows-design.md

## Global Constraints
- Rust 2021; MSRV 1.90; preserve existing policy bit values and tool behavior.
- All access is bound to the authenticated principal, exact thread/workspace pair, mode and runtime execution token.
- No command state inferred from terminal text; no implicit command/prompt replay.
- Native inspection does not consume legacy output delivery cursors.
- History: 128 records/session, 24-hour terminal TTL, 1 MiB/record, 16 MiB/session; pages 16 KiB default and 64 KiB max.
- Wait: maximum 32 IDs and 30 seconds. Prompt: maximum 16 KiB text and 64 receipts/generation.
- No runtime restart, install, external-agent messages or shared-branch publication during tests.
- just check is required before any commit.

## Review Focus
1. An old guardian must not accept a new prompt action without its required semantics.
2. Output monitoring must not steal output from normal BashCommand callers.
3. Identical visible thread IDs under different HTTP principals must never share history or Herdr targets.
4. Prompt delivery may be uncertain after timeout; no retry may duplicate input.
5. UTF-8, retention gaps and changed screen snapshots must not produce invalid or misleading cursors.

## Task 1: Native state explanations and observations
Files: src/state/turn.rs, src/state/pty.rs, src/tools/bash_command/{tui,interaction}.rs, a focused workflow module, src/types.rs, src/runtime/mod.rs, src/daemon/{protocol,client,server,control}.rs as needed.
Produces: serializable TurnExplanation; non-consuming native observation carrying screen, retained output, typed process state, output/activity sequence and execution identity; capability-negotiated transport.
- [x] Add tests for manifest Unknown, auto winner, short presentation independent of detection, and non-consuming inspection.
- Initial explanation-only RED evidence was not retained; final GREEN coverage is verified below. No unobserved initial failure is claimed.
- [x] Implement additive explanation method on TurnRecognizer and typed runtime observation.
- [x] Run focused behavior/protocol tests and independent review.

## Task 2: Receipt-bound native prompts
Files: native workflow module from Task 1, interaction.rs, PTY state, runtime/daemon transport, real PTY tests.
Consumes: Task 1 observation and explanation; ShellExecutionToken.
Produces: prompt receipt with caller prompt ID, payload fingerprint, pre-send activity sequence, delivery outcome, activity_observed and wait outcome.
- [x] Add real PTY/state-machine tests for stale idle, activity before the first wait, generation replacement and idempotent prompt IDs.
- [x] Reproduce native review regressions before fixes: delayed echo, queued-output watermark, EOF carry, and foreign guardian identity.
- [x] Implement bounded generation-bound text/Enter delivery and strict activity gate; preserve legacy waits.
- [x] Run focused tests and independent review.

## Task 3: Durable execution catalog and output pagination
Files: new src/state/execution_history.rs and focused submodules/tests; new server execution module; src/server.rs/sessions.rs/tool_dispatch.rs integration.
Consumes: typed native observation, ordinary BashCommand outcomes and exact execution tokens.
Produces: ExecutionCatalog record/list/get/read/change methods and serialized record/page types.
- [x] Add tests for process outcomes, restart readback, session isolation, retention/limits, UTF-8 and invalid/cross-source cursors.
- [x] Reproduce terminal ordering, pending output capture, and catalog-lock deadline regressions before fixes.
- [x] Implement bounded private persistence and observation updates, with truthful partial/unknown coverage.
- [x] Connect accepted Bash commands to catalog and reconcile live records before replacement/new commands.
- [x] Run all 19 catalog integration tests and independent review.

## Task 4: Public Execution tool and bounded wait-any
Files: src/tool_registry.rs, src/tool_policy.rs, src/server/{catalog,tool_dispatch,outcomes,principal}.rs as needed, workflow request types, integration tests.
Consumes: Task 3 catalog and Task 1 observations.
Produces: read-only list/get/read/explain/wait MCP contract with unchanged existing tools.
- [x] Add MCP coverage for discovery, exact workspace binding, cross-principal denial, wait-any changes and timeout/resync.
- [x] Run RED: two public Execution tests compiled and failed because Execution was absent (exit 101).
- [x] Register appended tool identity, publish schemas and implement bounded wait with real typed observations.
- [x] Verify legacy output delivery, resumable pages, shared admission deadlines and accepted workspace ownership; review.

## Task 5: Optional Herdr backend and Agent tool
Files: new src/tools/herdr.rs or focused directory, new server agent module, public request types/catalog/policy.
Consumes: explicit principal/workspace binding, mode, native prompt operation and operator Herdr config.
Produces: Agent native/herdr list/get/explain/read/prompt/wait behavior; disabled/unavailable and uncertainty errors.
- [x] Add subprocess-boundary and authorization tests using controlled executable fixtures.
- [x] Run RED: 13 adapter tests failed against the skeleton; the later CLI parser regression also failed before its correction.
- [x] Implement actual CLI argument/response contracts, private configuration bindings, deadlines and byte caps.
- [x] Wire Agent tool with BashCommand-equivalent control authority and wcgw-only external prompt.
- [x] Run all 14 adapter tests and independent review.

## Task 6: Integration, documentation and final verification
Files: README.md, docs/streamable-http.md or new focused workflow documentation, protocol fixtures and integration tests.
- [x] Document all six features, examples, limits, capabilities, retention/recovery and Herdr configuration.
- [x] Run cargo fmt, full just check and daemon/real PTY integration scenarios (926 passed, 0 failed, 23 ignored).
- [x] Obtain independent branch review, fix material findings and rerun affected gates.
- [x] Prepare the reviewed implementation for its feature-branch commit; the commit containing this completed plan is the delivery checkpoint.

## Progress
Baseline: eaa5fcc4f68f7929a542771fb317751a1d24e12c; branch feat/agent-workflows.
Worktree: /home/gabrielmaia/.cache/agents/2026-10-27-winx-agent-workflows.
Initial full cargo test --all-features --locked: exit 0 (2026-10-27), log agent-workflows-check.log.

## Preflight and rulings
| Tasks | Shared interface/file | Ruling |
|---|---|---|
| 1,2 | PTY/workflow runtime | One owner implements both; root integrates the server. |
| 1,3,4 | Typed observation | Freeze the serializable observation interface before catalog integration; never parse state from output. |
| 3,4,5 | Server/catalog/policy | Root owns dispatch/history; winx_workflow_design_review owns registry/policy/catalog/docs, with frozen request types. |
| 5,all | Authorization | Herdr binding is separate from Winx's internal thread scoping. |
| 6,all | Verification | Serialize builds against the shared cargo target; full gate only after source integration. |
| 1 | Explanation tests vs implementation | Existing manifest precedence remains unchanged. |
| 2 | Prompt tests vs implementation | Strict activity semantics apply only to the new operation. |
| 3 | Retention tests vs implementation | Explicit gaps and unknown outcomes; active records cannot be silently evicted. |
| 4 | Wait tests vs implementation | Bounded calls and precise sequence baseline; no silent event loss. |
| 5 | Adapter tests vs implementation | Fixtures model real CLI shapes; no live agent messaging. |
| 6 | Final gate vs implementation | No commit before just check. |

Ruling: The user's explicit approval covers all six proposals and authorizes implementation; ordinary design choices are settled in this spec instead of another approval round.
Ruling: Use the established external agent-worktree directory to isolate this work from main and other active branches.
Ruling: Component implementation can overlap only with disjoint file ownership; registration/integration and build gates stay coordinated by root.

Ruling: Prompt receipt capacity rejects additional new IDs; known IDs, including uncertain deliveries, remain deduplicated for the generation.
Ruling: Authoritative Exited state is irreversible for one execution identity; delayed Running observations are ignored, even if persisted later.
Ruling: Failed exact-token recovery records Unknown with no invented exit code; Unknown records have bounded retention and may be evicted for capacity.
Ruling: Rolling/truncated snapshots without provable byte positions invalidate the output cursor on new activity, including identical repeated tails; no guessed overlap.
Ruling: Wait cursors bind scope/epoch/sequence; missing watched records and retention gaps explicitly require resynchronization.
Design review: winx_workflow_design_review approved proceeding with the five rulings above and corresponding regressions.

Integration complete: all six capabilities are implemented and documented. The final source passed the full required gate after review and legacy-integration corrections.
Tooling incident: component-agent Winx EditFiles calls stalled without a result for about 30 minutes; root calls remained responsive. Interrupted pending calls and moved component patch preparation to local scratch; root applies canonical changes through Winx after fresh reads. No claim that a blocked edit succeeded.
Ruling: Session output budget includes both retained terminal output and retained detection screens. Metadata remains when output is pruned.

## Final verification

Final required command on the integrated source: `cargo fmt --all && just check`.
Exit status: 0. Winx background execution: `274b875ac5606433` (2026-10-27).

| Gate | Result |
|---|---|
| `cargo fmt --all -- --check` | Passed |
| Host Clippy, all targets/features, `-D warnings` | Passed |
| Windows GNU target Clippy, all targets/features, `-D warnings` | Passed |
| `cargo test --all-features --locked` | 926 passed, 0 failed, 23 ignored |
| `cargo +1.90.0 check --all-features --locked` | Passed |

The focused pre-gate run passed 57 native/server/protocol tests, 19 catalog tests, and 14 Herdr adapter tests. These overlap the complete suite; they are not 90 additional tests.

Observed RED-to-GREEN corrections include:

- Native prompt echo cannot establish submitted work; queued output and EOF carry use capture-safe activity watermarks.
- Foreign guardian identities are rejected before embedded input; guardian configuration remains authoritative.
- Shared wait deadlines include session admission; normalized-empty thread IDs cannot select a last-active session.
- Accepted commands retain their runtime-owned workspace; later observations only update existing exact-token records.
- Reordered terminal observations cannot erase authoritative exit or newer output; pending capture remains explicit.
- Herdr prompts preserve literal text through the real CLI parser contract, including flag-like and escaped inputs.
- Legacy background completion preserves its retained generation and history identity in both embedded and daemon workflows.

Full-suite integration also updated the terminal-profile expectation and kept tool descriptions within the original 4,000-byte budget (3,968 bytes). No assertion or identity check was relaxed to obtain GREEN.

Independent final reviews covered catalog/storage/public scope, native runtime/guardian behavior, and the Herdr boundary. Corrective patches received cross-review from a different component reviewer; the final tombstone-generation correction was approved independently.

Native activity recognition remains heuristic. Echo-like submitted text conservatively leaves activity unconfirmed for that receipt; process exit is reported independently. History is bounded and lazily pruned, and unchanged observation timestamps are not liveness heartbeats. Herdr was validated with controlled executables/parser fixtures; no prompt was sent to an active external agent.

Delivery remains on `feat/agent-workflows`. Runtime installation/restart and main-branch publication are outside this implementation validation.
