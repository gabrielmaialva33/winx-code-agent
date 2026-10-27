# Releasing managed temporary helpers

`EditFiles` can deliberately release individually named helpers from the exact
session directory returned by `Initialize`. This first implementation supports
Linux sessions owned by a durable guardian advertising `temporary_release`.
Embedded runtimes, unavailable owners and older guardians fail safely. Existing
PTYs are never reset or killed to enable cleanup.

Read each helper with `ReadFiles`, then copy its `revision` and
`temporary_artifact.artifact_generation` into an explicit request:

```json
{
  "thread_id": "<Initialize thread_id>",
  "workspace_root": "/exact/initialized/workspace",
  "operation": "release_temporary",
  "files": [{
    "file_path": "/exact/session/helper.py",
    "mode": "release_temporary",
    "expected_revision": "sha256:<ReadFiles revision>",
    "artifact_generation": "<ReadFiles generation>"
  }]
}
```

Requests contain 1–100 unique exact paths. Source files, another session's
helpers, symlinks, hardlinks, directories, traversal, globs, mixed edit modes and
verification commands are not accepted. Existing tool-policy and shell-mode
restrictions still apply. Cleanup does not create an undo checkpoint.

Generation tokens bind the actual opened file's physical identity and bytes to
the private scoped thread, workspace and path. They use a process-secret HMAC
with explicit length framing and no per-file registry. A changed file, even a
replacement with identical text, requires another read. A frontend restart
expires prior read tokens; successful persisted mutation receipts remain
replayable. Read metadata leaves `in_use` and `releasable` unknown: the actual
runtime owner rechecks eligibility when release is requested.

The guardian holds its session admission barrier through the operation, checks
pending actions and foreground/background execution state, and rejects active
or unresolved execution. An inline wait returning does not make a still-running
command disposable. This protection covers the owning guardian's managed
executions and the supported file tools, which replace files atomically. It is
not a filesystem sandbox against independent embedded runtimes, arbitrary
external processes, or an external writable descriptor.

All targets are checked before removal begins. Linux directory handles and
no-follow opens establish the hierarchy. Each target is atomically moved into a
private same-filesystem quarantine, its pinned physical identity and content are
rechecked, and only the verified captured object is removed. A replacement is
restored without overwriting an occupied pathname; if restoration cannot reclaim
the original name, the response identifies the preserved quarantine location.
A late failure may leave a released prefix and an untouched suffix. Results name
each outcome and report the bytes actually reclaimed; no deletion rollback is
claimed.

A repeated request with a recorded removal or quarantine side effect replays
its full per-target outcome, including recovery locations, without deleting
anything. Pure denials without a filesystem side effect are not checkpointed.
That receipt describes the removal of one historical generation, so a
new file at the same pathname survives. If a transport reply is lost before the
frontend can persist a receipt, the control plane does not automatically replay
the cleanup RPC. Inspect the exact targets; missing or changed generations fail
closed on a later explicit retry.

Runtime dropped output is stored under `.winx/scratch`; retained execution
payloads live under `~/.local/share/winx/executions`; verification execution
identity and results are held by mutation receipts. These are outside the helper
namespace. Release inspects these backing roots and descendants without following links,
under a shared 50,000-entry and 16-level traversal bound. Symlinks, unsupported
entries, inaccessible paths, scan limits, or physical aliases fail closed and
preserve every helper. Single-helper hardlinks are also rejected. The private
`.winx-release-*` quarantine namespace is reserved from all supported file
mutations, including undo and resolved aliases. A filename
merely appearing in command text is not a retained backing-store reference.

The old age-based maintenance entry points lack authoritative runtime ownership.
They now audit usage and report `deletion_deferred=runtime_protection_unknown`,
without deleting old helpers or sibling sessions. Explicit guarded release is
the supported reclamation route. The existing file/byte quotas remain enforced.
