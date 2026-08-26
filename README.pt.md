<div align="center">
  <img src=".github/assets/fairy.png" alt="Winx Logo" width="150" />

  # 🪄 Winx
  ### *Runtime MCP remoto de alta performance para agentes autônomos de código*

  **Sessões PTY Duráveis • Streamable HTTP • Operações de Arquivo Protegidas • Feito em Rust Nativo 🦀**

  <p align="center">
    <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/linguagem-Rust_2021-orange?style=flat&logo=rust" alt="Rust 2021" /></a>
    <a href="https://modelcontextprotocol.io/"><img src="https://img.shields.io/badge/MCP-2026--07--28-purple?style=flat" alt="Especificação MCP" /></a>
    <a href="docs/streamable-http.pt.md"><img src="https://img.shields.io/badge/transporte-Streamable_HTTP-2563eb?style=flat" alt="Streamable HTTP" /></a>
    <a href="SECURITY.md"><img src="https://img.shields.io/badge/auth-multi--principal-7c3aed?style=flat" alt="Autenticação multi-principal" /></a>
    <a href="#escolha-seu-transporte"><img src="https://img.shields.io/badge/transporte-stdio-2f855a?style=flat" alt="Transporte stdio" /></a>
    <a href="LICENSE"><img src="https://img.shields.io/badge/licen%C3%A7a-MIT-blue?style=flat" alt="Licença MIT" /></a>
  </p>

  <p align="center">
    <em>"Dê aos LLMs remotos e locais mãos duráveis e autenticadas no seu ambiente de desenvolvimento."</em>
  </p>

  <p align="center">
    <a href="#mcp-remoto-em-60-segundos">⚡ <b>Início Rápido</b></a> &nbsp;•&nbsp;
    <a href="#o-que-você-obtém">✨ <b>Recursos</b></a> &nbsp;•&nbsp;
    <a href="#arquitetura-remota">🏗️ <b>Arquitetura</b></a> &nbsp;•&nbsp;
    <a href="docs/streamable-http.pt.md">🌐 <b>Guia HTTP</b></a> &nbsp;•&nbsp;
    <a href="SECURITY.md">🛡️ <b>Segurança</b></a>
  </p>

  <p align="center">
    <a href="README.md">English</a> • <b>Português</b> • <a href="README.zh.md">中文</a>
  </p>
</div>

O Winx é um **runtime MCP focado em execução remota** para agentes que precisam de um shell real, primitivas protegidas de edição de arquivos, navegação de código consciente do repositório e sessões que sobrevivem a quedas de conexão. Seu caminho primário de implantação é um endpoint **Streamable HTTP** blindado para ChatGPT e outros clientes MCP na nuvem ou em rede; o transporte **stdio** permanece totalmente suportado para Claude Code, Codex CLI, Cursor, VS Code e outros clientes locais.

