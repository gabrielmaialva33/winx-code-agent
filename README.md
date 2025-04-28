<table style="width:100%" align="center" border="0">
  <tr>
    <td width="40%" align="center"><img src=".github/assets/fairy.png" alt="Winx" width="300"></td>
    <td><h1>✨ Ｗｉｎｘ ✨</h1></td>
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
</p>

---

## 📖 Overview

Winx is a Rust reimplementation of [WCGW](https://github.com/rusiaaman/wcgw), providing shell execution and file
management capabilities for LLM code agents. Designed for high performance and reliability, Winx integrates with Claude
and other LLMs via the Model Context Protocol (MCP).

## 🌟 Features

- ⚡ **High Performance**: Implemented in Rust for maximum efficiency
- 📁 **Advanced File Operations**:
    - 📖 Read files with line range support
    - ✏️ Write new files with syntax validation
    - 🔍 Edit existing files with intelligent search/replace
- 🖥️ **Command Execution**:
    - 🚀 Run shell commands with status tracking
    - 📺 GNU Screen integration for interactive sessions
    - ⌨️ Send input to running commands
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
           "RUST_LOG": "info"
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

Execute shell commands with intelligent error handling and status tracking.

```
bash_command(
  action_json={"command": "ls -la"},
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

6. **Save context for later**
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

## 🙏 Special Thanks

A huge thank you to [rusiaaman](https://github.com/rusiaaman) for the inspiring work
on [WCGW](https://github.com/rusiaaman/wcgw), which served as the primary inspiration for this project. Winx
reimplements WCGW's features in Rust for enhanced performance and reliability.

---

## 📜 License

MIT
