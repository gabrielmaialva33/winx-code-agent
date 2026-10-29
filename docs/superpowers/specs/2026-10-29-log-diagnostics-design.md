# Log-based diagnostics and recovery design

## Intent and evidence

Improve Winx's correctness and diagnosability based on the 2026-10-29 usage audit. The current build produced 26 committed, persisted edit receipts but logged edit_applied=false for 25 normal edits because the field was absent. Two measured remote requests spent hundreds of seconds before the instrumented server window. The newest 288 real calls also included missing/mismatched workspace identity, while conversation identity was unavailable.

The user approved implementing the improvements after reviewing these findings. The implementation choices below are engineering decisions for that authorized scope.

## Approach

Use focused contract fixes and bounded diagnostic events within the existing Rust implementation. A flag-only patch would leave transport and identity ambiguity unresolved. A separate observability stack or transport timeout/protocol changes would add operational work without evidence of the delay's cause.

## Requirements

### Applied edit state

- Mark edit_applied=true only at the successful typed Apply/Undo result boundary, including committed partial outcomes and verification failures after an edit.
- Preserve existing replay, postcondition drift, undo and temporary-release semantics; do not infer file edits from a generic mutation receipt.
- Telemetry distinguishes explicit true, explicit false and unknown. Missing, null and malformed legacy values remain unknown.
- Do not add receipt storage fields or replay any mutation to repair historical telemetry.

### HTTP lifecycle

- Add http_request_received at entry to the MCP security middleware, before admission waits, using a server-generated request_id.
- Preserve the existing http_request event's duration_ms meaning and accepted-request cardinality.
- Add at most one http_response_body event for complete, error or dropped, with request_elapsed_ms and body_duration_ms.
- Correlate existing admission rejection events with the entry ID.
- Observe empty bodies, end-stream hints, explicit EOF, errors and drops without changing frames, trailers, error delivery, backpressure or permit lifetime.
- No frame/poll logging, response buffering, raw commands, bodies, query strings, credentials, session identifiers or error text.
- Entry is not TCP accept or client start. Complete is not socket flush or client receipt. Drop is not proof of client cancellation. Abrupt process termination can leave incomplete traces.
- Report selection must ignore the new auxiliary lifecycle events before applying --last, with and without a build filter.

### Identity and recovery

- Log explicit availability of conversation identity and the scope of correlation (conversation, workspace or request).
- Scope describes supplied telemetry evidence, not successful authorization; workspace_coherence remains authoritative about validation.
- Reuse the exact conversation-header precedence already used by principal binding, including present-but-invalid higher-priority headers.
- Preserve principal scoping, affinity, path policy and session selection. No latest-session guessing, silent rebinding, or inferred chat identity.
- Workspace error guidance first points to reusing the exact initialized pair for the intended project. Any existing Initialize fallback remains a first_call for the requested project without the rejected thread or reset.
- Expose only information already authorized by current error handling.

## Constraints

Rust 2021; MSRV 1.90.0; no new dependencies; unsafe/libc only in src/os.rs. All canonical reads and edits use Winx. Keep the initialized workspace pair and the unrelated foreground shell intact. Run tests without inheriting the production usage log. AGENTS.md requires just check before any commit.

## Acceptance

Regression tests demonstrate false-versus-unknown and actual edit/replay/undo results; HTTP lifecycle ordering and body/permit semantics; report window stability; header-precedence consistency and safe recovery. Existing full checks pass. Documentation states what the observations can and cannot prove. This change does not claim to fix the still-unlocalized delay before middleware entry.
