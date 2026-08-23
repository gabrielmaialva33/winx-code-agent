# Streamable HTTP 部署指南

<p align="right">
  <a href="streamable-http.md">English</a> • <a href="streamable-http.pt.md">Português</a> • <b>中文</b>
</p>

Winx 通过经过身份验证的 **Streamable HTTP** 端点向 ChatGPT、云端智能体、远程自动化流程以及无法直接启动本地 stdio 进程的客户端公开可配置的 MCP 工具集。端点统一为 `/mcp`，默认监听在 `127.0.0.1:8000`，默认工具配置仍为 `full`。

由于该端点提供了真实的 Shell 和文件系统控制能力，Winx 采取了**安全失败（Fail-Closed）**的设计策略：强制要求强凭证、除非显式声明否则拒绝非回环绑定、对请求资源消耗设置严格上限，并在不同主体之间进行严格的安全隔离。

## 配置概览

| 属性 | 默认值 |
| :--- | :--- |
| MCP 端点 | `/mcp` |
| MCP 协议版本 | `2026-07-28` |
| 监听地址 | `127.0.0.1:8000` |
| 认证机制 | `Authorization: Bearer <token>` |
| 最小 Token 长度 | 32 字节 |
| 远程会话亲和性 | `workspace`（工作区亲和） |
| 请求体上限 | 64 MiB |
| 请求超时时间 | 120 秒 |
| 全局并发上限 | 32 个并发请求 |
| 单来源 IP 限流 | 120 请求/分钟 |
| 认证失败延迟响应 | 100 毫秒 |
| 未使用 Guardian TTL | 1,800 秒（30 分钟） |
| 已使用 Guardian TTL | 86,400 秒（24 小时） |
| 存活 Guardian 配额 | 32 |

Winx 同时支持现代无状态 MCP 调用与传统的 HTTP 会话初始化流程。通过 HTTP 与 stdio 可调用完全相同的工具、Prompt、资源及可选的 MCP Tasks。

## 架构设计

```text
远程 MCP 客户端
       │ HTTPS + Bearer Token
       ▼
私有隧道 / VPN / 认证 HTTPS 反向代理
       │ 回环 HTTP
       ▼
127.0.0.1:8000/mcp
       │
       ├─ Host 头部与请求体校验
       ├─ 超时、限流与并发控制
       ├─ 主体身份验证
       ├─ 会话亲和性解析
       └─ thread_id / MCP Task 隔离
              │
              ▼
        共享 WinxService
              │
              ▼
            winxd (控制守护进程)
              │
              └─ 每个逻辑会话专属 winx-guardian
                         │
                         └─ 真实 PTY / Bash 或 zsh / 前台与后台任务
```

在 Linux、macOS 和 WSL2 上，`winx-code-agent` 仅作为 MCP 适配层。`winxd` 管理控制平面，每个 `winx-guardian` 独立控制一个 PTY。断开 HTTP 连接或重启适配层不会终止后台 PTY。

原生 Windows 环境使用内置运行时（Embedded），会话生命周期与服务器进程相同。需要持久化远程会话时推荐使用 WSL2。

## 快速上手

安装 Unix 三件套二进制文件：

```bash
cargo install winx-code-agent
```

生成仅当前用户可读的强 Token 文件：

```bash
mkdir -p ~/.config
install -m 600 /dev/null ~/.config/winx-http-token
openssl rand -hex 32 > ~/.config/winx-http-token
```

在回环接口上启动 Winx：

```bash
winx-code-agent serve --http \
  --bind 127.0.0.1:8000 \
  --token-file ~/.config/winx-http-token
```

配置客户端：

```text
URL: http://127.0.0.1:8000/mcp
Authorization: Bearer <~/.config/winx-http-token 的内容>
```

云端客户端需要公网可达的 HTTPS 接口。请保持 Winx 监听在回环地址，并在其前方配合私有隧道、VPN 或认证反向代理。

