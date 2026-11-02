<div align="center">
  <img src=".github/assets/fairy.png" alt="Winx Logo" width="150" />

  # 🪄 Winx
  ### *Your environment. Any agent.*

  **Execução persistente • Contexto eficiente • Operações de arquivo seguras • Controle de sessão e recuperação**

  <p align="center">
    <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/linguagem-Rust_2021-orange?style=flat&logo=rust" alt="Rust 2021" /></a>
    <a href="https://modelcontextprotocol.io/"><img src="https://img.shields.io/badge/MCP-2026--07--28-purple?style=flat" alt="Especificação MCP" /></a>
    <a href="docs/streamable-http.pt.md"><img src="https://img.shields.io/badge/transporte-Streamable_HTTP-2563eb?style=flat" alt="Streamable HTTP" /></a>
    <a href="SECURITY.md"><img src="https://img.shields.io/badge/auth-multi--principal-7c3aed?style=flat" alt="Autenticação multi-principal" /></a>
    <a href="#escolha-seu-transporte"><img src="https://img.shields.io/badge/transporte-stdio-2f855a?style=flat" alt="Transporte stdio" /></a>
    <a href="LICENSE"><img src="https://img.shields.io/badge/licen%C3%A7a-MIT-blue?style=flat" alt="Licença MIT" /></a>
  </p>

  <p align="center">
    <em>Um runtime MCP que conecta agentes de IA remotos aos ambientes de desenvolvimento que eles não alcançam sozinhos.</em>
  </p>

  <p align="center">
    <a href="#mcp-remoto-em-60-segundos">⚡ <b>Início rápido</b></a> &nbsp;•&nbsp;
    <a href="#onde-a-winx-se-encaixa">🧭 <b>Onde se encaixa</b></a> &nbsp;•&nbsp;
    <a href="#o-que-a-winx-oferece-a-agentes-remotos">✨ <b>Recursos</b></a> &nbsp;•&nbsp;
    <a href="docs/streamable-http.pt.md">🌐 <b>Guia HTTP</b></a> &nbsp;•&nbsp;
    <a href="SECURITY.md">🛡️ <b>Segurança</b></a>
  </p>

  <p align="center">
    <a href="README.md">English</a> • <b>Português</b> • <a href="README.zh.md">中文</a>
  </p>
</div>

> [!NOTE]
> Esta página traduz a visão geral da Winx. A **referência técnica canônica está em inglês**, no
> [README.md](README.md) e em [docs/](docs/); em caso de divergência, vale a versão em inglês.

A Winx é um **runtime MCP que conecta agentes de IA remotos** (ChatGPT e outros clientes MCP) a ambientes de
desenvolvimento locais ou remotos que eles não acessam diretamente. Ela oferece execução persistente, leituras e saídas
de comando econômicas em contexto, operações de arquivo protegidas e sessões que sobrevivem a quedas de conexão. O
caminho principal de implantação é um endpoint **Streamable HTTP** endurecido; o stdio continua totalmente suportado para
clientes MCP locais.

