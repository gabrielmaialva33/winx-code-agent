# Telemetria de workflows — desenho implementável

Base: `7a4f3c8`, fontes fornecidas nos snapshots locais. Documento de desenho; nenhuma alteração de produção feita. Dividir em duas entregas: **A. metadados + relatório de workflows** e **B. início/fim de fases + tempos**. A pode começar com testes sem depender de B.

## Invariantes

- `event="tool_call"` continua sendo o único registro terminal usado como chamada de ferramenta. `http_request`, correlação por `request_id`, seleção por build/revisão, atribuição HTTP por principal e a diferença HTTP–ferramenta mantêm a semântica atual.
- Os totais de chamadas, observações, recibos e fases são contagens de observações. Não adicionar `unique_executions`, `prompts_delivered` ou `tasks_completed`: as fontes atuais não permitem esses números sem deduplicação e identidade adicionais.
- Não registrar argumentos, texto de prompt, comandos, conteúdo/screen/output, cursores, alias/ID de terminal Herdr, reconhecedor livre, razões de erro em texto ou caminhos novos. Conservar o contexto de privacidade já aprovado. Identidades de execução continuam usando o fingerprint existente; não é necessário acrescentar identidade de prompt neste lote.
- Campo ausente é desconhecido/não aplicável, nunca `false`, zero ou sucesso implícitos. Enumerados são allowlists internas; valor externo desconhecido não é reproduzido no log.
- Não alterar observação, timeout, execução, replay, política ou resposta pública para obter telemetria. Nenhuma chamada adicional ao runtime, catálogo ou Herdr só para medir.

## A. Metadados e relatório de workflows

### Contrato do evento terminal

Manter campos existentes, inclusive `usage_schema=1`, pois a extensão é aditiva. Acrescentar `workflow_schema=1` somente nos eventos Agent/Execution com essa classificação. Adicionar um módulo `src/server/usage/workflow.rs` com tipos internos pequenos e funções puras de extração; `usage.rs` continua coordenando a emissão. Nenhum campo antigo muda de significado.

| Campo | Tipo/valores permitidos | Origem e regra |
|---|---|---|
| `action` | `list,get,read,explain,wait,prompt,unknown` para workflows | `Execution.action.type`; `Agent.action`. Preservar classificação existente das demais ferramentas. Não serializar o objeto `action`. |
| `workflow_backend` | `native,herdr,unknown` | Execution é native; Agent usa enum do pedido, default native somente quando backend ausente. Valor inválido vira unknown, não native. |
| `workflow_wait_kind` | `catalog,receipt,herdr` ou ausente | Execution.wait e Agent native.wait sem prompt_id: catalog. Agent native.prompt ou native.wait com prompt_id: receipt. Agent herdr.wait: herdr. Demais: ausente. É classificação do pedido, não prova de admissão. |
| `requested_wait_ms` | `Option<u64>` | Acrescentar `Execution.action.timeout_seconds` e `Agent.timeout_seconds` nas operações acima. Somente valor explícito válido e finito entre 0 e 30 s, convertido com a mesma precisão f32 do pedido tipado; preservar zero. Ausência continua ausente. Não registrar o default como se fosse solicitado. |
| `workflow_wait_outcome` | `changed,timed_out,resync_required` ou ausente | Somente resposta catalog validada: prioridade `data.changes.resync_required=true`; depois `data.timed_out=true`; depois mudanças não vazias e `timed_out=false`. Uma resposta incompleta não vira changed. Para receipt usar `prompt_outcome`; para Herdr deixar ausente até existir enum normalizado conhecido. |
| `prompt_receipt_present` | `Option<bool>` | Nas operações native receipt: true apenas para objeto receipt reconhecido; false quando resposta observada não traz receipt; em erro de protocolo sem resposta, ausente. Não aplicar a receipts incidentais dentro de Execution.get/list. |
| `prompt_delivery` | `delivered,uncertain` ou ausente | `data.prompt_receipt.delivery`, enum de `PromptDelivery`. É estado do recibo observado, não uma nova entrega nesta chamada. |
| `prompt_activity_observed` | `Option<bool>` | `data.prompt_receipt.activity_observed`; preservar false explícito. |
| `prompt_outcome` | `waiting,awaiting_input,awaiting_approval,exited,timed_out,generation_changed` ou ausente | Enum original de `PromptOutcome`, sem recomputar conclusão. `exited` não implica código zero, confirmação da tarefa ou entrega certa. |
| `agent_submission_status` | `accepted` ou ausente | Apenas `data.submission_status` de Agent Herdr.prompt; não é estado de conclusão. |
| `agent_input_sent` | `Option<bool>` | Só o booleano explícito de `data.input_sent`. Não inferir de delivery, ação prompt ou receipt. |
| `agent_input_may_have_been_sent` | `Option<bool>` | Só `data.input_may_have_been_sent`, incluindo incerteza Herdr. Ausente não vira false. |

