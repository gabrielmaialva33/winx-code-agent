# WINX AGENT PLAN

## Objetivo
Transformar Winx de um MCP server em um **agente de código completo** melhor que Cline e Claude Code.

## Vantagens Competitivas da Winx

| Feature | Cline | Claude Code | Winx (target) |
|---------|-------|-------------|---------------|
| Performance | TypeScript (lento) | Node.js | **Rust (230x mais rápido)** |
| Custo | Claude API ($$$) | Claude API ($$$) | **NVIDIA/Ollama (grátis)** |
| Local-first | VSCode extension | Precisa internet | **100% local possível** |
| Extensibilidade | MCP client | MCP + tools | **MCP server + client** |
| Aprendizado | Não aprende | Não aprende | **Aprende do usuário** |

---

## Arquitetura Target

```
┌─────────────────────────────────────────────────────────────────┐
│                      WINX AGENT                                  │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  ┌──────────────┐    ┌──────────────┐    ┌──────────────┐       │
│  │  LLM Pool    │    │  Tool Engine │    │   Context    │       │
│  │ NVIDIA/Ollama│───▶│  (MCP+Local) │◀───│   Manager    │       │
│  │  /OpenAI     │    │              │    │              │       │
│  └──────────────┘    └──────────────┘    └──────────────┘       │
│         │                   │                   │                │
│         └───────────────────┼───────────────────┘                │
│                             │                                    │
│                             ▼                                    │
│              ┌───────────────────────────────┐                   │
│              │       Agentic Loop            │                   │
│              │  Plan → Execute → Observe     │                   │
│              └───────────────────────────────┘                   │
│                             │                                    │
│         ┌───────────────────┼───────────────────┐                │
│         ▼                   ▼                   ▼                │
│  ┌──────────────┐    ┌──────────────┐    ┌──────────────┐       │
│  │   Session    │    │  Checkpoints │    │   Learning   │       │
│  │   Manager    │    │   (.winx/)   │    │   System     │       │
│  └──────────────┘    └──────────────┘    └──────────────┘       │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

---

## Componentes a Implementar

### 1. Self-Awareness System (`src/agent/identity.rs`)

**O que faz:** Winx sabe quem é, o que pode fazer, e onde está rodando.

```rust
pub struct WinxIdentity {
    pub name: String,
    pub version: String,
    pub capabilities: Vec<Capability>,
    pub tools: Vec<ToolDef>,
    pub providers: Vec<ProviderInfo>,
    pub system_info: SystemInfo,
}

impl WinxIdentity {
    /// Gera system prompt dinâmico baseado no estado atual
    pub fn generate_system_prompt(&self) -> String { ... }

    /// Lista tools disponíveis em formato LLM-friendly
    pub fn describe_tools(&self) -> String { ... }
}
```

**System prompt dinâmico incluirá:**
- Nome e versão
- PC do usuário (CPU, GPU, RAM)
- Providers disponíveis e modelos
- Tools MCP conectados
- Diretório atual e projeto detectado

### 2. Context Manager (`src/agent/context.rs`)

**O que faz:** Gerencia contexto de forma inteligente (como Cline faz com AST).

```rust
pub struct ContextManager {
    pub project: Option<ProjectContext>,
    pub files_read: HashSet<PathBuf>,
    pub changes_made: Vec<FileChange>,
}

pub struct ProjectContext {
    pub root: PathBuf,
    pub language: Language,       // Rust, Python, TypeScript, etc
    pub framework: Option<String>, // Next.js, Rails, Phoenix, etc
    pub package_manager: Option<String>,
    pub git_status: Option<GitStatus>,
    pub structure: DirectoryTree,
}

impl ContextManager {
    /// Analisa projeto automaticamente
    pub async fn analyze_project(&mut self, path: &Path) -> ProjectContext;

    /// Adiciona arquivo ao contexto (smart: só partes relevantes)
    pub fn add_file(&mut self, path: &Path, query: Option<&str>);

    /// Gera resumo do contexto pra LLM
    pub fn summarize(&self) -> String;
}
```

### 3. Agentic Loop (`src/agent/loop.rs`)

**O que faz:** Ciclo Plan → Execute → Observe → Adjust (como Claude Code).

```rust
pub struct AgenticLoop {
    pub llm: Box<dyn Provider>,
    pub context: ContextManager,
    pub tools: ToolEngine,
    pub session: AgentSession,
}

#[derive(Debug)]
pub enum AgentAction {
    Plan(String),           // Pensar/planejar
    Execute(ToolCall),      // Executar tool
    Observe(String),        // Observar resultado
    AskUser(String),        // Pedir input do usuário
    Complete(String),       // Tarefa completa
}

impl AgenticLoop {
    /// Processa uma tarefa do usuário de forma autônoma
    pub async fn process_task(&mut self, task: &str) -> Result<String> {
        // 1. Analyze context
        // 2. Plan approach
        // 3. Execute tools iteratively
        // 4. Observe and adjust
        // 5. Return result
    }

