# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Winx is a high-performance Rust implementation of WCGW (What Could Go Wrong) for code agents. It provides shell execution and file management capabilities for LLM code agents, designed to integrate with Claude and other LLMs via the Model Context Protocol (MCP).

## Building and Running the Project

### Building

```bash
# For development
cargo build

# For production
cargo build --release
```

### Running

```bash
# Using cargo
cargo run

# Using the built binary
./target/release/winx-code-agent

# With verbose logging
cargo run -- --verbose

# With debug logging
cargo run -- --debug

# Display version
cargo run -- --version
```

### Testing

```bash
# Run all tests
cargo test

# Run specific tests
cargo test <test_name>

# Run JSON parsing tests
cargo run -- --test-json
```

## Project Architecture

Winx is a Rust MCP server implementation with the following core components:

1. **Server**: Manages the MCP protocol communication using stdio transport.
   - `server.rs`: Contains the main server implementation.

2. **Tools**: Core functionalities exposed to LLMs through MCP.
   - `tools/mod.rs`: Defines the WinxService and tool implementations.
   - Tool implementations:
     - `bash_command.rs`: Shell command execution
     - `read_files.rs`: File reading operations
     - `file_write_or_edit.rs`: File modification operations
     - `initialize.rs`: Environment initialization
     - `context_save.rs`: Save task context for resumption
     - `read_image.rs`: Image file processing

3. **State Management**: Handles persistent state for the shell.
   - `state/mod.rs`: State management module
   - `state/bash_state.rs`: Shell environment state
   - `state/terminal.rs`: Terminal state handling

4. **Utilities**: Helper functions for various operations.
   - `utils/file_cache.rs`: File content caching
   - `utils/path.rs`: Path handling utilities
   - `utils/path_analyzer.rs`: Path analysis
   - `utils/repo.rs`: Repository utilities
   - `utils/mmap.rs`: Memory-mapped file operations

5. **Error Handling**: Comprehensive error system.
   - `errors.rs`: Contains the WinxError enum and error handling utilities

## Key Functionality

Winx provides several key features:

1. **Shell Command Execution**: Run commands with full interactive capabilities
2. **File Operations**: Read, write, and edit files with change tracking
3. **Project Context**: Save and restore project context for task resumption
4. **Image Support**: Process image files as base64
5. **Multiple Operation Modes**: wcgw (full access), architect (read-only), code_writer (restricted)

## Integration with Claude

Winx is designed to be used as a Model Context Protocol (MCP) server with Claude Desktop:

1. In Claude Desktop's configuration, add Winx as an MCP server
2. Always initialize the environment at the start of a conversation
3. Use the exposed tools to interact with the file system and shell