强烈推荐使用 `--token-file` 代替 `--token`，避免敏感凭证出现在进程列表（`ps`）、Shell 历史及日志中。`WINX_HTTP_TOKEN` 环境变量可作为单主体部署的环境变量备选项。

## 会话亲和性 (Session Affinity)

### 工作区亲和性 (默认)

默认模式为：

```bash
--session-affinity workspace
```

针对远程 `Initialize(first_call)` 调用，Winx 根据以下维度推导逻辑会话：

```text
(认证主体, 规范化工作区绝对路径)
```

客户端首次调用传入的 `thread_id` 不作为持久化键值。模型生成的临时变体（如 `release_02333` 与 `release_0_2_333`）在指向同一主体和工作区时将解析至同一个 Guardian 守护进程。Winx 会返回稳定的外部 ID（如 `ws_project_<hash>`），后续工具调用需使用该返回的 ID。

特性与影响：
- 无状态重连会自动附加到已有会话，而不是重复创建 Guardian；
- 重复的首调请求会保留 PTY 进程、工作目录、输出日志及正在执行的命令；
- 不同主体之间严格隔离命名空间；
- **同一主体在同一工作区内的并行对话共享同一个 Shell 会话**与前台执行锁；
- 未指定工作区的调用按主体共享临时会话；
- 任务恢复根据保存的 Task ID 进行关联。

### 对话亲和性 (`conversation`)

使用：

```bash
--session-affinity conversation
```

适用于同一主体在同一仓库中进行多路并行对话且不希望共享 Shell 的场景。会话键由以下组合生成：

```text
(认证主体, 对话标识, 规范化工作区)
```

标识解析优先级：
1. `Mcp-Session-Id`（具备稳定会话传输时）；
2. 网关注入的 `X-Winx-Conversation-Id`；
3. 首次调用提供的 `thread_id`；
4. 若均无则降级为工作区亲和。

### 线程亲和性 (`thread`)

使用：

```bash
--session-affinity thread
```

当客户端具备管理稳定唯一 ID 的能力，并显式负责会话的创建与生命周期清理时使用此模式。

## 附加或创建 (Attach-or-create)

协议 `1.3+` Guardian 支持 `FirstCall` 的自动附加或创建：
1. 逻辑会话不存在时新建 PTY；
2. 逻辑会话存在时返回其权威状态快照；
3. 适配层根据快照同步本地状态；
4. Guardian 保留原 PTY 进程、工作目录、安全模式、输出历史及运行中的命令。

## 多主体认证配置

为每个客户端或自动化分配独立凭证：

```bash
mkdir -p ~/.config
install -m 600 /dev/null ~/.config/winx-chatgpt-token
install -m 600 /dev/null ~/.config/winx-automation-token
openssl rand -hex 32 > ~/.config/winx-chatgpt-token
openssl rand -hex 32 > ~/.config/winx-automation-token
```

创建 TOML 配置文件：

```toml
# ~/.config/winx-principals.toml
[[principals]]
name = "chatgpt"
token_file = "/home/alice/.config/winx-chatgpt-token"
tool_profile = "coding"

[[principals]]
name = "automation"
token_file = "/home/alice/.config/winx-automation-token"
tool_profile = "terminal"

[[principals]]
name = "ci"
token_env = "WINX_CI_MCP_TOKEN"
allowed_tools = ["Initialize", "BashCommand", "ReadFiles"]
```

启动服务：

```bash
chmod 600 ~/.config/winx-principals.toml
winx-code-agent serve --http \
  --principal-config ~/.config/winx-principals.toml
```

主体规则要求：
- 名称仅包含 ASCII 字母、数字、`_` 和 `-`；
- 每个条目精确配置 `token_file` 或 `token_env` 之一；
- 主体名称和 Token 必须全局唯一；
- Token 文件必须为常规文件且权限为 `0600`；
- Token 长度至少为 32 字节；
- `tool_profile` 默认为 `full`；若提供 `allowed_tools`，它将完全替代该配置且不得为空；
- 工具名称区分大小写，未知名称会阻止服务启动。

