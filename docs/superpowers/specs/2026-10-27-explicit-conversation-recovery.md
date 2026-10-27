# Explicit conversation-target recovery — bounded final design

Design-only; no Winx/host operations or implementation performed by this reviewer. Baseline: 7a4f3c8.
Root's final ruling supersedes the earlier transcript/hook proposals: capture explicit selectors only; do not discover or confirm backend sessions.

## Outcome and scope

Persist the explicit conversation selector from a supported launch that Winx actually accepted.
Recover the recorded selector deterministically even if another interaction later becomes the backend's most recent conversation.
A recorded selector is an intended launch target, not evidence that the backend accepted it or kept that conversation selected.
No transcript scanner, hook transport, helper executable, new launcher, configuration mutation or agent restart belongs in this batch.
Surviving guardian reattach remains unchanged. Recovery never executes a command or automatically resends a prompt.

## Current source findings

- `src/state/agent_resume.rs:27-50` stores agent/command/cwd/time; current hints choose Claude/Codex latest, or replay the original command for other agents.
- `src/server/sessions.rs:375-400` misleadingly calls every hint a way to resume *its conversation*.
- Actual writer is `src/tools/bash_command.rs:438-465`; caller `:523-537` invokes it after execute_command when result.is_ok() and !is_background.
- Result::Ok must be checked against actual start/generation semantics before treating that as accepted. A busy/no-start result must not replace or clear an earlier record.
- Embedded full execution_token is assigned by an outer wrapper (`:390-395`), after the current inner record write; do not assume that token is available at the writer today.
- Herdr `tmp/herdr/src/agent_resume.rs:135-150` captures explicit Codex resume arguments; `:199-214` supplies literal Claude/Codex resume argv. Copy the bounded pattern, not Herdr automatic replay.

## Recovery contract

| Classification | Meaning | Executable advice |
| --- | --- | --- |
| `explicit_target` | Supported UUID selector recorded from an accepted launch | Fixed argv targeting that UUID |
| `best_effort` | Recognized backend with no safely captured selector | None in primary recovery advice |
| `unavailable` | Unsupported/ambiguous launch, invalid scope or unsafe record | None |

Every explicit target reports `source=launch_argument` and `current_conversation_verified=false`.
Never use `backend_confirmed`, `verified_session`, or an unqualified `exact` classification for supplied selectors.
Describe the target as: “The accepted launch explicitly selected conversation UUID X. This targets that recorded ID; Winx has not verified backend acceptance or the conversation currently selected in the TUI.”
A `/clear`, `/resume`, fork, human interaction, rejected UUID or backend startup error may change what actually happened; the persisted selector remains historical launch evidence only.
For best_effort, explain that directory-wide --continue/--last may select another interaction. Do not silently substitute either flag for the recorded UUID.
For unsupported agents, never return the original launch command as resume advice; it could repeat the initial task or create a new conversation.

## Supported selector grammar

Initially accept only unambiguous direct literal forms:

- `claude --session-id UUID`: records a new-conversation selector and demonstrates a practical new-interaction path.
- `claude --resume UUID` and `claude -r UUID`: records an explicit existing-conversation selector.
- `codex resume UUID`: records an explicit existing-conversation selector.

Strict UUID validation prevents flag-like values, session-name ambiguity, arbitrary paths and shell quoting hazards.
Normalize only UUID hexadecimal case if desired; do not invent another UUID, infer one from output, or replace it with a newer conversation.
Additional safe flags/quoted tokens may be supported only if the existing parser proves a single simple command and option semantics are unambiguous.
Refuse exact-selector capture for duplicate/conflicting options, --continue/--last, --fork-session, missing values, names/paths, wrappers, shell substitutions, pipelines, redirects, background operators and command lists.
Conservative capture refusal does not reject shell execution. Recognized plain unbound launch becomes best_effort; ambiguous launch produces unavailable/no executable advice.
Keep the broad existing detect_agent_launch heuristic separate; split(';')/split('&&') is insufficient proof for selector capture.
No new dependency or general shell parser is justified for these narrow forms; reuse an existing parser only if its AST retains command structure and substitutions.

