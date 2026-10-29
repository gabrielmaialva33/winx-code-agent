# Log Diagnostics Implementation Plan

> **For agentic workers:** Use superpowers:executing-plans for canonical implementation; independent agents review focused designs and the final branch. The root owns all Winx calls.

**Goal:** Correct edit-state telemetry and make transport and workspace failures actionable without changing authorization or execution semantics.

**Architecture:** Typed edit results declare their state; logging preserves optionality. HTTP middleware records bounded lifecycle events, and identity telemetry uses the same header-selection rule as binding. Existing report selection treats lifecycle events as auxiliary.

**Tech Stack:** Rust 2021, Tokio, Axum, http-body, tracing, serde_json, existing test fixtures.

**Spec:** ../specs/2026-10-29-log-diagnostics-design.md

## Global Constraints

- Rust 2021; MSRV 1.90.0; no new dependencies; unsafe/libc only in src/os.rs.
- Canonical workspace changes only through Winx; all shell commands are background with explicit cwd.
- Preserve authorization, session affinity, mutation idempotency and permit ownership.
- Do not inherit WINX_USAGE_LOG into test processes.
- Run focused RED/GREEN regressions, then the repository's complete just check gate before committing.

## Review Focus

- Partial edits and temporary releases must not be conflated with complete fresh writes.
- Body end-stream hints may prevent a final EOF poll; empty responses still need a terminal observation.
- A body error or repeated poll/drop must never duplicate terminals or release the permit early.
- Empty/invalid high-priority conversation headers must not make telemetry disagree with binding.
- Auxiliary HTTP events must not evict real calls from bounded, build-filtered reports.

## Task 1: Applied edit state

**Files:** src/server/tool_dispatch.rs, src/server/usage.rs, src/server/handler.rs, src/server/tests/daemon_edit_tests.rs.
**Interface:** existing structured data.edit_applied remains optional boolean; UsageRecoveryMetadata.edit_applied becomes Option<bool>.

- [x] Add emitted-JSON tests for explicit true/false, absent/null/malformed, wrapped results, release and non-edit calls.
- [x] Add a real daemon edit-without-verification test covering persisted receipt, replay and undo.
- [x] Run focused tests and observe missing flag / invented false failures.
- [x] Mark successful typed Apply/Undo results with true; preserve unknown in logging; update typed assertions.
- [x] Run the focused tests and existing edit/recovery tests.

## Task 2: HTTP lifecycle

**Files:** src/http_server.rs, src/http_server/observability.rs, src/http_server/observability_tests.rs, src/report/selection.rs, tests/usage_report_test.rs, docs/streamable-http.md, docs/streamable-http.pt.md.
**Interface:** http_request_received and http_response_body are auxiliary events; http_request semantics unchanged. Terminal outcome is complete/error/dropped with request_elapsed_ms/body_duration_ms.

- [x] Add real middleware/log-capture tests for entry-before-admission, pending stream drop, empty body, final data frame, trailers/EOF, body error and admission rejection.
- [x] Add report regression preserving the two-event tool/HTTP window despite auxiliary lifecycle events, including a build-filtered case.
- [x] Run these tests and observe failures caused by missing events/window eviction.
- [x] Add bounded, privacy-safe lifecycle tracking; keep the admission permit until body Drop.
- [x] Exclude the auxiliary events from selection before --last.
- [x] Run focused HTTP/report tests and document the measurement boundaries.

## Task 3: Identity diagnostics and recovery

**Files:** src/server/usage.rs, src/server/principal.rs, src/server/outcomes.rs, src/errors.rs, relevant existing test modules, docs/streamable-http.md, docs/streamable-http.pt.md.
**Interface:** additive conversation_identity_available and correlation_scope fields; existing workspace_coherence and nextAction tool contracts retained.

- [x] Add tests for missing conversation, supplied workspace, explicit conversation, and invalid higher-priority header shadowing a valid fallback.
- [x] Add recovery assertions proving no rejected thread/reset is propagated and the binding remains unchanged.
- [x] Run focused RED tests.
- [x] Share or align header selection without altering precedence, add explicit telemetry, and improve structured recovery guidance.
- [x] Run focused identity/coherence/recovery tests.

## Final verification and review

- [x] Run cargo fmt and required just check (format, host/Windows clippy, complete tests, Rust 1.90.0).
- [x] Independent whole-branch review; fix material findings with reproducing tests.
- [x] Check diff, clean scope and final test evidence; prepare verified changes for commit on fix/log-diagnostics.
- [x] Record changes, evidence and the remaining upstream latency uncertainty. Integration/release follows the user's next instruction.

## Execution ledger

Base: ce410cd4b101020cbec9779d6109894c8f157d66.

Pre-flight: Tasks 1 and 3 share usage.rs; root serializes canonical edits and refreshes revisions. Task 2's new events require its report-selection regression.

Baseline: cargo test --all-features --locked --lib yielded 725 passed, 1 failed before changes. daemon::transport::unix_claim_tests::stale_socket_from_a_dead_daemon_is_replaced failed with AddrInUse in its temporary socket fixture; an immediate isolated exact rerun passed. No production fix is attributed to this pre-existing intermittent failure. The final complete gate subsequently passed.

Ruling: Use the existing external worktree layout and root-owned Winx edits, with local read-only source excerpts for independent reviewers. This avoids competing access to the live foreground shell and does not require another initialization.

Ruling: Preserve absent edit state as unknown and set true at the typed Apply/Undo boundary; keep receipt storage and temporary-release paths unchanged.

Ruling: Keep the existing authorization behavior for present-but-invalid higher-priority conversation headers. Telemetry must agree with it; this work does not introduce a new authentication fallback.

Task 1: RED reproduced missing normal-edit applied state and absent telemetry becoming false. GREEN passed all 219 server library tests, including real edit, persisted receipt replay and undo. No receipt schema changes.

Task 2: Seven lifecycle tests compiled and failed for missing events. The report regression reproduced auxiliary events displacing the selected tool/HTTP pair under --last. The implementation adds bounded entry/body observations and excludes them from report selection. Both regression groups now pass.

Task 3: Five JSON/recovery tests reproduced missing fields; the binding-preservation and principal-isolation guards passed before production changes. The header discrepancy was established by source inspection; its precedence test was added after extracting the shared helper, not claimed as a pre-change runtime failure. Structured errors use binding_changed=false, operation_executed=false and conditional recovery guidance without changing session selection.

Focused GREEN: cargo fmt --all, git diff --check, all 242 server/HTTP library tests and all 16 usage-report integration tests passed. Updated English/Portuguese documentation records measurement boundaries, optional edit state and safe recovery.

Final gate: just check exited 0 on the final formatted code. Host clippy and x86_64-pc-windows-gnu clippy passed with warnings denied; cargo test --all-features --locked reported 999 passed, 0 failed and 23 existing ignored tests across 28 suites, including 744 library tests; cargo +1.90.0 check --all-features --locked passed. Test processes did not inherit the production usage-log configuration. Routine per-test success lines were filtered from command output; all suite summaries, failures, diagnostics and exit status were retained.

Final independent review: no Critical, Important or Minor findings in the complete change or its lint follow-ups. The final entry-helper extraction preserves timer, method, server-generated ID, logging and admission order. Production lint rules were not relaxed; no dependencies were added.

Handoff: retain fix/log-diagnostics and its worktree for integration. The implementation improves diagnosis of the unlocalized delay outside the instrumented server window; it does not claim to remove that delay. No main integration, push or production restart is part of this batch.
