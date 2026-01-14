# ✨ Winx - High-Performance Rust MCP Server ✨

<p align="center">
  <strong>🚀 1:1 Optimized Rust Implementation of WCGW (What Could Go Wrong) 🚀</strong>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/language-Rust-orange?style=flat&logo=rust" alt="Language" />
  <img src="https://img.shields.io/badge/license-MIT-blue?style=flat" alt="License" />
  <img src="https://img.shields.io/badge/MCP-compatible-purple?style=flat" alt="MCP" />
</p>

Winx is a specialized Model Context Protocol (MCP) server that provides high-performance tools for LLM code agents. It implements the core functionality of [WCGW](https://github.com/rusiaaman/wcgw) in pure Rust for maximum efficiency and stability.

## ⚡ Performance

**Built for speed on i9-13900K + RTX 4090 environments.**

| Operation | Winx (Rust) | Speedup vs Python |
|-----------|:-----------:|:-------:|
| **Startup** | 3ms | 🚀 **833x** |
| **Shell Exec** | <1ms | 🚀 **56x** |
| **File Read (1MB)** | 0.45ms | 🚀 **107x** |
| **Memory Usage** | ~5MB | 🚀 **14x** |

## 🛠️ MCP Tools

| Tool | Description |
|------|-------------|
| `Initialize` | Setup workspace environment and shell mode. |
| `BashCommand` | Execute shell commands with full PTY support. |
| `ReadFiles` | Efficient zero-copy file reading using `mmap`. |
| `FileWriteOrEdit` | Robust file modification using SEARCH/REPLACE blocks. |
| `ContextSave` | Persistent project context management. |
| `ReadImage` | Base64 image reading for multimodal models. |

## 🚀 Quick Start

### Prerequisites
- Rust 1.75+
- Linux / macOS / WSL2

### Installation

```bash
git clone https://github.com/gabrielmaialva33/winx-code-agent.git
cd winx-code-agent
cargo build --release
```

### Integration with Claude Desktop

Add to `~/.config/Claude/claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "winx": {
      "command": "/path/to/winx-code-agent/target/release/winx-code-agent",
      "args": ["serve"],
      "env": { "RUST_LOG": "info" }
    }
  }
}
```

## 🏗️ Architecture

- **PTY Shell:** Full pseudo-terminal support for interactive commands.
- **Zero-Copy I/O:** Uses memory-mapped files for blazing fast reads.
- **Strict Typing:** Powered by Rust's safety and performance guarantees.
- **WCGW Parity:** Designed to be a drop-in replacement for Python-based toolsets.

## 📜 License

MIT - Gabriel Maia ([@gabrielmaialva33](https://github.com/gabrielmaialva33))

<p align="center">
  <strong>✨ Optimized for the next generation of AI Agents ✨</strong>
</p>