## Additive record and binding

Preserve old fields for backwards deserialization, adding serde-defaulted optional fields:

- `conversation_selector: { id, kind: new_session | resume_target, source: launch_argument }`.
- Runtime-owned canonical workspace_root and scoped internal thread identity, not a caller-supplied visible thread string.
- Accepted execution binding: full ShellExecutionToken if available, otherwise explicitly optional rather than fabricated.
- Derived RecoveryAdvice with classification, reason, source, current_conversation_verified=false, optional argv and cwd.

Bind records to the actual runtime state at launch. Capture cwd/workspace before await if subsequent command execution can change state; do not use an unrelated later request's workspace.
At the accepted result boundary, persist only when a new foreground generation actually started. Accepted non-agent commands clear prior records; rejected/busy/background actions do not.
If moving finalization to the outer token-producing wrapper, cover embedded and guardian paths and prevent a completed old incarnation from overwriting a reset/new session.
A generation/session mismatch or conflicting scoped workspace invalidates executable advice; never repair the mismatch by picking another record.
Legacy records without added fields stay readable and cannot gain explicit_target status. Missing binding must not be described as fully execution-verified.
Derive advice at read time; never deserialize arbitrary stored argv or a persisted boolean as authority.
Build argv only from fixed executable/options plus validated selector: [claude,--resume,UUID] or [codex,resume,UUID].
Do not replay initial task text, permission flags, shell prefixes or the original command.

## Persistence and presentation

Use existing private atomic storage utilities where suitable, with a 16 KiB serialized record cap and bounded command/path fields.
Refuse oversized/nonregular/symlink records and malformed selector/scope fields; reuse audited helpers rather than introducing unchecked paths or libc outside src/os.rs.
A failed atomic replacement preserves the prior valid record; persistence errors never authorize retrying the command.
At lost_shell_recovery_note, show the classification, selector provenance, recorded cwd and current_conversation_verified=false.
Keep any directory-most-recent continuation explanation explicitly optional/non-deterministic, with no primary executable recommendation.
Guard the reattach branch: it must not run recovery advice, reset a surviving PTY, or change context.
Document that fresh unbound Claude/Codex sessions remain best_effort; automatic backend-assigned ID capture requires a separate future integration.

## Focused tests

1. Legacy JSON loads; recognized unbound launch best_effort, unsupported unavailable; original command/initial prompt never becomes resume argv.
2. Each supported direct selector form yields explicit_target/source launch_argument/current_conversation_verified=false and fixed safe argv.
3. New Claude end-to-end fixture: accepted BashCommand launches controlled fake claude with --session-id A; persist/restart; introduce newer B in the same cwd; recovered advice still targets A. No transcript or backend-confirmation claim.
4. Fake backend rejects UUID/starts with nonzero exit: record remains intended selector only, never verified or task-success evidence.
5. Parser negatives: names, paths, flag-like UUIDs, malformed UUID, repeated/conflicting flags, --last/--continue/--fork-session, substitutions, quoted separators, wrappers, pipelines, redirects and command lists never yield executable selector advice.
6. Busy/rejected/no-start foreground result leaves prior record intact; accepted non-agent clears; background commands do not replace it.
7. Runtime-owned cwd/workspace/thread are persisted; cross-principal/scope mismatch is rejected; stale generation/old-session completion cannot overwrite newer record.
8. Restart roundtrip preserves selector/provenance/binding; missing optional token stays explicitly unverified; malformed/tampered fields cannot inject executable argv.
9. File size cap, corrupt JSON, symlink/nonregular input and atomic-write failure fail conservative; previous valid record survives replacement failure.
10. Existing reattach tests remain green; lost-shell note says intended recorded target, never asserts current TUI conversation or backend confirmation, and no advice is executed.

## Remaining narrow reads before code

- Existing Bash parser/API and Cargo dependencies: choose minimal conservative recognition with no extra library.
- execute_command outcome start/generation behavior and guardian result mapping; finalize at the correct acceptance/token boundary.
- Existing private atomic read/write utilities and scoped internal thread generation path.

Root owns implementation, remote operations, focused validation and full mandatory just check before every commit.