No Unix, o Winx separa o adaptador MCP dos processos que controlam cada PTY. O `winxd` gerencia o plano de controle e um processo `winx-guardian` dedicado por sessão lógica mantém o shell vivo durante desconexões HTTP, reinicializações de clientes e atualizações do adaptador. Começou como uma reescrita em Rust do [WCGW](https://github.com/rusiaaman/wcgw), mas não é um wrapper Python: `cd` persiste, `Ctrl+C` interrompe o processo real, TUIs interativas funcionam e saídas volumosas de terminal são renderizadas e orçadas em tokens antes de chegarem ao modelo.

> [!IMPORTANT]
> **Streamable HTTP é o principal método de implantação.** O Winx o vincula ao loopback por padrão, exige um bearer token forte, suporta múltiplos principals autenticados independentes e mantém por padrão uma sessão durável por principal/workspace. Chamadas repetidas e stateless de `Initialize` reconectam à sessão existente em vez de instanciar um novo guardian.

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

**A seguir:** [guia completo de implantação Streamable HTTP](docs/streamable-http.pt.md) · [modelo de segurança](SECURITY.md) · [configuração local via stdio](#instalação)

## Por que o Winx para agentes remotos

- **Sessões duráveis:** O HTTP é stateless do ponto de vista do cliente, mas os PTYs no Unix residem em guardians por sessão e podem ser retomados com o mesmo `thread_id`.
- **Isolamento consciente de identidade:** Um token por principal; IDs de thread e tarefas MCP (Tasks) são isolados internamente e traduzidos antes que a resposta saia do servidor. A afinidade de workspace absorve IDs de thread instáveis gerados por modelos.
- **Catálogos na medida certa:** Os perfis `full`, `coding`, `read-only` e `terminal` — ou uma allowlist exata por principal — reduzem o payload de descoberta/schema e bloqueiam chamadas fora da política.
- **Um protocolo de mutação:** As ferramentas públicas de edição são fachadas compatíveis sobre um único motor tipado de planejamento/commit; evidência de leitura, identidade canônica do caminho, escrita atômica, undo, verificação e recuperação seguem as mesmas regras em todos os formatos.
- **Padrões de rede com fail-closed:** Vinculação estrita a loopback por padrão, tokens de no mínimo 32 bytes, arquivos de token com permissão `chmod 600`, validação de Host contra DNS rebinding, limites de corpo/tempo/concorrência, rate limiting por IP e atraso deliberado em respostas de autenticação inválida.
- **Semântica de terminal nativa para agentes:** Comandos em primeiro e segundo plano, polling de status, entrada interativa, snapshots estáveis de TUI, detecção de turnos, códigos de saída reais e saída limitada.
- **Ferramentas de repositório completas:** Edições seguras via SEARCH/REPLACE, planejamento multi-arquivos atômico, desfazer (undo), leituras orçadas em tokens, entrada de imagens, handoff de contexto e navegação de símbolos via Tree-sitter.

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

## O que você obtém

- Sessão bash persistente com estado por thread com semântica de PTY real: foreground, background, checagem de status, envio de texto, Enter/Ctrl-C/Ctrl-D e caracteres ASCII puros. Scripts multilinha e atalhos de comando funcionam; bytes NUL são rejeitados antes de alcançar o shell.
- Workspaces com três modos de segurança: `wcgw` (acesso total), `architect` (somente leitura), `code_writer` (allowlist de comandos e globs de escrita). A allowlist de comandos é analisada via Tree-sitter, verificando **cada** comando na linha (pipelines, `&&`/`||`/`;`, substituição de comandos `$(...)`, subshells) e não apenas a primeira palavra, impossibilitando bypasses como `ls && curl … | sh` ou `ls $(rm …)`.
- PTY resiliente: um shell que não retorna ao prompt (mesmo após Ctrl-C) é automaticamente reiniciado no mesmo cwd/modo, processos filhos são eliminados ao encerrar e a detecção de prompt é robusta a `PS1` customizados. Suporte opcional ao `zsh` com `WINX_SHELL=zsh`.
- Leitura de arquivos com intervalos de linhas estilo WCGW (`file.rs:10-40`, `file.rs:10-`, `file.rs:-40`). Arquivos ativos são rastreados e priorizados no contexto do repositório.
- Um único motor tipado por trás de `FileWriteOrEdit`, `ApplyPatch`, `MultiFileEdit` e `UndoEdit`. Substituição integral, SEARCH/REPLACE, patch de linhas vinculado à revisão, lote e undo compartilham preflight canônico, evidência de leitura/frescor, planejamento limitado, substituição atômica por arquivo, diffs compactos, recuperação tipada e verificação vinculada a recibos. Falha de planejamento não grava nada; uma rara falha durante o commit relata exatamente o prefixo já persistido.
- Navegação de código via `CodeMap` com Tree-sitter: mapa de símbolos orçado em tokens de um arquivo ou do repositório inteiro, ou busca de definições/referências para um símbolo em 13 linguagens.
- `ContextSave` para exportar um resumo da tarefa e seus arquivos para a próxima sessão, incluindo contexto do workspace, arquivos ativos, git status/diff e estado do terminal para retomada limpa.
- `ReadImage` para que modelos multimodais possam receber capturas de tela, mockups e imagens de erro em blocos de imagem nativos do MCP.
- Saída de shell limpa e orçada em tokens: ruídos de cursor/ANSI de programas interativos (REPLs, barras de progresso) são renderizados através de um emulador de terminal, e repetições mecânicas são compactadas sem perdas (`linha  [winx: ×N]`) para economizar contexto. Desative a compactação com `WINX_NO_COMPRESS=1`. Quando a saída excede o limite, o excesso inicial é gravado em um arquivo scratch em `.winx/scratch/` que o agente pode ler posteriormente.
- Redação automática de segredos ativa por padrão: chaves de API, JWTs, blocos de chaves privadas PEM e URLs no formato `user:pass@` são mascarados de **todas** as saídas de ferramentas e memória salva antes de chegarem ao modelo (desative com `WINX_NO_REDACT=1`). Um sandbox opcional com Landlock (`WINX_SANDBOX=1`, Linux) adiciona uma camada de proteção a nível de kernel que confina escritas ao workspace e oculta o diretório home.
- Endpoint **Streamable HTTP** blindado (`winx-code-agent serve --http`) para clientes remotos, mantendo stdio para ferramentas locais.

## Ferramentas MCP

O catálogo público permanece estável enquanto as implementações convergem. As quatro fachadas públicas de mutação usam
o mesmo motor tipado. O wire de migração não anunciado `EditFiles` é interno, não é API pública e não deve ser chamado
por clientes; use apenas os nomes retornados por `tools/list`.

Escolha o menor catálogo que cubra o cliente. A política vale tanto na descoberta quanto no dispatch, e o wire interno
fica limitado à autoridade de mutação equivalente já concedida. Para um agente de código comum, `coding` é o ponto de
partida recomendado; use `full` quando também precisar de imagens ou handoff de contexto.

| Perfil | Ferramentas | Capacidades |
| :--- | ---: | :--- |
| `terminal` | 2 | `Initialize` e `BashCommand` |
| `read-only` | 4 | Inicialização, leitura exata de arquivos/imagens e `CodeMap` |
| `coding` | 9 | Terminal, leituras, navegação e todas as fachadas públicas de edição/verificação/undo |
| `full` | 11 | Padrão retrocompatível; adiciona `ContextSave` e `ReadImage` ao fluxo de código |

```bash
winx-code-agent serve --http \
  --token-file ~/.config/winx-http-token \
  --tool-profile coding
```

Uma lista exata repetindo `--allow-tool NOME` substitui o perfil. O catálogo reduz a superfície MCP; ele não altera a
autoridade de shell/arquivos concedida pelo modo inicializado.

| Ferramenta | O que faz |
| :--- | :--- |
| `Initialize` | Inicializa o workspace, define o modo de operação e retorna um `thread_id`. Deve ser chamada primeiro, a menos que o cliente exponha MCP Roots. Sem caminho especificado, cria um ambiente scratch temporário; retomar uma tarefa (`task_id_to_resume`) reabre a raiz do projeto salva. |
| `BashCommand` | Executa comandos, monitora processos longos, envia Enter/Ctrl-C e opera TUIs. A política `wait_policy` suporta: `adaptive` (padrão, mantém chamadas curtas inline e promove comandos longos para Task se suportado); `until_complete` (inicia uma Task imediatamente); `return_early` (retorna imediatamente). Suporta `is_background`, `status_check`, ações de input, `screen` e `wait_for_turn`. |
| `ReadFiles` | Lê um ou múltiplos arquivos com numeração de linhas e devolve revisão opaca e intervalos realmente visíveis. Adicione `:10-40` ao caminho; truncamento nunca registra linhas não exibidas. |
| `FileWriteOrEdit` | Visão de arquivo único do motor compartilhado: substituição integral ou SEARCH/REPLACE com âncoras opcionais. Valida cobertura, frescor e identidade canônica do alvo antes de gravar; retorna tolerâncias, problemas de sintaxe e diff compacto. |
| `ApplyPatch` | Visão vinculada à revisão do mesmo motor. Aplica patches de linha ordenados e não sobrepostos sobre uma revisão exata de `ReadFiles`; só linhas visíveis podem mudar e revisões/replays obsoletos falham antes da escrita. |
| `MultiFileEdit` | Visão em lote do mesmo motor. Planeja todas as edições antes de gravar; falha de planejamento não altera arquivo algum. Uma rara falha no commit informa caminhos persistidos e não persistidos sem alegar rollback inexistente. |
| `VerifyEdit` | Repete a checagem exata de um recibo pós-edição sem executar novamente a edição já confirmada. |
| `UndoEdit` | Visão de undo do motor compartilhado. Reverte um arquivo ao checkpoint anterior da sessão (até 10 retidos), em ordem LIFO por arquivo; recusa alvo modificado externamente e não remove arquivo recém-criado. |
| `ContextSave` | Salva descrição da tarefa + arquivos em um único documento estruturado com contexto do workspace, arquivos ativos e git diff/status para transferência rápida de contexto entre sessões. |
| `ReadImage` | Retorna um bloco nativo de imagem MCP (não base64 como texto comum), permitindo que modelos multimodais processem a imagem visualmente. Confinado ao workspace. |
| `CodeMap` | Navegação de código com Tree-sitter em uma ferramenta com duas operações: `outline` (mapa de símbolos de classes, funções e tipos com orçamento de tokens) e `references` (busca semântica de definições e referências em 13 linguagens). |

O Winx anuncia a especificação MCP `2026-07-28`. Cada ferramenta publica um `outputSchema` e retorna um envelope estruturado `structuredContent`, mantendo compatibilidade de texto/imagem com clientes mais antigos.
Edições recebem recibos persistidos por 30 minutos: chamadas idênticas não escrevem nem verificam duas vezes enquanto os hashes finais coincidirem. Falhas de verificação retornam `completed_with_issues` e uma ação `VerifyEdit`; três conflitos SEARCH repetidos escalam para `recovery_exhausted`.

O cancelamento de MCP Tasks é vinculado à geração exata. Se o cancelamento vencer durante o intervalo entre reserva e
lançamento, o Winx aguarda a identidade exata da execução ou a prova de que nenhum processo iniciou. Um fallback limitado
encerra a sessão afetada antes de confirmar o cancelamento; um interrupt tardio nunca pode atingir o comando seguinte.

## Edição com Busca/Substituição (Search/Replace)

Sintaxe padrão de blocos:

```text
<<<<<<< SEARCH
conteúdo original
=======
novo conteúdo
>>>>>>> REPLACE
```

Tolerâncias automáticas aplicadas pelo buscador:

- **Atômico:** correspondências ausentes ou ambíguas cancelam a operação sem tocar no arquivo.
- Ajusta a indentação do conteúdo de substituição se o LLM errar os espaços iniciais.
- Remove numerações de linha vazadas do `ReadFiles` dentro do bloco SEARCH.
- Normaliza aspas curvas ("smart quotes"), travessões (em-dash) e reticências unicode.
- Usa blocos adjacentes para desambiguar trechos idênticos que aparecem mais de uma vez.
- Suporta edições de substrings dentro de uma única linha.
- Tenta desescapar aspas `\"` se o modelo incluir escapes em excesso.
- Permite ancorar o bloco a uma linha ou intervalo (`<<<<<<< SEARCH @42` ou `@42-50`).
- Em caso de sucesso, detalha as tolerâncias aplicadas; em caso de falha, exibe a correspondência mais próxima marcando com `~` as linhas divergentes.

## Instalação

O início rápido remoto está [no topo deste documento](#mcp-remoto-em-60-segundos). Esta seção detalha a instalação e configurações para clientes locais via stdio.

```bash
cargo install winx-code-agent
```

No Linux/macOS/WSL2, isso instala os três binários juntos em `~/.cargo/bin`: `winx-code-agent`, `winxd` e `winx-guardian`. Mantenha os três no mesmo diretório.

Requisitos: Rust 1.88+, bash e um terminal real. O runtime de daemon durável é suportado em Linux/macOS/WSL2. O Windows nativo utiliza o runtime embutido (embedded).

<details>
<summary><b>Claude Code (CLI)</b></summary>

Via linha de comando:

```bash
claude mcp add winx -- winx-code-agent
```

Ou adicione um `.mcp.json` na raiz do projeto:

```json
{
  "mcpServers": {
    "winx": {
      "command": "winx-code-agent",
      "env": { "RUST_LOG": "winx_code_agent=info" }
    }
  }
}
```
</details>

<details>
<summary><b>Claude Desktop</b></summary>

Adicione ao arquivo de configuração (`~/Library/Application Support/Claude/claude_desktop_config.json` no macOS, `%APPDATA%\Claude\claude_desktop_config.json` no Windows):

```json
{
  "mcpServers": {
    "winx": {
      "command": "winx-code-agent",
      "env": { "RUST_LOG": "winx_code_agent=info" }
    }
  }
}
```
</details>

<details>
<summary><b>Codex (CLI OpenAI)</b></summary>

Comando direto:

```bash
codex mcp add winx -- winx-code-agent
```

Ou edite `~/.codex/config.toml`:

```toml
[mcp_servers.winx]
command = "winx-code-agent"
env = { RUST_LOG = "winx_code_agent=info" }
```
</details>

<details>
<summary><b>Cursor</b></summary>

Adicione a `~/.cursor/mcp.json` (ou `.cursor/mcp.json` no projeto):

```json
{
  "mcpServers": {
    "winx": {
      "command": "winx-code-agent",
      "env": { "RUST_LOG": "winx_code_agent=info" }
    }
  }
}
```
</details>

<details>
<summary><b>VS Code (Copilot Chat / MCP)</b></summary>

Adicione a `.vscode/mcp.json`:

```json
{
  "servers": {
    "winx": {
      "type": "stdio",
      "command": "winx-code-agent"
    }
  }
}
```
</details>

<details>
<summary><b>Zed</b></summary>

Adicione às configurações do Zed (`~/.config/zed/settings.json`):

```json
{
  "context_servers": {
    "winx": {
      "source": "custom",
      "command": "winx-code-agent",
      "args": [],
      "env": { "RUST_LOG": "winx_code_agent=info" }
    }
  }
}
```
</details>

<details>
<summary><b>Windsurf</b></summary>

Adicione a `~/.codeium/windsurf/mcp_config.json`:

```json
{
  "mcpServers": {
    "winx": {
      "command": "winx-code-agent",
      "env": { "RUST_LOG": "winx_code_agent=info" }
    }
  }
}
```
</details>

<details>
<summary><b>OpenCode</b></summary>

Adicione a `opencode.json`:

```json
{
  "mcp": {
    "winx": {
      "type": "local",
      "command": ["winx-code-agent"],
      "enabled": true,
      "environment": { "RUST_LOG": "winx_code_agent=info" }
    }
  }
}
```
</details>

<details>
<summary><b>Gemini CLI</b></summary>

Adicione a `~/.gemini/settings.json`:

```json
{
  "mcpServers": {
    "winx": {
      "command": "winx-code-agent",
      "args": [],
      "env": { "RUST_LOG": "winx_code_agent=info" }
    }
  }
}
```
</details>

<details>
<summary><b>agy (Google Antigravity CLI)</b></summary>

Edite `~/.gemini/config/mcp_config.json` (ou `~/.gemini/antigravity/mcp_config.json`):

```json
{
  "mcpServers": {
    "winx": {
      "command": "winx-code-agent",
      "env": { "RUST_LOG": "winx_code_agent=info" }
    }
  }
}
```
</details>

<details>
<summary><b>Continue.dev</b></summary>

Adicione ao `~/.continue/config.yaml`:

```yaml
mcpServers:
  - name: winx
    command: winx-code-agent
    env:
      RUST_LOG: winx_code_agent=info
```
</details>

<details>
<summary><b>Kiro</b></summary>

Adicione a `~/.kiro/settings/mcp.json`:

```json
{
  "mcpServers": {
    "winx": {
      "command": "winx-code-agent",
      "env": { "RUST_LOG": "winx_code_agent=info" }
    }
  }
}
```
</details>

<details>
<summary><b>Warp</b></summary>

**Settings → MCP Servers → Add MCP Server**:

```json
{
  "winx": {
    "command": "winx-code-agent",
    "env": { "RUST_LOG": "winx_code_agent=info" }
  }
}
```
</details>

<details>
<summary><b>Roo Code</b></summary>

Adicione à configuração MCP do Roo Code:

```json
{
  "mcpServers": {
    "winx": {
      "type": "stdio",
      "command": "winx-code-agent"
    }
  }
}
```
</details>

<details>
<summary><b>Compilação a partir do código-fonte</b></summary>

```bash
git clone https://github.com/gabrielmaialva33/winx-code-agent.git
cd winx-code-agent
cargo install --path .
```

Ou compile o pacote Unix completo sem instalar:

```bash
cargo build --release --locked --bins
./target/release/winx-code-agent
```

Para desenvolvimento rápido in-process (ignorando `winxd` e `winx-guardian`):

```bash
WINX_EMBEDDED=1 cargo run --release
```
</details>

## Ciclo de vida de sessões duráveis (Unix)

O runtime com daemon limita guardians ativos a 32 por padrão e usa retenção em camadas: um shell que nunca executou um comando expira após 30 minutos; um shell utilizado expira após 24 horas. Comandos em execução ativa nunca são descartados.

```bash
# Inspecionar e conectar a uma sessão
winx-code-agent list
winx-code-agent attach <thread_id> --follow

# Limpeza explícita
winx-code-agent kill <thread_id>
winx-code-agent kill --all

# Aplicar a limpeza padrão (30 min não usados / 24 h usados)
winx-code-agent prune

# Forçar limpeza de todas as sessões inativas (preserva comandos ativos)
winx-code-agent prune --idle-seconds 0

# Recarregar winxd após alterar variáveis de ambiente (guardians permanecem vivos)
winx-code-agent restart-daemon

# Relatório de diagnóstico e ambiente
winx-code-agent doctor

# Agregar telemetria sem modificar os logs e auditar invariantes de recuperação
winx-code-agent report --last 10000 --since-minutes 120
```

## Implantação Streamable HTTP

Streamable HTTP é a interface primária do Winx para ChatGPT, agentes na nuvem e automações remotas. O endpoint é sempre `/mcp` e o listener padrão é `127.0.0.1:8000`.

Por padrão, utiliza-se `--session-affinity workspace`: a chave de sessão interna é `(principal, workspace canônico)`, portanto reconexões com IDs cosméticos diferentes compartilham o mesmo guardian e PTY. Para conversas paralelas separadas no mesmo repositório, utilize `--session-affinity conversation`.

> [!TIP]
> O guia completo cobre cabeçalhos, configuração multi-principal, túneis privados, limites operacionais, códigos de status e modelo de segurança:
> **[docs/streamable-http.pt.md](docs/streamable-http.pt.md)**.

### Perfis de implantação

| Perfil | Modelo de Credenciais | Exposição Recomendada |
| :--- | :--- | :--- |
| Agente remoto pessoal | Arquivo `--token-file` com `chmod 600` | Loopback + túnel privado ou VPN |
| Múltiplos clientes ou automações | `--principal-config` com um token por cliente | Loopback + proxy HTTPS autenticado |
| ChatGPT / Produtos OpenAI | Um principal dedicado por app | [Secure MCP Tunnel](https://developers.openai.com/api/docs/guides/secure-mcp-tunnels) ou proxy HTTPS |
| IDE ou CLI Local | Sem servidor HTTP; use stdio | Processo local |

### Configuração multi-principal

```toml
# ~/.config/winx-principals.toml
[[principals]]
name = "chatgpt"
token_file = "/home/alice/.config/winx-chatgpt-token"

[[principals]]
name = "automacao"
token_env = "WINX_AUTOMATION_TOKEN"
```

```bash
chmod 600 ~/.config/winx-principals.toml ~/.config/winx-chatgpt-token
winx-code-agent serve --http \
  --principal-config ~/.config/winx-principals.toml
```

### Padrões HTTP

| Controle | Padrão |
| :--- | :--- |
| Endereço de Bind | `127.0.0.1:8000`; endereços externos exigem `--allow-non-loopback` |
| Autenticação | `Authorization: Bearer <token>` |
| Tamanho mínimo do token | 32 bytes (exceto com `--allow-weak-token`) |
| Limite de corpo da requisição | 64 MiB |
| Timeout de requisição | 120 segundos |
| Concorrência global | 32 requisições |
| Limite de taxa (Rate limit) | 120 requisições/minuto por IP de origem |
| Atraso em autenticação inválida | 100 ms |

> [!WARNING]
> Um principal autenticado no Winx possui privilégios de execução de shell e modificação de arquivos como o usuário do sistema operacional. Mantenha o serviço em loopback, use VPNs/túneis seguros e restrinja as permissões via modos `architect` ou `code_writer` quando apropriado.

## Variáveis de ambiente

Todas são opcionais. Variáveis booleanas aceitam `1/true/yes/on` e `0/false/no/off`.

| Variável | Efeito |
| :--- | :--- |
| `RUST_LOG` | Verbosidade dos logs, ex: `winx_code_agent=info`. |
| `WINX_LOG_FORMAT` | Defina como `json` para logs operacionais JSONL no stderr; deixe ausente para formato legível. É separado do log de eventos de uso. |
| `WINX_USAGE_LOG` | Caminho para gravação de eventos de uso em JSONL assíncrono. |
| `WINX_HTTP_TOKEN` | Bearer token HTTP para modo single-principal quando argumentos CLI não são informados. |
| `WINX_RUNTIME` | Seleção de runtime no Unix: `daemon` (padrão) ou `embedded`. |
| `WINX_EMBEDDED` | Valor verdadeiro (`1`, `true`) força o runtime in-process. |
| `WINX_MAX_GUARDIANS` | Limite de sessões simultâneas no daemon (padrão: `32`). |
| `WINX_NO_COMPRESS` | Defina como `1` para desativar a compactação de linhas repetidas no shell. |
| `WINX_NO_REDACT` | Defina como `1` para desativar o mascaramento automático de segredos. |
| `WINX_ALLOW_PATHS` | Caminhos absolutos separados por `:` permitidos para ferramentas de arquivo fora do workspace. |
| `WINX_SANDBOX` | Defina como `1` para ativar o sandbox Landlock no Linux (Linux 5.13+). |
| `WINX_SHELL` | Defina como `zsh` para usar o zsh em vez do bash. |

## Verificação de releases

Cada release no GitHub contém o binário da plataforma, seu arquivo `.sha256`, o agregado `SHA256SUMS` e o SBOM CycloneDX JSON.

```bash
sha256sum --check winx-linux-amd64.tar.gz.sha256
sha256sum --check SHA256SUMS
```

## Desenvolvimento e Testes

```bash
cargo fmt --all -- --check
cargo check --all-features --locked
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-features --locked
cargo +1.88.0 check --all-features --locked
cargo package --locked
cargo bench --bench performance --locked --no-run
cargo deny --all-features check
cargo audit --deny warnings
cargo +nightly fuzz build
```

## Licença

MIT – Gabriel Maia ([@gabrielmaialva33](https://github.com/gabrielmaialva33))
