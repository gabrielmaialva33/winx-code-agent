<p align="center">
  <img src=".github/assets/fairy.png" alt="Winx fairy mascot" width="160" />
</p>

# ✨ Winx - MCP Server for Shell & Coding Agents ✨

<p align="center">
  <strong>🦀 Native Rust implementation inspired by WCGW, built for local code-agent workflows</strong>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/language-Rust-orange?style=flat&logo=rust" alt="Language" />
  <img src="https://img.shields.io/badge/license-MIT-blue?style=flat" alt="License" />
  <img src="https://img.shields.io/badge/MCP-compatible-purple?style=flat" alt="MCP" />
  <img src="https://img.shields.io/badge/transport-stdio-2f855a?style=flat" alt="stdio transport" />
</p>

<p align="center">
  <em>A local MCP server you can hand to a coding agent and stop worrying about the shell.</em>
</p>

Winx is the MCP server I wanted while running Claude, Codex, and friends against real repos: one process that handles
the shell, file IO, and PTY-backed interactive sessions, written in Rust so it doesn't fight you on stdio.

It started as a Rust port of [WCGW](https://github.com/rusiaaman/wcgw) but isn't a Python wrapper. Everything runs on a
real PTY (via `portable-pty`), `cd` actually sticks, `Ctrl+C` actually interrupts, and background shells survive
long-running TUIs without leaking output buffers into your token budget.

## What you get

- A stateful bash session per thread with proper PTY semantics — foreground, background, status checks, text input,
  Enter/Ctrl-C/Ctrl-D, raw ASCII. Multiline scripts and top-level `command` shorthand both work; NUL bytes are
  rejected before they reach the shell.
- Workspaces with three modes: `wcgw` (full access), `architect` (read-only), `code_writer` (allowlist of commands and
  write globs).
- File reads with WCGW-style line ranges (`file.rs:10-40`, `file.rs:10-`, `file.rs:-40`). Active files are tracked
  and prioritized in the repository context across calls.
- File writes and SEARCH/REPLACE edits that survive ambiguous matches, indentation drift, and the usual unicode
  quote-mismatches from LLMs. Writes are blocked when the file hasn't been read or the cached content is stale.
- `ContextSave` for handing a task summary plus its files to the next session — including workspace context, active
  files, git status/diff, and terminal sharing for proper resumption.
- `ReadImage` so multimodal clients can pull screenshots, mockups, error PNGs, etc.
- Two transports: **stdio** for local clients, plus an optional token-gated **Streamable HTTP** server
  (`winx serve --http`) for remote MCP clients like ChatGPT — see
  [Remote access](#remote-access-chatgpt--other-remote-mcp-clients).

## MCP Tools

| Tool              | What it does                                                                                                                                                                                              |
|-------------------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `Initialize`      | Boots the workspace, picks the mode, hands you a `thread_id`. Call this first or everything else errors out.                                                                                              |
| `BashCommand`     | Runs commands, polls long-running ones, sends Enter/Ctrl-C, drives TUIs. Supports `is_background`, `status_check`, `send_text`, `send_specials`, `send_ascii`, `allow_multi` for multi-statement scripts. |
| `ReadFiles`       | One or many files, with line numbers. Append `:10-40` to a path for a range.                                                                                                                              |
| `FileWriteOrEdit` | Full overwrites or SEARCH/REPLACE blocks. Validates file read coverage and freshness before writing.                                                                                                      |
| `ContextSave`     | Dumps task description + file globs into a single text file with workspace context, active files, and git status/diff for clean handoff and task resumption.                                              |
| `ReadImage`       | Base64 + MIME, for clients that can render images.                                                                                                                                                        |

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
- single-line substring edits work — you don't need the whole line in SEARCH

## Install

```bash
cargo install winx-code-agent
```

Binary lands in `~/.cargo/bin` — every config snippet below assumes that's on `$PATH`. If your MCP client launches with
a sterile env, swap `winx-code-agent` for the absolute path (`which winx-code-agent`).

Needs Rust 1.75+, Linux/macOS/WSL2, and a real terminal (any modern one — Winx spawns its own PTY).

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

`agy` is Google's new Gemini-powered CLI (Go binary, usually at `~/.local/bin/agy`). No `mcp add` subcommand yet — it
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

Or run it without installing:

```bash
cargo run --release
```
</details>

### Check it's wired up

List MCP tools in your client. You should see six entries: `Initialize`, `BashCommand`, `ReadFiles`, `FileWriteOrEdit`,
`ContextSave`, `ReadImage`. The first call always has to be `Initialize` — Winx tracks workspace + mode per thread.

## Remote access (ChatGPT & other remote MCP clients)

By default Winx speaks MCP over **stdio** — the local transport every desktop client (Claude Desktop, Cursor, VS Code)
uses. For clients that live in the cloud and can't reach your machine over stdio — like ChatGPT's developer-mode custom
connectors — Winx can also serve MCP over **Streamable HTTP**:

```bash
winx serve --http --bind 127.0.0.1:8000 --token "$(openssl rand -hex 24)"
```

The MCP protocol is served at `/mcp`. Every request must carry the token, either as `Authorization: Bearer <token>` or a
`?token=<token>` query parameter. Without a token the server refuses to start — serving a shell over the network without
auth is remote code execution waiting to happen.

| Flag             | Purpose                                                                                          |
|------------------|-------------------------------------------------------------------------------------------------|
| `--http`         | Serve over Streamable HTTP instead of stdio.                                                     |
| `--bind`         | Listen address. Defaults to `127.0.0.1:8000`. Keep it on loopback.                               |
| `--token`        | Shared secret required on every request. Falls back to the `WINX_HTTP_TOKEN` env var.            |
| `--allowed-host` | Extra `Host` authority to accept (your tunnel hostname). Repeatable. Loopback is always allowed. |

Remote clients run in the cloud, so the endpoint has to be reachable over HTTPS — put a tunnel in front of the loopback
listener and allow its hostname through the built-in DNS-rebinding guard:

```bash
# 1. tunnel first, to learn the public hostname
cloudflared tunnel --url http://localhost:8000
#    -> https://<random>.trycloudflare.com

# 2. start Winx, allowing that host
winx serve --http --bind 127.0.0.1:8000 \
     --token "$(openssl rand -hex 24)" \
     --allowed-host <random>.trycloudflare.com
```

In ChatGPT (Settings → Apps → Advanced → **Developer mode**), add a connector with:

- **URL**: `https://<random>.trycloudflare.com/mcp?token=<your-token>`
- **Authentication**: **None** (the secret rides in the URL)

Remote clients are effectively **stateless** — they don't reuse the MCP session between tool calls — so the HTTP
transport shares one shell session across all requests: the shell `Initialize` creates stays alive for the lifetime of
the server, and later `BashCommand` calls find it. Reuse the same `thread_id` across calls.

> [!WARNING]
> The HTTP transport puts arbitrary shell and file access on the network. Anyone with the token (and URL) gets a shell
> on your machine as your user — and not just inside the workspace, since `BashCommand` in `wcgw` mode isn't
> path-restricted. Bind to loopback, keep it behind an authenticated tunnel, prefer `architect`/`code_writer` mode or a
> container, and shut it down when you're done.

## Hacking on it

```bash
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

CI runs the same three. If you touch `src/state/pty.rs` or anything in `src/tools/bash_command.rs`, the regression suite
at `tests/bash_pty_regression_test.rs` is what protects against the usual TUI/PTY foot-guns — run it first.

## A note on security

By default this is a local (stdio) MCP server. Anything connected to it can read files, edit files, and run shell
commands inside the workspace — same blast radius as letting the model into your terminal. The optional HTTP transport
(`--http`) extends that reach to the network; see
[Remote access](#remote-access-chatgpt--other-remote-mcp-clients) for the extra precautions it demands.

If you want a tighter leash:

- `architect` mode disables writes and most commands;
- `code_writer` mode lets you allowlist commands and write globs.

[SECURITY.md](SECURITY.md) has the disclosure process and threat model.

## License

MIT - Gabriel Maia ([@gabrielmaialva33](https://github.com/gabrielmaialva33))
