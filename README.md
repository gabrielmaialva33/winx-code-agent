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
  <em>Stateful shell execution, workspace-aware file operations, and agent-friendly editing in one local MCP server.</em>
</p>

Winx is a specialized Model Context Protocol (MCP) server for LLM code agents that need local shell execution, file reads, file edits, image reads, and task context snapshots.

It is inspired by [WCGW](https://github.com/rusiaaman/wcgw), but it is not a Python wrapper. Winx provides a native Rust MCP server with PTY-backed shell sessions, workspace-aware file access, mode-aware write restrictions, and robust SEARCH/REPLACE editing behavior designed for real coding-agent workflows.

## Features

- Stateful shell execution through `BashCommand`, including foreground commands, background commands, status checks, text input, special keys, and ASCII input.
- Workspace initialization through `Initialize`, with `wcgw`, `architect`, and `code_writer` modes.
- File reading through `ReadFiles`, including path suffix line ranges such as `/path/file.rs:10-40`, `/path/file.rs:10-`, and `/path/file.rs:-40`.
- File editing through `FileWriteOrEdit`, including full writes and tolerant SEARCH/REPLACE blocks.
- Context capture through `ContextSave`.
- Image reads through `ReadImage` for multimodal MCP clients.

## MCP Tools

| Tool | Purpose |
| --- | --- |
| `Initialize` | Initializes the workspace, mode, and thread state. Call this before other tools. |
| `BashCommand` | Runs shell commands and interacts with running foreground/background commands. |
| `ReadFiles` | Reads one or more files with line numbers and optional line ranges. |
| `FileWriteOrEdit` | Writes full files or applies SEARCH/REPLACE edits after the file has been read. |
| `ContextSave` | Saves a task summary and relevant file contents for handoff/resume. |
| `ReadImage` | Reads an image file and returns base64 content with MIME metadata. |

## Search/Replace Editing

`FileWriteOrEdit` supports standard blocks:

```text
  <<<<<<< SEARCH
  old content
  =======
  new content
  >>>>>>> REPLACE
```

The matcher is intentionally tolerant for common agent mistakes:

- preserves atomicity: ambiguous or missing matches fail without writing;
- handles indentation drift and adjusts replacement indentation;
- removes `ReadFiles` line numbers when they are accidentally included;
- normalizes common Unicode quote, dash, and ellipsis mistakes;
- can use surrounding blocks as context to disambiguate repeated snippets;
- supports single-line substring edits when the search block is part of a line.

## Installation

Install the binary from crates.io:

```bash
cargo install winx-code-agent
```

This puts `winx-code-agent` on your `$PATH` (usually `~/.cargo/bin`). Every example below assumes the binary is reachable; if it isn't, use the absolute path returned by `which winx-code-agent`.

Requirements:
- Rust **1.75+** (`rustc --version`)
- Linux, macOS, or WSL2
- A terminal that can host a PTY (any standard terminal works)

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

`agy` is Google's new Gemini-powered Antigravity CLI (Go binary published as `agy` in `~/.local/bin`). It reads MCP servers from a shared JSON config — there's no `mcp add` subcommand yet.

Add to `~/.gemini/config/mcp_config.json` (the CLI also reads `~/.gemini/antigravity/mcp_config.json`; keep both in sync if you also use the Antigravity IDE):

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

Any MCP client that supports a local stdio process can run Winx with this shape:

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

If the client cannot find `winx-code-agent` on `$PATH`, replace it with the absolute path (`which winx-code-agent` or `~/.cargo/bin/winx-code-agent`).
</details>

<details>
<summary><b>Build from source (optional)</b></summary>

If you want the latest unreleased changes or a custom build:

```bash
git clone https://github.com/gabrielmaialva33/winx-code-agent.git
cd winx-code-agent
cargo install --path .
```

`cargo install --path .` is the same as `cargo install winx-code-agent` but pinned to the working tree. You can also run the binary directly without installing:

```bash
cargo build --release
./target/release/winx-code-agent
```
</details>

### Verify the connection

After configuring your client, the first tool call should be `Initialize`. From any MCP client you can confirm Winx is reachable by listing available tools — you should see `Initialize`, `BashCommand`, `ReadFiles`, `FileWriteOrEdit`, `ContextSave`, and `ReadImage`.

## Development

Useful local checks:

```bash
cargo fmt --all -- --check
cargo check --tests
cargo clippy --all-targets --all-features
cargo test --all-features
```

For formatting changes:

```bash
cargo fmt --all
```

## Security Model

Winx is a local MCP server with filesystem and shell access. Treat any MCP client connected to it as capable of reading files, editing files, and running commands within the configured workspace and mode.

Use `architect` mode for read-oriented sessions and `code_writer` mode when you want to restrict writable globs and allowed commands. See [SECURITY.md](SECURITY.md) for reporting and operational guidance.

## License

MIT - Gabriel Maia ([@gabrielmaialva33](https://github.com/gabrielmaialva33))
