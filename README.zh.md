<div align="center">
  <img src=".github/assets/fairy.png" alt="Winx Logo" width="150" />

  # 🪄 Winx
  ### *Your environment. Any agent.*

  **持久执行 • 高效上下文 • 安全的文件操作 • 会话控制与恢复**

  <p align="center">
    <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/language-Rust_2021-orange?style=flat&logo=rust" alt="Rust 2021" /></a>
    <a href="https://modelcontextprotocol.io/"><img src="https://img.shields.io/badge/MCP-2026--07--28-purple?style=flat" alt="MCP 规范" /></a>
    <a href="docs/streamable-http.zh.md"><img src="https://img.shields.io/badge/transport-Streamable_HTTP-2563eb?style=flat" alt="Streamable HTTP" /></a>
    <a href="SECURITY.md"><img src="https://img.shields.io/badge/auth-多主体认证-7c3aed?style=flat" alt="多主体认证" /></a>
    <a href="#选择传输协议"><img src="https://img.shields.io/badge/transport-stdio-2f855a?style=flat" alt="stdio 传输" /></a>
    <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue?style=flat" alt="MIT 许可证" /></a>
  </p>

  <p align="center">
    <em>一个 MCP 运行时，把远程 AI 智能体连接到它们自身无法触及的开发环境。</em>
  </p>

  <p align="center">
    <a href="#60-秒快速开始远程-mcp">⚡ <b>快速开始</b></a> &nbsp;•&nbsp;
    <a href="#winx-的定位">🧭 <b>定位</b></a> &nbsp;•&nbsp;
    <a href="#winx-为远程智能体提供什么">✨ <b>核心特性</b></a> &nbsp;•&nbsp;
    <a href="docs/streamable-http.zh.md">🌐 <b>HTTP 指南</b></a> &nbsp;•&nbsp;
    <a href="SECURITY.md">🛡️ <b>安全模型</b></a>
  </p>

  <p align="center">
    <a href="README.md">English</a> • <a href="README.pt.md">Português</a> • <b>中文</b>
  </p>
</div>

> [!NOTE]
> 本页是 Winx 的概览翻译。**权威的技术参考文档为英文版**，见 [README.md](README.md) 与 [docs/](docs/)；
> 如有出入，以英文版为准。

Winx 是一个**把远程 AI 智能体**（ChatGPT 及其他 MCP 客户端）**连接到本地或远程开发环境的 MCP 运行时**，这些环境是它们无法直接访问的。
它提供持久执行、节省上下文的文件读取与命令输出、受保护的文件操作，以及在连接中断后仍能延续的会话。主要部署方式是经过加固的
**Streamable HTTP** 端点；面向本地 MCP 客户端的 stdio 仍然完整支持。