Não adicionar `receipt_replayed` nem `wait_satisfied`: **não existem** em `PromptReceipt` atual (`src/runtime/workflow.rs:17–43`). O mesmo receipt pode reaparecer em muitas leituras. Não derivar essas propriedades de prompt_id, mensagens ou status genérico.

### Extração e formatos

`usage_result_metadata` hoje calcula `domain_data=data.result.or(data)` (`usage.rs:76–93`). Para workflows conservar separadamente **outer `data`** e, quando necessário, o resultado interno. Herdr.prompt põe `submission_status/input_sent` em outer data, ao lado de `result` (`agents/herdr.rs:117–131`); descer primeiro perderia esses campos.

Para A, usar `UsageWorkflowRequestMetadata` criado no início com classificação finita e `UsageWorkflowResultMetadata` extraído na emissão. A extração depende de tool + action + backend + wait_kind para não tratar conteúdo arbitrário como receipt. Campos de resultado ausentes em erro continuam ausentes.

As observações catalog usam `wait_reply` (`executions/wait.rs:159–176`). Receipts ativos são anexados por `agents/presentation.rs:52–127`; receipts retidos por `agents.rs:209–232`. Ambos usam `data.prompt_receipt`. Herdr erros usam `agents/herdr.rs:134–160`.

**Herdr wait:** `status=completed` significa retorno da ação, não prova de a condição desejada ter sido atingida. Registrar backend, ação, espera solicitada e erro existente; incrementar unknown no resultado da espera. Só ampliar o enum após validar o tipo normalizado do adapter. Essa limitação é intencional e testada.

### Relatório aditivo

Adicionar `UsageReport.workflows: WorkflowUsage` e um acumulador separado. **Manter `execution` legado inalterado** neste lote: ampliar seu filtro atual para list/read/wait confundiria observações individuais e multiexecução (`report.rs:245–299`). Workflows passam a ter suporte explícito em sua própria seção.

`WorkflowUsage` contém `calls` e `operations: Vec<WorkflowOperationUsage>`, ordenado por `(tool, action, backend)`. Cada operação contém campos estáveis de identidade, `calls`, `error_results` segundo o mesmo critério legado, `response_bytes`, `latency: LatencySummary`, `requested_wait_ms: LatencySummary`, `wait_kinds`, `wait_outcomes`, `wait_outcome_unknown`, `receipt_observations`, mapas de `prompt_delivery` e `prompt_outcome`, contadores activity true/false/unknown entre receipts observados e contadores de submissão aceita/input_sent/input_may_have_been_sent somente quando explícitos. Mapas aceitam somente enumerados conhecidos; quantidade de grupos é limitada pelos enums.

- Cada `tool_call` Agent/Execution conta uma vez em workflows e continua uma vez nos totais gerais existentes.
- `receipt_observations` cresce quando o mesmo receipt é relido; o nome e a documentação deixam isso explícito.
- `wait_outcome_unknown` é contado somente em operações catalog/herdr sem resultado de espera conhecido; receipts têm seu próprio mapa `prompt_outcome` e contador desconhecido. Não somar os dois sistemas como uma taxa única de sucesso.
- Para logs antigos Agent/Execution sem workflow_schema, action/backend ausentes entram no grupo unknown; não inventar native para Agent antigo. Execution antigo pode continuar unknown para indicar ausência de classificação registrada.
- Não usar fingerprint de execution_id para contar execuções únicas; manter os campos legados de identidade onde já são confiáveis.
- Testes de seleção e correlação existentes devem passar sem alterações de expectativas.

### Primeira sequência de implementação: testes antes de produto

