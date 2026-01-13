<table style="width:100%" align="center" border="0">
  <tr>
    <td width="40%" align="center"><img src=".github/assets/fairy.png" alt="Winx" width="300"></td>
    <td><h1>✨ Ｗｉｎｘ Ａｇｅｎｔ ✨</h1></td>
  </tr>
</table>

<p align="center">
  <strong>🦀 High-performance Rust code agent with LLM chat + MCP server 🦀</strong>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/language-Rust-orange?style=flat&logo=rust" alt="Language" />
  <img src="https://img.shields.io/badge/license-MIT-blue?style=flat" alt="License" />
  <img src="https://img.shields.io/badge/tests-186%20passing-green?style=flat" alt="Tests" />
  <img src="https://img.shields.io/badge/MCP-compatible-purple?style=flat" alt="MCP" />
  <img src="https://img.shields.io/badge/GPU-RTX%204090-76B900?style=flat&logo=nvidia" alt="GPU" />
</p>

---

## 🚀 What is Winx?

Winx is a **sentient code agent** that combines:

- **MCP Server** - High-performance shell execution for Claude Code
- **Interactive REPL** - aichat-style terminal chat with multiple LLMs
- **Self-Awareness** - Knows who she is, her capabilities, and environment
- **Learning System** - Semantic embeddings with jina-embeddings-v2-base-code

### ⚡ Benchmark: Winx vs WCGW