**适用对象。** Winx 面向*不在*你机器上的智能体。如果你已经在自己的终端里使用 Claude Code 或 Codex，请继续直接使用它们：
Winx 不会取代它们。它让远程智能体在同一个环境中工作、协调在其中运行的智能体，并使用可选的本地工具，详见
[Winx 的定位](#winx-的定位)。

在 Linux、macOS 和原生 Windows 上，Winx 将 MCP 适配器与掌管各个 PTY 的进程分离：`winxd` 负责控制平面，每个逻辑会话由一个
`winx-guardian` 维持 shell，使其在 HTTP 断开、客户端重启和适配器升级后仍然存活。Winx 最初是
[WCGW](https://github.com/rusiaaman/wcgw) 的 Rust 移植，但它不是 Python 包装器：`cd` 会保持，`Ctrl+C` 中断真实进程，
交互式 TUI 可以正常工作，大量终端输出会先经过渲染与 token 预算再交给模型。

> [!IMPORTANT]
> **Streamable HTTP 是主要的部署方式。** Winx 默认只绑定回环地址，要求强 bearer token，支持相互独立的已认证 principal，
> 并默认为每个 principal/workspace 保持一个持久会话。

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

**后续阅读：** [完整 Streamable HTTP 部署指南](docs/streamable-http.zh.md) · [安全模型](SECURITY.md) · [本地 stdio 配置](README.md#install)

## Winx 的定位

Winx 与你已经在用的工具集成，而不是与它们竞争。

| 工具 | 与 Winx 的关系 |
|------|----------------|
| **Claude Code、Codex 及其他终端智能体** | 自带工具的本地智能体，请直接在终端中使用。Winx 把环境开放给无法在那里运行的智能体，并能把同一个环境同时提供给多个 MCP 客户端。 |
| **Herdr** | 面向编码智能体的终端工作区管理器。`Agent` 工具的可选 Herdr 后端通过 Herdr 的 socket API，让远程智能体列出、读取、等待并提示其工作区中运行的智能体。由运维方配置。 |
| **[Eulix](https://github.com/Nurysso/eulix)** | 本地代码智能引擎。当运维方设置 `WINX_EULIX_PARSER` 时，作为 `CodeMap` 的 `callers`、`callees` 和 `impact` 的可选后端。 |
| **[WCGW](https://github.com/rusiaaman/wcgw)** | Winx 的起点：WCGW 的 Rust 移植。它的模式与行范围语法得以延续。 |

## Winx 为远程智能体提供什么

### 持久执行

- **持久会话：** 对客户端而言 HTTP 是无状态的，但 PTY 存活在每会话一个的 guardian 中，可凭同一个 `thread_id` 恢复。
- **面向智能体的终端语义：** 前台与后台命令、状态轮询、交互式输入、稳定的 TUI 快照、轮次检测、真实退出码以及有界输出。
- **执行历史与智能体工作流：** 持久且隔离的历史、不消耗的输出分页、状态解释、wait-any 以及带回执的原生提示；可选的 Herdr
  后端可访问已配置的目标，或发现在会话工作区内运行的智能体。参见[工作流指南](docs/agent-workflows.md)。

### 高效上下文

- **按需裁剪的工具目录：** `full`、`coding`、`read-only` 和 `terminal` 配置档，或按 principal 的精确白名单，减少发现阶段的负载，
  并拒绝不具备等效权限的调用。
- **有界的读取与输出：** 行范围读取与按 `路径#符号` 读取定义、经过渲染并无损折叠的终端输出，以及带精确续读的单次调用预算。
  对 ChatGPT 与 Codex 客户端，若已完成命令的输出引用了少量 `路径:行号`（搜索结果、编译错误、traceback），结果会直接附带
  这些代码片段及可用于编辑的 revision，省去一次往返读取。
- **不止是 shell 的仓库工具：** 统一的编辑协议、多文件规划、撤销、按 token 预算的读取、图像输入、上下文交接以及基于 tree-sitter
  的符号导航。

### 安全的文件操作

- **单一的变更工具：** `EditFiles` 通过一个类型化的规划/提交引擎完成创建、编辑、批量、补丁、验证与撤销，同时保留读取证据、
  规范路径绑定、原子写入、回执与恢复能力。
- **默认受保护：** 所有工具输出都会脱敏密钥；提供 `architect`（只读）与 `code_writer`（白名单）模式；每个文件工具都执行路径策略；
  Linux 上可选启用 Landlock 沙箱。

### 会话控制与恢复

- **基于身份的隔离：** 每个 principal 一个 token；线程 ID 与 MCP Task ID 在内部隔离，并在响应离开服务器前转换回来。工作区亲和性
  可吸收模型生成的不稳定线程 ID。
- **默认失败即关闭的网络策略：** 默认仅回环、token 至少 32 字节、token 文件权限 `chmod 600`、防 DNS 重绑定的 Host 校验、
  请求体/时间/并发限制、先认证后占用共享资源，以及按 IP 的认证失败限流。
- **为恢复而设计：** 每个结果都带有权威的 `status`，并在 Winx 能安全推断时附上具体的 `nextAction`；守护进程或适配器重启后会恢复
  已持久化的会话，停止响应的 shell 会在同一目录与模式下原地重置。

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

## MCP 工具

| 工具 | 作用 |
|------|------|
| `Initialize` | 启动工作区、选择模式，并返回不可拆分的 `thread_id`/`workspace_root` 对。 |
| `BashCommand` | 执行命令、跟踪长时间运行的命令、发送按键并驱动 TUI。 |
| `ReadFiles` | 带行号读取文件，支持行范围（`file.rs:10-40`）或定义（`file.rs#symbol`）。 |
| `EditFiles` | 以 `line_patch`、`search_replace`、`replace` 或 `undo` 创建、修改或撤销一个或多个文件。 |
| `CodeMap` | 语法导航（`outline`、`references`），配合 Eulix 时提供 `callers`、`callees` 和 `impact`。 |
| `Execution` | 观察已接受命令的历史，支持游标与 wait-any。 |
| `Agent` | 观察原生执行或 Herdr 智能体，或向其发送提示。 |
| `ContextSave` | 保存任务描述与相关文件，便于在另一个会话中恢复。 |
| `ReadImage` | 以原生 MCP 内容返回经过校验的图像。 |

配置档 `terminal`（4 个工具）、`read-only`（5）、`coding`（7）与 `full`（9，默认）用于限制工具目录。各工具的细节、schema 与规则见
[MCP Tools](README.md#mcp-tools)。

## 技术参考（英文）

- [安装与 stdio 客户端配置](README.md#install)
- [SEARCH/REPLACE 编辑](README.md#searchreplace-editing)
- [持久会话生命周期](README.md#durable-session-lifecycle)
- [Streamable HTTP 部署](README.md#streamable-http-deployment) 与[完整指南](docs/streamable-http.md)
  （[中文译本](docs/streamable-http.zh.md)，可能滞后）
- [环境变量](README.md#environment-variables)
- [发布校验](README.md#verifying-releases)
- [开发与测试](README.md#hacking-on-it)
- [架构与不变量](docs/architecture.md)与[智能体工作流](docs/agent-workflows.md)

## 范围与原则

Winx 是一个爱好者项目，围绕真实使用中观察到的问题构建，而不是功能清单。

- **集成，而非取代。** Winx 补充终端智能体、Herdr 和 Eulix，而不是重新实现它们。集成均为可选，并由运维方配置。
- **先有证据，再有功能。** 改动来自真实会话中测得的摩擦（不含敏感数据的使用遥测与 `winx-code-agent report`），并附带回归测试。
- **小而稳定的接口。** 九个工具、用于裁剪的配置档，以及以隐藏的兼容别名代替破坏性的重命名。
- **默认安全。** 回环绑定、强 token、脱敏与受保护的编辑都是默认行为；扩大访问范围需要运维方明确选择。

Winx 不是智能体、不是 IDE、不是终端复用器，也不是代码索引器。

## 安全说明

Winx 支持本地 stdio 与远程 Streamable HTTP。任何连接到它的客户端都能以所选模式授予的能力读写文件并执行命令，影响范围等同于
把一个终端交给该客户端。HTTP 会把这种能力延伸到本地进程边界之外；对外暴露前请阅读[部署指南](docs/streamable-http.md)和
[SECURITY.md](SECURITY.md)。如需收紧：`architect` 模式（禁止写入）、`code_writer` 模式（命令与写入路径白名单）以及
`WINX_SANDBOX=1`（Linux 上的 Landlock 沙箱）。

## 许可证

MIT – Gabriel Maia ([@gabrielmaialva33](https://github.com/gabrielmaialva33))
