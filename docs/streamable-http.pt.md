# Implantação de Streamable HTTP

<p align="right">
  <a href="streamable-http.md">English</a> • <b>Português</b> • <a href="streamable-http.zh.md">中文</a>
</p>

O Winx expõe todo o seu conjunto de ferramentas MCP através de um endpoint autenticado **Streamable HTTP** para ChatGPT, agentes hospedados na nuvem, automação remota e clientes que não podem iniciar um processo stdio local. O endpoint é `/mcp`; o listener padrão é `127.0.0.1:8000`.

Como esse endpoint concede recursos reais de shell e acesso ao sistema de arquivos, o Winx adota a postura de segurança **fail-closed**: exige credenciais fortes, recusa conexões fora do loopback a menos que explicitamente autorizado, impõe limites de custo de requisição e isola rigorosamente cada principal autenticado.

## Visão Geral

| Propriedade | Padrão |
| :--- | :--- |
| Endpoint MCP | `/mcp` |
| Protocolo MCP | `2026-07-28` |
| Listener | `127.0.0.1:8000` |
| Autenticação | `Authorization: Bearer <token>` |
| Comprimento mínimo do token | 32 bytes |
| Afinidade de sessão remota | `workspace` |
| Limite de corpo da requisição | 64 MiB |
| Timeout de requisição | 120 segundos |
| Concorrência global | 32 requisições simultâneas |
| Rate limit por IP de origem | 120 requisições por minuto |
| Atraso em falha de autenticação | 100 ms |
| TTL de guardian nunca utilizado | 1.800 segundos (30 minutos) |
| TTL de guardian utilizado | 86.400 segundos (24 horas) |
| Quota de guardians ativos | 32 |

O Winx suporta chamadas MCP stateless modernas e o fluxo legado de inicialização/sessão HTTP. As mesmas ferramentas, prompts, recursos, conteúdo estruturado e MCP Tasks estão disponíveis via HTTP e stdio.

## Arquitetura

```text
Cliente MCP Remoto
       │ HTTPS + bearer token
       ▼
Túnel privado / VPN / Proxy reverso HTTPS autenticado
       │ loopback HTTP
       ▼
127.0.0.1:8000/mcp
       │
       ├─ Validação de Host e corpo da requisição
       ├─ Limites de timeout, taxa e concorrência
       ├─ Autenticação de principal
       ├─ Resolução de afinidade de sessão
       └─ Isolamento de thread_id / MCP Task
              │
              ▼
        WinxService compartilhado
              │
              ▼
            winxd (daemon de controle)
              │
              └─ winx-guardian por sessão lógica
                         │
                         └─ PTY real / bash ou zsh / tarefas em primeiro e segundo plano
```

No Linux, macOS e WSL2, o `winx-code-agent` atua apenas como o adaptador MCP. O `winxd` gerencia o plano de controle, enquanto cada `winx-guardian` controla um PTY independente. Encerrar a conexão HTTP ou reiniciar o adaptador não encerra o PTY.

No Windows nativo utiliza-se o runtime embutido (embedded), logo as sessões duram apenas enquanto o processo do servidor estiver ativo. Recomenda-se o WSL2 quando forem necessárias sessões remotas duráveis.

## Início Rápido

Instale os três binários para Unix:

```bash
cargo install winx-code-agent
```

Gere um token forte em um arquivo protegido (apenas leitura do usuário):

```bash
mkdir -p ~/.config
install -m 600 /dev/null ~/.config/winx-http-token
openssl rand -hex 32 > ~/.config/winx-http-token
```

Inicie o Winx no loopback:

```bash
winx-code-agent serve --http \
  --bind 127.0.0.1:8000 \
  --token-file ~/.config/winx-http-token
```

Configure o cliente com:

```text
URL: http://127.0.0.1:8000/mcp
Authorization: Bearer <conteúdo de ~/.config/winx-http-token>
```

Clientes na nuvem necessitam de uma URL HTTPS pública. Mantenha o Winx em loopback e coloque um túnel privado, VPN ou proxy reverso HTTPS autenticado à frente. O Winx não realiza terminação TLS diretamente.

O uso de `--token-file` é preferível a `--token`, pois segredos passados por linha de comando podem ser expostos na lista de processos do sistema (`ps`), histórico do shell e logs de automação. A variável de ambiente `WINX_HTTP_TOKEN` permanece disponível como fallback para modo single-principal.

## Afinidade de Sessão

### Afinidade por Workspace (Padrão)

A opção padrão é:

```bash
--session-affinity workspace
```

Para cada chamada remota `Initialize(first_call)`, o Winx deriva a sessão lógica a partir de:

```text
(principal autenticado, workspace canônico)
```

O `thread_id` enviado pelo cliente na primeira chamada não é considerado chave única durável. Variações estéticas geradas por modelos como:

```text
release_02333
release_0_2_333
```

são mapeadas para o mesmo guardian interno quando pertencem ao mesmo principal e workspace. O Winx retorna um identificador externo estável (ex: `ws_project_<hash>`) e espera que as chamadas subsequentes utilizem esse ID retornado.

