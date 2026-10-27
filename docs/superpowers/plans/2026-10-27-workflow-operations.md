# Workflow Operations Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement task by task.

**Goal:** Deliver the approved next round: workflow telemetry, bounded latency diagnostics, deterministic recovery targets, and explicit temporary-artifact lifecycle.
**Architecture:** Add observation metadata without changing execution semantics; reuse existing scoped runtime and mutation boundaries for recovery and artifact work.
**Tech Stack:** Rust 2021, Tokio, serde, tracing, existing rmcp; MSRV 1.90.
**Spec:** docs/superpowers/specs/2026-10-27-workflow-observability-design.md (Tasks 1–2); component designs for Tasks 3–4 are finalized before those tasks begin.

## Global Constraints
- Preserve existing principal/thread/workspace, mode, tool-policy and exact execution-token boundaries.
- Never log prompt/command text, output, cursors, secrets, untrusted arbitrary enum strings or new raw paths.
- Missing metadata remains unknown; receipt observation is not a new delivery or successful task.
- No automatic command or prompt replay during recovery.
- Existing tool-call and HTTP report denominators/correlation remain unchanged.
- Tracing is bounded and cannot extend operation deadlines or trigger additional inspections.
- Only named managed temporary files may be released; active or unresolved ownership/reference state prevents release.
- Real PTY behavior, file mutation atomicity, replay and isolation require focused regression tests.
- Rustfmt, host and Windows Clippy, full tests and MSRV via just check are required before any commit.
- Root alone calls Winx and applies canonical edits using fresh ReadFiles receipts. Agents prepare scratch drafts and review snapshots.
- Do not mutate canonical source while a build/test command is running; serialize builds against the shared target directory.

## Review Focus
1. Repeated receipt reads must never become repeated-delivery or unique-execution counts.
2. Herdr outer metadata must survive its nested result envelope; unknown backend outcomes stay unknown.
3. New trace events must not evict terminal/HTTP records from --last or weaken build attribution.
4. Recovery must retain an explicit conversation target without selecting a newer conversation or claiming unobserved backend confirmation.
5. Cleanup retries and pathname replacement must not delete a different file; scope, runtime activity and retained evidence remain authoritative.

## Task 1: Workflow telemetry and report
Files: src/server/usage.rs; new src/server/usage/workflow.rs; src/report.rs; new src/report/workflows.rs; tests/usage_report_test.rs.
Consumes: current Agent and Execution typed request/response contracts; existing usage capture and report selector.
Produces: optional finite workflow metadata plus UsageReport.workflows grouped by tool/action/backend; legacy execution section remains unchanged.
- [x] Add emitting-JSON tests for native receipt states, false/zero/unknown, nested catalog waits and Herdr outer fields.
- [x] Add report fixture tests for operation grouping, repeated receipt observations, unknown legacy records and unchanged HTTP/build totals.
- [x] Run tests against the baseline and record the intended failures.
- [x] Implement the additive extraction/emission and bounded operation aggregates.
- [x] Focused tests passed (59), independent review closed, and the integrated repository gate passed.

## Task 2: Request lifecycle and phase diagnostics
Files: src/server/usage/lifecycle.rs; usage.rs; handler.rs; executions/history.rs; executions/wait.rs; agents.rs; agents/herdr.rs; tool_dispatch.rs; report/selection.rs; report/workflows.rs.
Consumes: Task 1 workflow metadata and existing request correlation.
Produces: bounded auxiliary start/phase events, monotonic inclusive phase summaries, explicit unfinished-observation semantics.
- [x] Add behavioral tests for phase completion/abandonment, 16-pair cap and summary preservation.
- [x] Add selector tests showing trace events do not alter --last/build/HTTP results.
- [x] Observe intended RED failures, implement task-local trace guards and narrow phase instrumentation.
- [x] Validate real request paths, cancellation and unchanged wait deadlines; independent review and the integrated repository gate passed.

## Task 3: Deterministic conversation recovery
- [x] Finalize the bounded component design with explicit target provenance and uncertainty.
- [x] Add end-to-end accepted-launch/recovery regression fixtures before implementation.
- [x] Implement supported explicit targets and honest fallback without executing recovery commands.
- [x] Validate isolation, malformed targets, legacy records and unchanged live reattach; close both review findings with observed RED/GREEN regressions.

## Task 4: Explicit managed temporary lifecycle
- [x] Finalize additive EditFiles release contract against existing mutation receipts and permissions.
- [x] Add regression cases for scope, active runtime, revision/generation conflict, retry and precise partial outcomes.
- [x] Implement scope-bound read generations and runtime-owned release, including all accepted review fixes.
- [x] Close all four independent review findings; validate release/capacity behavior and obtain independent re-review with no open material findings.

## Task 5: Integration and operation
- [x] Document supported behavior, limits, evidence and remaining deferred ideas.
- [x] Run full just check on the integrated source and obtain independent review of all four components.
- [x] Inspect the versioned installer and live rollout constraints; preserve existing processes and their executable inodes.
- [ ] Commit the reviewed changes, then build and smoke-test the clean committed three-executable bundle. This ledger is the pre-commit validation snapshot; Git and CLI output record these final delivery actions.

