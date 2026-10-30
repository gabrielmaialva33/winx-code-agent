# Auditoria do contrato MCP da Winx

Data: 30/10/2026. Base de código: `c9b8465`. Escopo: contrato das ferramentas, esperas, descoberta, respostas estruturadas, erros e anotações. O serviço instalado não foi alterado. Implementação e validação concluídas no worktree `fix/mcp-contract-alignment`, sem commit ou integração à main; evidências no plano `docs/superpowers/plans/2026-10-30-mcp-contract-alignment.md`.

## Referência normativa e limite da conclusão
A página oficial de versionamento identifica `2026-07-28` como a revisão corrente [1]. Foram consultados o contrato de ferramentas MCP [2], o versionamento por requisição [3], o perfil de autorização HTTP [4], a referência de plugins da OpenAI [5], JSON Schema [6–7] e a explicação oficial das anotações [8]. Não foram usadas afirmações de blogs de terceiros como requisitos.

Esta é uma revisão focada, não certificação completa do MCP. Uma verificação estrutural ou um teste de exemplo não prova todos os requisitos do protocolo. Valores de timeout escolhidos pela Winx não são limites impostos pelo MCP; medições locais não devem ser apresentadas como garantias universais do ChatGPT, Claude, Codex ou de um túnel.

## Resultado da revisão
| Área | Evidência local | Avaliação |
|---|---|---|
| Catálogo determinístico | `src/server/catalog.rs`: `ToolKind::ALL`, catálogo em `OnceLock`, filtro por política do principal | Alinhado no código inspecionado; não depende de uma operação anterior da conexão. |
| Identidade de estado | `thread_id` + `workspace_root`, escopo de principal, recibos e cursores; `coherence.rs` e `principal.rs` | Alinhado ao desenho de identificadores explícitos. Conhecer um ID não concede autoridade. |
| Schemas de saída | Nove ferramentas publicam inputSchema/outputSchema; validação Draft 2020-12 dos 18 schemas e de 10 respostas HTTP reais | Sem erros nessa amostra. `data` continua extensível em várias ferramentas; tipagem mais específica é oportunidade, não automaticamente violação. |
| Espera explícita | `wait_budget.rs` promovia return_early/zero/esperas curtas e reescrevia nextAction | Corrigido: padrões automáticos preservados; prazos explícitos apenas limitados para baixo; continuações preservam política, prazo e eventos, inclusive background. |
| Valores ausentes/nulos | Schema aceitava null, mas normalização para f32 rejeitava null | Corrigido: null equivale à omissão; entrada inválida retorna erro antes de iniciar o comando. |
| Erros corrigíveis pelo modelo | Envelopes tipados e novo teste HTTP com comando-marcador | Caminhos de espera deste lote corrigidos e testados; marcador permaneceu ausente. Demais parsers não foram auditados integralmente. |
| Initialize.status | Recuperação persistida pode iniciar nova shell | Descrição corrigida sem retirar a recuperação. É ferramenta Winx, não a mensagem de handshake MCP. |
| Anotação de Initialize | A mesma ferramenta aceita reset e troca de modo | Corrigida para potencialmente destrutiva, sem readOnlyHint=true. Autorizações e permissões não foram alteradas. |
| Texto e dados estruturados | `result_delivery.rs` remove cópia comprovadamente redundante para um perfil e mantém fallback | Otimização deliberada. A OpenAI documenta os dois campos como visíveis; não afirmar que o host sempre descarta `content`. |
| Autorização | HTTP com bearer, controles por principal e validação de vínculo | Controles privados observados não equivalem a implementação integral do perfil OAuth MCP. Não mudar autenticação silenciosamente. |

## Requisitos versus escolhas de produto
MCP exige que resultados estruturados obedeçam ao schema anunciado e distingue falhas do protocolo de falhas corrigíveis da ferramenta [2]. A recomendação de repetir JSON serializado em texto é `SHOULD`, não `MUST`; dispensá-la deve ser uma decisão de compatibilidade documentada. O modo `full` da Winx restaura o espelhamento textual, mas isso não deve ser descrito automaticamente como serialização integral do envelope JSON.

A referência OpenAI expõe tanto `content` quanto `structuredContent` ao modelo; `_meta` possui outro destinatário [5]. O patch preserva a deduplicação atual e corrige a justificativa, sem alegar economia de cobrança ou latência já comprovada.

Em JSON Schema, `default`/`description` são anotações; não aplicam valores nem limites por si só [6]. Limites numéricos precisam de constraints e validação coerente [7]. Null e ausência podem ser distintos em uma API; neste campo opcional da Winx, a decisão documentada é tratar ambos como pedido do padrão.

Anotações de risco ajudam a apresentação, mas não concedem autoridade [8]. Uma ferramenta multiplexada precisa considerar suas ações mais consequentes. Tornar `Initialize` conservadora pode aumentar pedidos de confirmação do cliente, e não deve ser contornado pelo servidor.

## Implementação e validação
O patch preserva `return_early`, os prazos explícitos e as continuações, mantendo padrões automáticos por cliente. `effectiveWait` apresenta a política, o modo e os milissegundos selecionados; não representa tempo decorrido, conclusão nem cancelamento. A evidência também é entregue quando uma chamada adaptativa com Tasks termina durante a janela inline.

O `just check` completo terminou com 1.112 testes aprovados, zero falhas e 23 ignorados, além de formatação, Clippy no host e Windows GNU e compatibilidade Rust 1.90. A revisão independente encontrou dois caminhos restantes (background e Task inline); ambos foram reproduzidos com regressões antes da correção e passaram na validação completa final.

Um servidor HTTP isolado produziu nove descritores e dez respostas de teste. O validador `jsonschema 4.26.0` verificou os 18 schemas completos, nove entradas válidas, uma entrada inválida rejeitada, dez saídas e uma nextAction: zero divergências. Foram usados os objetos originais, sem remover anotações. Isso não cobre todas as combinações de argumentos, ferramentas e clientes.

Fora deste lote: OAuth/discovery de autorização completo, tipagem exaustiva de `data` por ferramenta, revisão de todos os parsers, notificações e Tasks em todas as revisões, otimização do polling e conformance suite integral. Não há certificação completa de conformidade.

## Fontes oficiais
[1] https://modelcontextprotocol.io/specification/versioning
[2] https://modelcontextprotocol.io/specification/2026-07-28/server/tools
[3] https://modelcontextprotocol.io/specification/2026-07-28/basic/versioning
[4] https://modelcontextprotocol.io/specification/2026-07-28/basic/authorization
[5] https://developers.openai.com/plugins/reference
[6] https://json-schema.org/understanding-json-schema/reference/annotations
[7] https://json-schema.org/understanding-json-schema/reference/numeric
[8] https://blog.modelcontextprotocol.io/posts/2026-03-16-tool-annotations/