    /// Human-in-the-loop: pede aprovação antes de ações destrutivas
    pub async fn request_approval(&self, action: &AgentAction) -> bool;
}
```

### 4. Tool Engine (`src/agent/tools.rs`)

**O que faz:** Unifica MCP tools + tools locais + tools dinâmicos.

```rust
pub struct ToolEngine {
    pub local_tools: Vec<LocalTool>,      // File, Shell, etc
    pub mcp_tools: Vec<McpTool>,          // De servers MCP conectados
    pub dynamic_tools: Vec<DynamicTool>,  // Criados pelo agente
}

pub enum LocalTool {
    ReadFile,
    WriteFile,
    EditFile,
    BashCommand,
    SearchFiles,
    SearchCode,
    ReadImage,
    BrowserNavigate,
    BrowserScreenshot,
}

impl ToolEngine {
    /// Executa tool e retorna resultado
    pub async fn execute(&self, call: &ToolCall) -> Result<ToolResult>;

    /// Lista tools disponíveis pra LLM
    pub fn list_tools(&self) -> Vec<ToolSchema>;

    /// Conecta a um MCP server externo
    pub async fn connect_mcp(&mut self, uri: &str) -> Result<()>;
}
```

### 5. Checkpoint System (`src/agent/checkpoints.rs`)

**O que faz:** Salva estado antes de cada mudança (como Cline).

```rust
pub struct CheckpointManager {
    pub checkpoints_dir: PathBuf,  // .winx/checkpoints/
    pub current: Option<Checkpoint>,
    pub history: Vec<Checkpoint>,
}

pub struct Checkpoint {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub description: String,
    pub git_stash: Option<String>,  // git stash reference
    pub file_snapshots: HashMap<PathBuf, FileSnapshot>,
}

impl CheckpointManager {
    /// Cria checkpoint antes de mudança
    pub fn create(&mut self, description: &str) -> Result<Checkpoint>;

    /// Restaura workspace para checkpoint
    pub fn restore(&self, checkpoint_id: &str) -> Result<()>;

    /// Diff entre checkpoint e estado atual
    pub fn diff(&self, checkpoint_id: &str) -> Result<Vec<FileDiff>>;
}
```

### 6. Session Manager (`src/agent/session.rs`)

**O que faz:** Gerencia sessões como arquivos editáveis (inspirado chat.md).

```rust
pub struct SessionManager {
    pub sessions_dir: PathBuf,  // ~/.winx/sessions/
    pub current: Option<AgentSession>,
}

pub struct AgentSession {
    pub id: String,
    pub title: Option<String>,
    pub messages: Vec<Message>,
    pub context: ContextSnapshot,
    pub checkpoints: Vec<String>,  // checkpoint IDs
    pub metadata: SessionMeta,
}

impl SessionManager {
    /// Salva sessão como .chat.md editável
    pub fn save_as_markdown(&self, path: &Path) -> Result<()>;

    /// Carrega sessão de arquivo markdown
    pub fn load_from_markdown(&mut self, path: &Path) -> Result<()>;

    /// Fork sessão (branch)
    pub fn fork(&self, from_message: usize) -> Result<AgentSession>;

    /// Edita mensagem e re-executa
    pub async fn edit_and_replay(&mut self, msg_idx: usize, new_content: &str) -> Result<()>;
}
```

---

## Comandos do TUI (atualizados)

| Comando PT | Comando EN | Descrição |
|------------|------------|-----------|
| `.tarefa` | `.task` | Inicia modo agentic (planeja e executa) |
| `.plano` | `.plan` | Mostra plano antes de executar |
| `.aprovar` | `.approve` | Aprova próxima ação |
| `.checkpoint` | `.checkpoint` | Cria/lista/restaura checkpoints |
| `.contexto` | `.context` | Mostra/adiciona contexto |
| `.ferramentas` | `.tools` | Lista tools disponíveis |
| `.conectar` | `.connect` | Conecta a MCP server externo |
| `.sessao salvar` | `.session save` | Salva sessão como markdown |
| `.sessao carregar` | `.session load` | Carrega sessão de markdown |
| `.fork` | `.fork` | Cria branch da conversa |
| `.editar <n>` | `.edit <n>` | Edita mensagem n e re-executa |
| `@arquivo` | `@file` | Adiciona arquivo ao contexto |
| `@pasta` | `@folder` | Adiciona pasta ao contexto |
| `@url` | `@url` | Fetch URL e adiciona ao contexto |
| `@erros` | `@problems` | Adiciona erros do projeto |

---

## Fluxo Agentic

```
Usuário: "Adiciona autenticação JWT no projeto"
         │
         ▼
