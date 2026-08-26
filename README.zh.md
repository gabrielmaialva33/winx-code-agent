<div align="center">
  <img src=".github/assets/fairy.png" alt="Winx Logo" width="150" />

  # 🪄 Winx
  ### *面向 AI 编码智能体的高性能远程 MCP 运行时*

  **持久化 PTY 会话 • Streamable HTTP • 受控文件操作 • 原生 Rust 构建 🦀**

  <p align="center">
    <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/language-Rust_2021-orange?style=flat&logo=rust" alt="Rust 2021" /></a>
    <a href="https://modelcontextprotocol.io/"><img src="https://img.shields.io/badge/MCP-2026--07--28-purple?style=flat" alt="MCP 规范" /></a>
    <a href="docs/streamable-http.zh.md"><img src="https://img.shields.io/badge/transport-Streamable_HTTP-2563eb?style=flat" alt="Streamable HTTP" /></a>
    <a href="SECURITY.md"><img src="https://img.shields.io/badge/auth-多主体认证-7c3aed?style=flat" alt="多主体认证" /></a>
    <a href="#选择传输协议"><img src="https://img.shields.io/badge/transport-stdio-2f855a?style=flat" alt="stdio 传输" /></a>
    <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue?style=flat" alt="MIT 许可证" /></a>
  </p>

  <p align="center">
    <em>"为远程与本地 LLM 提供在开发环境中持久、安全的实际操作能力。"</em>
  </p>

  <p align="center">
    <a href="#60-秒快速开始远程-mcp">⚡ <b>快速开始</b></a> &nbsp;•&nbsp;
    <a href="#核心功能特性">✨ <b>核心特性</b></a> &nbsp;•&nbsp;
    <a href="#远程架构">🏗️ <b>架构设计</b></a> &nbsp;•&nbsp;
    <a href="docs/streamable-http.zh.md">🌐 <b>HTTP 指南</b></a> &nbsp;•&nbsp;
    <a href="SECURITY.md">🛡️ <b>安全模型</b></a>
  </p>

  <p align="center">
    <a href="README.md">English</a> • <a href="README.pt.md">Português</a> • <b>中文</b>
  </p>
</div>

Winx 是一个专为 AI 智能体设计的**远程优先 MCP 运行时**。它为需要真实 Shell、受控文件编辑原语、代码库感知符号导航以及在网络断开后仍能持久保持会话的智能体而构建。其首选部署方式是经过强化的 **Streamable HTTP** 端点（用于 ChatGPT 及其他云端/网络化 MCP 客户端）；同时全面支持 **stdio** 传输（用于 Claude Code、Codex CLI、Cursor、VS Code 等本地客户端）。

