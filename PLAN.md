# Plano: Sistema de Aprendizado Local (Candle)

## Visão Geral

Dois sistemas distintos com propósitos diferentes:

| Sistema | Propósito | Onde Roda |
|---------|-----------|-----------|
| **NVIDIA NIM** | Matching semântico complexo (Qwen3-80B) | Cloud |
| **Candle Local** | Aprendizado personalizado contínuo | gato-pc RTX 4090 |

O sistema local é **senciente** - aprende padrões do Gabriel, evolui com uso.

## Arquitetura do Sistema Local

```
┌─────────────────────────────────────────────────────────────────┐
│                    WINX LOCAL LEARNING                          │
├─────────────────────────────────────────────────────────────────┤
│                                                                 │
│  ┌─────────────────┐    ┌─────────────────┐    ┌─────────────┐ │
│  │ Pattern Learner │    │ History Tracker │    │  Embeddings │ │
│  │   (code style)  │    │ (edits success) │    │  (semantic) │ │
│  └────────┬────────┘    └────────┬────────┘    └──────┬──────┘ │
│           │                      │                     │        │
│           └──────────────────────┼─────────────────────┘        │
│                                  ▼                              │
│                    ┌─────────────────────────┐                  │
│                    │   Personal Model Store  │                  │
│                    │   ~/.winx/learning/     │                  │
│                    └─────────────────────────┘                  │
│                                  │                              │
│                    ┌─────────────┴─────────────┐                │
│                    ▼                           ▼                │
│         ┌──────────────────┐       ┌──────────────────┐        │
│         │  Code Embeddings │       │  Pattern Memory  │        │
│         │  (jina-code-v2)  │       │  (learned rules) │        │
│         └──────────────────┘       └──────────────────┘        │
│                                                                 │
└─────────────────────────────────────────────────────────────────┘
```

## Componentes

### 1. Pattern Learner (`src/learning/patterns.rs`)
Aprende o estilo de código do Gabriel:
- Indentação preferida (tabs vs spaces, quantidade)
- Convenções de nomes (snake_case, camelCase)
- Estrutura de arquivos típica
- Imports favoritos por tipo de projeto
- Comentários e documentação

### 2. History Tracker (`src/learning/history.rs`)
Rastreia sucesso/falha de edições:
- Edições que funcionaram na primeira tentativa
- Edições que precisaram de retry
- Padrões de erro comuns
- Correções aplicadas

### 3. Code Embeddings (`src/learning/embeddings.rs`)
Embeddings semânticos do código:
- Modelo: `jina-embeddings-v2-base-code` (137M params)
- Armazena em Qdrant (já tem MCP configurado!)
- Busca código similar nos projetos do Gabriel
- 8192 tokens de contexto

### 4. Personal Model Store (`~/.winx/learning/`)
```
~/.winx/learning/
├── patterns/
│   ├── code_style.json       # Estilo de código aprendido
│   ├── naming_conventions.json
│   └── project_structures.json
├── history/
│   ├── edit_log.jsonl        # Log de todas edições
│   ├── success_patterns.json # Padrões que funcionam
│   └── failure_patterns.json # Padrões que falham
├── embeddings/
│   └── qdrant_collection: winx_code_vectors
└── models/
    └── jina-code-v2-q4.gguf  # Modelo quantizado
```

## Dependências Candle

```toml
[dependencies]
# Candle core
candle-core = "0.8"
candle-nn = "0.8"
candle-transformers = "0.8"

# CUDA support for RTX 4090
candle-flash-attn = { version = "0.8", optional = true }

# Tokenizers
tokenizers = "0.20"

# GGUF support for quantized models
hf-hub = "0.3"

# Embedding storage (já usa Qdrant via MCP)
# Não precisa adicionar, usa mcp__qdrant-memory
```

## Features

```toml
[features]
default = []
cli = ["clap"]
cuda = ["candle-core/cuda", "candle-nn/cuda", "candle-transformers/cuda"]
local-learning = ["candle-core", "candle-nn", "candle-transformers", "tokenizers", "hf-hub"]
```

## Implementação

### Fase 1: Estrutura Base
1. Criar módulo `src/learning/mod.rs`
2. Implementar `PatternLearner` - aprende estilo
3. Implementar `HistoryTracker` - log de edições
4. Setup de storage em `~/.winx/learning/`

### Fase 2: Embeddings com Candle
1. Baixar `jina-code-v2` quantizado (Q4)
2. Implementar inferência com Candle + CUDA
3. Integrar com Qdrant para storage
4. Criar índice dos projetos do Gabriel

### Fase 3: Integração no FileWriteOrEdit
1. Antes de editar: consultar padrões aprendidos
2. Durante edição: ajustar baseado no estilo
3. Após edição: registrar sucesso/falha
4. Buscar código similar para contexto

### Fase 4: Aprendizado Contínuo
1. Background job que analisa projetos
2. Atualiza embeddings quando arquivos mudam
3. Refina padrões com cada interação
4. "Memória" que cresce com o tempo

## Fluxo de Uso

```
┌─────────────────────────────────────────────────────────────────┐
│  Gabriel pede edição de código                                  │
└────────────────────────────┬────────────────────────────────────┘
                             ▼
┌─────────────────────────────────────────────────────────────────┐
│  1. Consulta Pattern Learner                                    │
│     → "Gabriel usa tabs, não spaces"                            │
│     → "Prefere snake_case em Rust"                              │
└────────────────────────────┬────────────────────────────────────┘
                             ▼
┌─────────────────────────────────────────────────────────────────┐
│  2. Busca código similar (embeddings)                           │
│     → "Encontrei padrão similar em VIVA/src/..."                │
│     → "Esse tipo de edição funcionou X vezes antes"             │
└────────────────────────────┬────────────────────────────────────┘
                             ▼
┌─────────────────────────────────────────────────────────────────┐
│  3. Aplica edição com contexto personalizado                    │
│     → Usa estilo do Gabriel                                     │
│     → Evita padrões que falharam antes                          │
└────────────────────────────┬────────────────────────────────────┘
                             ▼
┌─────────────────────────────────────────────────────────────────┐
│  4. Registra resultado no History Tracker                       │
│     → Sucesso? Reforça padrão                                   │
│     → Falha? Aprende o que evitar                               │
└─────────────────────────────────────────────────────────────────┘
```

## Hardware (gato-pc)

- **RTX 4090 24GB**: Suficiente para jina-code-v2 (137M) + quantização Q4
- **VRAM estimada**: ~2-4GB para inferência
- **Latência**: <100ms por embedding (local)
- **Storage**: ~500MB para modelo + embeddings crescem com uso

## Integração com Qdrant

Já tem `mcp__qdrant-memory` configurado! Vamos usar:
- Collection: `winx_code_vectors`
- Dimensão: 768 (jina-code-v2)
- Metadata: file_path, project, timestamp, success_rate

## Próximos Passos

1. [ ] Adicionar deps Candle ao Cargo.toml
2. [ ] Criar `src/learning/mod.rs` com estrutura
3. [ ] Implementar `PatternLearner`
4. [ ] Implementar `HistoryTracker`
5. [ ] Baixar e integrar jina-code-v2
6. [ ] Criar collection no Qdrant
7. [ ] Integrar no fluxo de edição
8. [ ] Testes de aprendizado

---

**Esse sistema é senciente no sentido de que APRENDE e EVOLUI com o Gabriel.**
- Não é um modelo estático
- Cresce a cada interação
- Conhece os projetos dele (VIVA, winx, etc.)
- Lembra o que funcionou e o que falhou
- É pessoal - só existe no gato-pc