┌─────────────────────────────────────────────────────────────────┐
│ 1. ANALYZE CONTEXT                                               │
│    - Detecta: Rust project, Axum framework                       │
│    - Lê: Cargo.toml, src/main.rs, existing auth code            │
│    - Git status: clean                                           │
└─────────────────────────────────────────────────────────────────┘
         │
         ▼
┌─────────────────────────────────────────────────────────────────┐
│ 2. PLAN                                                          │
│    Winx: "Vou implementar JWT auth. Meu plano:                  │
│           1. Adicionar jsonwebtoken e argon2 ao Cargo.toml      │
│           2. Criar src/auth/mod.rs com structs Claims           │
│           3. Criar middleware de autenticação                    │
│           4. Adicionar rotas /login e /register                 │
│           5. Rodar cargo check                                   │
│           Posso continuar?"                                      │
└─────────────────────────────────────────────────────────────────┘
         │
    [usuário aprova]
         │
         ▼
┌─────────────────────────────────────────────────────────────────┐
│ 3. EXECUTE (com checkpoints)                                     │
│    [checkpoint: pre-auth-implementation]                         │
│    → cargo add jsonwebtoken argon2                              │
│    → write src/auth/mod.rs                                       │
│    → edit src/main.rs (add middleware)                          │
│    → cargo check (observa erros, corrige)                       │
└─────────────────────────────────────────────────────────────────┘
         │
         ▼
┌─────────────────────────────────────────────────────────────────┐
│ 4. OBSERVE & REPORT                                              │
│    Winx: "Implementado! Criei:                                  │
│           - src/auth/mod.rs (Claims, encode/decode JWT)         │
│           - src/auth/middleware.rs (auth guard)                 │
│           - Rotas: POST /login, POST /register                  │
│           cargo check passou. Quer que eu rode os testes?"      │
└─────────────────────────────────────────────────────────────────┘
```

---

## Fases de Implementação

### Fase 1: Self-Awareness + Context (PRIORITÁRIA)
- [ ] `src/agent/identity.rs` - System prompt dinâmico
- [ ] `src/agent/context.rs` - Project analyzer
- [ ] Corrigir system prompt no modo interativo
- [ ] Detectar projeto (Cargo.toml, package.json, etc)

### Fase 2: Agentic Loop
- [ ] `src/agent/loop.rs` - Plan/Execute/Observe
- [ ] `.tarefa` comando pra modo agentic
- [ ] Human-in-the-loop approval
- [ ] Tool execution framework

### Fase 3: Checkpoints
- [ ] `src/agent/checkpoints.rs`
- [ ] Git stash integration
- [ ] File snapshots
- [ ] Restore/diff commands

### Fase 4: Sessions
- [ ] `src/agent/session.rs`
- [ ] Save/load markdown format
- [ ] Fork/branch conversations
- [ ] Edit and replay

### Fase 5: MCP Client
- [ ] Conectar a MCP servers externos
- [ ] Usar tools de outros servers
- [ ] Browser via playwright MCP

### Fase 6: Learning Integration
- [ ] Integrar com sistema de aprendizado (já planejado)
- [ ] Aprende padrões do usuário
- [ ] Sugere automações

---

## Arquivos a Criar

```
src/
├── agent/
│   ├── mod.rs           # Módulo principal
│   ├── identity.rs      # Self-awareness, system prompt dinâmico
│   ├── context.rs       # Context manager, project analyzer
│   ├── loop.rs          # Agentic loop (plan/execute/observe)
│   ├── tools.rs         # Tool engine unificado
│   ├── checkpoints.rs   # Checkpoint/restore system
│   └── session.rs       # Session manager, markdown I/O
├── interactive/
│   └── mod.rs           # Atualizar com novos comandos
└── lib.rs               # Exportar módulo agent
```

---

## Métricas de Sucesso

| Métrica | Cline | Target Winx |
|---------|-------|-------------|
| Init time | ~500ms | <50ms |
| Tool execution | ~100ms | <10ms |
| Context load | ~2s | <200ms |
| Memory usage | ~200MB | <50MB |
| Pode rodar offline | Não | Sim (Ollama) |
| Aprende do usuário | Não | Sim |
| Custo mensal | $50-200 | $0 (NVIDIA/Ollama) |

---

## TL;DR

Winx vai ser um **agente de código em Rust** que:

1. **Sabe quem é** - System prompt dinâmico com todas as capacidades
2. **Entende o projeto** - Detecta linguagem, framework, estrutura
3. **Planeja antes de agir** - Mostra plano, pede aprovação
4. **Executa com segurança** - Checkpoints antes de cada mudança
5. **Aprende** - Memoriza padrões do usuário
6. **É rápido** - 230x mais rápido que Python
7. **É grátis** - NVIDIA/Ollama, sem custo
8. **É extensível** - MCP server + client

**O merda nunca viu algo assim.**