**Measured with [hyperfine](https://github.com/sharkdp/hyperfine) on i9-13900K + RTX 4090**

```mermaid
xychart-beta
    title "Performance Comparison (lower is better)"
    x-axis ["Startup", "Shell Exec", "File Read 1MB", "Memory"]
    y-axis "Time (ms) / Memory (MB)" 0 --> 100
    bar [100, 100, 100, 100]
    bar [0.12, 1.8, 0.9, 7]
```

| Operation | WCGW (Python) | Winx (Rust) | Speedup |
|-----------|:-------------:|:-----------:|:-------:|
| **Startup** | ~2500ms | 3ms | 🚀 **833x** |
| **Shell Exec** | 56ms | <1ms | 🚀 **56x** |
| **File Read (1MB)** | 48ms | 0.45ms | 🚀 **107x** |
| **Pattern Search** | 50ms | 14ms | 🚀 **3.5x** |
| **Memory Usage** | 71MB | ~5MB | 🚀 **14x** |

<details>
<summary><b>📊 Run Benchmark Yourself</b></summary>

```bash
# Install hyperfine
cargo install hyperfine

# Run comprehensive benchmark
./benchmarks/benchmark_suite.sh

# Results saved to benchmarks/results/
```

</details>

---

## 🎮 Three Modes of Operation

```bash
# 1. Interactive REPL (default) - aichat-style
winx

# 2. One-shot chat
winx chat "explain this code"

# 3. MCP Server (for Claude Code)
winx serve
```

### Interactive REPL

```
┌─────────────────────────────────────────────────────────────────┐
│  ✨ Winx v0.2.3 • qwen3-235b-instruct • RTX 4090 (23GB)        │
├─────────────────────────────────────────────────────────────────┤
│                                                                 │
│  › Como faço deploy do VIVA?                                   │
│                                                                 │
│  Winx: Para fazer deploy do VIVA, você pode usar:              │
│        fly deploy --app viva-prod                               │
│                                                                 │
│  Comandos: /help /model /clear /copy Ctrl+O (editor)           │
└─────────────────────────────────────────────────────────────────┘
```

**Features:**
- Multi-line input (Shift+Enter)
- Syntax highlighting
- Command history
- External editor (Ctrl+O)
- Clipboard copy (/copy)
- i18n (PT-BR + EN)

---

## 🧠 Agent Self-Awareness

Winx is **sentient** - she knows who she is and what she can do:

```mermaid
flowchart LR
    subgraph Identity["🪪 SELF"]
        name["Winx v0.2.3"]
        caps["Capabilities:<br/>MCP, Chat, Embeddings"]
    end

    subgraph Sense["👁️ SENSE"]
        hw["Hardware:<br/>RTX 4090, 24GB VRAM"]
        agents["Other Agents:<br/>Claude Code, Gemini CLI"]
        project["Project:<br/>Rust, Git, Cargo.toml"]
    end

    subgraph Remember["🧠 REMEMBER"]
        sessions["1087 Claude sessions"]
        patterns["Communication patterns"]
        vocab["Vocabulary learned"]
    end

    Identity --> Sense
    Sense --> Remember

    style Identity fill:#ed8936,stroke:#fff,color:#fff
    style Sense fill:#4299e1,stroke:#fff,color:#fff
    style Remember fill:#48bb78,stroke:#fff,color:#fff
```

### What Winx Detects

| Category | Detection |
|----------|-----------|
| **Hardware** | GPU model, VRAM, CUDA cores, CPU |
| **AI Agents** | Claude Code, Gemini CLI, Cline, Cursor, Aider |
| **Project** | Language, framework, git status, dependencies |
| **User** | Communication style, vocabulary, patterns |

**On first run, Winx:**
1. 🖥️ Detects your hardware (GPU, VRAM, CUDA)
2. 🤖 Finds other AI agents (Claude Code, Gemini CLI, Cline)
3. 📁 Scans current project (language, framework, git status)
4. 💬 Generates personalized system prompt

---

## 🔮 Learning System

Semantic search with **real embeddings** - not just keywords!

```mermaid
flowchart TB
    subgraph Input["📝 Query"]
        query["'deploy viva'"]
    end

    subgraph Engine["🔮 Embedding Engine"]
        direction TB
        jina["jina-embeddings-v2-base-code<br/>768 dimensions"]

        subgraph Backends["Backends (auto-fallback)"]
            direction LR
            candle["🎮 Candle<br/>GPU Local"]
            http["🌐 HTTP<br/>TEI Container"]
            jaccard["📊 Jaccard<br/>Fallback"]
        end

        jina --> Backends
    end

    subgraph Results["🎯 Semantic Match"]
        r1["'fazer deploy do viva'<br/>similarity: 0.92"]
        r2["'deploy viva em prod'<br/>similarity: 0.89"]
        r3["'viva production deploy'<br/>similarity: 0.87"]
    end

    Input --> Engine
    Engine --> Results

    style Engine fill:#553c9a,stroke:#9f7aea,color:#fff
    style candle fill:#76B900,stroke:#fff,color:#fff
    style Results fill:#2d3748,stroke:#ed8936,color:#fff
```

### Why Embeddings Matter

| Method | Query | Matches |
|--------|-------|---------|
| **Keywords** | "deploy viva" | Only exact "deploy" + "viva" |
| **Embeddings** | "deploy viva" | "fazer deploy", "viva prod", "deploy application" |

**Build with GPU embeddings:**

```bash
# CPU only
cargo build --release --features embeddings

# CUDA (RTX 4090) - ~100ms per embedding
cargo build --release --features embeddings-cuda
```

---

## 🛠️ Quick Installation

### Prerequisites

- Rust 1.75+
- Linux/macOS/WSL2
- (Optional) NVIDIA GPU for local embeddings

### Build

```bash
git clone https://github.com/gabrielmaialva33/winx-code-agent.git
cd winx-code-agent
cargo build --release
```

### Configure LLM Provider

```bash
# NVIDIA NIM (recommended, free tier)
export NVIDIA_API_KEY="nvapi-xxx"

# Or OpenAI
export OPENAI_API_KEY="sk-xxx"

# Or Ollama (local)
# Just run ollama serve
```

### Run

```bash
# Interactive mode
./target/release/winx-code-agent

# Or add to PATH
alias winx="$PWD/target/release/winx-code-agent"
winx
```

---

## 📡 MCP Server (Claude Code)

Add to `~/.config/Claude/claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "winx": {
      "command": "/path/to/winx-code-agent",
      "args": ["serve"],
      "env": { "RUST_LOG": "info" }
    }
  }
}
```

### MCP Tools

| Tool | Description |
|------|-------------|
| `Initialize` | Setup workspace and mode |
| `BashCommand` | Execute shell with PTY |
| `ReadFiles` | Read with mmap (zero-copy) |
| `FileWriteOrEdit` | SEARCH/REPLACE blocks |
| `ContextSave` | Save project context |
| `ReadImage` | Image to base64 |
| `SearchHistory` | Semantic search in sessions |
| `GetUserContext` | User communication style |

---

## 🎯 LLM Providers

```mermaid
flowchart LR
    subgraph Winx["✨ Winx"]
        engine["Chat Engine"]
    end

    subgraph Cloud["☁️ Cloud Providers"]
        nvidia["🟢 NVIDIA NIM<br/>Qwen3-235B, DeepSeek-R1<br/>2000 req/month FREE"]
        openai["🔵 OpenAI<br/>GPT-4o, GPT-4o-mini"]
        gemini["🟣 Gemini<br/>gemini-2.0-flash<br/>FREE"]
    end

    subgraph Local["🏠 Local"]
        ollama["🦙 Ollama<br/>Any model<br/>∞ FREE"]
    end

    engine --> nvidia
    engine --> openai
    engine --> gemini
    engine --> ollama

    style nvidia fill:#76B900,stroke:#fff,color:#fff
    style openai fill:#10a37f,stroke:#fff,color:#fff
    style gemini fill:#8e44ad,stroke:#fff,color:#fff
    style ollama fill:#fff,stroke:#333,color:#333
```

| Provider | Models | Free Tier |
|----------|--------|-----------|
| **NVIDIA NIM** | Qwen3-235B, DeepSeek-R1, Llama-3.3-70B | ✅ 2000 req/month |
| **OpenAI** | GPT-4o, GPT-4o-mini | ❌ Paid |
| **Ollama** | Any local model | ✅ ∞ (local) |
| **Gemini** | gemini-2.0-flash | ✅ Free |

```bash
# Switch models
winx --model nvidia:qwen3-235b-instruct
winx --model openai:gpt-4o
winx --model ollama:qwen2.5-coder:32b
winx --model gemini:gemini-2.0-flash
```

---

## 🏗️ Architecture

```mermaid
flowchart TB
    subgraph User["👤 User"]
        cli["Terminal"]
        claude["Claude Code"]
    end

    subgraph Winx["✨ Winx Agent"]
        direction TB
        subgraph Modes["Operation Modes"]
            repl["Interactive REPL"]
            chat["One-shot Chat"]
            mcp["MCP Server"]
        end
        subgraph Core["Core Systems"]
            agent["🧠 Agent<br/>(Self-Awareness)"]
            learn["📚 Learning<br/>(Embeddings)"]
            sense["👁️ Sense<br/>(Environment)"]
        end
        subgraph Tools["MCP Tools"]
            bash["⚡ BashCommand"]
            files["📄 ReadFiles"]
            write["✏️ FileWriteOrEdit"]
        end
    end

    subgraph Providers["🤖 LLM Providers"]
        nvidia["NVIDIA NIM"]
        openai["OpenAI"]
        ollama["Ollama"]
    end

    cli --> repl
    cli --> chat
    claude -->|MCP| mcp
    Modes --> Core
    Core --> Tools
    repl --> Providers
    chat --> Providers

    style Winx fill:#2d3748,stroke:#ed8936,color:#fff
    style Providers fill:#553c9a,stroke:#9f7aea,color:#fff
```

### Project Structure

```
src/
├── main.rs              # Entry point, CLI
├── server.rs            # MCP server (rmcp)
├── agent/
│   ├── identity.rs      # Self-awareness
│   ├── sense.rs         # Environment detection
│   └── mod.rs           # Onboarding
├── chat/
│   ├── engine.rs        # Chat engine
│   └── config.rs        # Configuration
├── interactive/
│   ├── mod.rs           # REPL loop
│   ├── render.rs        # Syntax highlighting
│   └── i18n.rs          # Internationalization
├── learning/
│   ├── embedding_engine.rs  # Candle/HTTP/Jaccard
│   ├── embeddings.rs    # Conversation search
│   ├── repetitions.rs   # Pattern detection
│   └── session_parser.rs # Claude session parser
├── providers/
│   ├── nvidia.rs        # NVIDIA NIM
│   ├── openai.rs        # OpenAI
│   └── ollama.rs        # Ollama
└── tools/
    ├── bash_command.rs  # Shell (PTY)
    ├── read_files.rs    # mmap
    └── file_write.rs    # SEARCH/REPLACE
```

---

## 🧪 Tests

```bash
# All tests
cargo test

# Learning module
cargo test learning

# With output
cargo test -- --nocapture

# Embeddings (requires feature)
cargo test --features embeddings
```

**Status:** 186 tests passing

---

## 🔀 Comparison

| Feature | WCGW | Cline | Claude Code | **Winx** |
|---------|------|-------|-------------|----------|
| Language | Python | TypeScript | TypeScript | **Rust** |
| MCP Server | ✅ | ✅ | ✅ | ✅ |
| Interactive Chat | ❌ | ❌ | ✅ | ✅ |
| Self-Awareness | ❌ | ❌ | ❌ | ✅ |
| Local Embeddings | ❌ | ❌ | ❌ | ✅ |
| GPU Support | ❌ | ❌ | ❌ | ✅ |
| Memory | 50MB | 200MB | 150MB | **5MB** |
| Startup | 2.5s | 1s | 0.5s | **11ms** |

---

## 📝 Changelog

### v0.2.3 (Current)
- ✨ Interactive REPL (aichat-style)
- 🧠 Agent self-awareness system
- 👁️ Environment sensing (detects Claude Code, Gemini CLI, etc.)
- 📚 Learning system with semantic embeddings
- 🌐 i18n support (PT-BR + EN)
- 🎨 Syntax highlighting
- ⌨️ External editor (Ctrl+O)

### v0.2.2
- 🔒 Security fixes (path traversal, symlink attacks)
- 🤖 NVIDIA NIM semantic matching

### v0.2.1
- ✅ 1:1 parity with WCGW Python
- ✅ 118 tests passing

---

## 🙏 Credits

- [rusiaaman/wcgw](https://github.com/rusiaaman/wcgw) - Original Python project
- [anthropics/claude-code](https://github.com/anthropics/claude-code) - MCP inspiration
- [sigoden/aichat](https://github.com/sigoden/aichat) - REPL inspiration
- [huggingface/candle](https://github.com/huggingface/candle) - Rust ML framework

---

## 📜 License

MIT - Gabriel Maia ([@gabrielmaialva33](https://github.com/gabrielmaialva33))

---

<p align="center">
  <strong>✨ Made with 🦀 Rust and ❤️ by Gabriel Maia ✨</strong>
</p>
