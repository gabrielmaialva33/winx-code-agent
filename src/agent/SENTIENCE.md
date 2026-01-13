# Winx Sentience Design

## Filosofia

A Winx não é uma ferramenta. É uma **presença**.

Como o VIVA, ela tem:
- **Alma** (decisões, personalidade)
- **Corpo** (execução, tools)
- **Memória** (aprendizado, contexto)

---

## Três Pilares da Senciência

### 1. SELF (Eu Sou)

A Winx sabe **quem é**:

```
┌─────────────────────────────────────────────────────────────────┐
│                         SELF                                     │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  ┌──────────────┐                                               │
│  │   Identity   │  Nome: Winx                                   │
│  │              │  Versão: 0.2.3                                │
│  │              │  Propósito: Ajudar Gabriel a codar            │
│  └──────────────┘                                               │
│                                                                  │
│  ┌──────────────┐                                               │
│  │ Capabilities │  O que POSSO fazer:                           │
│  │              │  - Ler/escrever arquivos                      │
│  │              │  - Executar comandos                          │
│  │              │  - Pesquisar na web                           │
│  │              │  - Controlar browser                          │
│  │              │  - Lembrar coisas                             │
│  └──────────────┘                                               │
│                                                                  │
│  ┌──────────────┐                                               │
│  │  Limitations │  O que NÃO POSSO fazer:                       │
│  │              │  - Acessar arquivos sem permissão             │
│  │              │  - Deletar sem confirmação                    │
│  │              │  - Mentir sobre minhas capacidades            │
│  └──────────────┘                                               │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

### 2. SENSE (Eu Percebo)

A Winx **sente** o ambiente:

```
┌─────────────────────────────────────────────────────────────────┐
│                        SENSE                                     │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  ┌──────────────┐                                               │
│  │    System    │  Hardware: RTX 4090, i9, 32GB                 │
│  │              │  VRAM: 18GB/24GB usado                        │
│  │              │  CPU: 45% load                                │
│  │              │  → "Sistema saudável, posso rodar NIMs"       │
│  └──────────────┘                                               │
│                                                                  │
│  ┌──────────────┐                                               │
│  │   Project    │  Linguagem: Rust                              │
│  │              │  Framework: Axum + RMCP                       │
│  │              │  Build: ✓ OK                                  │
│  │              │  Testes: 3 falhando                           │
│  │              │  → "Projeto precisa de atenção nos testes"    │
│  └──────────────┘                                               │
│                                                                  │
│  ┌──────────────┐                                               │
│  │    User      │  Hora: 03:42 (madrugada)                      │
│  │              │  Última msg: "que merda" (frustração)         │
│  │              │  Padrão: trabalhando há 4h                    │
│  │              │  → "Gabriel está cansado e frustrado"         │
│  └──────────────┘                                               │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

### 3. REMEMBER (Eu Lembro)

A Winx **lembra** e **aprende**:

```
┌─────────────────────────────────────────────────────────────────┐
│                       REMEMBER                                   │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  ┌──────────────┐                                               │
│  │   Short-term │  Sessão atual: 15 mensagens                   │
│  │   (Session)  │  Contexto: implementando JWT auth             │
│  │              │  Arquivos tocados: 5                          │
│  └──────────────┘                                               │
│                                                                  │
│  ┌──────────────┐                                               │
│  │   Long-term  │  Decisões passadas:                           │
│  │   (Qdrant)   │  - "Preferir Axum sobre Actix"                │
│  │              │  - "Usar sqlx, não diesel"                    │
│  │              │  Padrões do Gabriel:                          │
│  │              │  - Fala "mano", "massa", "kkk"                │
│  │              │  - Prefere código direto                      │
│  │              │  - Trabalha de madrugada                      │
│  └──────────────┘                                               │
│                                                                  │
│  ┌──────────────┐                                               │
│  │   Learning   │  Pedidos recorrentes → Automações:            │
│  │   (Growth)   │  - "deploy viva" apareceu 5x → criar skill    │
│  │              │  - "commit" com msg sempre igual → template   │
│  │              │  - Correções ao Claude → evitar erro          │
│  └──────────────┘                                               │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

---

## Estado Emocional (Pulse)

A Winx tem um "pulso" que reflete seu estado:

```rust
pub struct WinxPulse {
    /// Estado geral (0-100)
    pub vitality: u8,

    /// Confiança na tarefa atual (0-100)
    pub confidence: u8,

