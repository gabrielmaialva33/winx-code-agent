<table style="width:100%" align="center" border="0">
  <tr>
    <td width="40%" align="center"><img src=".github/assets/fairy.png" alt="Winx" width="300"></td>
    <td><h1>✨ Ｗｉｎｘ Ａｇｅｎｔ ✨</h1></td>
  </tr>
</table>

<p align="center">
  <strong>🦀 A high-performance Rust implementation of WCGW for code agents 🦀</strong>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/language-Rust-orange?style=flat&logo=rust" alt="Language" />
  <img src="https://img.shields.io/badge/license-MIT-blue?style=flat&logo=appveyor" alt="License" />
  <img src="https://img.shields.io/github/languages/count/gabrielmaialva33/winx-code-agent?style=flat&logo=appveyor" alt="GitHub language count" >
  <img src="https://img.shields.io/github/repo-size/gabrielmaialva33/winx-code-agent?style=flat&logo=appveyor" alt="Repository size" >
  <a href="https://github.com/gabrielmaialva33/winx-code-agent/commits/main">
    <img src="https://img.shields.io/github/last-commit/gabrielmaialva33/winx-code-agent?style=flat&logo=appveyor" alt="Last Commit" >
  </a>
  <img src="https://img.shields.io/badge/made%20by-Maia-15c3d6?style=flat&logo=appveyor" alt="Made by Maia" >