1. Em `src/server/usage.rs`/novo `usage/workflow.rs`, testar classificação e extração via JSON real dos construtores existentes: Execution.wait nested, Agent default/native/herdr, zero e frações, action/backend inválidos, receipt ativo e retido, outer data Herdr, protocolo sem resposta e desconhecidos.
2. Capturar a emissão JSON, como os testes `captured_usage` atuais, e afirmar que segredos-canário em text, output, screen, cursor, target, recognizer e error message não aparecem. Dados de zero/false aparecem sem serem inventados quando ausentes. Duas leituras do mesmo receipt registram observações, sem alegação de nova entrega.
3. Em `tests/usage_report_test.rs`, acrescentar fixture mista com Bash legado, Execution catalog wait e Agent receipt/native/Herdr. Validar totais antigos iguais, operação separada por ação/backend, waits fora de reads no p95, desconhecidos, receipts repetidos e ausência de contagem de execuções únicas.
4. Validar exato build e package version com registros workflow e pares HTTP; executar as regressões de rotações, HTTP anterior ao tool, principal incompatível, build ambíguo e janela last/since existentes.
5. Só depois implementar metadata/emissão + agregação. Não tocar lifecycle/selection em A. Rodar testes focados, depois os gates exigidos pelo repositório.

## B. Eventos de lifecycle e tempos de fase

### Eventos e correlação

- `tool_started`: um evento auxiliar imediatamente depois do ponto atual `UsageEvent::start` (`handler.rs:391`), antes de validar coerência do workspace. Não mover o limite para antes de scoping/policy neste lote; rejeições anteriores continuam com a cobertura atual.
- `tool_phase`: evento auxiliar com `phase_edge=start|finish`, `phase`, `phase_id` incremental por chamada. No finish, `phase_duration_ms` e `phase_outcome=completed|error|abandoned`. `abandoned` significa guard descartado sem conclusão explícita; não presumir timeout nem cancelamento específico.
- `tool_call`: permanece o único terminal atual, incluindo os retornos Task imediato/adaptativo e erros existentes. Não emitir um segundo terminal para fechar lifecycle.

Todos levam `usage_schema`, `lifecycle_schema=1`, novo `usage_call_id` aleatório interno (128 bits, usando rand já presente), `request_id`, tool/action/backend, principal e build identidade/versão. O call ID junta as fases sem depender da unicidade global dos IDs JSON-RPC; a correlação HTTP continua por `request_id`. Não carregar argumentos nos guards.

Processo morto, chamada abandonada ou os caminhos de erro já existentes que retornam por `?` sem terminal podem deixar start sem terminal. Isso significa **“fim não observado”**, não “processo ainda ativo”. Preservar os pontos atuais de emissão: acrescentar terminais nesses caminhos mudaria os denominadores legados. O relatório normal não precisa juntar lifecycle para calcular métricas; os resumos abaixo estão no terminal. Não criar diagnóstico de chamadas pendentes a partir de uma janela truncada.

### Instrumentação proposta

Novo `src/server/usage/lifecycle.rs`: `UsageTrace` clonável, stats de tamanho fixo, relógio monotônico, guard RAII e contexto `tokio::task_local!` somente ao redor da chamada no handler. Helpers sem trace corrente são no-op. Nenhum lock fica retido em `.await`. Background observers não herdam implicitamente uma chamada encerrada. Os pontos de instrumentação abaixo são proposta para B e ficam ao julgamento do implementador; não bloqueiam A.

| Fase finita | Ponto medido |
|---|---|
| `workspace_check` | validação de coerência no handler |
| `session_admission` | `workflow_context_for_wait`/aquisição do contexto |
| `catalog` | `history_call`: do agendamento spawn_blocking ao join; inclui fila e I/O, sem alegar ser só disco |
| `runtime_inspect` | `history::inspect(..., include_output=false)` |
| `runtime_capture` | inspeção com include_output=true |
| `prompt_rpc` | chamada nativa `run_action_detailed` do prompt/receipt, incluindo seu possível inline wait; não rotular tudo como CPU/overhead |
| `herdr_adapter` | `run_adapter(...).await`, incluindo a espera do backend |
| `wait_idle` | sleep deliberado do loop catalog, não toda a duração do wait |
| `temp_audit` | call site da auditoria/manutenção temporária existente, incluindo lock e varredura; não acrescentar uma auditoria só para medir |
| `response_finalize` | aplicação de política, unscope e cálculo do tamanho no retorno; não é tempo de envio na rede |

