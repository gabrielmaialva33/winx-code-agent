<div align="center">
  <img src=".github/assets/fairy.png" alt="Winx Logo" width="150" />

  # 🪄 Winx
  ### *The High-Performance, Remote-First MCP Runtime for AI Coding Agents*

  **Durable PTY Sessions • Streamable HTTP • Guarded File Operations • Blazing Fast Rust 🦀**

  <p align="center">
    <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/language-Rust_2021-orange?style=flat&logo=rust" alt="Rust 2021" /></a>
    <a href="https://modelcontextprotocol.io/"><img src="https://img.shields.io/badge/MCP-2026--07--28-purple?style=flat" alt="MCP Spec" /></a>
    <a href="docs/streamable-http.md"><img src="https://img.shields.io/badge/transport-Streamable_HTTP-2563eb?style=flat" alt="Streamable HTTP" /></a>
    <a href="SECURITY.md"><img src="https://img.shields.io/badge/auth-multi--principal-7c3aed?style=flat" alt="Multi-principal authentication" /></a>
    <a href="#choose-your-transport"><img src="https://img.shields.io/badge/transport-stdio-2f855a?style=flat" alt="stdio transport" /></a>
    <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue?style=flat" alt="License MIT" /></a>
  </p>

  <p align="center">
    <em>"Give remote and local LLMs durable, authenticated hands on your development machine."</em>
  </p>

  <p align="center">
    <a href="#remote-mcp-in-60-seconds">⚡ <b>Quickstart</b></a> &nbsp;•&nbsp;
    <a href="#what-you-get">✨ <b>Features</b></a> &nbsp;•&nbsp;
    <a href="#remote-architecture">🏗️ <b>Architecture</b></a> &nbsp;•&nbsp;
    <a href="docs/streamable-http.md">🌐 <b>HTTP Guide</b></a> &nbsp;•&nbsp;
    <a href="SECURITY.md">🛡️ <b>Security</b></a>
  </p>

  <p align="center">
    <b>English</b> • <a href="README.pt.md">Português</a> • <a href="README.zh.md">中文</a>
  </p>
</div>

Winx is a **remote-first MCP runtime** for agents that need a real shell, guarded file-editing primitives, repository-aware
code navigation, and sessions that survive dropped connections. Its primary deployment path is a hardened
**Streamable HTTP** endpoint for ChatGPT and other cloud or networked MCP clients; stdio remains fully supported for
Claude Code, Codex CLI, Cursor, VS Code, and other local clients.

