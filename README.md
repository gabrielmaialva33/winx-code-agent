<table style="width:100%" align="center" border="0">
  <tr>
    <td width="40%" align="center"><img src=".github/assets/fairy.png" alt="Winx" width="300"></td>
    <td><h1>✨ Ｗｉｎｘ Ａｇｅｎｔ ✨</h1></td>
  </tr>
</table>

<p align="center">
  <strong>🦀 High-performance Rust implementation of WCGW for code agents 🦀</strong>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/language-Rust-orange?style=flat&logo=rust" alt="Language" />
  <img src="https://img.shields.io/badge/license-MIT-blue?style=flat" alt="License" />
  <img src="https://img.shields.io/badge/tests-118%20passing-green?style=flat" alt="Tests" />
  <img src="https://img.shields.io/badge/MCP-compatible-purple?style=flat" alt="MCP" />
</p>

---

## 🚀 Por que Winx?

Winx é uma reimplementação em **Rust** do [WCGW](https://github.com/rusiaaman/wcgw) (Python), oferecendo performance drasticamente superior para operações de código em agentes LLM.

### ⚡ Benchmark: Winx vs WCGW

| Operação | WCGW (Python) | Winx (Rust) | Speedup |
|----------|---------------|-------------|---------|
| **MCP Init** | 2538ms | 11ms | **230x** |
| Shell Exec | 17.5ms | 0.7ms | **24x** |
| File Read | 7.0ms | 1.0ms | **7x** |
| Pattern Search | 11.9ms | 1.2ms | **10x** |

> **MCP Protocol real:** 230x mais rápido no handshake
> **Média geral:** 8.7x mais rápido em operações típicas

---

## 📖 Visão Geral

```
┌─────────────────────────────────────────────────────────────┐
│                     Claude / LLM                            │
└─────────────────────┬───────────────────────────────────────┘
                      │ MCP Protocol (JSON-RPC 2.0)
                      ▼
┌─────────────────────────────────────────────────────────────┐
│                   Winx Agent (Rust)                         │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────────────┐  │
│  │ BashCommand │  │  ReadFiles  │  │  FileWriteOrEdit    │  │
│  │   (PTY)     │  │   (mmap)    │  │  (search/replace)   │  │
│  └─────────────┘  └─────────────┘  └─────────────────────┘  │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────────────┐  │
│  │ Initialize  │  │ ContextSave │  │     ReadImage       │  │
│  │  (modes)    │  │  (resume)   │  │     (base64)        │  │
│  └─────────────┘  └─────────────┘  └─────────────────────┘  │
└─────────────────────────────────────────────────────────────┘
                      │
                      ▼
┌─────────────────────────────────────────────────────────────┐
│                    Sistema Operacional                      │
│         Shell (bash/zsh) │ Filesystem │ Processos           │
└─────────────────────────────────────────────────────────────┘
```

---

## 🛠️ Instalação Rápida

### Pré-requisitos

- Rust 1.75+
- Linux/macOS/WSL2

### Build

```bash
git clone https://github.com/gabrielmaialva33/winx-code-agent.git
cd winx-code-agent
cargo build --release
```

### Configurar Claude Desktop

Adicione em `~/.config/Claude/claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "winx": {
      "command": "/caminho/para/winx-code-agent/target/release/winx-code-agent",
      "args": [],
      "env": {
        "RUST_LOG": "info"
      }
    }
  }
}
```

---

## 🔧 Tools Disponíveis

### `Initialize`

Inicializa o ambiente de trabalho. **Sempre chame primeiro.**

```json
{
  "type": "first_call",
  "any_workspace_path": "/home/user/projeto",
  "mode_name": "wcgw"
}
```

**Modos:**
- `wcgw` - Acesso completo (padrão)
- `architect` - Somente leitura
- `code_writer` - Escrita restrita

### `BashCommand`

Executa comandos shell com PTY completo.

```json
{
  "action_json": {
    "type": "command",
    "command": "ls -la"
  },
  "thread_id": "abc123"
}
```

**Ações suportadas:**
- `command` - Executa comando
- `status_check` - Verifica status
- `send_text` - Envia texto
- `send_specials` - Envia teclas especiais (Enter, Ctrl-c, etc)
- `send_ascii` - Envia códigos ASCII

### `ReadFiles`

Lê arquivos com suporte a ranges de linhas.

```json
{
  "file_paths": [
    "/caminho/arquivo.rs",
    "/caminho/outro.rs:10-50"
  ]
}
```

### `FileWriteOrEdit`

Escreve ou edita arquivos com SEARCH/REPLACE blocks.

```json
{
  "file_path": "/caminho/arquivo.rs",
  "percentage_to_change": 30,
  "text_or_search_replace_blocks": "<<<<<<< SEARCH\nold code\n=======\nnew code\n>>>>>>> REPLACE",
  "thread_id": "abc123"
}
```

### `ContextSave`

Salva contexto do projeto para retomar depois.

```json
{
  "id": "minha-tarefa",
  "project_root_path": "/home/user/projeto",
  "description": "Implementando feature X",
  "relevant_file_globs": ["src/**/*.rs", "Cargo.toml"]
}
```

### `ReadImage`

Lê imagens e retorna em base64.

```json
{
  "file_path": "/caminho/imagem.png"
}
```

---

## 🏗️ Arquitetura

```
src/
├── main.rs              # Entry point
├── server.rs            # MCP server (rmcp)
├── lib.rs               # Library exports
├── types.rs             # Tipos e schemas
├── errors.rs            # Error handling
├── tools/
│   ├── mod.rs           # Tool registry
│   ├── bash_command.rs  # Shell execution (PTY)
│   ├── read_files.rs    # File reading (mmap)
│   ├── file_write.rs    # File writing
│   ├── initialize.rs    # Mode initialization
│   ├── context_save.rs  # Context persistence
│   └── read_image.rs    # Image processing
├── state/
│   ├── mod.rs           # State management
│   ├── bash_state.rs    # Shell state (Mutex)
│   └── terminal.rs      # Terminal handling
└── utils/
    ├── file_cache.rs    # File caching
    ├── mmap.rs          # Memory-mapped I/O
    ├── path.rs          # Path utilities
    └── repo.rs          # Repository analysis
```

### Tecnologias Core

| Componente | Tecnologia | Por quê |
|------------|------------|---------|
| Runtime | Tokio | Async I/O de alta performance |
| MCP | rmcp | SDK oficial Rust para MCP |
| Shell | portable-pty | PTY cross-platform |
| Files | memmap2 | Zero-copy file reading |
| Concurrency | tokio::sync::Mutex | Thread-safe state |
| Matching | rayon | Parallel fuzzy matching |

---

## 🧪 Testes

```bash
# Rodar todos os testes
cargo test

# Testes com output
cargo test -- --nocapture

# Testes específicos
cargo test bash_command
cargo test file_write
```

**Status:** 118 testes passando (90 unit + 28 integration)

---

## 📊 Performance Details

### Por que Rust é mais rápido?

1. **Shell Exec (353x)**
   - Python: subprocess fork + interpreter overhead
   - Rust: syscall direto via PTY

2. **File Read (3.7x)**
   - Python: objeto allocation + GIL
   - Rust: mmap zero-copy

3. **Fuzzy Match (1186x)**
   - Python: loop interpretado, heap allocation por char
   - Rust: SIMD automático, inline agressivo

### Quando usar cada um?

| Cenário | Recomendação |
|---------|--------------|
| Hot paths (autocomplete) | **Winx** |
| Comandos leves (ls, cat) | **Winx** |
| Comandos pesados (build) | Tanto faz |
| Debug/compatibilidade | WCGW |

---

## 🔀 Comparação com WCGW

| Feature | WCGW (Python) | Winx (Rust) |
|---------|---------------|-------------|
| Linguagem | Python 3.10+ | Rust 1.75+ |
| Performance | Baseline | **2-1000x faster** |
| Memory | ~50MB | ~5MB |
| PTY Support | ✅ | ✅ |
| MCP Protocol | ✅ | ✅ |
| Search/Replace | ✅ | ✅ |
| Context Save | ✅ | ✅ |
| AI Integration | ❌ | ✅ (NVIDIA NIM) |
| Parallel Matching | ❌ | ✅ (rayon) |
| Memory-mapped I/O | ❌ | ✅ (memmap2) |

---

## 🤖 Integração com AI (Opcional)

Winx suporta integração com provedores de AI para análise de código:

```bash
# DashScope (Qwen3)
export DASHSCOPE_API_KEY="sua-chave"

# NVIDIA NIM
export NVIDIA_API_KEY="sua-chave"

# Google Gemini
export GEMINI_API_KEY="sua-chave"
```

**Tools AI:**
- `code_analyzer` - Análise de bugs/segurança
- `ai_generate_code` - Geração de código
- `ai_explain_code` - Explicação de código
- `winx_chat` - Chat com assistente

---

## 📝 Changelog

### v0.2.1 (Atual)
- ✅ Paridade 1:1 com WCGW Python
- ✅ 118 testes passando
- ✅ SpecialKey serialization corrigida
- ✅ Mutex safe error handling
- ✅ Race condition fix com tokio::sync::Mutex

### v0.2.0
- Core port de wcgw Python para Rust
- 6 MCP tools implementadas
- 3 modos operacionais

### v0.1.5
- Integração multi-provider AI
- DashScope, NVIDIA NIM, Gemini

---

## 🙏 Créditos

- [rusiaaman/wcgw](https://github.com/rusiaaman/wcgw) - Projeto original em Python
- [anthropics/claude-code](https://github.com/anthropics/claude-code) - Inspiração MCP
- [modelcontextprotocol](https://github.com/modelcontextprotocol) - Especificação MCP

---

## 📜 Licença

MIT - Gabriel Maia ([@gabrielmaialva33](https://github.com/gabrielmaialva33))

---

<p align="center">
  <strong>✨ Feito com 🦀 Rust e ❤️ por Gabriel Maia ✨</strong>
</p>
