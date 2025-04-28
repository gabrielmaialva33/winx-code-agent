<table style="width:100%" align="center" border="0">
  <tr>
    <td width="40%" align="center"><img src=".github/assets/fairy.png" alt="Winx" width="300"></td>
    <td><h1>✨ Ｗｉｎｘ Ｃｏｄｅ Ａｇｅｎｔ ✨</h1></td>
  </tr>
</table>

<p align="center">
  <strong>✨ A high-performance code agent written in Rust, combining the best features of WCGW with reinforcement learning capabilities. 🦀</strong>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/language-Rust-orange?style=flat&logo=rust" alt="Language" />
  <img src="https://img.shields.io/badge/license-MIT-blue?style=flat&logo=appveyor" alt="License" />
  <img src="https://img.shields.io/github/languages/count/gabrielmaialva33/winx-code-agent?style=flat&logo=appveyor" alt="GitHub language count" >
  <img src="https://img.shields.io/github/repo-size/gabrielmaialva33/winx-code-agent?style=flat&logo=appveyor" alt="Repository size" >
  <a href="https://github.com/gabrielmaialva33/winx-code-agent/commits/master">
    <img src="https://img.shields.io/github/last-commit/gabrielmaialva33/winx-code-agent?style=flat&logo=appveyor" alt="Last Commit" >
  </a>
  <img src="https://img.shields.io/badge/made%20by-Maia-15c3d6?style=flat&logo=appveyor" alt="Made by Maia" >
</p>

---

## 🌟 Features

- ⚡ **High Performance**: Implemented in Rust for maximum efficiency
- 🔄 **Reinforcement Learning**: Adaptive tool selection based on context and success patterns
- 📁 **Advanced File Operations**:
    - 📖 Read files with line range support and chunking for large files
    - ✏️ Write new files with syntax validation
    - 🔍 Edit existing files with intelligent search/replace
    - ✅ Syntax checking to prevent code errors
- 🖥️ **Command Execution**:
    - 🚀 Run shell commands with status tracking
    - 📺 Full GNU Screen integration for interactive sessions
    - 🔄 Automatic screen session management and orphan cleanup
    - 🖥️ Attach/detach from active sessions like wcgw
    - ⌨️ Interactive terminal commands with real-time feedback
    - 🏃‍♂️ Background process execution in detached screens
- 🔀 **Operational Modes**:
    - 🔓 `wcgw`: Complete access to all features
    - 🔎 `architect`: Read-only mode for planning and analysis
    - 🔒 `code_writer`: Restricted access for controlled modifications
- 📊 **Project Management**:
    - 📝 Repository structure analysis
    - 💾 Context saving and task resumption
    - 🧠 Task memory system
- 🖼️ **Media Support**: Read images and encode as base64
- 🧩 **RMCP Protocol**: Seamless integration with Claude and other LLMs

---

## 🖇️ Installation & Setup

### Prerequisites

- Rust 1.70 or higher
- Tokio runtime
- RMCP SDK

### 1. Clone the Repository

```bash
git clone https://github.com/gabrielmaialva33/winx-code-agent.git && cd winx-code-agent
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

Winx Code Agent is designed to work seamlessly with Claude via the MCP interface:

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

### 🚀 Initialize

Always call this first to set up your workspace environment.

### 🖥️ BashCommand

Execute shell commands with intelligent error handling and status tracking.

### 📁 File Operations

- **ReadFiles**: Read file content with line range support
- **WriteIfEmpty**: Create new files safely
- **FileEdit**: Edit existing files using intelligent search/replace
- **ReadImage**: Process image files as base64

### 💾 ContextSave

Save task context for later resumption.

---

## 👨‍💻 Usage Workflow

1. **Initialize the workspace**
   ```
   initialize(path="/path/to/your/project")
   ```

2. **Explore the codebase**
   ```
   bash_command(command="find . -type f -name '*.rs' | sort")
   ```

3. **Read key files**
   ```
   read_files(files=["/path/to/important_file.rs"])
   ```

4. **Make changes**
   ```
   file_edit(file="/path/to/file.rs", edit_blocks="...")
   ```

5. **Run tests**
   ```
   bash_command(command="cargo test")
   ```

6. **Save context for later**
   ```
   context_save(id="my_task", description="Implementation of feature X")
   ```

---

## 📺 Screen Integration

Winx Code Agent now includes comprehensive GNU Screen integration, similar to wcgw:

- **Automatic session management**: Commands run in screen sessions when available
- **Interactive terminal access**: Use `screen -x` to attach to running sessions
- **Background process support**: Run long processes with `&` in detached screens
- **Orphan cleanup**: Automatically detects and cleans abandoned sessions
- **Screen commands**: List, attach, detach, and get content via tool actions

See [SCREEN_INTEGRATION.md](docs/SCREEN_INTEGRATION.md) for detailed documentation.

---

## 🏷 Need Support or Assistance?

If you need help or have any questions about Winx Code Agent, feel free to reach out via the following channels:

- [GitHub Issues](https://github.com/gabrielmaialva33/winx-code-agent/issues/new?assignees=&labels=question&title=support%3A+):
  Open a support issue on GitHub.
- Email: gabrielmaialva33@gmail.com

---

## ❣️ Support the Project

If you enjoy **Winx Code Agent** and want to support its development, consider:

- ⭐ [Starring the repository](https://github.com/gabrielmaialva33/winx-code-agent) on GitHub.
- 🍴 [Forking the repository](https://github.com/gabrielmaialva33/winx-code-agent) and contributing improvements.
- 📝 Sharing your experience with tutorials or articles on [Dev.to](https://dev.to/), [Medium](https://medium.com/), or
  your personal blog.

Together, we can make **Winx Code Agent** even better!

---

## 🙏 Special Thanks

A huge thank you to [rusiaaman](https://github.com/rusiaaman) for the inspiring work
on [WCGW](https://github.com/rusiaaman/wcgw), which served as a primary inspiration for this project. Winx Code Agent
reimplements many of WCGW's best features in Rust for enhanced performance while adding reinforcement learning
capabilities.

---

## 📜 License

MIT