### 工具目录配置

配置可为不需要全部能力的客户端缩小 `tools/list` Schema 载荷。Winx 在分发前也会执行同一策略，因此客户端无法按名称调用未公开的工具。

| 配置 | 公开的工具 |
| :--- | :--- |
| `full` | 全部九个工具（向后兼容的默认值） |
| `coding` | `Initialize`、`BashCommand`、`ReadFiles`、两个编辑工具、`UndoEdit` 和 `CodeMap` |
| `read-only` | `Initialize`、`ReadFiles`、`ReadImage` 和 `CodeMap` |
| `terminal` | `Initialize` 和 `BashCommand` |

单主体服务可通过命令行选择配置：

```bash
winx-code-agent serve --http --token-file ~/.config/winx-http-token \
  --tool-profile coding
```

也可以重复使用 `--allow-tool` 构造精确目录；显式名称会替代 `--tool-profile`：

```bash
winx-code-agent serve --http --token-file ~/.config/winx-http-token \
  --allow-tool Initialize --allow-tool BashCommand --allow-tool ReadFiles
```

工具目录策略不是 Shell 沙箱。任何包含 `BashCommand` 的配置仍拥有已初始化 Winx 模式和操作系统用户所允许的命令能力。

## LLM 编排规范 (Orchestration Contract)

MCP 握手协议定义了确定性的调用序列约束：仅初始化一次、保留返回的 `thread_id`、优先使用 `CodeMap` 获取概览、使用 `ReadFiles` 批量读取、编辑前必须先读取、用 `&&` 合并相关的快速失败检查，并且绝不原样重复已被拒绝的调用。两个编辑工具都可通过 `verify_command` 在同一次调用中执行有限的编辑后检查，从而节省一次网络和模型往返。

每个工具均声明了 `outputSchema` 并返回统一的 `structuredContent` 封装：

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

可恢复的执行失败在 HTTP/JSON-RPC 层面返回成功，而在 MCP 层面设置 `isError: true`，并附带明确的下一步修复动作（`nextAction`）。

`FileWriteOrEdit` 和 `MultiFileEdit` 接受可选的 `verify_command` 与 `verify_wait_for_seconds`（默认 `15`，最大
`60`）。验证仅在提交成功后运行，并以前台命令形式遵循与 `BashCommand` 相同的模式白名单。退出码为零时组合结果成功；非零退出码返回 `isError: true`、`errorCode: verification_failed` 和
`data.edit_applied: true`，Winx 不会错误地声称已回滚编辑。若检查在有限等待时间后仍在运行，结果会提供标准的 `BashCommand` `status_check` 下一步动作。主体必须同时允许编辑工具和 `BashCommand` 才能使用此选项。

## Guardian 生命周期管理

`winxd` 统一管理连接到控制套接字的所有 Guardian：

- **权威活动时钟：** 协议 1.3+ Guardian 记录创建时间、终端实际活动时间、最近指令时间以及是否曾执行过指令。
- **分层 TTL 回收机制：**
  - `WINX_UNUSED_SESSION_IDLE_TTL_SECS=1800`（30 分钟，从未执行过命令的会话）；
  - `WINX_SESSION_IDLE_TTL_SECS=86400`（24 小时，已执行过命令的会话）。
  - 正在执行前台或后台任务的会话不会被自动回收。
- **配额控制与释放：** 配额满时，`winxd` 会优先回收已无活动且从未执行过命令的会话。

## 运维控制命令

```bash
# 查看会话列表
winx-code-agent list

# 实时跟踪会话终端输出
winx-code-agent attach <thread_id> --follow

# 触发分层默认清理
winx-code-agent prune

# 强制清理所有空闲会话 (保留活跃命令)
winx-code-agent prune --idle-seconds 0

# 明确销毁特定会话或全部会话
winx-code-agent kill <thread_id>
winx-code-agent kill --all

# 热重启控制平面守护进程 (保持 Guardian 和 PTY 存活)
winx-code-agent restart-daemon

# 输出脱敏后的系统与环境诊断报告
winx-code-agent doctor
```

