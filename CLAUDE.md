# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Winx is a high-performance Rust implementation of [WCGW](https://github.com/rusiaaman/wcgw) (What Could Go Wrong) for LLM code agents. It provides shell execution, file management, and context saving capabilities via the Model Context Protocol (MCP).

**Key performance gains over Python WCGW:**
- MCP Init: 230x faster (11ms vs 2538ms)
- Shell Exec: 24x faster (0.7ms vs 17.5ms)
- File Read: 7x faster (mmap zero-copy)

## Build & Run Commands

```bash
# Development build
cargo build

# Production build (optimized, LTO enabled)
cargo build --release

# Run the MCP server
cargo run                    # Development
./target/release/winx-code-agent  # Production

# With logging
cargo run -- --verbose       # Info level
cargo run -- --debug         # Debug level

# Run all tests
cargo test

# Run specific test
cargo test <test_name>
cargo test bash_command
cargo test file_write

# Tests with output
cargo test -- --nocapture

# Linting (pedantic by default in Cargo.toml)
cargo clippy

# Format check
cargo fmt --check
```

## Architecture Overview

```
src/
├── main.rs              # Entry point, CLI parsing
├── server.rs            # MCP server using rmcp 0.12 (ServerHandler impl)
├── lib.rs               # Library exports
├── types.rs             # MCP tool schemas (schemars)
├── errors.rs            # WinxError enum with error recovery
├── tools/
│   ├── mod.rs           # WinxService definition
│   ├── bash_command.rs  # Shell execution via PTY
│   ├── read_files.rs    # File reading with mmap
│   ├── file_write_or_edit.rs  # SEARCH/REPLACE blocks
│   ├── initialize.rs    # Mode setup (wcgw/architect/code_writer)
│   ├── context_save.rs  # Project context persistence
│   └── read_image.rs    # Image to base64
├── state/
│   ├── bash_state.rs    # Shell state, file whitelist tracking
│   ├── pty.rs           # Real PTY via portable-pty
│   ├── terminal.rs      # VTE-based terminal emulation
│   └── persistence.rs   # State save/restore to disk
└── utils/
    ├── mmap.rs          # Memory-mapped file operations
    ├── file_cache.rs    # File content caching
    └── ...              # Various utilities
```

### Key Design Patterns

1. **Shared State with Async Mutex**: `SharedBashState = Arc<Mutex<Option<BashState>>>`
   - Uses `tokio::sync::Mutex` for async safety (not `std::sync::Mutex`)
   - State is lazily initialized on first `Initialize` call

2. **PTY Shell (preferred)**: `src/state/pty.rs`
   - Real pseudo-terminal via `portable-pty`
   - Handles interactive programs (sudo, vim, less)
   - WCGW-style prompt detection: `◉ /path──➤`

3. **WCGW Compatibility**: Tool schemas and behavior match Python WCGW
   - `BashCommand` uses `action_json` field for command/status/send actions
   - `FileWriteOrEdit` uses SEARCH/REPLACE block format
   - File whitelist tracking ensures files are read before editing

4. **Three Operation Modes** (set via `Initialize`):
   - `wcgw`: Full access (default)
   - `architect`: Read-only mode
   - `code_writer`: Restricted by allowed_globs and allowed_commands

## MCP Tools

| Tool | Description |
|------|-------------|
| `Initialize` | Setup workspace, mode, and optional initial files |
| `BashCommand` | Execute commands, check status, send input/keys |
| `ReadFiles` | Read files with optional line ranges |
| `FileWriteOrEdit` | Write or edit with SEARCH/REPLACE blocks |
| `ContextSave` | Save project context for task resumption |
| `ReadImage` | Read image as base64 |

## Important Implementation Details

### BashCommand Action Types
```rust
// In action_json field:
{ "type": "command", "command": "ls -la" }           // Execute command
{ "type": "status_check" }                           // Check running command
{ "type": "send_text", "text": "yes\n" }            // Send text input
{ "type": "send_specials", "special_keys": ["CtrlC"] }  // Send special keys
```

### FileWriteOrEdit Format
- `percentage_to_change > 50`: Provide full file content
- `percentage_to_change <= 50`: Use SEARCH/REPLACE blocks:
```
<<<<<<< SEARCH
old content
=======
new content
>>>>>>> REPLACE
```

### Error Handling
- `WinxError` enum in `src/errors.rs` covers all error types
- `ErrorRecovery` struct provides retry with exponential backoff
- Errors include suggestions for common issues

### State Persistence
- State saved to `~/.local/share/wcgw/bash_state/{thread_id}_bash_state.json`
- Compatible with WCGW Python implementation
- File whitelist tracks read ranges for edit validation

## Testing Notes

- Integration tests in `tests/integration_tests.rs`
- Use `tempfile::TempDir` for isolated test directories
- Tests require `#[tokio::test(flavor = "multi_thread")]` for async shell operations
- 118+ tests (90 unit + 28 integration)

## Claude Desktop Integration

Add to `~/.config/Claude/claude_desktop_config.json`:
```json
{
  "mcpServers": {
    "winx": {
      "command": "/path/to/winx-code-agent/target/release/winx-code-agent",
      "args": [],
      "env": { "RUST_LOG": "info" }
    }
  }
}
```

## AI Team Configuration

| Task | Agent | Notes |
|------|-------|-------|
| Rust Development | `backend-developer` | Systems programming, async patterns |
| MCP Protocol | `api-architect` | Protocol design, integration |
| Code Review | `code-reviewer` | Security-focused for systems code |
| Performance | `performance-optimizer` | Shell execution, memory-mapped I/O |
| Architecture | `code-archaeologist` | Codebase exploration |
