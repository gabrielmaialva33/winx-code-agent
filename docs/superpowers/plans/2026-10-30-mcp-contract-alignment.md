# MCP contract alignment implementation plan

> **For agentic workers:** Execute task-by-task with failing behavioral tests before production changes. Do not commit or deploy.

**Goal:** Align the advertised plugin contract and effective waits while preserving optimized automatic defaults.
**Architecture:** Narrow changes in WaitBudget/handler normalization and follow-up generation, additive effective-wait result data, corrected schema descriptions, and bounded integration tests. Reuse existing result envelopes, policy checks and telemetry.
**Tech Stack:** Rust 2021, existing rmcp/schemars/serde_json/tracing test infrastructure; Rust 1.90 minimum.
**Spec:** docs/superpowers/specs/2026-10-30-mcp-contract-alignment.md

## Constraints and ownership
Base c9b8465; branch fix/mcp-contract-alignment. Preserve unrelated changes and all running sessions. No commit, push, merge, release installation, service restart, permission changes or dependency upgrades. Test output stays in WINX_TEMP_DIR with stable contract-* names. Build/test parallelism bounded to 3/4; no subagents or recursive reviews. Coordinator owns the standards audit document and final full gate.

## Review focus
- Explicit zero and return_early must not be silently promoted, including nextAction.
- Null equals omission where the advertised schema allows it; malformed/negative/nonfinite inputs fail before launching.
- Profile defaults do not become authority or leak client/config secrets; stdio and background keep their contracts.
- JSON tool error results retain output schema and actionable information; unknown tool and malformed RPC remain protocol errors.
- Task negotiation and receipt verification never acquire a foreground policy they cannot support.

## Task 1 — Wait behavior and validation
Files: src/server/wait_budget.rs, src/server/handler.rs, src/server/handler/tests/wait_metrics.rs and focused tests as needed.
- [x] Record baseline attempts and environmental limits; do not claim a clean baseline (see evidence).
- [x] Add failing behavioral regressions for return_early, short/zero waits, continuations, null and negative values.
- [x] Correct normalization without removing client defaults/ceilings or negotiated Tasks.
- [x] Preserve selected policy, wait and events in continuations, including background launches/polls.
- [x] Run focused tests and the complete project gate.

## Task 2 — Published contract and result evidence
Files: src/server/catalog.rs, src/server/workflow_types.rs, src/types.rs, src/server/handler.rs, src/server/outcomes.rs, src/tool_registry.rs and tests.
- [x] Prove invalid wait input does not launch the command using an HTTP marker fixture.
- [x] Add effectiveWait to synchronous results and adaptive Task-inline completions using one helper.
- [x] Correct optional-null handling, numerical constraints, descriptions and conservative Initialize risk hints.
- [x] Validate actual HTTP inputs/outputs and nextAction against the published JSON Schemas.

## Task 3 — Documentation and handoff
- [x] Correct host-visibility assumptions without changing deduplication or its configurable fallback.
- [x] Document explicit-value precedence, observation versus cancellation, and interoperability trade-offs.
- [x] Perform independent read-only review, reproduce its two findings, fix and verify them.
- [x] Record final state; no commit, merge, push, release installation or service restart.

## Verification evidence
All logs are under the session WINX_TEMP_DIR, not tracked source. Agent logs were moved intact from contract-tmp to `contract-agent-evidence/` after that worker finished.
- Agent baseline: 793 passed / 31 failed; the sandbox denied socket creation, including mock Unix sockets. Post-change sandbox library run had the same 31 failures. These are not claimed fixed by this patch.
- A concurrent attempted outside baseline was invalidated: it shared CARGO_TARGET_DIR with the changing worktree and included newly added worktree tests. It is not evidence of the clean main baseline. Subsequent complete gates ran after the implementer stopped.
- Initial contract RED: four tests failed on policy/time promotion, continuation and null; the negative-wait regression also failed. Focused GREEN: 14 wait-budget, 3 metrics and 11 schema tests.
- Initialize annotation RED: one failure, one passing observation-hint test; corrected conservative annotation later passed in the full gate (`contract-annotation-red.log`).
- First coordinator gate stopped on Clippy warnings; corrected without adding lint suppressions. The next full gate passed 1,110 tests before the two review regressions were added.
- Independent review (`contract-review.md`) found background continuation wait expansion and missing effectiveWait for adaptive Task-inline completions. Both added regressions failed as expected in `contract-review-red.log`, then passed in the final gate.
- Final `just check`: exit 0; 1,112 passed, zero failed, 23 ignored. Formatting, host and Windows GNU Clippy with warnings denied, plus Rust 1.90 check passed. Log: `contract-check-verified.log`. Windows execution was not tested.
- Isolated HTTP probe: 9 tools, 10 responses; invalid-command marker absent. Fixtures: `contract-wire-fixtures.json`.
- JSON Schema Draft 2020-12 validation via cached `jsonschema 4.26.0`: 18 original schemas, 9 valid inputs, 1 invalid input rejected, 10 outputs and 1 nextAction; zero errors. No schema annotations removed; no dependencies installed. Evidence: `contract-schema-validation.json`.
- Final source branch: fix/mcp-contract-alignment based on c9b8465, changes uncommitted. Main remains clean at c9b8465; HTTP PID 3149 still uses 0.2.357+g87f85c9a243d. Only evidence documentation changed after the final full gate.
