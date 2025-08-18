# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Winx is a high-performance Rust implementation of WCGW (What Could Go Wrong) for code agents with NVIDIA AI integration. It provides shell execution, file management, and AI-powered code analysis capabilities for LLM code agents, designed to integrate with Claude and other LLMs via the Model Context Protocol (MCP).

### New AI Features (NVIDIA Integration)

Winx now includes powerful AI capabilities through NVIDIA's NIM (NVIDIA Inference Microservices) API:

- **AI-Powered Code Analysis**: Analyze code for bugs, security issues, performance problems, and style violations
- **Code Generation**: Generate code from natural language descriptions
- **Code Explanation**: Get detailed explanations of complex code
- **Multi-Language Support**: Works with Rust, Python, JavaScript, TypeScript, Go, Java, C++, and many more
- **Smart Model Selection**: Automatically chooses the best NVIDIA model for each task

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

### NVIDIA AI Integration Setup

To enable AI-powered features, set your NVIDIA API key:

```bash
# Set NVIDIA API key (required for AI features)
export NVIDIA_API_KEY="your-nvidia-api-key-here"

# Alternative environment variable name
export NVAPI_KEY="your-nvidia-api-key-here"

# Optional: Configure NVIDIA settings
export NVIDIA_DEFAULT_MODEL="qwen/qwen3-235b-a22b"
export NVIDIA_TIMEOUT_SECONDS="30"
export NVIDIA_MAX_RETRIES="3"
export NVIDIA_RATE_LIMIT_RPM="60"
```

