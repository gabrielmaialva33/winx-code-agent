# Usage observation metrics design

## Intent
The 2026-10-29 live-log review found observe batches logged as zero items, omitted Bash wait defaults, and ReadFiles accounting for about 76% of recorded response bytes. Improve diagnosis with additive counters while preserving execution and file-reading behavior.

## Approach
Use the existing usage event and offline report. Read request classifications reuse the actual ReadFiles parser. Effective waits come from the handler's already normalized request and selected route. Observe counts are scoped strictly to Execution.observe.
Changing wait defaults or file budgets would change behavior without evidence of a defect; collecting raw paths or content would expand telemetry unnecessarily. The selected approach adds aggregate metadata only.

## Contract
- Keep usage_schema=1 and all existing fields and meanings; new fields are optional/additive.
- Execution.observe: batch_items counts action.items; observe_schema=1 marks trustworthy new counting. observe_requested_items is the array length when known. observe_returned_items and observe_items_with_issues count result entries and entries with nonempty errors, respectively. Missing/malformed results stay unknown; two errors on one item count as one affected item.
- ReadFiles: read_request_schema=1; read_whole_file_requests, read_bounded_range_requests, and read_open_ended_range_requests are emitted together only for a request accepted by the existing typed ReadFiles parser. An end bound means bounded; a start without an end means open-ended; no bounds means whole-file. Duplicate entries count separately. Invalid/empty requests have unknown shapes.
- BashCommand: preserve requested_wait_policy/requested_wait_ms exactly as today. Add effective_wait_policy/effective_wait_ms/effective_wait_mode for the selected inline allowance, not measured time or proof a command started.
- Modes are synchronous, task_inline, and task. Use actual normalized allowances; a successfully admitted immediate Task has zero inline allowance. Special actions screen/inspect/wait_for_turn/prompt use different semantics and do not receive these fields.
- Rejected requests before routing and protocol failures before inline execution have no committed effective allowance. A failed Task promotion after a completed inline execution retains that inline allowance. Structured results may record the selected allowance even when runtime work could not start; documentation must preserve that distinction.
- Reports expose read shapes, observe request/result counts, partial results, and effective wait distributions. Legacy or malformed fields count as unknown, never fabricated zero; existing error_results continues counting nonempty error_code.
- No new raw command, output, path, cursor, prompt or identity values are logged. No filesystem reads are added for telemetry.
- Rust 2021; MSRV 1.90; no new dependencies; all arithmetic in report counters saturates.

## Verification
Regression tests inspect emitted JSON and public report output, with literal fixtures for defaults/caps, special-action exclusions, unknown legacy data, item-level partial results, and privacy. A handler-level check must exercise a real call route. Run just check before committing.

## Delivery
Work on fix/usage-observation-metrics in an isolated worktree. Existing service and plugin keep running their installed build; deployment is a separate action.
