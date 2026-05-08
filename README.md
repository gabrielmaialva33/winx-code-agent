# ✨ Winx - Rust MCP Server for Code Agents ✨

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

## Quick Start

### Requirements

- Rust 1.75 or newer
- Linux, macOS, or WSL2

### Build

```bash
git clone https://github.com/gabrielmaialva33/winx-code-agent.git
cd winx-code-agent
cargo build --release
```

### Run

```bash
./target/release/winx-code-agent serve
```

The server uses MCP over stdio.

### MCP Client Configuration

Example configuration:

```json
{
  "mcpServers": {
    "winx": {
      "command": "/absolute/path/to/winx-code-agent/target/release/winx-code-agent",
      "args": ["serve"],
      "env": {
        "RUST_LOG": "warn"
      }
    }
  }
}
```

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
