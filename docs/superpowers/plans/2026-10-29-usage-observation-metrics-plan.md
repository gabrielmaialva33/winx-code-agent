# Usage Observation Metrics Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make live usage diagnostics accurately describe observe batches, resolved Bash waits, and ReadFiles request shapes.

**Architecture:** Extend the current usage emitter and offline report with optional, finite metadata. Reuse typed parsing and actual wait normalization; preserve runtime and response behavior.

**Tech Stack:** Rust 2021, serde_json, tracing, rmcp, existing test utilities.

**Spec:** docs/superpowers/specs/2026-10-29-usage-observation-metrics-design.md

## Global Constraints
- Rust 2021; MSRV 1.90; no new dependencies.
- No new raw command, output, path, cursor, prompt or identity values are logged.
- No execution, read-budget, read-coverage, authentication, or principal-isolation changes.
- Preserve existing requested wait and error_results semantics; historical missing fields remain unknown.
- Run just check before any commit.

## Review Focus
- Legacy logs with batch_items=0 must not be interpreted as measured empty observe batches.
- Partial observe items with multiple errors must count affected items once.
- Immediate Tasks and special Bash actions must not inherit an inapplicable 15-second allowance.
- Read range classification must follow existing parser behavior, including colons, open bounds, duplicates, and malformed input.
- Telemetry additions must not expose argument values or alter routing and recovery.

## Task 1: Usage metadata and regression coverage
**Files:** src/server/usage.rs; new src/server/usage/observations.rs; src/server/handler.rs; focused usage/handler tests.
**Interfaces:** Emit observe_schema/observe_requested_items/observe_returned_items/observe_items_with_issues; read_request_schema/read_whole_file_requests/read_bounded_range_requests/read_open_ended_range_requests; effective_wait_policy/effective_wait_ms/effective_wait_mode.
- [x] Add emitted-JSON regression tests before production changes. Observe fixtures request four entries and return three, including one entry with two errors; assert 4/3/1, batch_items=4, and unchanged partial status.
- [x] Add read-shape fixtures using whole files, :10-20, :-20, :20-, :20, duplicate entries, colon names, malformed arrays, and non-ReadFiles lookalikes.
- [x] Add wait tests with literal expected allowances: sync adaptive 15000 ms, task inline 2000 ms, return_early 250 ms, sync until_complete 60000 ms; explicit zero, fractional 125 ms, caps 5000/2000/60000, special exclusions, immediate Task zero.
- [x] Run focused tests and verify intended failures.
- [x] Implement request/result metric extraction and actual normalized wait hooks, with no duplicated policy defaults or speculative Bash deserialization.
- [x] Run focused tests and a handler-level call route check; all pass.

## Task 2: Offline report and documentation
**Files:** src/report.rs; new src/report/observations.rs; tests/usage_report_test.rs or a focused new integration test; docs/agent-workflows.md.
**Interfaces:** Consume Task 1 metadata; add public reads/observations summaries and effective wait distributions, with explicit known/unknown counts.
- [x] Add public report JSON tests before implementation: modern/legacy/malformed records, tool/action gating, partial results, saturation, and preserved requested waits.
- [x] Run these tests and verify intended failures.
- [x] Implement bounded numeric aggregation and documentation explaining configuration versus elapsed time and observations versus unique processes.
- [x] Run report and usage tests; all pass.

## Task 3: Review and completion
- [x] Inspect diff and obtain independent fresh review of the whole change.
- [x] Triage review findings: none at Critical, Important, or Minor severity; no corrective patch required.
- [x] Run just check (format, host and Windows clippy, full tests, MSRV).
- [x] Commit the complete verified change with a Conventional Commit subject and verify clean worktree.

## Execution ledger
- Base: f0c56aeaf0c9cc86506b4b4dd49008a468141af7, canonical main clean.
- Worktree: /home/gabrielmaia/.cache/agents/2026-10-29-winx-usage-observation-metrics.
- Baseline: cargo test --all-features --locked --lib -> 761 passed, 0 failed.
- Root performs canonical operations through Winx; independent agents review local exports/proposals.
- Pre-flight: Tasks 1 and 2 share only the documented optional log-field contract.
- Scope decision: read-volume work adds classification and reporting; budget/coverage changes require evidence from these metrics.
- Task 1: complete. Usage regression RED: 4 intended failures; GREEN: 6/6. Real HTTP regression RED: effective_wait_policy absent; GREEN after handler hook.
- Task 2: complete. Public report RED: 5/5 failed for missing fields; GREEN: 5/5 new and 16/16 existing report tests.
- Verification: 770/770 library tests and real HTTP usage/privacy test passed after implementation.
- Ruling: a failed Task promotion after successful inline execution retains the selected inline allowance; failures before execution leave it unknown. This avoids hiding a known allowance but readers must not interpret it as elapsed time.
- Full gate: just check exited 0. Format, host Clippy, Windows GNU Clippy, 1,040 passing tests (0 failures, 23 ignored), and Rust 1.90 checks passed.
- Lint resolution: fixed doc-markdown formatting and narrowly scoped a too_many_lines expectation to the compatible, atomic flat usage event; no broad lint suppression.
- Independent whole-change review: Ready, with no Critical, Important, or Minor findings. Reviewer checked the complete 13-file diff and relevant parser/normalizer context; root owns execution and final git-state verification.
- No minor findings deferred. Runtime rollout remains a separate integration step; this change does not claim measured token savings or lower latency.