**Para quem é.** A Winx é para o agente que *não* está na sua máquina. Se você já trabalha com Claude Code ou Codex no
seu terminal, continue usando-os diretamente: a Winx não os substitui. Ela permite que um agente remoto trabalhe nesse
mesmo ambiente, coordene os agentes que rodam ali e use ferramentas locais opcionais, como descrito em
[onde a Winx se encaixa](#onde-a-winx-se-encaixa).

No Linux, no macOS e no Windows nativo, a Winx separa o adaptador MCP dos processos que controlam cada PTY: o `winxd`
gerencia o plano de controle e um `winx-guardian` por sessão lógica mantém o shell vivo entre desconexões HTTP,
reinícios do cliente e atualizações do adaptador. A Winx começou como um port em Rust do
[WCGW](https://github.com/rusiaaman/wcgw), mas não é um wrapper de Python: o `cd` persiste, `Ctrl+C` interrompe o
processo real, TUIs interativas funcionam e saídas grandes são renderizadas e orçadas em tokens antes de chegar ao modelo.

> [!IMPORTANT]
> **Streamable HTTP é o principal método de implantação.** A Winx o vincula ao loopback por padrão, exige um bearer
> token forte, suporta principals autenticados independentes e mantém por padrão uma sessão durável por
> principal/workspace.

## MCP remoto em 60 segundos

```bash
cargo install winx-code-agent

mkdir -p ~/.config
install -m 600 /dev/null ~/.config/winx-http-token
openssl rand -hex 32 > ~/.config/winx-http-token

winx-code-agent serve --http \
  --bind 127.0.0.1:8000 \
  --token-file ~/.config/winx-http-token
```

Conecte um cliente MCP a:

```text
http://127.0.0.1:8000/mcp
Authorization: Bearer <conteúdo de ~/.config/winx-http-token>
```

Clientes em nuvem precisam de um endpoint HTTPS acessível. Mantenha o Winx em loopback e use um túnel MCP privado, VPN ou proxy reverso HTTPS autenticado à frente. Para produtos OpenAI, o [Secure MCP Tunnel](https://developers.openai.com/api/docs/guides/secure-mcp-tunnels) mantém o Winx privado enquanto expõe um endpoint hospedado pela OpenAI.

**A seguir:** [guia completo de implantação Streamable HTTP](docs/streamable-http.pt.md) · [modelo de segurança](SECURITY.md) · [configuração local via stdio](README.md#install)

## Onde a Winx se encaixa

A Winx se integra às ferramentas que você já usa em vez de competir com elas.

| Ferramenta | Relação com a Winx |
|------------|--------------------|
| **Claude Code, Codex e outros agentes de terminal** | Agentes locais com ferramentas próprias. Use-os diretamente no seu terminal. A Winx expõe o ambiente a agentes que não rodam ali e pode servir um mesmo ambiente a vários clientes MCP. |
| **Herdr** | Gerenciador de workspaces de terminal para agentes de código. O backend Herdr opcional da ferramenta `Agent` fala com a API de socket do Herdr, para que um agente remoto liste, leia, aguarde e envie prompts aos agentes do seu workspace. Configurado pelo operador. |
| **[Eulix](https://github.com/Nurysso/eulix)** | Motor local de inteligência de código. Backend opcional para `CodeMap` `callers`, `callees` e `impact` quando o operador define `WINX_EULIX_PARSER`. |
| **[WCGW](https://github.com/rusiaaman/wcgw)** | Origem da Winx: um port em Rust do WCGW. Seus modos e sua sintaxe de intervalos de linhas continuam. |

## O que a Winx oferece a agentes remotos

### Execução persistente

- **Sessões duráveis:** o HTTP é stateless para o cliente, mas os PTYs vivem em guardians por sessão e podem ser
  retomados com o mesmo `thread_id`.
- **Semântica de terminal para agentes:** comandos em primeiro e segundo plano, polling de status, entrada interativa,
  snapshots estáveis de TUI, detecção de turnos, códigos de saída reais e saída limitada.
- **Histórico de execuções e fluxos de agentes:** histórico durável e isolado, páginas de saída que não consomem,
  explicação de estado, wait-any e prompts nativos com recibo; o backend Herdr opcional alcança alvos configurados ou
  descobre os agentes que rodam no workspace da sessão. Veja o [guia de fluxos](docs/agent-workflows.md).

### Contexto eficiente

- **Catálogos na medida:** os perfis `full`, `coding`, `read-only` e `terminal`, ou uma allowlist exata por principal,
  reduzem o payload de descoberta e recusam chamadas sem autoridade equivalente.
- **Leituras e saídas limitadas:** intervalos de linhas e leitura de definições por `caminho#símbolo`, saída de terminal
  renderizada e compactada sem perdas, e orçamentos por chamada com continuações exatas. Para clientes ChatGPT e Codex,
  um comando concluído cuja saída cita poucos `caminho:linha` (resultados de busca, erros de compilação, tracebacks) já
  traz esses trechos com revisões prontas para edição, economizando uma ida e volta.
- **Ferramentas de repositório, não só um shell:** um protocolo único de edição, planejamento multi-arquivo, undo,
  leituras orçadas em tokens, entrada de imagens, handoff de contexto e navegação por símbolos com tree-sitter.

### Operações de arquivo seguras

- **Uma ferramenta de mutação:** `EditFiles` cria, edita, agrupa, aplica patches, verifica e desfaz por um único motor
  tipado de planejamento e commit, preservando evidência de leitura, caminho canônico, escritas atômicas, recibos e
  recuperação.
- **Protegido por padrão:** redação de segredos em toda saída de ferramenta, modos `architect` (somente leitura) e
  `code_writer` (allowlist), política de caminhos em toda ferramenta de arquivo e sandbox Landlock opcional no Linux.

### Controle de sessão e recuperação

- **Isolamento por identidade:** um token por principal; IDs de thread e de MCP Tasks são isolados internamente e
  traduzidos antes da resposta sair do servidor. A afinidade por workspace absorve thread IDs instáveis do modelo.
- **Rede com fail-closed:** loopback por padrão, tokens de no mínimo 32 bytes, arquivo de token `chmod 600`, validação
  de Host contra DNS rebinding, limites de corpo, tempo e concorrência, autenticação antes de qualquer capacidade
  compartilhada e limitação de falhas por IP.
- **Recuperável por design:** todo resultado traz um `status` autoritativo e, quando a Winx consegue inferir com
  segurança, um `nextAction` concreto; sessões persistidas são recuperadas após reinícios do daemon ou do adaptador, e
  um shell que para de responder é reiniciado no mesmo diretório e modo.

## Escolha seu transporte

| Transporte | Recomendado para | Endpoint / Inicialização | Autenticação | Modelo de Sessão |
| :--- | :--- | :--- | :--- | :--- |
| **Streamable HTTP** | ChatGPT, agentes hospedados, automação remota, múltiplos clientes MCP | `https://host/mcp` via túnel/proxy, com Winx em `127.0.0.1:8000` | Bearer token forte; TOML multi-principal opcional | Requisições stateless mapeadas para sessões duráveis por principal/workspace por padrão |
| **stdio** | Claude Code, Codex CLI, Cursor, VS Code, clientes desktop e IDEs locais | O cliente inicia o executável `winx-code-agent` | Limite de processo local | Um cliente local, usando o mesmo runtime com daemon durável no Unix |

## Arquitetura remota

```text
Cliente MCP Remoto
       │  HTTPS + bearer token
       ▼
Secure MCP Tunnel / VPN / proxy reverso HTTPS autenticado
       │  loopback HTTP
       ▼
127.0.0.1:8000/mcp
       │
       ├─ Validação de Host / corpo / timeout
       ├─ Rate limit por IP + limite global de concorrência
       ├─ Autenticação de principal
       └─ Isolamento de thread_id e MCP Tasks
              │
              ▼
        WinxService compartilhado
              │
              ├─ coerência do workspace + evidência de arquivos
              │       └─ motor unificado de mutações ── filesystem do workspace
              │
              └─ runtime de shell
                      └─ winxd
                          └─ winx-guardian por sessão ── PTY real / shell / TUI
```

## Ferramentas MCP

| Ferramenta | O que faz |
|------------|-----------|
| `Initialize` | Inicia o workspace, escolhe o modo e devolve o par inseparável `thread_id`/`workspace_root`. |
| `BashCommand` | Executa comandos, acompanha os longos, envia teclas e controla TUIs. |
| `ReadFiles` | Lê arquivos com números de linha, intervalos (`arquivo.rs:10-40`) ou definições (`arquivo.rs#simbolo`). |
| `EditFiles` | Cria, altera ou desfaz um ou vários arquivos com `line_patch`, `search_replace`, `replace` ou `undo`. |
| `CodeMap` | Navegação por sintaxe (`outline`, `references`) e, com Eulix, `callers`, `callees` e `impact`. |
| `Execution` | Observa o histórico de comandos aceitos, com cursores e wait-any. |
| `Agent` | Observa ou envia prompts a execuções nativas ou a agentes do Herdr. |
| `ContextSave` | Salva a descrição da tarefa e os arquivos relevantes para retomar em outra sessão. |
| `ReadImage` | Devolve imagens validadas como conteúdo MCP nativo. |

Os perfis `terminal` (4 ferramentas), `read-only` (5), `coding` (7) e `full` (9, padrão) limitam o catálogo. Detalhes,
schemas e regras de cada ferramenta estão em [MCP Tools](README.md#mcp-tools).

## Referência técnica (em inglês)

- [Instalação e configuração de clientes stdio](README.md#install)
- [Edição com SEARCH/REPLACE](README.md#searchreplace-editing)
- [Ciclo de vida de sessões duráveis](README.md#durable-session-lifecycle)
- [Implantação Streamable HTTP](README.md#streamable-http-deployment) e o [guia completo](docs/streamable-http.md)
  ([tradução](docs/streamable-http.pt.md), possivelmente defasada)
- [Variáveis de ambiente](README.md#environment-variables)
- [Verificação de releases](README.md#verifying-releases)
- [Desenvolvimento e testes](README.md#hacking-on-it)
- [Arquitetura e invariantes](docs/architecture.md) e [fluxos de agentes](docs/agent-workflows.md)

## Escopo e princípios

A Winx é um projeto de entusiasta construído em torno de problemas observados no uso real, não de uma lista de recursos.

- **Integrar, não substituir.** A Winx complementa agentes de terminal, Herdr e Eulix em vez de reimplementá-los. As
  integrações são opcionais e configuradas pelo operador.
- **Evidência antes de recursos.** As mudanças vêm de atritos medidos em sessões reais (telemetria de uso sem dados
  sensíveis e `winx-code-agent report`) e entram com testes de regressão.
- **Uma superfície pequena e estável.** Nove ferramentas, perfis para reduzi-las e aliases de compatibilidade ocultos
  em vez de renomeações que quebram clientes.
- **Seguro por padrão.** Loopback, tokens fortes, redação e edições protegidas são o padrão; ampliar o acesso é uma
  escolha explícita do operador.

A Winx não é um agente, uma IDE, um multiplexador de terminal nem um indexador de código.

## Uma nota sobre segurança

A Winx suporta stdio local e Streamable HTTP remoto. Qualquer cliente conectado pode ler e editar arquivos e executar
comandos com a capacidade concedida pelo modo selecionado: o mesmo alcance de dar um terminal ao cliente. O HTTP leva
esse alcance além do limite do processo local; leia o [guia de implantação](docs/streamable-http.md) e o
[SECURITY.md](SECURITY.md) antes de expô-lo. Para restringir: modo `architect` (sem escritas), `code_writer`
(allowlist de comandos e globs de escrita) e `WINX_SANDBOX=1` (sandbox Landlock no Linux).

## Licença

MIT – Gabriel Maia ([@gabrielmaialva33](https://github.com/gabrielmaialva33))