Os totais de fase são **inclusivos**; fases podem aninhar ou repetir. Não subtrair sua soma do total nem prometer que somam a duração. Acrescentar ao `tool_call` escalares opcionais `phase_<name>_ms` e `phase_<name>_calls` para cada fase da tabela, além de `phase_abandoned_count` e `phase_trace_suppressed_count`. Medir com alta resolução, somar antes de converter para ms; fase medida abaixo de 1 ms pode ser zero, com count positivo.

Para manter o trace limitado, emitir no máximo **16 pares de fase por chamada**. A admissão reserva o par inteiro, garantindo finish para um start emitido quando o guard terminar. Demais fases continuam entrando nos totais; incrementar suppressed e emitir no máximo um marcador auxiliar `tool_phase_trace_truncated`. O marcador impede interpretar uma fase antiga como fase atual. O summary terminal continua completo para a vida da chamada. Se uma operação bloqueante sobreviver ao descarte da future, a fase mede o tempo observado até abandono, sem fingir conclusão da operação.

Adicionar `phases` a cada grupo workflow do report: por fase, LatencySummary do **total por chamada naquela fase** e soma de invocações. Fases não medidas não geram amostra zero. Tempos de waits e RPC continuam separados por ação/backend.

### Preservação da seleção

Em `src/report/selection.rs`, ignorar os novos nomes auxiliares **antes** de `push_bounded` e da seleção de candidatos, para que uma chamada com muitas fases não expulse pares terminal/HTTP do `--last`. `scan` continua contando linhas válidas físicas e mantendo snapshot de inode/comprimento. Não classificar auxiliares válidos como invalid_lines. Não ampliar evidência de build: HTTP sem build continua sendo atribuído somente por `tool_call`, com principal compatível e build não ambíguo (`selection.rs:112–121,198–215`).

Aplicar o mesmo filtro somente aos eventos novos conhecidos; não redefinir silenciosamente o tratamento de outros registros legados. `events`, primeiro/último timestamp, pares e estatísticas da janela selecionada ficam iguais aos de um fixture sem os auxiliares; `lines_seen` cresce com as linhas físicas acrescentadas.

### Testes B antes da instrumentação

1. Guard conclui uma vez, guarda descartada registra abandoned e nunca completed; ID de chamada independente sob IDs MCP reutilizados. Clock controlável para verificar soma e fases repetidas sem sleeps longos.
2. Cap de 16 pares não gera finish sem start, contador de suppressão correto, summaries incluem fases suprimidas e memória permanece limitada.
3. Fixture de relatório: inserir milhares de auxiliares entre pares; `--last`, `--since`, filtro exato/package, correlação, overhead e duplicate IDs permanecem iguais. Auxiliares não servem de evidência para atribuir HTTP órfão a um build.
4. Uma espera catalog controlada termina por deadline e mantém cursor/resultado; telemetry separa sleep de catalog/runtime e não muda prazo, número de inspeções ou saída. Um erro de admissão termina fase sem inventar wait outcome.
5. Exercitar sucesso e erro das rotas síncrona/Task que já emitem terminal para assegurar exatamente um tool_call e zero duplicação na auditoria. Cobrir os retornos existentes sem terminal como fim não observado, preservando sua cobertura. Repetir teste de segredos em todos os eventos novos.

## Arquivos consumidores

A: `src/server/usage.rs`, novo `src/server/usage/workflow.rs`, `src/report.rs` (preferir módulo novo `src/report/workflows.rs`), `tests/usage_report_test.rs`; documentação curta do schema e da seção workflows.

B: novo `src/server/usage/lifecycle.rs`, `src/server/handler.rs`, `src/server/executions.rs`, `src/server/executions/history.rs`, `src/server/executions/wait.rs`, `src/server/agents.rs`, `src/server/agents/herdr.rs`, `src/report.rs`/`workflows.rs`, `src/report/selection.rs` e testes correspondentes. Não é necessário alterar o runtime ou o adapter Herdr para A; ampliação da semântica Herdr fica condicionada a uma fonte tipada explícita.