在 Unix 系统上，Winx 将 MCP 适配层与拥有每个 PTY 的进程进行解耦。由 `winxd` 管理控制平面，每个逻辑会话配有一个独立的 `winx-guardian` 守护进程，即使在 HTTP 断开、客户端重启或适配器升级时也能保持 Shell 会话存活。Winx 最初基于 [WCGW](https://github.com/rusiaaman/wcgw) 的理念，但并非简单的 Python 包装器：它原生支持 `cd` 状态持久化、`Ctrl+C` 中断真实进程、交互式 TUI 交互，并且在终端海量输出返回给大模型前进行完整的虚拟终端渲染与 Token 预算控制。

> [!IMPORTANT]
> **Streamable HTTP 是主要部署途径。** Winx 默认绑定到本地回环地址（loopback），要求使用强 Bearer Token，支持多主体（Multi-principal）独立认证，并默认为每个主体/工作区分配一个持久化会话。重复发起的无状态 `Initialize` 调用会自动重新连接，而不会重复创建无用的守护进程。

## 60 秒快速开始远程 MCP

```bash
cargo install winx-code-agent

mkdir -p ~/.config
install -m 600 /dev/null ~/.config/winx-http-token
openssl rand -hex 32 > ~/.config/winx-http-token

winx-code-agent serve --http \
  --bind 127.0.0.1:8000 \
  --token-file ~/.config/winx-http-token
```

将 MCP 客户端连接至：

```text
http://127.0.0.1:8000/mcp
Authorization: Bearer <~/.config/winx-http-token 的内容>
```

云端客户端需要可访问的 HTTPS 端点。建议将 Winx 保留在回环接口，并在前方配合私有 MCP 隧道、VPN 或经过身份验证的 HTTPS 反向代理。对于 OpenAI 生态，使用 [Secure MCP Tunnel](https://developers.openai.com/api/docs/guides/secure-mcp-tunnels) 可以在不对公网暴露入站端口的情况下将本地 Winx 连接至云端。

**后续阅读：** [完整 Streamable HTTP 部署指南](docs/streamable-http.zh.md) · [安全模型](SECURITY.md) · [本地 stdio 配置](#安装)

## 为什么远程智能体需要 Winx

- **持久化会话：** 从客户端看 HTTP 是无状态的，但 Unix PTY 运行在独立的会话守护进程中，使用相同的 `thread_id` 即可随时恢复会话。
- **身份隔离机制：** 每个主体对应独立 Token；线程 ID 与 MCP Task ID 在内部严格隔离命名空间，并在响应返回前自动还原。工作区亲和性（Workspace Affinity）可自动吸收模型生成的不稳定线程 ID。
- **按需裁剪的工具目录：** `full`、`coding`、`read-only`、`terminal` 配置或每个主体的精确白名单可减少发现与 Schema 负载，并拒绝策略之外的调用。
- **统一变更协议：** 面向客户端的编辑工具只是同一个强类型计划/提交引擎的兼容视图；读取证据、规范路径绑定、原子写入、撤销检查点、验证回执与恢复规则始终一致。
- **安全失败默认策略（Fail-Closed）：** 默认仅允许绑定回环地址、Token 最小长度 32 字节、Token 文件权限强制 600、DNS 重绑定检查、请求体/超时/并发限制、基于 IP 的速率限制以及针对无效身份验证的延迟响应。
- **专为智能体优化的终端语义：** 支持前台/后台命令、状态轮询、交互式输入、稳定的 TUI 快照、轮次检测（Turn Detection）、真实进程退出码与 Token 预算限制。
- **完备的代码库工具：** 安全的 SEARCH/REPLACE 编辑、多文件原子规划、撤销操作（Undo）、Token 控制的文件读取、图像输入、上下文切换与基于 Tree-sitter 的符号导航。

## 选择传输协议

| 传输方式 | 适用场景 | 端点 / 启动命令 | 身份验证 | 会话模型 |
| :--- | :--- | :--- | :--- | :--- |
| **Streamable HTTP** | ChatGPT、云端智能体、远程自动化、多个 MCP 客户端 | 通过隧道/代理访问 `https://host/mcp`，Winx 监听 `127.0.0.1:8000` | 强 Bearer Token；可选多主体 TOML 配置 | 默认将无状态请求映射到主体/工作区的持久会话 |
| **stdio** | Claude Code、Codex CLI、Cursor、VS Code、本地桌面 IDE | 由客户端启动 `winx-code-agent` 进程 | 本地进程边界 | 单一本地客户端，在 Unix 上复用持久化守护进程运行时 |

## 远程架构

```text
远程 MCP 客户端
       │  HTTPS + Bearer Token
       ▼
Secure MCP Tunnel / VPN / 认证 HTTPS 反向代理
       │  回环 HTTP (Loopback)
       ▼
127.0.0.1:8000/mcp
       │
       ├─ Host 头部与请求体校验
       ├─ 基于 IP 限流 + 全局并发控制
       ├─ 主体身份验证
       └─ thread_id 与 MCP Task 隔离
              │
              ▼
        共享 WinxService
              │
              ├─ 工作区一致性 + 文件读取证据
              │       └─ 统一变更引擎 ── 工作区文件系统
              │
              └─ Shell 运行时
                      └─ winxd (控制平面守护进程)
                          └─ 每个会话专属 winx-guardian ── 真实 PTY / Shell / TUI
```

## 核心功能特性

- 每个线程独立的有状态 Bash 会话与真实 PTY 语义：支持前台/后台运行、状态检查、文本输入、Enter/Ctrl-C/Ctrl-D 及原始 ASCII。支持多行脚本；NUL 字节在到达 Shell 前会被拦截。
- 三种工作区安全模式：`wcgw`（完全权限）、`architect`（只读模式）、`code_writer`（白名单命令与写入通配符）。命令白名单通过 Tree-sitter 进行 AST 深度解析，检查命令行中的**每一个**指令（管道、`&&`/`||`/`;`、`$(...)`、子 Shell），防止通过 `ls && curl … | sh` 等手法绕过。
- 高弹性 PTY：如果 Shell 无法返回提示符（即使在 Ctrl-C 之后），系统会在相同的工作目录和模式下自动重置，并在清理时回收子进程。可通过 `WINX_SHELL=zsh` 启用 zsh。
- 行号范围读取（如 `file.rs:10-40`、`file.rs:10-`、`file.rs:-40`）。智能跟踪活跃文件并在上下文之中进行优先级排序。
- 单一公共工具 `EditFiles` 统一处理完整替换、SEARCH/REPLACE、修订绑定行补丁、原子批量编辑、验证与撤销。所有模式共享规范路径预检、读取/新鲜度证据、有界计划、逐文件原子替换、紧凑 Diff、类型化恢复与回执。计划失败不会写入；极少发生的提交阶段故障会准确报告已提交前缀。
- 基于 Tree-sitter 的 `CodeMap` 代码导航：提供具备 Token 预算控制的文件或仓库符号图谱，支持 13 种编程语言的定义与引用查找。
- `ContextSave` 将当前任务摘要、工作区上下文、活跃文件、Git 状态与 Diff 打包保存，供后续会话恢复。
- `ReadImage` 为多模态模型提供原生 MCP 图像块支持。
- 清晰可控的终端输出：通过虚拟终端模拟器消除交互式程序中的 ANSI 控制符与光标噪音，无损压缩重复日志（`line  [winx: ×N]`）。输出溢出时将多余部分写入 `.winx/scratch/` 临时文件供后续查阅。
- 默认开启敏感信息脱敏：自动从所有输出与保存的内存中过滤 API 密钥、JWT、PEM 私钥和 `user:pass@` URL（可通过 `WINX_NO_REDACT=1` 禁用）。Linux 上可选启用 Landlock 沙箱（`WINX_SANDBOX=1`）。

## MCP 工具列表

Winx 有意保持面向模型的目录精简。`EditFiles` 将五个重叠的变更入口合并为一次调用和每文件一个明确模式。
旧名称仍作为隐藏兼容别名供缓存客户端或已打开会话调用，但新客户端只会看到且应使用 `EditFiles`。

请选择能够覆盖客户端需求的最小目录。策略同时作用于发现与调度，内部迁移 Wire 也只能使用该策略已
授予的等价变更权限。一般代码智能体建议从 `coding` 开始；只有需要图像输入或上下文交接时才使用 `full`。

| 配置 | 工具数量 | 能力范围 |
| :--- | ---: | :--- |
| `terminal` | 2 | `Initialize` 与 `BashCommand` |
| `read-only` | 4 | 初始化、精确文件/图像读取与 `CodeMap` |
| `coding` | 5 | `Initialize`、`BashCommand`、`ReadFiles`、`CodeMap` 与 `EditFiles` |
| `full` | 7 | 默认值；在精简代码工作流上增加 `ContextSave` 与 `ReadImage` |

```bash
winx-code-agent serve --http \
  --token-file ~/.config/winx-http-token \
  --tool-profile coding
```

重复使用 `--allow-tool NAME` 可构造精确目录，并替代所选配置。目录只缩小 MCP 表面，不会改变初始化模式
授予的 Shell 或文件权限。

| 工具名称 | 功能描述 |
| :--- | :--- |
| `Initialize` | 初始化工作区、选择安全模式并返回 `thread_id`。除非客户端支持 MCP Roots 自动引导，否则应首先调用此工具。未指定路径时创建临时游乐场；恢复任务时重开保存的项目根目录。 |
| `BashCommand` | 执行命令、轮询长时间运行的任务、发送 Enter/Ctrl-C 并操作 TUI。`wait_policy` 支持：`adaptive`（默认，短调用内联返回，长命令按需提升为 Task）；`until_complete`（直接创建 Task）；`return_early`（立即返回）。支持 `is_background`、`status_check`、按键输入与 `wait_for_turn`。 |
| `ReadFiles` | 读取单个或多个文件，带行号输出，并返回不透明修订令牌与实际可见范围。支持 `path:10-40`；截断不会记录模型未看到的行。 |
| `EditFiles` | 创建、更改或撤销一个或多个文件。一次编辑调用最多接受 100 个唯一目标，并在写入前验证整个批次。每个条目选择明确模式：小范围精确修改用 `search_replace`；已有 `ReadFiles` 修订与可见坐标时用 `line_patch`；新文件或有意完整重写用 `replace`；撤销则用返回的精确 `undo_id`。可选 `verify_command` 仅在提交后运行一次；检查失败绝不会重复编辑。 |
| `ContextSave` | 将任务说明、工作区上下文、活跃文件与 Git Diff 导出为单一文本文件，便于后续会话无缝接力。 |
| `ReadImage` | 返回原生 MCP 图像内容块（非纯文本 Base64），以便多模态大模型直接查看图像。受工作区目录限制。 |
| `CodeMap` | 基于 Tree-sitter 的代码导航工具，支持两项操作：`outline`（提取文件或仓库符号大纲）和 `references`（跨仓库查找符号定义与调用位置，支持 13 种语言）。 |

已提交的编辑会保留 30 分钟的持久化回执；目标哈希未变化时，完全相同的调用不会再次写入或运行验证。验证失败返回 `completed_with_issues` 和回执绑定的 `BashCommand` 动作；同一目标连续三次 SEARCH 冲突会升级为 `recovery_exhausted`。

MCP Task 取消操作绑定到精确执行代次。如果取消发生在预留与启动之间，Winx 会等待获得精确执行身份，
或确认根本没有进程启动。若有界握手无法完成，系统会先终止受影响的会话再确认取消；延迟到达的中断
永远不会误伤下一条命令。

## 查找/替换编辑语法 (SEARCH/REPLACE)

标准块语法：

```text
<<<<<<< SEARCH
原代码内容
=======
替换后的新代码内容
>>>>>>> REPLACE
```

智能匹配引擎支持的容错特性：

- **原子性：** 匹配缺失或存在歧义时直接中止，不修改原文件。
- 当大模型缩进空格不精确时自动调整替换缩进。
- 自动清理混入 SEARCH 块中的 `ReadFiles` 行号。
- 规范化弯引号、破折号和省略号等 Unicode 符号。
- 自动利用邻近代码块消除多处相同代码片段的歧义。
- 支持单行内的局部子字符串替换。
- 允许使用行号进行精确定位（如 `<<<<<<< SEARCH @42` 或 `@42-50`）。
- 成功时返回所应用的容错细节；失败时展示最接近的代码位置并用 `~` 标出差异行。

## 安装

远程快速开始请参考[文档顶部](#60-秒快速开始远程-mcp)。本节介绍包安装及本地 stdio 客户端配置。

```bash
cargo install winx-code-agent
```

在 Linux/macOS/WSL2 上，这会在 `~/.cargo/bin` 中同时安装 `winx-code-agent`、`winxd` 和 `winx-guardian` 三个二进制文件，请保持它们在同一目录中。

环境要求：Rust 1.88+、bash 和真实终端环境。持久化守护进程运行时支持 Linux/macOS/WSL2；原生 Windows 使用内置（embedded）运行时。

<details>
<summary><b>Claude Code (CLI)</b></summary>

命令行一键添加：

```bash
claude mcp add winx -- winx-code-agent
```

或在项目根目录创建 `.mcp.json`：

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

在配置文件中添加（macOS 上为 `~/Library/Application Support/Claude/claude_desktop_config.json`，Windows 上为 `%APPDATA%\Claude\claude_desktop_config.json`）：

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
<summary><b>Codex (OpenAI CLI)</b></summary>

命令行添加：

```bash
codex mcp add winx -- winx-code-agent
```

或编辑 `~/.codex/config.toml`：

```toml
[mcp_servers.winx]
command = "winx-code-agent"
env = { RUST_LOG = "winx_code_agent=info" }
```
</details>

<details>
<summary><b>Cursor</b></summary>

在 `~/.cursor/mcp.json`（或项目的 `.cursor/mcp.json`）中添加：

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

在 `.vscode/mcp.json` 中添加：

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

在 Zed 配置文件（`~/.config/zed/settings.json`）中添加：

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

在 `~/.codeium/windsurf/mcp_config.json` 中添加：

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

在 `opencode.json` 中添加：

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

在 `~/.gemini/settings.json` 中添加：

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

编辑 `~/.gemini/config/mcp_config.json`（或 `~/.gemini/antigravity/mcp_config.json`）：

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

在 `~/.continue/config.yaml` 中添加：

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

在 `~/.kiro/settings/mcp.json` 中添加：

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

**Settings → MCP Servers → Add MCP Server**：

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

在 Roo Code MCP 配置中添加：

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
<summary><b>从源码构建</b></summary>

```bash
git clone https://github.com/gabrielmaialva33/winx-code-agent.git
cd winx-code-agent
cargo install --path .
```

或直接构建完整的 Unix 守护进程套件：

```bash
cargo build --release --locked --bins
./target/release/winx-code-agent
```

以进程内开发模式启动（跳过 `winxd` 与 `winx-guardian`）：

```bash
WINX_EMBEDDED=1 cargo run --release
```
</details>

## 持久化会话生命周期 (Unix)

守护进程运行时默认最多维护 32 个活跃 Guardian，并采用分层空闲保留机制：从未运行过命令的 Shell 将在 30 分钟后过期；运行过命令的 Shell 保留 24 小时。当前有前台或后台正在执行的任务永不销毁。

```bash
# 查看会话列表并附加到会话
winx-code-agent list
winx-code-agent attach <thread_id> --follow

# 明确清理会话
winx-code-agent kill <thread_id>
winx-code-agent kill --all

# 应用分层默认清理策略 (30分钟未使用 / 24小时已使用)
winx-code-agent prune

# 强制清理所有空闲会话 (正在运行的命令会被保留)
winx-code-agent prune --idle-seconds 0

# 修改配置后重启控制平面守护进程 (会话进程不受影响)
winx-code-agent restart-daemon

# 运行环境诊断报告
winx-code-agent doctor

# 离线汇总遥测并检查恢复流程不变量（不会修改日志）
winx-code-agent report --last 10000 --since-minutes 120
```

## Streamable HTTP 部署

Streamable HTTP 是 Winx 连接 ChatGPT、云端智能体及远程自动化的核心接口。端点统一为 `/mcp`，默认监听在 `127.0.0.1:8000`。

远程连接默认使用 `--session-affinity workspace`：内部会话键基于 `(principal, canonical_workspace)`，即使客户端生成了不一致的临时 ID，也会复用同一个 Shell。若需要同一工作区内的多会话并行隔离，可使用 `--session-affinity conversation`。

> [!TIP]
> 完整部署指南涵盖请求头、多主体配置、私有隧道、操作配额及安全防护细节：
> **[docs/streamable-http.zh.md](docs/streamable-http.zh.md)**。

### 部署方案对比

| 部署场景 | 认证方式 | 推荐暴露方式 |
| :--- | :--- | :--- |
| 个人远程智能体 | 单一 `chmod 600` `--token-file` 文件 | 回环监听 + 私有 VPN / 隧道 |
| 多个客户端或自动化服务 | `--principal-config` 为每个客户端配置独立 Token | 回环监听 + 认证 HTTPS 边缘代理 |
| ChatGPT / OpenAI 产品 | 每个应用配置独立主体 | [Secure MCP Tunnel](https://developers.openai.com/api/docs/guides/secure-mcp-tunnels) 或 HTTPS 代理 |
| 本地 IDE 或 CLI | 无需 HTTP，直接使用 stdio | 本地进程直接通信 |

### 多主体配置示例

```toml
# ~/.config/winx-principals.toml
[[principals]]
name = "chatgpt"
token_file = "/home/alice/.config/winx-chatgpt-token"

[[principals]]
name = "automation"
token_env = "WINX_AUTOMATION_TOKEN"
```

```bash
chmod 600 ~/.config/winx-principals.toml ~/.config/winx-chatgpt-token
winx-code-agent serve --http \
  --principal-config ~/.config/winx-principals.toml
```

### HTTP 默认参数

| 配置项 | 默认值 |
| :--- | :--- |
| 监听地址 | `127.0.0.1:8000`；非回环地址需指定 `--allow-non-loopback` |
| 认证机制 | `Authorization: Bearer <token>` |
| 最小 Token 长度 | 32 字节（除非使用 `--allow-weak-token`） |
| 请求体上限 | 64 MiB |
| 请求超时 | 120 秒 |
| 全局并发数 | 32 个请求 |
| 速率限制 | 每个来源 IP 限制 120 次/分钟 |
| 认证失败延迟响应 | 100 毫秒（防时序攻击） |

> [!WARNING]
> 有效的 Winx 凭证相当于在操作系统中拥有与运行该进程相同的执行权限。请始终将 HTTP 限制在回环接口或私有隧道中，并优先使用 `architect` 或受限的 `code_writer` 模式。

## 环境变量参考

所有环境变量均为可选。布尔值支持 `1/true/yes/on` 与 `0/false/no/off`。

| 环境变量 | 作用描述 |
| :--- | :--- |
| `RUST_LOG` | 日志详细度，例如 `winx_code_agent=info`。 |
| `WINX_LOG_FORMAT` | 设为 `json` 时在 stderr 输出 JSONL 运行日志；未设置时使用人类可读格式。它与隐私安全的使用事件日志相互独立。 |
| `WINX_USAGE_LOG` | 异步记录 `winx::usage` JSONL 格式遥测事件的文件路径。 |
| `WINX_HTTP_TOKEN` | 未配置命令行凭证时的单主体默认 Bearer Token。 |
| `WINX_RUNTIME` | Unix 运行时模式：`daemon`（默认）或 `embedded`。 |
| `WINX_EMBEDDED` | 设为 `1` 强制启用进程内运行（跳过守护进程）。 |
| `WINX_MAX_GUARDIANS` | 守护进程支持的最大存活会话数（默认 `32`）。 |
| `WINX_NO_COMPRESS` | 设为 `1` 禁用 Shell 输出中的重复行自动折叠。 |
| `WINX_NO_REDACT` | 设为 `1` 禁用敏感信息（API Key、JWT、私钥等）脱敏。 |
| `WINX_ALLOW_PATHS` | 允许文件操作工具访问工作区之外的绝对路径列表（以 `:` 分隔）。 |
| `WINX_SANDBOX` | 设为 `1` 启用 Linux Landlock 内核级文件沙箱保护。 |
| `WINX_SHELL` | 设为 `zsh` 优先使用 zsh 启动 Shell 会话。 |

## Release 校验

每个 GitHub Release 均提供平台编译包、对应的 `.sha256` 校验文件、汇总 `SHA256SUMS` 以及 CycloneDX JSON SBOM。

```bash
sha256sum --check winx-linux-amd64.tar.gz.sha256
sha256sum --check SHA256SUMS
```

## 开发与测试

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

## 许可证

MIT – Gabriel Maia ([@gabrielmaialva33](https://github.com/gabrielmaialva33))