## Progress and rulings
Base: 7a4f3c8e56b0ed550d3578d3234f61657303d322.
Branch: feat/workflow-operations.
Worktree: /home/gabrielmaia/.cache/agents/2026-10-27-winx-workflow-operations.
Baseline cargo test --all-features --locked: exit 0; 926 passed, 0 failed, 23 ignored. Winx bg e1411ff2e25ec0b7.
Ruling: The user accepted the previous response's first four priorities with “Vamos la entao”; implement those without another approval loop. Structured backend activity events, polling redesign and receipt-cap expansion remain measurement-driven follow-ups.
Ruling: Split independently testable components and finalize their details at their boundary rather than blocking telemetry on backend integration research.
Ruling: Keep usage_schema=1 and add workflow_schema=1 for additive workflow fields; no legacy field changes meaning.
Ruling: Root owns remote source, test runs and commits. Review artifacts stay in local scratch; the git-tracked plan is the durable ledger because the remote helper directory is at its 128-file cap.
Ruling: Current live processes are still 0.2.355+geaa5fcc4f68f, launched under a terminal scope, not a dedicated service unit. Preserve active PTYs and other chats during rollout.
Ruling: Preserve existing terminal-emission coverage. Existing post-start error exits using `?` can have no tool_call; start without terminal means completion not observed, not proof of a hang. Exactly-once assertions apply to paths already emitting terminals.
Ruling: Recovery uses only recognized explicit UUID launch selectors, classified explicit_target with source=launch_argument and current_conversation_verified=false. No transcript scanner or backend-confirmation claim.
Ruling: Cleanup must execute at the authoritative runtime owner under its operation barrier. Maintenance sites unable to prove idle/reference protection become audit-only; they must not prune other sessions speculatively.
Task 1 RED: 8 intended failures, existing workflow cases passed. GREEN after implementation: 56 library + 3 report tests passed; bg f259279c8097c9e8 exit 0.
Task 3/4 RED: library compiled; 691 passed, 9 intended failures (8 recovery, 1 release wire). Busy/background preservation already passed. bg 907bc5b64c848764 exit 101.
Task 2 RED: after correcting a test-only json! indexing syntax error, three behavioral failures reproduced the missing lifecycle instrumentation and selector behavior; bg 845aebbca6604702.
Task 1–3 integrated GREEN: 89 library + 1 workflow integration + 6 report tests passed, 1 existing ignored; bg 6fdc8a6fbd0bbf34 exit 0.
Task 3 review: real-PTY cwd drift and ambiguous launch classification reproduced before their fixes (bg ef7c790e43573e0a), then passed in the integrated library run. The independent reviewer closed both P2 findings.
Integrated library check: 718 passed and one temporary-release test-fixture thread-normalization failure; bg 826a0ef9f8c96231. Normalize the fixture exactly as production does; do not relax scope validation.
Task 4 review RED: bg 7cf0cca4531f1573 compiled and produced six intended regression failures across four findings: quarantine namespace writes/aliases, retained-output leaf aliases, incomplete recovery receipts, and unbounded/uncertain release RPC. The corrected real-background-execution test passed.
Ruling: Initial explicit release is supported only on Linux through a capable durable guardian; embedded/external or older owners remain unsupported without resetting active PTYs.
Ruling: Reserve the private claim namespace for all ordinary edit/undo paths, including resolved aliases. Bound retained-backing-tree inspection and fail closed when references cannot be ruled out.
Ruling: Persist typed complete release outcomes only when removal or quarantine retention changed state. Ordinary side-effect-free denials remain retryable after fresh inspection. Preserve absent optional fields in older receipts.
Ruling: A release RPC gets one bounded disposable connection. Deadline or transport loss is an unknown outcome, with inspection guidance and no automatic mutation resend.
Task 4 supplemental Undo RED: the first fixture used a non-normalized identifier and was corrected before relying on it. With a valid thread and a specific reserved-namespace assertion, bg d8c6c94bd6099c11 reproduced the missing denial.
Task 4 GREEN: all eight focused namespace, retained-reference, full-outcome, RPC and real-background regressions passed; bg 2db63d2276591648. Independent re-review closed the two P1 and two P2 findings without expanding the supported-owner boundary.
Integration fixture correction: the HTTP usage test previously selected the first ReadFiles event, which can now be tool_started. It now selects the terminal tool_call explicitly and checks its correlated lifecycle start while preserving content-free logging assertions.
Final repository gate: bg 4d0fe136dd230cc5 exit 0. cargo fmt --all -- --check; host Clippy --all-targets --all-features --locked -D warnings; Windows GNU target Clippy with the same flags; full cargo test --all-features --locked; cargo +1.90.0 check --all-features --locked all passed.
Final test totals: 980 passed, 0 failed, 23 pre-existing ignored (28 test groups); baseline was 926 passed. The normal suite includes 726 library tests, the real-PTY recovery/ownership regressions, and HTTP discovery/isolation/telemetry tests. git diff --check also passed.
Review disposition: workflow telemetry, lifecycle instrumentation, explicit conversation recovery and guarded temporary release have no open material findings. Windows was cross-compiled/linted; the release implementation deliberately remains Linux-only.
Delivery sequence: create the Conventional Commit from this reviewed tree, build all three siblings with --profile dist --locked --bins, and inspect their build identities before any activation. Do not reset old guardians to make new capabilities appear.

## Preflight
| Tasks | Shared file/interface | Ruling |
|---|---|---|
| 1,2 | usage.rs and report/workflows.rs | Task 1 freezes finite metadata; Task 2 adds summaries without changing counters. |
| 2,4 | tool_dispatch.rs and session admission | Root integrates phase guards around existing authorization and mutation boundaries. |
| 3,4 | scoped runtime/session state | Recovery is advisory; cleanup requires authoritative idle/reference checks. Neither weakens execution identity. |
| 1 | metadata vs tests | Test actual response envelopes, unknowns and repeated observations. |
| 2 | lifecycle vs report selection | Auxiliary events cannot change legacy selected-event denominators. |
| 3 | explicit target vs certainty | Persist provenance; do not claim backend confirmation without evidence. |
| 4 | file identity vs retries | Revision plus physical-generation binding and existing mutation receipts protect replacement. |
| 5 | rollout vs ongoing sessions | Build and smoke-test before activation; no PTY kill as an upgrade shortcut. |