[![Trust Score](https://archestra.ai/mcp-catalog/api/badge/quality/gabrielmaialva33/winx-code-agent)](https://archestra.ai/mcp-catalog/gabrielmaialva33__winx-code-agent)
</p>

---

## 📖 Overview

Winx is a Rust reimplementation of [WCGW](https://github.com/rusiaaman/wcgw), providing shell execution and file
management capabilities for LLM code agents. Designed for high performance and reliability, Winx integrates with Claude
and other LLMs via the Model Context Protocol (MCP).

## 🌟 Features

- ⚡ **High Performance**: Implemented in Rust for maximum efficiency
- 🤖 **Multi-Provider AI Integration** (v0.1.5):
    - 🎯 **DashScope/Qwen3**: Primary AI provider with Alibaba Cloud's Qwen3-Coder-Plus model
    - 🔄 **NVIDIA NIM**: Fallback 1 with Qwen3-235B-A22B model and thinking mode
    - 💎 **Google Gemini**: Fallback 2 with Gemini-1.5-Pro and Gemini-1.5-Flash models
    - 🔧 **AI-Powered Code Analysis**: Detect bugs, security issues, and performance problems
    - 🚀 **AI Code Generation**: Generate code from natural language descriptions
    - 📚 **AI Code Explanation**: Get detailed explanations of complex code
    - 🎭 **AI-to-AI Chat**: Winx fairy assistant with personality and multiple conversation modes
    - 🛡️ **Smart Fallback System**: Automatic provider switching on failures
- 📁 **Advanced File Operations**:
    - 📖 Read files with line range support
    - ✏️ Write new files with syntax validation
    - 🔍 Edit existing files with intelligent search/replace
    - 🔄 Smart file caching with change detection
    - 📏 Line-level granular read tracking
- 🖥️ **Command Execution**:
    - 🚀 Run shell commands with status tracking
    - 📟 Interactive shell with persistent session
    - ⌨️ Full input/output control via PTY
    - 🏃‍♂️ Background process execution
- 🔀 **Operational Modes**:
    - 🔓 `wcgw`: Complete access to all features
    - 🔎 `architect`: Read-only mode for planning and analysis
    - 🔒 `code_writer`: Restricted access for controlled modifications
- 📊 **Project Management**:
    - 📝 Repository structure analysis
    - 💾 Context saving and task resumption
- 🖼️ **Media Support**: Read images and encode as base64
- 🧩 **MCP Protocol**: Seamless integration with Claude and other LLMs

---

## 🖇️ Installation & Setup

### Prerequisites

- Rust 1.70 or higher
- Tokio runtime

### 1. Clone the Repository

```bash
git clone https://github.com/gabrielmaialva33/winx-code-agent.git && cd winx
```

### 2. Build the Project

```bash
# For development
cargo build

# For production
cargo build --release
```

### 3. Run the Agent

```bash
# Using cargo
cargo run

# Or directly
./target/release/winx-code-agent
```

---

## 🔧 Integration with Claude

Winx is designed to work seamlessly with Claude via the MCP interface:

1. **Edit Claude's Configuration**
   ```json
   // In claude_desktop_config.json (Mac: ~/Library/Application Support/Claude/claude_desktop_config.json)
   {
     "mcpServers": {
       "winx": {
         "command": "/path/to/winx-code-agent",
         "args": [],
         "env": {
           "RUST_LOG": "info",
           "DASHSCOPE_API_KEY": "your-dashscope-api-key",
           "DASHSCOPE_MODEL": "qwen3-coder-plus",
           "NVIDIA_API_KEY": "your-nvidia-api-key",
           "NVIDIA_DEFAULT_MODEL": "qwen/qwen3-235b-a22b",
           "GEMINI_API_KEY": "your-gemini-api-key",
           "GEMINI_MODEL": "gemini-1.5-pro"
         }
       }
     }
   }
   ```

2. **Restart Claude** after configuration to see the Winx MCP integration icon.

3. **Start using the tools** through Claude's interface.

---

## 🛠️ Available Tools

### 🚀 initialize

Always call this first to set up your workspace environment.

```
initialize(
  type="first_call",
  any_workspace_path="/path/to/project",
  mode_name="wcgw"
)
```

### 🖥️ bash_command

Execute shell commands with persistent shell state and full interactive capabilities.

```
# Execute commands
bash_command(
  action_json={"command": "ls -la"},
  chat_id="i1234"
)

# Check command status
bash_command(
  action_json={"status_check": true},
  chat_id="i1234"
)

# Send input to running commands
bash_command(
  action_json={"send_text": "y"},
  chat_id="i1234"
)

# Send special keys (Ctrl+C, arrow keys, etc.)
bash_command(
  action_json={"send_specials": ["Enter", "CtrlC"]},
  chat_id="i1234"
)
```

### 📁 File Operations

- **read_files**: Read file content with line range support
  ```
  read_files(
    file_paths=["/path/to/file.rs"],
    show_line_numbers_reason=null
  )
  ```

- **file_write_or_edit**: Write or edit files
  ```
  file_write_or_edit(
    file_path="/path/to/file.rs",
    percentage_to_change=100,
    file_content_or_search_replace_blocks="content...",
    chat_id="i1234"
  )
  ```

- **read_image**: Process image files as base64
  ```
  read_image(
    file_path="/path/to/image.png"
  )
  ```

### 💾 context_save

Save task context for later resumption.

```
context_save(
  id="task_name",
  project_root_path="/path/to/project",
  description="Task description",
  relevant_file_globs=["**/*.rs"]
)
```

### 🤖 AI-Powered Tools (v0.1.5)

- **code_analyzer**: AI-powered code analysis for bugs, security, and performance
  ```
  code_analyzer(
    file_path="/path/to/code.rs",
    language="Rust"
  )
  ```

- **ai_generate_code**: Generate code from natural language description
  ```
  ai_generate_code(
    prompt="Create a REST API for user management",
    language="Rust",
    context="Using Axum framework",
    max_tokens=1000,
    temperature=0.7
  )
  ```

- **ai_explain_code**: Get AI explanation and documentation for code
  ```
  ai_explain_code(
    file_path="/path/to/code.rs",
    language="Rust",
    detail_level="expert"
  )
  ```

- **winx_chat**: Chat with Winx, your AI assistant fairy ✨
  ```
  winx_chat(
    message="Oi Winx, como funciona o sistema de fallback?",
    conversation_mode="technical",
    include_system_info=true,
    personality_level=8
  )
  ```

  **Conversation Modes:**
  - `casual`: Informal, friendly chat with personality 😊
  - `technical`: Focused technical responses 🔧
  - `help`: Help mode with detailed explanations 🆘
  - `debug`: Debugging assistance 🐛
  - `creative`: Creative brainstorming 💡
  - `mentor`: Teaching and best practices 🧙‍♀️

---

## 👨‍💻 Usage Workflow

1. **Initialize the workspace**
   ```
   initialize(type="first_call", any_workspace_path="/path/to/your/project")
   ```

2. **Explore the codebase**
   ```
   bash_command(action_json={"command": "find . -type f -name '*.rs' | sort"}, chat_id="i1234")
   ```

3. **Read key files**
   ```
   read_files(file_paths=["/path/to/important_file.rs"])
   ```

4. **Make changes**
   ```
   file_write_or_edit(file_path="/path/to/file.rs", percentage_to_change=30, 
   file_content_or_search_replace_blocks="<<<<<<< SEARCH\nold code\n=======\nnew code\n>>>>>>> REPLACE", 
   chat_id="i1234")
   ```

5. **Run tests**
   ```
   bash_command(action_json={"command": "cargo test"}, chat_id="i1234")
   ```

6. **Chat with Winx for help**
   ```
   winx_chat(message="Winx, posso ter ajuda para otimizar este código?", 
   conversation_mode="mentor", include_system_info=true)
   ```

7. **Save context for later**
   ```
   context_save(id="my_task", project_root_path="/path/to/project", 
   description="Implementation of feature X", relevant_file_globs=["src/**/*.rs"])
   ```

---

## 🏷 Need Support or Assistance?

If you need help or have any questions about Winx, feel free to reach out via the following channels:

- [GitHub Issues](https://github.com/gabrielmaialva33/winx-code-agent/issues/new?assignees=&labels=question&title=support%3A+):
  Open a support issue on GitHub.
- Email: gabrielmaialva33@gmail.com

---

## 📝 Changelog

### v0.2.1 (Latest) - WCGW 1:1 Parity & Stability

**🎯 Full WCGW Python Parity:**
- **SpecialKey Serialization**: Fixed to match Python exactly (`Key-up`, `Ctrl-c`, `Ctrl-z`)
- **Ctrl+Z Support**: Added SIGTSTP (suspend) signal support
- **Field Names**: Fixed `file_paths` to match wcgw Python API exactly
- **105 Tests**: 90 unit tests + 15 integration tests all passing

**🛡️ Stability Improvements:**
- Removed all critical `.unwrap()` calls that could cause panics
- Converted Mutex lock unwraps to safe error handling with `.expect()` or `.map_err()`
- Fixed `panic!()` in ANSI code handling with proper error propagation
- Refactored `is_some()` + `unwrap()` anti-patterns to idiomatic `if let Some()`

**🔧 Code Quality:**
- Added integration test suite (`tests/integration_tests.rs`)
- Improved error messages for better debugging
- Professional-grade error handling throughout

---

### v0.2.0 - Core WCGW Port

**🚀 Major Rewrite:**
- Complete port of wcgw Python core tools to Rust
- 6 MCP tools: Initialize, BashCommand, ReadFiles, ReadImage, FileWriteOrEdit, ContextSave
- 3 operation modes: wcgw (full), architect (read-only), code_writer (restricted)
- SEARCH/REPLACE block format with 5 tolerance levels

---

### v0.1.5 - Multi-Provider AI Integration

**🚀 Major Features:**
- **Multi-Provider AI System**: Primary DashScope, fallback to NVIDIA, then Gemini
- **DashScope/Qwen3 Integration**: Alibaba Cloud's Qwen3-Coder-Plus as primary AI provider
- **Smart Fallback System**: Automatic provider switching with comprehensive error handling
- **3 New AI Tools**: `code_analyzer`, `ai_generate_code`, `ai_explain_code`

**🎯 AI Providers:**
- **DashScope**: Primary provider with OpenAI-compatible API format
- **NVIDIA NIM**: Qwen3-235B-A22B with thinking mode and MoE architecture
- **Google Gemini**: Gemini-1.5-Pro and Gemini-1.5-Flash models

**🛠️ Technical Improvements:**
- Rate limiting and retry logic for all AI providers
- Comprehensive logging and error reporting
- Environment-based configuration management
- Full CI/CD quality checks (formatting, linting, testing)

---

## 🙏 Special Thanks

A huge thank you to [rusiaaman](https://github.com/rusiaaman) for the inspiring work
on [WCGW](https://github.com/rusiaaman/wcgw), which served as the primary inspiration for this project. Winx
reimplements WCGW's features in Rust for enhanced performance and reliability.

---

## 📜 License

MIT
