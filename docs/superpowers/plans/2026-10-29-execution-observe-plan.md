# Execution observation implementation plan

> Goal: one bounded, read-only Execution observation call for multiple executions.
> Base: feat/plugin-workflows at 2b7eea18a1a0b16b326b7447a6d2b1c53980c82f.
> Prior log fixes remain the parent commit. No merge, push, or running-service replacement is included.

## Implementation tasks

1. Add public-wire regressions for the new action and prove RED against the current parser.
2. Implement bounded request validation and a single catalog observation transaction.
3. Add one-pass server orchestration with a shared deadline, compact responses, isolated errors, and explicit typed result status.
4. Extend discovery, documentation, and finite usage/report classification.
5. Run focused regression suites, independent review, and just check; commit the completed feature.

## Progress ledger

- [x] Read repository guidance and inspect execution, history, pagination, finalization, and usage paths.
- [x] Create isolated worktree based on the completed log-diagnostics fixes.
- [x] Baseline: cargo test --all-features --locked --lib --quiet: 744 passed.
- [x] Design review: global shape/resource validation; per-item operational errors; coherent final snapshot; shared deadline.
- [x] RED: five new public tests compiled and failed for the missing observe variant.
- [x] RED: three usage/report tests failed because observe was classified as unknown.
- [x] GREEN: catalog/server behavior, telemetry, discovery, and docs implemented.
- [x] Focused verification: 26 library observation tests, 19 existing history tests, and 10 new catalog tests passed.
- [x] Independent final review: no Critical, Important, or Minor findings; approval conditional on just check.
- [x] Required just check gate: formatting, host/Windows Clippy with warnings denied, all tests, and Rust 1.90 MSRV passed.

## Decisions

- Default page size 2048 bytes; minimum 4 bytes preserves UTF-8 progress.
- One final transaction supplies metadata, pages, and catalog cursor.
- Fixed execution_observation_issues top-level code makes partials visible in existing report counters.
- Deadline includes admission and I/O; no lifecycle wait metrics for observation.
- Only root uses Winx and mutates the canonical worktree; helpers prepare/review local proposals.

## Final review decisions and gate findings

- Ruling: retain existing refresh behavior for unavailable inspections and failed full captures — unknown and output_pending communicate incomplete evidence without inventing success; callers must inspect those fields.
- Ruling: retain existing bounded blocking catalog workers after a response timeout — the deadline bounds response latency, while already-started I/O may briefly outlive it; documentation states this limitation.
- Ruling: the 65536-byte budget covers raw page text — JSON, metadata, cursors, and redaction can increase wire size; this is not a token or billing guarantee.
- Ruling: deployment and connector catalog refresh remain outside this implementation — the source feature is reviewable and tested here, while using it through the currently connected plugin requires publishing a newer server/catalog.
- No deferred minor review findings.
- First just check stopped on test-only Clippy diagnostics. Fixed assertions, helper placement/ownership, and oversized composed test futures without suppressing lints.
- Second just check passed host and Windows Clippy; the full library suite exposed the existing 4000-byte description budget (4118 bytes). Shortened only the Execution description, keeping the same budget and full schema/docs. The existing failing catalog test is the regression proof for this correction.

## Final verification

- Full gate: `env -u WINX_USAGE_LOG -u WINX_USAGE_LOG_ROTATION -u WINX_USAGE_LOG_KEEP_DAYS CARGO_TARGET_DIR=/home/gabrielmaia/Projects/personal/winx-code-agent/target CARGO_BUILD_JOBS=8 just check` exited 0.
- 1026 tests passed, 0 failed, 23 existing opt-in tests ignored; 27 new tests were added by this feature.
- Existing catalog-budget regression passed with 3997 description bytes; its 4000-byte limit and 128 KiB serialized-catalog limit remain unchanged.
- The first description-shortening attempt still exceeded the limit by 4 bytes; its owned background gate was stopped, the text was shortened again, and the complete gate above passed.
- Reviewed outcome: no unresolved findings or deferred minor issues.
- Commit subject: `feat: add bounded multi-execution observations`. The branch and worktree remain available for integration.