On Unix, Winx separates the MCP adapter from the processes that own each PTY. `winxd` manages the control plane and one
`winx-guardian` per logical session keeps the shell alive across HTTP disconnects, client restarts, and adapter upgrades.
It started as a Rust port of [WCGW](https://github.com/rusiaaman/wcgw), but it is not a Python wrapper: `cd` persists,
`Ctrl+C` interrupts the real process, interactive TUIs work, and large terminal output is rendered and token-budgeted
before reaching the model.

> [!IMPORTANT]
> **Streamable HTTP is the main deployment path.** Winx binds it to loopback by default, requires a strong bearer token,
> supports independent authenticated principals, and defaults to one durable session per principal/workspace. Repeated
> stateless `Initialize` calls reattach instead of manufacturing a new guardian.

## Remote MCP in 60 seconds

```bash
cargo install winx-code-agent

mkdir -p ~/.config
install -m 600 /dev/null ~/.config/winx-http-token
openssl rand -hex 32 > ~/.config/winx-http-token

winx-code-agent serve --http \
  --bind 127.0.0.1:8000 \
  --token-file ~/.config/winx-http-token
```

Connect an MCP client to:

```text
http://127.0.0.1:8000/mcp
Authorization: Bearer <contents of ~/.config/winx-http-token>
```

Cloud clients need a reachable HTTPS endpoint. Keep Winx on loopback and use a private MCP tunnel, VPN, or authenticated
HTTPS reverse proxy in front. For OpenAI products, the
[Secure MCP Tunnel](https://developers.openai.com/api/docs/guides/secure-mcp-tunnels) keeps Winx private while exposing
an OpenAI-hosted MCP endpoint.

**Next:** [complete Streamable HTTP deployment guide](docs/streamable-http.md) ·
[security model](SECURITY.md) · [local stdio setup](#install)

## Why Winx for remote agents

- **Durable sessions:** HTTP is stateless from the client's point of view, but Unix PTYs live in per-session guardians and
  can be resumed with the same `thread_id`.
- **Identity-aware isolation:** one token per principal; thread IDs and MCP Task IDs are scoped internally and translated
  back before the response leaves the server. Workspace affinity absorbs unstable model-generated thread IDs.
- **Right-sized tool catalogs:** `full`, `coding`, `read-only`, and `terminal` profiles—or an exact per-principal
  allowlist—reduce discovery/schema payloads and reject calls outside the advertised catalog.
- **Fail-closed network defaults:** loopback-only binding, 32-byte minimum tokens, chmod-600 token files, DNS-rebinding
  host checks, body/time/concurrency limits, per-IP rate limiting, and delayed invalid-auth responses.
- **Agent-native terminal semantics:** foreground and background commands, status polling, interactive input, stable TUI
  snapshots, turn detection, real exit codes, and bounded output.
- **Repository tools, not just a shell:** guarded SEARCH/REPLACE edits, multi-file planning, undo, token-budgeted reads,
  image input, context handoff, and tree-sitter symbol navigation.

## Choose your transport

| Transport           | Best for                                                               | Endpoint / launch                                                        | Authentication                                     | Session model                                                                |
|---------------------|------------------------------------------------------------------------|--------------------------------------------------------------------------|----------------------------------------------------|------------------------------------------------------------------------------|
| **Streamable HTTP** | ChatGPT, hosted agents, remote automation, multiple MCP clients        | `https://host/mcp` through a tunnel/proxy, with Winx on `127.0.0.1:8000` | Strong bearer token; optional multi-principal TOML | Stateless requests mapped to durable principal/workspace sessions by default |
| **stdio**           | Claude Code, Codex CLI, Cursor, VS Code, desktop and local IDE clients | client launches `winx-code-agent`                                        | Local process boundary                             | One local client, using the same durable daemon runtime on Unix              |

## Remote architecture

```text
Remote MCP client
       │  HTTPS + bearer token
       ▼
Secure MCP Tunnel / VPN / authenticated reverse proxy
       │  loopback HTTP
       ▼
127.0.0.1:8000/mcp
       │
       ├─ Host / body / timeout checks
       ├─ Per-IP rate limit + global concurrency cap
       ├─ Principal authentication
       └─ thread_id and MCP Task scoping
              │
              ▼
        shared WinxService
              │
              ▼
            winxd
              │
              └─ winx-guardian per session ── real PTY / shell / TUI
```

## What you get

- A stateful bash session per thread with proper PTY semantics - foreground, background, status checks, text input,
  Enter/Ctrl-C/Ctrl-D, raw ASCII. Multiline scripts and top-level `command` shorthand both work; NUL bytes are
  rejected before they reach the shell.
- Workspaces with three modes: `wcgw` (full access), `architect` (read-only), `code_writer` (allowlist of commands and
  write globs). The command allowlist is parsed with tree-sitter, so it checks **every** command on the line -
  pipelines, `&&`/`||`/`;`, command substitution, subshells - not just the first word, and can't be bypassed with
  `ls && curl … | sh` or `ls $(rm …)`.
- A resilient PTY: a shell that won't return to a prompt (even after Ctrl-C) is auto-reset at the same cwd/mode, child
  processes are reaped on drop, and prompt detection is robust to a custom `PS1`. Opt into `zsh` with `WINX_SHELL=zsh`.
- File reads with WCGW-style line ranges (`file.rs:10-40`, `file.rs:10-`, `file.rs:-40`). Active files are tracked
  and prioritized in the repository context across calls.
- File writes and SEARCH/REPLACE edits that survive ambiguous matches, indentation drift, and the usual unicode
  quote-mismatches from LLMs. Writes are blocked when the file hasn't been read or the cached content is stale, the
  success message shows a compact diff of what changed, and recent edits are reversible with `UndoEdit`.
  `MultiFileEdit` validates and computes every file before writing any of them, so a validation failure on the last file
  leaves the earlier ones untouched. The commit phase uses atomic per-file renames; a rare mid-commit I/O failure stops
  immediately but does not roll back files already written.
- Tree-sitter code navigation via `CodeMap`: a token-budgeted symbol map of a file or the whole repo, or a
  definition/reference lookup for a symbol name - the semantic view that plain `grep` can't give you, across 13
  languages.
- `ContextSave` for handing a task summary plus its files to the next session - including workspace context, active
  files, git status/diff, and terminal sharing for proper resumption. Resuming reopens the saved project root and
  token-caps the restored memory so it never overflows the context window.
- `ReadImage` so multimodal clients can pull validated JPEG/PNG/GIF/WebP content. Oversized images are resized and
  recompressed before base64 expansion, and unchanged repeats become compact references unless the caller explicitly
  requests a fresh delivery.
- Clean, token-aware shell output: cursor/ANSI noise from interactive programs (REPLs, progress
  bars) is rendered away through a terminal emulator, and mechanical repetition is collapsed
  losslessly (`line  [winx: ×N]`) so build/install logs don't blow your context budget. Toggle the
  collapsing with `WINX_NO_COMPRESS`. When output still overflows the cap, the dropped head is streamed
  to a scratch file under `.winx/scratch/` the agent can re-read, instead of being lost.
- `.winx/` is the reserved local home for workspace-local agent artifacts and should be ignored by version control.
  `Initialize` returns a bounded `.winx/tmp/session-…/` path for each session without creating it until needed. Derived
  representations, adapters, and helpers may live there when independently useful, but remain non-canonical and must keep
  source-path/line provenance. Agents reuse one stable helper per purpose, use `ReadFiles` for unsupported languages or
  exact source, and never transform source, command output, lint reports, or search results solely for `CodeMap`. Helper
  `CodeMap` calls accept one existing file, cap their navigation payload at 12 KiB, and are bounded per live session to 24 unique files / 64
  calls; canonical source maps remain unrestricted by that aggregate budget. File tools cap temporary storage at 256 MiB
  per workspace, 64 MiB and 128 files per session, and 32 MiB per helper; they also bound path depth/name length and prune
  inactive managed session directories after 24 hours. Never encode payload in filesystem names or create
  `.winx-*`/`.winx_tmp` artifacts at the project root. Every `BashCommand` PTY exports the exact session path as `WINX_TEMP_DIR`;
  a preflight rejects statically visible shell writes to direct `.winx/tmp` children, another session, or root `.winx-*`
  artifacts. Winx also audits actual usage after every Bash action, including dynamic writes that static parsing cannot
  predict. Results expose counts and bytes; an over-budget session blocks ordinary commands until the agent explicitly
  inspects and removes obsolete helpers. An overage never triggers automatic deletion (the documented 24-hour stale-session
  pruning still applies). This targeted guard complements
  the separately selected command policy; it is not a general shell sandbox.
- Secret redaction on by default: provider API keys, JWTs, PEM private-key blocks and `user:pass@` URLs
  are scrubbed from **all** tool output and saved memory before they reach the model (disable with
  `WINX_NO_REDACT=1`). An opt-in Landlock sandbox (`WINX_SANDBOX=1`, Linux) adds a kernel-enforced second
  layer that confines writes to the workspace and hides the home directory.
- A hardened **Streamable HTTP** endpoint (`winx-code-agent serve --http`) for remote MCP clients, with stdio retained for
  local tools. HTTP binds to loopback by default, requires a strong bearer token, limits request size/rate/concurrency,
  and isolates sessions and MCP Tasks across authenticated principals - see the
  [deployment guide](docs/streamable-http.md).

## MCP Tools

| Tool              | What it does                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
|-------------------|------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `Initialize`      | Boots the workspace, picks the mode, hands you an inseparable `thread_id`/`workspace_root` pair plus a bounded `temporary_artifact_dir` for session-local derived helpers. Call it once unless a local MCP client exposes Roots, in which case Winx can bootstrap automatically. Repeated `first_call` requests reattach without rebuilding or resending unchanged workspace context. A request that tries to rebind an already-bound remote conversation is terminal: keep the current pair for allowed external targets or start a new conversation for another project. With no workspace path it creates a scratch playground; resuming a task (`task_id_to_resume`) reopens its saved project root.                                                                                           |
| `BashCommand`     | Runs commands, polls long-running ones, sends Enter/Ctrl-C, and drives TUIs. Related finite fail-fast checks can be composed with `&&` in one call. `wait_policy` is generic: `adaptive` (default) keeps short calls inline and promotes an already-running foreground command when Tasks are available; `until_complete` is only for a finite foreground command; `return_early` always stays inline. An incompatible policy is a recoverable tool result with corrected retry arguments. Supports `is_background`, `status_check`, input actions, `screen`, and `wait_for_turn`. Foreground and background PTYs export `WINX_WORKSPACE_ROOT` and the managed helper directory as `WINX_TEMP_DIR`; structured results include its post-action usage. Over-budget sessions require explicit cleanup before ordinary commands continue. When a foreground command finishes, its runtime-owned state reports the real exit code. |
| `ReadFiles`       | One or many files, with line numbers. Batched reads use a bounded parallel worker pool while preserving request order and read-before-edit coverage. Append `:10-40` to a path for a range. When the token budget is hit it tells you the exact line + `file:N-M` syntax to resume from instead of silently dropping the tail.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| `FileWriteOrEdit` | Full overwrites or SEARCH/REPLACE blocks (with optional `@start-end` line anchors to pin a repeated block). Validates file read coverage and freshness before writing, reports fuzzy tolerances, runs a syntax check, and returns a compact diff. Optional `verify_command` runs one finite post-edit check in the same MCP round trip.                                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| `MultiFileEdit`   | Validates and computes every requested edit in memory before writing any file, so a validation failure leaves the whole batch untouched. Commits then use atomic per-file renames; if a rare I/O failure occurs during that phase, already-written files are reported and are not rolled back. Optional `verify_command` runs after every commit. For a single file use `FileWriteOrEdit`.                                                                                                                                                                                                                                                                                                                                                                                        |
| `UndoEdit`        | Reverts a file to its content before the last `FileWriteOrEdit`/`MultiFileEdit` this session (per-file, last ~10 edits kept in memory). Refused if the file changed on disk since your edit; a brand-new file's creation isn't undoable.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| `ContextSave`     | Dumps task description + file globs into a single text file with workspace context, active files, and git status/diff for clean handoff and task resumption.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| `ReadImage`       | Returns validated JPEG, PNG, GIF, or WebP as a native MCP image content block (not base64 text), so multimodal models actually see it. Sources are capped at 50 MiB and bounded by decoded dimensions/allocation; delivery is at most 2 MiB and 2560 px on the long edge, with oversized inputs converted to JPEG. A 32-entry per-session content cache returns compact metadata for unchanged repeats; use `force=true` only for an intentional resend. Path policy matches the other file tools. |
| `CodeMap`         | Tree-sitter code navigation, in one tool with two `operation`s. `outline`: a symbol map (functions, types, methods, ...) - a file returns its definitions, a directory (or empty) a relevance-ranked, token-budgeted repo symbol map, in 13 languages including Python and Elixir. `references`: where a `name` is defined and used (called) across the repo, counting only real identifier occurrences (never inside strings/comments, unlike grep), definitions first. Unsupported single files return a structured `ReadFiles` fallback; for plain-text/regex search and file discovery, use `rg`/`fd`/`grep` via `BashCommand`.                                                                                                                                                              |

Winx advertises MCP `2026-07-28`. Every tool publishes an `outputSchema` and returns a shared
`structuredContent` orchestration envelope while preserving its existing text or image content for older clients. The
`status` field is authoritative (`completed`, `running`, `awaiting_input`, `awaiting_approval`, `needs_read`,
`needs_initialize`, `conflict`, `not_found`, `denied`, `invalid_input`, or `failed`). Recoverable failures are normal MCP
tool results with `isError: true`, `retrySameCall: false`, and (when Winx can infer one safely) a concrete `nextAction` plus
`requiredReads`. They are not opaque JSON-RPC errors, and agents should never repeat the rejected call unchanged.
`BashCommand` receives process state as a separate runtime-owned `BashCommandState`; rendered PTY output, child output,
and background-command metadata never feed orchestration decisions. Daemon protocol `1.5` transports that same typed
state and a separately rendered compact payload end to end, so marker-looking text cannot manufacture or suppress a polling loop. A `ReadFiles` batch with any
failed path is an honest `isError: true` result while retaining successfully read content; `MultiFileEdit` preserves
unread, stale, and SEARCH conflict recovery instead of flattening planning failures into generic invalid input.
Edit verification is bounded to 60 seconds and uses the same mode command policy as `BashCommand`. A non-zero exit returns
`isError: true`, `errorCode: verification_failed`, and `data.edit_applied: true`; it never falsely claims the edit was
rolled back. If the check is still running, the outer result supplies the normal `BashCommand` `status_check` next action.

MCP Tasks are optional on `BashCommand` and apply only to a foreground `command`; include an explicit `thread_id` and do
not combine the protocol-level `task` object with `is_background=true`. Routing is capability-driven: `adaptive` promotes
only after the inline runtime state is `running`, `until_complete` creates a Task immediately, and `return_early` never
does. Both client Task support and generation-bound runtime actions are required; protocol-1.4 guardians use the bounded
synchronous fallback. Task results retain the same structured envelope, are kept for the bounded TTL, and may be fetched repeatedly.
Daemon capability negotiation is bound to the effective guardian for that session and cached on an epoch-bound channel;
the control daemon's version or process name never enables Task promotion by itself.
Clients may explicitly advertise the `io.winx/compact-bash-output` extension to receive runtime-rendered Bash content
without the legacy textual status trailer; clients that omit it retain the existing text exactly.

On local stdio connections, a client that advertises MCP Roots can initialize Winx from its first usable
local `file://` root. Winx never changes an active workspace after a Roots update. Automatic Roots bootstrap
is disabled on the shared HTTP service so one remote client cannot silently select another client's workspace.

## Search/Replace editing

Standard block syntax:

```text
<<<<<<< SEARCH
old content
=======
new content
>>>>>>> REPLACE
```

Things the matcher forgives so you don't have to babysit the model:

- atomic: ambiguous or missing matches abort without touching the file
- adjusts replacement indentation when the LLM gets the leading whitespace wrong
- strips `ReadFiles` line numbers if they leak into a SEARCH block
- normalizes the usual "smart quote" / em-dash / ellipsis substitutions
- uses neighboring blocks to disambiguate when the same snippet appears twice
- single-line substring edits work - you don't need the whole line in SEARCH
- retries once with `\"` unescaped when the model over-escapes quotes in SEARCH
- refuses edits that only matched after too much fuzzy fixup, and rejects blocks
  that match in too many places - so you re-read instead of corrupting the file
- anchor a block to a line number to pin one of several identical snippets -
  `<<<<<<< SEARCH @42` (or a range `@42-50`); a stale anchor falls back to the
  normal search, so it never fails an otherwise-valid edit
- tells you on success which tolerances it had to apply (so you learn your
  SEARCH drifted), and on a miss how close the nearest match was, with `~`
  marking the lines that diverged

## Install

The remote-first quickstart is [at the top of this README](#remote-mcp-in-60-seconds). This section covers package
installation and local stdio client recipes.

```bash
cargo install winx-code-agent
```

On Linux/macOS/WSL2 this installs `winx-code-agent`, `winxd`, and `winx-guardian` together in
`~/.cargo/bin`. Keep the three binaries together: the adapter auto-starts `winxd`, which starts one guardian per shell
session. Every config snippet below assumes that directory is on `$PATH`; with a sterile client environment, use the
absolute path returned by `which winx-code-agent`.

Needs Rust 1.88+, bash, and a real terminal. The durable daemon runtime is supported on Linux/macOS/WSL2. Native
Windows uses the embedded runtime, so its shell sessions remain tied to the MCP server process; WSL2 is recommended.
GitHub Release downloads for Unix are `.tar.gz` bundles containing all three sibling binaries, while the Windows asset
remains a standalone embedded-runtime executable.

<details>
<summary><b>Claude Code (CLI)</b></summary>

One-liner via the CLI (stdio is the default transport):

```bash
claude mcp add winx -- winx-code-agent
```

Or drop a `.mcp.json` in your project root:

```json
{
  "mcpServers": {
    "winx": {
      "command": "winx-code-agent",
      "env": { "RUST_LOG": "winx_code_agent=info" }
    }
  }
}
```
</details>

<details>
<summary><b>Claude Desktop</b></summary>

Add to your config file (`~/Library/Application Support/Claude/claude_desktop_config.json` on macOS, `%APPDATA%\Claude\claude_desktop_config.json` on Windows):

```json
{
  "mcpServers": {
    "winx": {
      "command": "winx-code-agent",
      "env": { "RUST_LOG": "winx_code_agent=info" }
    }
  }
}
```

Restart Claude Desktop after saving.
</details>

<details>
<summary><b>Codex (OpenAI CLI)</b></summary>

One-liner:

```bash
codex mcp add winx -- winx-code-agent
```

Or edit `~/.codex/config.toml`:

```toml
[mcp_servers.winx]
command = "winx-code-agent"
env = { RUST_LOG = "winx_code_agent=info" }
```
</details>

<details>
<summary><b>Cursor</b></summary>

Add to `~/.cursor/mcp.json` (or `.cursor/mcp.json` for project-local):

```json
{
  "mcpServers": {
    "winx": {
      "command": "winx-code-agent",
      "env": { "RUST_LOG": "winx_code_agent=info" }
    }
  }
}
```
</details>

<details>
<summary><b>VS Code (Copilot Chat / MCP)</b></summary>

Add to `.vscode/mcp.json`:

```json
{
  "servers": {
    "winx": {
      "type": "stdio",
      "command": "winx-code-agent"
    }
  }
}
```
</details>

<details>
<summary><b>Zed</b></summary>

Add to your Zed settings (`~/.config/zed/settings.json`):

```json
{
  "context_servers": {
    "winx": {
      "source": "custom",
      "command": "winx-code-agent",
      "args": [],
      "env": { "RUST_LOG": "winx_code_agent=info" }
    }
  }
}
```
</details>

<details>
<summary><b>Windsurf</b></summary>

Add to `~/.codeium/windsurf/mcp_config.json`:

```json
{
  "mcpServers": {
    "winx": {
      "command": "winx-code-agent",
      "env": { "RUST_LOG": "winx_code_agent=info" }
    }
  }
}
```
</details>

<details>
<summary><b>OpenCode</b></summary>

Add to `opencode.json`:

```json
{
  "mcp": {
    "winx": {
      "type": "local",
      "command": ["winx-code-agent"],
      "enabled": true,
      "environment": { "RUST_LOG": "winx_code_agent=info" }
    }
  }
}
```
</details>

<details>
<summary><b>Gemini CLI</b></summary>

Add to `~/.gemini/settings.json`:

```json
{
  "mcpServers": {
    "winx": {
      "command": "winx-code-agent",
      "args": [],
      "env": { "RUST_LOG": "winx_code_agent=info" }
    }
  }
}
```
</details>

<details>
<summary><b>agy (Google Antigravity CLI)</b></summary>

`agy` is Google's new Gemini-powered CLI (Go binary, usually at `~/.local/bin/agy`). No `mcp add` subcommand yet - it
reads MCP servers from JSON.

Edit `~/.gemini/config/mcp_config.json` (also `~/.gemini/antigravity/mcp_config.json` if you run the Antigravity IDE
alongside):

```json
{
  "mcpServers": {
    "winx": {
      "command": "winx-code-agent",
      "env": { "RUST_LOG": "winx_code_agent=info" }
    }
  }
}
```

If `winx-code-agent` is not on the agy process `$PATH`, swap `command` for the absolute path (`~/.cargo/bin/winx-code-agent` after `cargo install winx-code-agent`).
</details>

<details>
<summary><b>Continue.dev</b></summary>

Add to your `~/.continue/config.yaml`:

```yaml
mcpServers:
  - name: winx
    command: winx-code-agent
    env:
      RUST_LOG: winx_code_agent=info
```
</details>

<details>
<summary><b>Kiro</b></summary>

Add to `~/.kiro/settings/mcp.json`:

```json
{
  "mcpServers": {
    "winx": {
      "command": "winx-code-agent",
      "env": { "RUST_LOG": "winx_code_agent=info" }
    }
  }
}
```
</details>

<details>
<summary><b>Warp</b></summary>

**Settings → MCP Servers → Add MCP Server**:

```json
{
  "winx": {
    "command": "winx-code-agent",
    "env": { "RUST_LOG": "winx_code_agent=info" }
  }
}
```
</details>

<details>
<summary><b>Roo Code</b></summary>

Add to your Roo Code MCP config:

```json
{
  "mcpServers": {
    "winx": {
      "type": "stdio",
      "command": "winx-code-agent"
    }
  }
}
```
</details>

<details>
<summary><b>Other clients (generic stdio)</b></summary>

Any client that speaks stdio MCP works with this shape:

```json
{
  "mcpServers": {
    "winx": {
      "command": "winx-code-agent",
      "args": [],
      "env": { "RUST_LOG": "winx_code_agent=info" }
    }
  }
}
```

If your client launches Winx with an empty `$PATH`, swap `command` for the absolute path (
`~/.cargo/bin/winx-code-agent`).
</details>

<details>
<summary><b>Build from source</b></summary>

For unreleased changes or a custom build:

```bash
git clone https://github.com/gabrielmaialva33/winx-code-agent.git
cd winx-code-agent
cargo install --path .
```

Or build and run the complete Unix daemon bundle without installing:

```bash
cargo build --release --locked --bins
./target/release/winx-code-agent
```

For a quick in-process development run that deliberately skips `winxd` and `winx-guardian`:

```bash
WINX_EMBEDDED=1 cargo run --release
```
</details>

### Check it's wired up

List MCP tools in your client. You should see nine entries: `Initialize`, `BashCommand`, `ReadFiles`, `FileWriteOrEdit`,
`MultiFileEdit`, `UndoEdit`, `ContextSave`, `ReadImage`, `CodeMap`. Start with `Initialize` unless your local client exposes
MCP Roots and Winx bootstraps from them; Winx tracks workspace + mode per thread.

## Durable session lifecycle (Unix)

The daemon runtime caps live guardians at 32 by default and uses tiered idle retention: a shell that has never run a
command expires after 30 minutes, while a used shell expires after 24 hours. A foreground command or live background
command is never removed. New guardians report their own creation, activity, and command clocks; the control-plane JSON
beside each socket is only a permission-protected cache. For protocol-1.2 guardians, Winx uses real request metadata and
the socket birth time instead of treating a recreated tmpfs metadata file as fresh activity.

```bash
# Inspect and attach
winx-code-agent list
winx-code-agent attach <thread_id> --follow

# Explicit cleanup
winx-code-agent kill <thread_id>
winx-code-agent kill --all

# Apply the tiered defaults (30 min never-used / 24 h used)
winx-code-agent prune

# Override both tiers; zero removes every idle session but preserves active commands
winx-code-agent prune --idle-seconds 0

# Reload winxd after changing lifecycle environment variables; guardians survive
winx-code-agent restart-daemon

# Print a redacted configuration/runtime report
winx-code-agent doctor
```

The quota and TTL are enforced by `winxd`, not just by one MCP adapter, so reconnecting clients and multiple adapters
share the same resource boundary. Under quota pressure, Winx first reclaims the oldest inactive guardian that has never
run a command; used or active shells are never sacrificed to admit a new session. If no disposable guardian exists, the
error points to the effective force-prune command: `winx-code-agent prune --idle-seconds 0`.

A newly installed adapter upgrades an older control plane automatically when it advertises safe planned restarts; the
per-session guardians and their PTYs stay alive throughout. Protocol `1.3` introduced attach-or-create: repeating a first
call for the same key preserves the PTY, cwd, output journal, and running command. Protocol `1.4` adds
`typed_action_result`; protocol `1.5` adds optional `compact_action_output` and generation-bound actions. An older durable guardian remains inspectable, but `BashCommand` fails closed until that session is
removed and initialized again; Winx never reconstructs machine state from its terminal text.

## Streamable HTTP deployment

Streamable HTTP is Winx's primary interface for ChatGPT, hosted agents, remote automation, and any MCP client that cannot
launch a local stdio process. The endpoint is always `/mcp`; the default listener is `127.0.0.1:8000`.

`Initialize` returns a canonical `thread_id`/`workspace_root` pair. Every later remote stateful tool call must copy both
values unchanged. Winx validates the pair before selecting a PTY or touching a file, so a thread borrowed from another
chat/project fails closed. `workspace_root` is a session identity guard, not a containment boundary: with
`WINX_ALLOW_PATHS=/`, tools may still intentionally operate anywhere allowed by the active mode. A different project gets
its own pair through `Initialize(first_call)`; remote sessions are never silently repointed in place.

Remote first calls default to `--session-affinity workspace`: the internal key is `(principal, canonical workspace)`, so
reconnections and harmless variations such as `release_02333` versus `release_0_2_333` resolve to one guardian. Parallel
conversations from the same principal in the same repository therefore share one shell. Use `--session-affinity
conversation` to key by principal + conversation + workspace: Winx prefers `Mcp-Session-Id`, accepts a reviewed
`X-Winx-Conversation-Id` gateway header, and falls back to the supplied first-call `thread_id`. Use `thread` only when the
caller owns stable IDs and explicit cleanup.

> [!TIP]
> The full guide covers request headers, multi-principal configuration, private tunnels, operational limits, status codes,
> durable sessions, troubleshooting, and the exact security boundary:
> **[docs/streamable-http.md](docs/streamable-http.md)**.

### Deployment profiles

| Profile                        | Credential model                               | Recommended exposure                                                                                                   |
|--------------------------------|------------------------------------------------|------------------------------------------------------------------------------------------------------------------------|
| Personal remote agent          | One chmod-600 `--token-file`                   | Loopback + private tunnel or VPN                                                                                       |
| Several clients or automations | `--principal-config` with one token per client | Loopback + authenticated HTTPS edge                                                                                    |
| ChatGPT / OpenAI products      | One dedicated principal per app                | [Secure MCP Tunnel](https://developers.openai.com/api/docs/guides/secure-mcp-tunnels) or a reviewed public HTTPS proxy |
| Local IDE or CLI               | No HTTP server; use stdio                      | Local process only                                                                                                     |

### Multi-principal configuration

```toml
# ~/.config/winx-principals.toml
[[principals]]
name = "chatgpt"
token_file = "/home/alice/.config/winx-chatgpt-token"
tool_profile = "coding"

[[principals]]
name = "automation"
token_env = "WINX_AUTOMATION_TOKEN"
allowed_tools = ["Initialize", "BashCommand", "ReadFiles"]
```

```bash
chmod 600 ~/.config/winx-principals.toml ~/.config/winx-chatgpt-token
winx-code-agent serve --http \
  --principal-config ~/.config/winx-principals.toml
```

Each authenticated principal receives its own internal namespace. The same external `thread_id` can therefore be reused
by different clients without sharing a workspace, shell, guardian, or MCP Task. Internal prefixes are translated back out
of normal results and errors before the response reaches the client. `tool_profile` defaults to `full`; an
`allowed_tools` array replaces the selected profile. The policy applies to discovery and execution, not only display.

### ChatGPT and OpenAI products

ChatGPT connects to remote MCP endpoints rather than directly launching a server on your machine. For private or local
Winx deployments, OpenAI documents the
[Secure MCP Tunnel](https://developers.openai.com/api/docs/guides/secure-mcp-tunnels) as an outbound-only path that avoids
opening inbound firewall ports. Developer Mode availability and workspace controls evolve, so use OpenAI's
[current Developer Mode guide](https://developers.openai.com/api/docs/guides/developer-mode) for the latest UI and plan
requirements.

### HTTP defaults

| Control                      | Default                                                        |
|------------------------------|----------------------------------------------------------------|
| Bind address                 | `127.0.0.1:8000`; non-loopback requires `--allow-non-loopback` |
| Authentication               | `Authorization: Bearer <token>`                                |
| Minimum token length         | 32 bytes, unless `--allow-weak-token` is explicitly set        |
| Request body                 | 64 MiB maximum                                                 |
| Request timeout              | 120 seconds                                                    |
| Concurrent requests          | 32 globally                                                    |
| Rate limit                   | 120 requests/minute per source IP                              |
| Invalid authentication delay | 100 ms                                                         |
| Query token                  | Disabled; requires `--allow-query-token`                       |
| Session affinity             | `workspace`; optional `conversation` or caller-owned `thread`  |
| Never-used guardian TTL      | 1,800 seconds (30 minutes)                                     |
| Used guardian TTL            | 86,400 seconds (24 hours)                                      |

### Connector metadata

- **Name:** `Winx`
- **Title:** `Winx High-Performance MCP`
- **Description:**

> Remote-first Rust MCP runtime that gives agents durable, authenticated access to a real PTY, guarded file editing,
> token-budgeted reads, image input, and tree-sitter code navigation. Streamable HTTP supports multi-principal session and
> Task isolation; stdio remains available for local clients.

The server advertises its icon in the `initialize` handshake (`serverInfo.icons`, MCP `2026-07-28`). Source art lives in
[`.github/assets/icon.png`](.github/assets/icon.png).

> [!WARNING]
> A valid Winx principal is equivalent to shell and file access as the operating-system user. `wcgw` mode is intentionally
> powerful and `BashCommand` is not workspace-confined. Keep HTTP on loopback, prefer private connectivity, use
> `architect` or constrained `code_writer` sessions where possible, rotate leaked tokens, and stop the endpoint when it is
> no longer needed.

## Environment variables

All optional - Winx works out of the box without any of these. Boolean variables accept the same case-insensitive values:
`1/true/yes/on` and `0/false/no/off`.

| Variable                                                   | Effect                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
|------------------------------------------------------------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `RUST_LOG`                                                 | Log verbosity, e.g. `winx_code_agent=info`. At `info` you get the per-call audit trail without command or file content.                                                                                                                                                                                                                                                                                                                                  |
| `WINX_USAGE_LOG`                                           | Optional path for non-blocking JSONL `winx::usage` events. Contains tool/action, principal, scoped thread, hashed request/session correlation, client/protocol, outcome, result status, duration, and response size. Initialize events also identify created/reused transitions, compact/full response mode, and generated context sizes. Command text, file contents, and credentials are never logged. On Unix files are `0600` (`O_NOFOLLOW`) and new log directories are `0700`. |
| `WINX_USAGE_LOG_ROTATION`                                  | Usage-log rotation: `daily` (default), `hourly`, or `never`. Daily/hourly filenames receive UTC timestamps.                                                                                                                                                                                                                                                                                                                                              |
| `WINX_USAGE_LOG_KEEP_DAYS`                                 | Approximate retention window for daily/hourly usage logs. Defaults to `7`; `0` disables pruning. Ignored with `never`.                                                                                                                                                                                                                                                                                                                                    |
| `WINX_HTTP_TOKEN`                                          | Single-principal HTTP bearer token used when `--token`, `--token-file`, and `--principal-config` are absent. Prefer a token file for long-lived deployments; see the [Streamable HTTP guide](docs/streamable-http.md).                                                                                                                                                                                                                                  |
| `WINX_RUNTIME`                                             | Runtime selection on Unix: `daemon` (default) or `embedded`. Native Windows is embedded-only.                                                                                                                                                                                                                                                                                                                                                           |
| `WINX_EMBEDDED`                                            | Truthy value (`1`, `true`, `yes`, `on`) forcing the in-process runtime; useful as a fail-safe kill switch.                                                                                                                                                                                                                                                                                                                                              |
| `WINX_SOCKET`                                              | Override the Unix socket used to reach `winxd`.                                                                                                                                                                                                                                                                                                                                                                                                         |
| `WINXD_BIN` / `WINX_GUARDIAN_BIN`                          | Override daemon/guardian executable discovery. Normally unnecessary when the three release binaries remain together.                                                                                                                                                                                                                                                                                                                                    |
| `WINX_MAX_GUARDIANS`                                       | Maximum live daemon-owned sessions across all adapters. Defaults to `32`; accepted range is `1..=4096`. Read when `winxd` starts; use `restart-daemon` after changing it.                                                                                                                                                                                                                                                                               |
| `WINX_SESSION_IDLE_TTL_SECS`                               | Idle lifetime for sessions that have run a command. Defaults to `86400` (24 hours); `0` disables this tier. Active foreground/background commands are preserved. Read when `winxd` starts.                                                                                                                                                                                                                                                              |
| `WINX_UNUSED_SESSION_IDLE_TTL_SECS`                        | Idle lifetime for guardians that have never run a command. Defaults to `1800` (30 minutes); `0` disables this tier. Under hard quota pressure, the oldest inactive never-used guardian may still be reclaimed.                                                                                                                                                                                                                                          |
| `WINX_GUARDIAN_SWEEP_INTERVAL_SECS`                        | Interval between automatic guardian sweeps. Defaults to `60` seconds; accepted range is `1..=86400`. Read when `winxd` starts.                                                                                                                                                                                                                                                                                                                          |
| `WINX_NO_COMPRESS`                                         | Set to `1` to disable output compression and see raw, uncollapsed shell output (the `[winx: ×N]` collapsing is on by default).                                                                                                                                                                                                                                                                                                                          |
| `WINX_NO_REDACT`                                           | Set to `1` to disable secret redaction. By default winx scrubs high-confidence credentials (provider API keys, JWTs, PEM private keys, `user:pass@` URLs) from all tool output and saved memory, replacing each with `[REDACTED:<rule>]`. Turn this off only when you knowingly need a raw value.                                                                                                                                                       |
| `WINX_ALLOW_PATHS`                                         | `:`-separated absolute paths the file tools (`ReadFiles`, `FileWriteOrEdit`, `MultiFileEdit`, `ReadImage`, `UndoEdit`, `CodeMap`) may reach **outside** the workspace (e.g. `WINX_ALLOW_PATHS=/tmp`). Empty by default: everything stays workspace-confined. Read once at startup, so the policy is set by whoever launches the server and cannot be widened mid-session by a tool argument or a shell command. `WINX_ALLOW_PATHS=/` turns containment off entirely (every absolute path is under `/`) - the explicit way to run unconfined. Note `BashCommand` was never path-confined; this only governs the file tools. |
| `WINX_SANDBOX`                                             | Set to `1` to enable an opt-in Landlock filesystem sandbox (Linux 5.13+, EXPERIMENTAL). Confines winx and its shell to write only the workspace (the cwd at startup) plus `/tmp`, and makes the home directory unreadable, so a manipulated agent can't read `~/.ssh`/`~/.aws` or modify files outside the project. Landlock is applied before the usage-log worker, Tokio, or PTY threads are created, so they inherit the same domain. Coarse and best-effort: a command needing a path outside the allowlist fails. Degrades to a warning (unsandboxed) on older kernels. |
| `WINX_SANDBOX_RO_PATHS` / `WINX_SANDBOX_RW_PATHS`          | `:`-separated absolute paths to additionally allow read-only / read-write under `WINX_SANDBOX` (e.g. `WINX_SANDBOX_RO_PATHS=$HOME/.cargo:$HOME/.rustup` so cargo still works).                                                                                                                                                                                                                                                                          |
| `WINX_TURN_RECOGNIZER_CONFIG`                              | JSON `{"busy":[…],"awaiting_input":[…],"awaiting_approval":[…]}` of marker strings/regexes. With `recognizer:"configurable"`, lets `wait_for_turn` drive an arbitrary TUI without bespoke code.                                                                                                                                                                                                                                                         |
| `WINX_CODING_TOKEN_BUDGET` / `WINX_NONCODING_TOKEN_BUDGET` | Override the per-file token budget for `ReadFiles` (and saved memory) - raise it for large-context models. Defaults: `24000` / `8000`.                                                                                                                                                                                                                                                                                                                  |
| `WINX_READ_PARALLELISM`                                    | Blocking workers used by one batched `ReadFiles` call. Defaults to `4`; values above `32` are clamped. Results and guard-rail coverage always follow request order.                                                                                                                                                                                                                                                                                     |
| `WINX_KEEP_TAIL_PIPE`                                      | Set to `1` to keep a trailing `\| tail …` instead of stripping it. Winx truncates output server-side, so by default it drops a redundant trailing `tail` (wcgw parity).                                                                                                                                                                                                                                                                                 |
| `WINX_USE_SCREEN` / `WINX_ATTACH_TERMINAL`                 | Run the shell inside `screen`/`tmux` so you can attach to the live session. Set to `screen`, `tmux`, or any truthy value; Winx prints an attach hint on `Initialize`.                                                                                                                                                                                                                                                                                   |
| `WINX_OPEN_CONTEXT`                                        | Set to `1` to open the saved context file in your default app after `ContextSave`.                                                                                                                                                                                                                                                                                                                                                                      |
| `WINX_SHELL`                                               | Set to `zsh` to run the session under zsh instead of bash (opt-in; bash stays the default). Falls back to bash if zsh isn't on `PATH` or the mode is restricted.                                                                                                                                                                                                                                                                                        |
| `WINX_SERVER_INSTRUCTIONS`                                 | Extra operator instructions appended to the MCP handshake and every `Initialize` response, after Winx's built-in orchestration contract.                                                                                                                                                                                                                                                                                                                |

## Verifying releases

Each GitHub release contains the platform artifact, its `.sha256` file, an aggregate `SHA256SUMS`, and a CycloneDX JSON
SBOM. The workflow also publishes GitHub artifact attestations for the binaries and the SBOM relationship.

```bash
sha256sum --check winx-linux-amd64.tar.gz.sha256
sha256sum --check SHA256SUMS
```

## Hacking on it

```bash
cargo fmt --all -- --check
cargo check --all-features --locked
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-features --locked
cargo +1.88.0 check --all-features --locked
cargo package --locked
cargo bench --bench performance --locked --no-run
cargo deny --all-features check
cargo audit --deny warnings
cargo +nightly fuzz build
```

To compare the ordered `ReadFiles` batch path with one versus four blocking workers on the current machine:

```bash
cargo bench --bench performance --locked -- read_files_batch
```

Criterion reports throughput and latency for both `workers/1` and `workers/4`. Treat absolute numbers as machine-specific;
use the same host and power profile when comparing revisions. For production latency, enable `WINX_USAGE_LOG` and aggregate
the metadata-only `duration_ms`, `response_bytes`, `batch_items`, and `worker_limit` fields described in the HTTP guide.

CI runs these contracts in dedicated jobs, including the ignored real-PTY/TUI tests on Linux. If you touch PTY, terminal,
BashCommand, file editing, authentication, or persistence, run the focused regression suite before the full matrix.

Robustness is also fuzzed and model-checked:

- **proptest** feeds arbitrary/adversarial bytes into the live terminal emulator, the ANSI stripper, and the exit-code
  parser, asserting they never panic and stay within the viewport. (This is how we found - and worked around - a vt100
  underflow on tiny grids and a reflow panic on column shrink that would otherwise crash the `panic = "abort"` release.)
- **loom** exhaustively model-checks the session pin counter (the lock-free guard that keeps an in-flight session from
  being LRU-evicted) across every thread interleaving. It's behind a feature so it doesn't perturb the normal build:

  ```bash
  cargo test --features loom --lib loom_
  ```

## A note on security

Winx supports local stdio and remote Streamable HTTP. Anything connected to either transport can read files, edit files,
and run shell commands with the capability granted by the selected mode - the same blast radius as giving the client a
terminal. HTTP extends that reach beyond the local process boundary; read the
[Streamable HTTP deployment guide](docs/streamable-http.md) and [SECURITY.md](SECURITY.md) before exposing it.

Two things are on by default to reduce the blast radius: **secret redaction** scrubs high-confidence credentials
from all tool output and saved memory (`WINX_NO_REDACT=1` to disable), and the PTY shell's whole process group is
killed on teardown so background jobs it spawned don't leak.

If you want a tighter leash:

- `architect` mode disables writes and most commands;
- `code_writer` mode lets you allowlist commands and write globs;
- `WINX_SANDBOX=1` enables an opt-in Landlock filesystem sandbox (Linux): writes are confined to the workspace
  plus `/tmp`, and the home directory is unreadable, so a manipulated agent can't read `~/.ssh`/`~/.aws` or modify
  files outside the project.

[SECURITY.md](SECURITY.md) has the disclosure process and threat model.

## License

MIT – Gabriel Maia ([@gabrielmaialva33](https://github.com/gabrielmaialva33))