## 遥测与使用日志

配置异步非阻塞的 `winx::usage` JSONL 写入路径：

```bash
WINX_USAGE_LOG="$HOME/.local/state/winx/usage.jsonl" \
WINX_USAGE_LOG_ROTATION=daily \
WINX_USAGE_LOG_KEEP_DAYS=7 \
winx-code-agent serve --http --token-file ~/.config/winx-http-token
```

日志文件权限严格限定为 `0600`。命令文本、文件内容、Token 及原始对话标识绝不会写入遥测日志中。

## 网络暴露建议

### 推荐方案
保持监听在本地回环（`127.0.0.1:8000`），并在前方配合：
- 私有 VPN（如 WireGuard、Tailscale）；
- 仅出站（Outbound-only）MCP 安全隧道；
- 经过认证的 HTTPS 反向代理。

当代理转发公网 `Host` 头部时，通过 `--allowed-host` 进行白名单授权：

```bash
winx-code-agent serve --http \
  --token-file ~/.config/winx-http-token \
  --allowed-host mcp.example.com
```

### 直接非回环监听
若经评估后确实需要在局域网接口监听：

```bash
winx-code-agent serve --http \
  --bind 192.168.1.20:8000 \
  --allow-non-loopback \
  --token-file ~/.config/winx-http-token
```

## 资源限制与响应码

| 异常条件 | HTTP 响应 | 备注 |
| :--- | :--- | :--- |
| Token 缺失或无效 | `401 Unauthorized` | 延迟 100ms 响应 |
| 单 IP 请求频次超限 | `429 Too Many Requests` | 包含 `Retry-After: 1` 头部 |
| 全局并发配额耗尽 | `503 Service Unavailable` | 包含 `Retry-After: 1` 头部 |
| 请求执行超过 120 秒 | `408 Request Timeout` | 中止该次请求 |
| 请求体大于 64 MiB | `413 Payload Too Large` | 在 MCP 分发前直接拒绝 |

## CLI 选项速查

| 选项 | 说明 |
| :--- | :--- |
| `serve --http` | 启用 Streamable HTTP 模式 |
| `--bind <IP:PORT>` | 监听地址（默认 `127.0.0.1:8000`） |
| `--token-file <PATH>` | 单主体 Token 文件路径 |
| `--principal-config <PATH>` | 多主体 TOML 配置文件路径 |
| `--tool-profile <PROFILE>` | 单主体目录：`full`、`coding`、`read-only` 或 `terminal` |
| `--allow-tool <NAME>` | 构造精确的单主体目录；可重复使用 |
| `--session-affinity <MODE>` | 亲和性模式：`workspace`、`conversation` 或 `thread` |
| `--allow-weak-token` | 允许少于 32 字节的弱 Token（仅限测试） |
| `--allow-non-loopback` | 允许绑定非回环网络地址 |
| `--allowed-host <HOST>` | 显式允许的 Host 域名白名单 |
| `--allow-query-token` | 允许通过 URL 参数 `?token=...` 传递凭证 |
| `--max-concurrency <N>` | 全局最大并发请求数（默认 32） |
| `--requests-per-minute <N>` | 单 IP 每分钟最大请求数（默认 120） |

## 安全边界说明

经过身份验证的主体拥有以启动 Winx 的操作系统用户身份执行命令和读写文件的权限。

- `wcgw` 模式拥有完全访问权限；
- `architect` 模式限制为只读探索；
- `code_writer` 模式限制为命令和通配符白名单；
- 敏感信息脱敏默认始终开启；
- `WINX_SANDBOX=1` 可在 Linux 上启用内核级 Landlock 文件沙箱。

详情请参阅 [SECURITY.md](../SECURITY.md)。
