# Winx Agent - GEMINI.md

This file provides an overview and context for the `winx-code-agent` project, a high-performance Rust implementation of the WCGW agent.

## Project Overview

The `winx-code-agent` is a Rust-based reimplementation of the [WCGW](https://github.com/rusiaaman/wcgw) agent, which is designed for LLM code agents. It offers significantly superior performance for code operations compared to its Python predecessor, with benchmarks showing speedups of up to **230x** for MCP initialization and substantial improvements across various operations like shell execution, file reading, and pattern searching.

## Core Technologies

-   **Language:** Rust (1.75+)
-   **Runtime:** Tokio (for asynchronous I/O)
-   **MCP Protocol:** Implemented using the `rmcp` Rust SDK.
-   **Shell Execution:** Utilizes `portable-pty` for full PTY support.
-   **File Operations:** Employs `memmap2` for efficient, zero-copy file reading and standard Rust file I/O for writing/editing.
-   **Concurrency:** Leverages `tokio::sync::Mutex` for thread-safe state management and `rayon` for parallel processing.

## Key Features & Tools

Winx provides a set of tools essential for LLM agents interacting with a system, adhering to the Model Context Protocol (MCP).

### 1. `Initialize`
- **Purpose:** Initializes the workspace environment for the agent.
- **Usage:** `Initialize({ type: "first_call", any_workspace_path: "/path/to/workspace", mode_name: "wcgw" })`
- **Modes:**
    - `wcgw`: Full access (default).
    - `architect`: Read-only mode.
    - `code_writer`: Restricted write access.

### 2. `BashCommand`
- **Purpose:** Executes shell commands with full PTY support, enabling interactive command-line operations.
- **Usage:** `BashCommand({ action_json: { type: "command", command: "ls -la" }, thread_id: "your-thread-id" })`
- **Supported actions:** `command`, `status_check`, `send_text`, `send_specials` (e.g., Enter, Ctrl-c), `send_ascii`.

### 3. `ReadFiles`
- **Purpose:** Reads the content of one or more files. Supports specifying line ranges.
- **Usage:** `ReadFiles({ file_paths: ["/path/to/file.rs", "/path/to/other.rs:10-50"] })`

### 4. `FileWriteOrEdit`
- **Purpose:** Writes or edits files using a specified percentage of changes and supporting search/replace blocks.
- **Usage:** `FileWriteOrEdit({ file_path: "/path/to/file.rs", percentage_to_change: 30, text_or_search_replace_blocks: "<<<<<<< SEARCH\nold code\n=======\nnew code\n>>>>>>> REPLACE", thread_id: "your-thread-id" })`

### 5. `ContextSave`
- **Purpose:** Saves project context (description, relevant files) for later resumption.
- **Usage:** `ContextSave({ id: "task-id", project_root_path: "/path/to/project", description: "Implementing feature X", relevant_file_globs: ["src/**/*.rs"] })`

### 6. `ReadImage`
- **Purpose:** Reads image files and returns their content as a base64 encoded string.
- **Usage:** `ReadImage({ file_path: "/path/to/image.png" })`

## Performance

Winx boasts superior performance due to its Rust implementation, minimizing overhead from garbage collection, interpreter startup, and efficient I/O operations:

-   **Shell Exec:** 24x faster than Python WCGW.
-   **File Read:** 7x faster than Python WCGW.
-   **MCP Init:** 230x faster than Python WCGW.

## Installation & Configuration

### Prerequisites
- Rust 1.75+
- Linux/macOS/WSL2

### Build Steps

```bash
git clone https://github.com/gabrielmaialva33/winx-code-agent.git
cd winx-code-agent
cargo build --release
```

### Claude Desktop Configuration

To integrate Winx with Claude Desktop, add the following to `~/.config/Claude/claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "winx": {
      "command": "/path/to/winx-code-agent/target/release/winx-code-agent",
      "args": [],
      "env": {
        "RUST_LOG": "info"
      }
    }
  }
}
```

## Testing

-   Run all tests with `cargo test`.
-   Run tests with output using `cargo test -- --nocapture`.
-   The project has **118 passing tests** (90 unit + 28 integration).

## AI Integration (Optional)

Winx can integrate with various AI providers (DashScope, NVIDIA NIM, Gemini) for advanced code analysis, generation, and explanation. This typically involves setting API keys as environment variables.

## Credits

-   [rusiaaman/wcgw](https://github.com/rusiaaman/wcgw) - Original Python project.
-   [anthropics/claude-code](https://github.com/anthropics/claude-code) - MCP inspiration.
-   [modelcontextprotocol](https://github.com/modelcontextprotocol) - MCP specification.

## License

MIT License - Gabriel Maia ([@gabrielmaialva33](https://github.com/gabrielmaialva33))
