# Usage metadata and backend discovery implementation plan

> **For agentic workers:** Use superpowers:executing-plans task-by-task.

**Goal:** Repair usage metadata and expose safe Herdr configuration hints.
**Architecture:** Extend usage extraction and Initialize metadata; preserve legacy layouts and authorization.
**Tech Stack:** Rust 2021, serde_json, tracing, rmcp; MSRV 1.90.
**Spec:** docs/superpowers/specs/2026-10-30-usage-capability-signals.md

## Global constraints
No new dependencies, raw telemetry arguments, secrets or permissions. Preserve unrelated work and sessions. The initial implementation excluded deployment, cleanup, commit and push; the user subsequently authorized a commit and local integration into main. Push and deployment remain excluded. Base c2a1f1d; branch fix/usage-capability-signals.

## Review focus
Canonical root versus nested metadata; zero/false versus absence; malformed types; config presence versus validated availability; old logs without schema.

## Tasks
- [x] Verify baseline library suite.
- [x] Add emitted-JSON tests in src/server/usage/tests/result_metadata.rs and observe RED.
- [x] Correct usage_result_metadata and temporary types; preserve overflow tests. Verify GREEN.
- [x] Add backend discovery/disabled-recovery regressions and observe RED.
- [x] Add no-IO hints to Initialize and disabled Herdr responses. Verify GREEN.
- [x] Extend real HTTP usage/privacy regression for CodeMap and temporary measurements.
- [x] Document fields and limitations; review diff and run just check.
- [x] Record evidence and final git/runtime state.

## Implementation evidence (before integration)
- Baseline: 770 library tests passed, zero failures.
- Telemetry regressions before correction: 5 failed as expected and 2 compatibility cases passed. After correction: all 7 passed.
- Herdr HTTP discovery before correction: both tests failed on missing metadata. Both passed in the final full suite.
- Added 10 tests total; strengthened the existing real HTTP usage/privacy test with CodeMap and optional temporary measurements. Uncertain prompt delivery remains distinct from a disabled backend.
- Full gate: just check exited 0; 1,050 tests passed, 0 failed, 23 existing ignored tests. Formatting, host Clippy, Windows GNU cross-target Clippy, and Rust 1.90 checks passed. Runtime tests ran on Linux, not Windows.
- Verification log: /home/gabrielmaia/Projects/personal/winx-code-agent/.winx/tmp/session-9ae711654df1dec1/fix-check.log. Related logs: fix-baseline.log, fix-red.log, fix-green.log, fix-herdr-red.log in the same directory.
- Reviewed the complete tracked diff and new test modules directly. No independent reviewer was run.
- Worktree retains uncommitted changes on fix/usage-capability-signals. Main remains clean at c2a1f1d1a74d7f9320ccecc7e641b10fe2d87a73. No commit, merge, push, configuration changes or deployment.
- HTTP PID 173695 still uses the installed 0.2.357+gc2a1f1d1a74d binary. These fixes are not yet active in that service.
- Ruling: polling optimization is deferred to a separately measured change; this patch changes telemetry and configuration hints only.

## Integration verification (2026-10-30)
- User authorized bringing the changes to main; preserve the running service and do not push.
- Fetched origin: main and origin/main both remained at c2a1f1d before integration.
- Independent Codex read-only review found no actionable defects and recommended merge conditional on a passing full gate. Report: merge-review.md in the session temporary directory. The reviewer did not execute tests.
- First fresh pre-merge gate failed in daemon_cancel_uses_a_channel_independent_of_pending_status_output: the interrupted shell did not return to a prompt within three seconds. Full details retained in merge-precheck.log.
- No runtime or daemon-test changes are part of this patch. Without changing code, deadlines or assertions, five isolated repetitions passed and the full daemon_rpc_test suite passed 24/24. Log: merge-daemon-recheck.log. The initial timeout was not reproduced; its underlying cause is not established or claimed fixed.
- Second full pre-merge gate: just check exited 0 with 1,050 passing tests, zero failures and 23 ignored tests; formatting, host/Windows GNU Clippy and Rust 1.90 passed. Log: merge-precheck-retry.log. No test exclusions or assertion/deadline changes were introduced.
- Integration procedure: commit only the reviewed patch, fast-forward main, and re-run just check on main. Preserve the worktree and installed service; no push or release installation.