Consequências:
- Reconexões stateless reanexam à sessão existente em vez de criar um novo guardian;
- Primeiras chamadas repetidas preservam o PTY, cwd, journal de saída e comandos em execução;
- Principals diferentes mantêm namespaces totalmente isolados;
- Conversas paralelas do **mesmo principal no mesmo workspace compartilham o mesmo shell** e seu lock de comandos em foreground;
- Chamadas sem workspace compartilham uma sessão scratch por principal;
- Retomadas de tarefas são indexadas pelo ID de tarefa salvo.

### Afinidade por Conversa (`conversation`)

Utilize:

```bash
--session-affinity conversation
```

quando conversas paralelas de um mesmo principal precisarem operar no mesmo repositório sem compartilhar o mesmo shell. O Winx deriva a chave a partir de:

```text
(principal autenticado, identidade da conversa, workspace canônico)
```

A ordem de preferência de identidade é:
1. `Mcp-Session-Id`, quando o transporte mantém uma sessão MCP estável;
2. `X-Winx-Conversation-Id`, quando um gateway autenticado injeta um valor estável;
3. O `thread_id` fornecido na primeira chamada;
4. Afinidade por workspace caso nenhuma identidade de conversa seja encontrada.

### Afinidade por Thread (`thread`)

Utilize:

```bash
--session-affinity thread
```

quando o cliente for responsável por gerenciar e manter identificadores de thread estáveis e únicos, controlando a criação e o encerramento explícito de cada sessão.

## Conexão ou Criação (Attach-or-create)

Os guardians com protocolo `1.3+` implementam attach-or-create para `FirstCall`:
1. Se a sessão lógica não existir, cria-se um novo PTY;
2. Se a sessão lógica já existir, retorna-se o snapshot autoritativo atual;
3. O adaptador sincroniza seu estado local a partir do snapshot;
4. O guardian preserva o processo PTY, cwd, modo de segurança, histórico de saída e comandos em execução.

## Múltiplos Principals Autenticados

Gere credenciais independentes para cada cliente ou automação:

```bash
mkdir -p ~/.config
install -m 600 /dev/null ~/.config/winx-chatgpt-token
install -m 600 /dev/null ~/.config/winx-automation-token
openssl rand -hex 32 > ~/.config/winx-chatgpt-token
openssl rand -hex 32 > ~/.config/winx-automation-token
```

Crie o arquivo TOML de configuração:

```toml
# ~/.config/winx-principals.toml
[[principals]]
name = "chatgpt"
token_file = "/home/alice/.config/winx-chatgpt-token"

[[principals]]
name = "automacao"
token_file = "/home/alice/.config/winx-automation-token"

[[principals]]
name = "ci"
token_env = "WINX_CI_MCP_TOKEN"
```

Inicie o servidor:

```bash
chmod 600 ~/.config/winx-principals.toml
winx-code-agent serve --http \
  --principal-config ~/.config/winx-principals.toml
```

Regras dos principals:
- Nomes podem conter letras ASCII, dígitos, `_` e `-`;
- Cada entrada deve definir exatamente `token_file` ou `token_env`;
- Nomes, IDs derivados e tokens devem ser exclusivos;
- Arquivos de token devem ser arquivos regulares (sem links simbólicos) com permissão restrita (`0600`);
- Tokens devem ter no mínimo 32 bytes de comprimento.

## Contrato de Orquestração com LLM

O handshake inicial do MCP estabelece um contrato de orquestração sequencial determinístico: inicializar uma vez, manter o `thread_id` retornado, utilizar `CodeMap` antes de leituras extensas, agrupar leituras com `ReadFiles`, ler arquivos antes de editá-los e nunca repetir chamadas rejeitadas sem alterações.

Cada ferramenta define um `outputSchema` e retorna um envelope `structuredContent`:

```json
{
  "status": "needs_read",
  "tool": "FileWriteOrEdit",
  "message": "FileWriteOrEdit failed: ...",
  "errorCode": "read_required",
  "retryable": true,
  "retrySameCall": false,
  "nextAction": {
    "tool": "ReadFiles",
    "instruction": "Perform every required read before retrying the edit.",
    "arguments": {
      "file_paths": ["/workspace/README.md:231-301"],
      "thread_id": "ws_project_hash"
    }
  },
  "requiredReads": [
    { "path": "/workspace/README.md", "ranges": ["231-301"] }
  ]
}
```

Falhas recuperáveis retornam resposta com sucesso HTTP/JSON-RPC e `isError: true` no protocolo MCP, incluindo a próxima ação corretiva (`nextAction`).

## Ciclo de Vida do Guardian

O `winxd` gerencia o ciclo de vida de todos os guardians conectados ao mesmo socket de controle:

- **Relógio de Atividade Autoritativo:** Os guardians com protocolo 1.3+ registram a hora de criação, a última atividade real do terminal, o timestamp do último comando e se algum comando já foi executado.
- **TTL em Camadas:**
  - `WINX_UNUSED_SESSION_IDLE_TTL_SECS=1800` (30 minutos para sessões que nunca executaram comandos);
  - `WINX_SESSION_IDLE_TTL_SECS=86400` (24 horas para sessões utilizadas).
  - Comandos ativos em primeiro ou segundo plano nunca são finalizados por expiração de tempo.