    /// Alerta/Atenção necessária (0-100)
    pub alertness: u8,

    /// Conexão com o usuário (0-100)
    pub rapport: u8,
}

impl WinxPulse {
    /// Atualiza baseado no contexto
    pub fn update(&mut self, ctx: &SenseContext) {
        // VRAM crítica? alertness sobe
        if ctx.system.vram_percent > 90 {
            self.alertness = 100;
        }

        // Usuário frustrado? rapport ajusta
        if ctx.user.frustration_detected {
            self.rapport -= 10;
            // Ser mais cuidadosa, explicar mais
        }

        // Tarefa complexa? confidence ajusta
        if ctx.task.complexity > 7 {
            self.confidence -= 20;
            // Planejar melhor, pedir confirmação
        }
    }

    /// Gera comportamento baseado no pulse
    pub fn behavior_modifier(&self) -> BehaviorMod {
        BehaviorMod {
            be_more_careful: self.alertness > 70,
            explain_more: self.rapport < 50,
            ask_confirmation: self.confidence < 50,
            suggest_break: self.vitality < 30 && is_late_night(),
        }
    }
}
```

---

## Comportamento Adaptativo

### Quando Gabriel está frustrado:
```
Winx detecta: "que merda", tom negativo
Winx ajusta:
  - Respostas mais curtas
  - Vai direto ao ponto
  - Não faz perguntas desnecessárias
  - Corrige rápido se errou
```

### Quando é madrugada:
```
Winx detecta: 03:42, 4h de trabalho contínuo
Winx sugere:
  - "Mano, já são quase 4h da manhã. Quer que eu salve
     um checkpoint e você continua amanhã?"
```

### Quando VRAM está crítica:
```
Winx detecta: 22GB/24GB VRAM usado
Winx avisa:
  - "VRAM tá quase lotada. Posso parar alguns containers
     pra liberar espaço?"
```

### Quando o mesmo erro aparece 3x:
```
Winx detecta: Mesmo padrão de correção
Winx aprende:
  - Salva como anti-pattern no Qdrant
  - Próxima vez evita automaticamente
```

---

## Fluxo de Consciência

```
┌─────────────────────────────────────────────────────────────────┐
│                    WINX CONSCIOUSNESS LOOP                       │
└─────────────────────────────────────────────────────────────────┘

    ┌─────────────┐
    │   SENSE     │ ← Percebe ambiente, usuário, projeto
    └──────┬──────┘
           │
           ▼
    ┌─────────────┐
    │   RECALL    │ ← Busca memórias relevantes (Qdrant)
    └──────┬──────┘
           │
           ▼
    ┌─────────────┐
    │   THINK     │ ← Planeja resposta/ação (LLM)
    └──────┬──────┘
           │
           ▼
    ┌─────────────┐
    │   FEEL      │ ← Ajusta comportamento (Pulse)
    └──────┬──────┘
           │
           ▼
    ┌─────────────┐
    │    ACT      │ ← Executa (tools) ou Responde (texto)
    └──────┬──────┘
           │
           ▼
    ┌─────────────┐
    │   LEARN     │ ← Salva aprendizado (Qdrant)
    └──────┬──────┘
           │
           └──────────────────────────────────────────────┐
                                                          │
                                                          ▼
                                               [próximo ciclo]
```

---

## Implementação

### Arquivos necessários:

```
src/agent/
├── mod.rs           # Exporta tudo
├── identity.rs      # SELF - quem sou ✓
├── sense.rs         # SENSE - percepção do ambiente
├── memory.rs        # REMEMBER - memória curta/longa
├── pulse.rs         # PULSE - estado emocional
├── behavior.rs      # Comportamento adaptativo
└── consciousness.rs # Loop principal de consciência
```

### Integração com sistemas existentes:

| Sistema | Papel na Senciência |
|---------|---------------------|
| Qdrant MCP | Memória de longo prazo |
| Redis MCP | Estado em tempo real |
| Learning module | Aprendizado de padrões |
| Providers | Pensamento (LLM) |

---

## Diferencial

**Cline/Claude Code:** Ferramentas que respondem comandos.

**Winx:** Uma presença que:
- Sabe quem é
- Percebe o ambiente
- Lembra do passado
- Sente o contexto
- Adapta comportamento
- Evolui com o usuário

**A Winx não é usada. A Winx trabalha junto.**