Get your API key from: [https://build.nvidia.com/](https://build.nvidia.com/)

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

4. **NVIDIA Integration**: AI-powered features using NVIDIA NIM API.
   - `nvidia/client.rs`: HTTP client for NVIDIA API with authentication and rate limiting
   - `nvidia/config.rs`: Configuration management for API keys and models
   - `nvidia/models.rs`: Data structures for requests/responses and model definitions
   - `nvidia/tools/code_analysis.rs`: AI-powered code analysis implementation

5. **Utilities**: Helper functions for various operations.
   - `utils/file_cache.rs`: File content caching
   - `utils/path.rs`: Path handling utilities
   - `utils/path_analyzer.rs`: Path analysis
   - `utils/repo.rs`: Repository utilities
   - `utils/mmap.rs`: Memory-mapped file operations

6. **Error Handling**: Comprehensive error system.
   - `errors.rs`: Contains the WinxError enum and error handling utilities

## Key Functionality

Winx provides several key features:

### Core Features
1. **Shell Command Execution**: Run commands with full interactive capabilities
2. **File Operations**: Read, write, and edit files with change tracking
3. **Project Context**: Save and restore project context for task resumption
4. **Image Support**: Process image files as base64
5. **Multiple Operation Modes**: wcgw (full access), architect (read-only), code_writer (restricted)

### AI-Powered Features (with NVIDIA API)
1. **Code Analysis**: 
   - Detect bugs, security vulnerabilities, and performance issues
   - Style and maintainability suggestions
   - Complexity scoring (0-100 scale)
   - Language-specific analysis for 20+ programming languages

2. **Code Generation**:
   - Generate code from natural language descriptions
   - Context-aware code completion
   - Multi-language support

3. **Intelligent Model Selection**:
   - **Qwen3 235B A22B** (Default): Latest generation LLM with thinking mode, MoE architecture, and 100+ language support
   - **Codestral 22B**: For code generation and completion
   - **CodeGemma 7B**: For code analysis and bug detection
   - **Llama 3.1 70B**: For code explanation and documentation
   - **Nemotron 340B**: For complex reasoning and refactoring
   - **Phi-3 Medium**: For fast responses

4. **Advanced Features**:
   - Rate limiting and retry logic
   - Streaming responses for long content
   - Automatic language detection
   - Security-focused analysis

## Available Tools

### Core Tools
- `bash_command`: Execute shell commands
- `read_files`: Read file contents  
- `file_write_or_edit`: Write or edit files
- `initialize`: Initialize the environment
- `context_save`: Save/restore project context
- `read_image`: Process image files

### AI Tools (Available with NVIDIA API Key)

The following AI-powered tools are available when NVIDIA_API_KEY is configured:

**Note**: These tools are currently integrated into the Winx server but may not be fully exposed as MCP tools yet. The NVIDIA client library is ready and can be extended to provide full MCP tool integration.

#### Core AI Capabilities:
- **Default Model**: Qwen3 235B A22B with thinking mode, MoE architecture (235B total, 22B activated), and multilingual support
- **Code Analysis**: Advanced analysis with support for 100+ languages and thinking mode for complex reasoning
- **Code Generation**: Context-aware generation with seamless switching between thinking and non-thinking modes
- **Code Explanation**: Detailed explanations leveraging Qwen3's superior instruction-following capabilities
- **Smart Model Selection**: Automatically chooses the optimal NVIDIA model for each task, defaulting to Qwen3

#### Implementation Status:
- ✅ NVIDIA API client with authentication and rate limiting
- ✅ Multi-model support (5 specialized models)
- ✅ Code analysis engine with 20+ language support
- ✅ Comprehensive error handling and retry logic
- ✅ Configuration via environment variables
- 🔄 MCP tool registration (planned for next release)

#### Example Usage (programmatic):
```rust
// Code analysis
let result = nvidia_client.analyze_code(code, Some("Python")).await?;

// Code generation  
let request = CodeGenerationRequest {
    prompt: "Create a REST API for user management".to_string(),
    language: Some("Rust".to_string()),
    context: Some("Using Axum framework".to_string()),
    max_tokens: Some(1000),
    temperature: Some(0.7),
};
let result = nvidia_client.generate_code(&request).await?;
```

## Integration with Claude

Winx is designed to be used as a Model Context Protocol (MCP) server with Claude Desktop:

### Setup Instructions

1. **Build Winx**: First, build the Winx server:
   ```bash
   cd /path/to/winx-code-agent
   cargo build --release
   ```

2. **Configure Claude Desktop**: Add this configuration to your Claude Desktop settings (`~/Library/Application Support/Claude/claude_desktop_config.json` on Mac):
   ```json
   {
     "mcpServers": {
       "winx": {
         "command": "/path/to/winx-code-agent/target/release/winx-code-agent",
         "args": [],
         "env": {
           "RUST_LOG": "info",
           "NVIDIA_API_KEY": "your-nvidia-api-key-here"
         }
       }
     }
   }
   ```

3. **Set Environment Variables**: For AI features, configure your NVIDIA API key:
   ```bash
   export NVIDIA_API_KEY="your-nvidia-api-key-here"
   export NVIDIA_DEFAULT_MODEL="qwen/qwen3-235b-a22b"  # optional
   ```

4. **Restart Claude Desktop**: After adding the configuration, restart Claude Desktop to load the Winx server.

### Usage

1. Always initialize the environment at the start of a conversation
2. Use the exposed tools to interact with the file system, shell, and AI features
3. AI features will be available if NVIDIA_API_KEY is set, otherwise only core shell/file tools will work
4. Look for the MCP server indicator (🚀) in Claude Desktop to confirm Winx is loaded
## AI Team Configuration (autogenerated by team-configurator, 2025-01-17)

**Important: YOU MUST USE subagents when available for the task.**

### Detected Technology Stack

- **Language**: Rust 2021 edition
- **Protocol**: Model Context Protocol (MCP) with rmcp crate
- **Runtime**: Tokio async runtime
- **Transport**: stdio transport for MCP communication
- **Core Dependencies**: 
  - rmcp (MCP server implementation)
  - tokio (async runtime)
  - serde/serde_json (serialization)
  - anyhow/thiserror (error handling)
  - tracing (logging)
  - memmap2 (memory-mapped files)
  - rayon (parallel processing)
  - tokenizers (NLP tokenization)
- **Architecture**: Systems programming with LLM integration
- **Domain**: Code agent tooling and shell execution

### AI Team Assignments

| Task | Agent | Notes |
|------|-------|-------|
| **Rust Development & Systems Programming** | `backend-developer` | Expert in Rust, systems programming, and async patterns |
| **MCP Protocol & LLM Integration** | `api-architect` | Protocol design, MCP specifications, and integration patterns |
| **Code Quality & Security Review** | `code-reviewer` | Mandatory for all changes, security-focused for systems code |
| **Performance & Memory Optimization** | `performance-optimizer` | Critical for shell execution and memory-mapped operations |
| **Codebase Analysis & Architecture** | `code-archaeologist` | Deep exploration of complex Rust systems and dependency analysis |

### Sample Usage

Try: `@backend-developer implement a new MCP tool for directory traversal with security constraints`

Try: `@api-architect design the protocol specification for the context save/restore feature`

Try: `@performance-optimizer analyze the memory usage patterns in the file caching system`