- **Pressão de Quota:** Sob limite de capacidade, o `winxd` limpa sockets órfãos e descarta o guardian inativo mais antigo que nunca executou comandos antes de recusar uma nova sessão.

## Comandos Operacionais

```bash
# Inspecionar sessões ativas
winx-code-agent list

# Acompanhar a saída de uma sessão em tempo real
winx-code-agent attach <thread_id> --follow

# Aplicar a limpeza com as regras de TTL padrão
winx-code-agent prune

# Limpar todas as sessões inativas preservando processos ativos
winx-code-agent prune --idle-seconds 0

# Encerrar uma sessão específica ou todas
winx-code-agent kill <thread_id>
winx-code-agent kill --all

# Reiniciar o daemon de controle mantendo os guardians e PTYs vivos
winx-code-agent restart-daemon

# Gerar relatório de diagnóstico sanitizado
winx-code-agent doctor
```

## Telemetria de Uso Persistente

Para registrar eventos estruturados `winx::usage` em formato JSONL de maneira não-bloqueante:

```bash
WINX_USAGE_LOG="$HOME/.local/state/winx/usage.jsonl" \
WINX_USAGE_LOG_ROTATION=daily \
WINX_USAGE_LOG_KEEP_DAYS=7 \
winx-code-agent serve --http --token-file ~/.config/winx-http-token
```

Nenhum comando, conteúdo de arquivo, saída de ferramenta ou credencial é gravado nesse log de telemetria; gravam-se apenas durações, status de resultado, tamanhos de resposta e metadados de protocolo.

## Exposição de Rede

### Recomendado
Mantenha o listener em loopback (`127.0.0.1:8000`) e utilize:
- VPN privada (WireGuard, Tailscale);
- Túnel MCP outbound-only (como o Secure MCP Tunnel da OpenAI);
- Proxy reverso HTTPS autenticado.

Se o proxy encaminhar um cabeçalho `Host` específico, informe-o via `--allowed-host`:

```bash
winx-code-agent serve --http \
  --token-file ~/.config/winx-http-token \
  --allowed-host mcp.exemplo.com
```

### Ligação Direta Externa
Caso deseje expor o socket de rede diretamente (não recomendado para produção sem proxy/TLS):

```bash
winx-code-agent serve --http \
  --bind 192.168.1.20:8000 \
  --allow-non-loopback \
  --token-file ~/.config/winx-http-token
```

## Limites de Recursos e Respostas

| Condição | Resposta | Detalhes |
| :--- | :--- | :--- |
| Token ausente ou inválido | `401 Unauthorized` | Resposta atrasada em 100 ms |
| Limite de requisições por IP excedido | `429 Too Many Requests` | Inclui cabeçalho `Retry-After: 1` |
| Concorrência global esgotada | `503 Service Unavailable` | Inclui cabeçalho `Retry-After: 1` |
| Requisição exceder 120 segundos | `408 Request Timeout` | A requisição é encerrada |
| Corpo maior que 64 MiB | `413 Payload Too Large` | Rejeitado antes do dispatch |

## Referência da CLI

| Opção | Finalidade |
| :--- | :--- |
| `serve --http` | Inicia o transporte Streamable HTTP |
| `--bind <IP:PORT>` | Endereço de escuta (padrão: `127.0.0.1:8000`) |
| `--token-file <PATH>` | Caminho para o arquivo contendo o Bearer Token |
| `--principal-config <PATH>` | Caminho para o arquivo TOML com múltiplos principals |
| `--token <VAL>` | Token direto via CLI (visível em `ps`) |
| `--session-affinity <MODE>` | Modo de afinidade: `workspace`, `conversation` ou `thread` |
| `--allow-weak-token` | Permite tokens com menos de 32 bytes (apenas testes) |
| `--allow-non-loopback` | Permite bind em interfaces não-loopback |
| `--allowed-host <HOST>` | Autoriza cabeçalhos `Host` adicionais |
| `--allow-query-token` | Permite passar o token via `?token=...` na URL |
| `--max-concurrency <N>` | Limite global de requisições concorrentes (padrão: 32) |
| `--requests-per-minute <N>` | Limite de requisições por IP (padrão: 120) |

## Limite de Segurança

Um principal autenticado executa comandos e acessa arquivos com os privilégios do usuário do sistema operacional que iniciou o servidor.

- O modo `wcgw` concede acesso total ao shell e sistema de arquivos;
- O modo `architect` limita a sessão a operações de leitura;
- O modo `code_writer` restringe a comandos e caminhos autorizados;
- A redação de segredos está sempre ativa por padrão;
- O sandbox Landlock (`WINX_SANDBOX=1`) adiciona proteção de kernel no Linux.

Consulte [SECURITY.md](../SECURITY.md) para detalhes adicionais de segurança.
