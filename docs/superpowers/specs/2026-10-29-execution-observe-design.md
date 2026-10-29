# Bounded execution observation for plugin clients

## Goal

Let a chat client obtain compact status and independently paginated output for several executions in one Execution call. This builds on retained execution history and does not run commands or send terminal input.

## Contract

Add Execution.action.type=observe with items and optional timeout_seconds. Each item has execution_id, source (default output), cursor (optional), and page_bytes (default 2048). Accept 1–32 items, 4–65536 bytes per page, and at most 65536 requested page bytes in total. Reject duplicate execution/source pairs and malformed IDs before session admission or catalog/runtime I/O. The raw-page budget does not include JSON, cursors, metadata, or redaction expansion.

Refresh each distinct eligible execution once using existing identity-bound inspection and output_pending behavior. A refresh may include a metadata inspection and a separate capture. Runtime observations are not atomic across processes. Read final metadata, pages, and catalog_cursor in one catalog transaction, preserving caller order. Catalog cursor is compatible with Execution.wait; each page cursor remains compatible with Execution.read.

Return compact execution metadata, an optional page, and a finite errors array per item. Preserve metadata on page failure and preserve both refresh and read failures when both occur. Missing executions, invalid cursors, and payload failures affect only their items. Catalog-wide failures and timeouts fail the whole request without a success cursor. Unknown process state and pending output stay explicit.

Use completed for all successful items and completed_with_issues with the fixed execution_observation_issues code for partial results. Structured pages contain output once; no concatenated text content or mirrored top-level output. Existing recursive finalization redacts the structured response. No command labels, paths, runtime tokens, or prompt receipts in the compact metadata.

One absolute deadline covers session admission, capability lookup, refreshes, and final catalog I/O: default 5 seconds, finite range 0–30 seconds, zero grants a 250 ms operation budget. Observation is one pass without lifecycle polling or deliberate waiting. A deadline bounds response waiting; an already-started blocking catalog worker completes under existing storage lock bounds.

## Integration

Keep Execution's existing policy, principal/workspace scoping, read-only annotation, closed-world annotation, and cursor/storage formats. Extend generated discovery, tool description, workflow docs, and finite telemetry/report action classification. Observation timeout is not a lifecycle wait and must not populate wait or prompt metrics.

The currently connected plugin catalog exposes fewer tools than the source server. Server behavior and discovery can be validated here; using the new action through this connector requires a deployment/catalog refresh outside this change.

## Verification

Public wire regressions first demonstrate the missing action. Cover mixed exits, caller order, independent cursors, partial errors, preflight before locked session access, operation deadline, legacy delivery preservation, output redaction/duplication, scope, final snapshot consistency, retention/resynchronization, missing payloads, finite telemetry, and report grouping. Run focused tests, an independent final review, and the repository-required just check before committing.

## Scope

This slice does not add arbitrary code evaluation, command batching, lifecycle wait composition, new permission paths, or a second terminal runtime. It builds on ideas from Desktop Commander, Serena, terminal-mcp, and code-mode systems while keeping the existing execution contract.
