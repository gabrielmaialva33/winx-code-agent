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

## 工作区会话一致性

远程 `Initialize` 返回两个共同构成会话绑定的值：

```text
thread_id + workspace_root
```

后续每次有状态的 Winx 调用都必须原样携带这两个值。在选择 PTY 或执行任何工具操作前，Winx 会验证线程亲和性、提交的规范化根目录与已初始化会话是否一致。绑定缺失或混用时，服务器返回结构化的 `needs_initialize`/`conflict` 工具结果，且不会触达 Shell 或文件系统。

此检查**不会**把工具目标限制在 `workspace_root` 内。该根目录只标识拥有终端、cwd、读取历史和编辑状态的项目上下文；路径权限由独立策略控制。例如设置 `WINX_ALLOW_PATHS=/` 后，只要当前模式允许，一致的会话仍可读取或编辑项目外的辅助路径。这样既支持真实的单体仓库和跨目录工作，也防止一个对话静默继承其他项目的终端。

切换到其他项目时，请使用新路径调用 `Initialize(type="first_call")`，之后改用新返回的绑定。远程 `user_asked_change_workspace` 会安全失败，避免把持久会话键原地重定向到其他项目。

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

需要替换 Shell 时仍应显式使用重置；模式切换也必须显式请求。远程切换项目时应创建新的 `FirstCall` 绑定，而不是修改原工作区身份。重复调用 `FirstCall` 不会隐式重置会话。

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

配置可为不需要全部能力的客户端缩小 `tools/list` Schema 载荷。Winx 在分发前也会执行同一策略。旧编辑别名仅在已有等价变更权限时保持可调用，使缓存会话继续工作而不扩大权限。

| 配置 | 公开的工具 |
| :--- | :--- |
| `full` | 七个工具：精简代码目录加 `ContextSave` 与 `ReadImage`（默认值） |
| `coding` | `Initialize`、`BashCommand`、`ReadFiles`、`CodeMap` 与 `EditFiles` |
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

MCP 握手协议定义了确定性的调用序列约束：仅初始化一次、保留返回的 `thread_id`、优先使用 `CodeMap` 获取概览、使用 `ReadFiles` 批量读取、编辑前必须先读取、用 `&&` 合并相关的快速失败检查，并且绝不原样重复已被拒绝的调用。`Initialize` 还会返回一个受限的 `<workspace_root>/.winx/tmp/session-…/` 目录，供确有独立用途的派生辅助文件使用。这些文件不是权威源，必须保留原始源码路径和行号来源，并复用稳定文件名；不得仅为了调用 `CodeMap` 而把源码或命令输出转换为载体。辅助文件映射只接受一个已存在的文件，单次响应上限为 12 KiB，每个活动会话最多映射 24 个不同文件、调用 64 次；规范源码映射不受此聚合配额限制。临时存储每个会话限制为 64 MiB / 128 个文件，每个工作区限制为 256 MiB，闲置会话会在 24 小时后清理。禁止把内容编码进文件名或目录名，也禁止用 `.winx-*`/`.winx_tmp` 文件污染项目根目录。每个前台或后台 PTY 都会把这个精确目录导出为 `WINX_TEMP_DIR`；若 Shell 写入的静态目标绕过该目录，调用会以 `temporary_artifact_policy` 被拒绝。Winx 还会在每次 Bash 操作后审计实际用量，包括静态分析无法预测的动态写入；结果会报告字节数和文件数。超出配额后，普通命令将被阻止，直到代理显式检查并删除过时辅助文件。超额本身不会触发自动删除；原有的 24 小时闲置会话清理规则保持不变。若 `Initialize` 返回 `initialize_workspace_already_bound` 或 `workspace_change_requires_new_session`，则当前对话不得重试该调用：访问策略允许的绝对路径时继续使用现有绑定，真正切换项目时应开启新的对话。`EditFiles` 可通过 `verify_command` 在同一次调用中执行有限的编辑后检查，从而节省一次网络和模型往返。

`ReadFiles` 批次中的文件会在受限并行池中处理（`WINX_READ_PARALLELISM`，默认 `4`，最大 `32`），但响应内容和读取保护范围始终严格按请求顺序发布。

每个工具均声明了 `outputSchema` 并返回统一的 `structuredContent` 封装：

```json
{
  "status": "needs_read",
  "tool": "EditFiles",
  "message": "EditFiles failed: ...",
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

`BashCommand.wait_policy=until_complete` 仅适用于有限的前台命令。若用于后台、状态、输入、屏幕或等待操作，
Winx 会返回可恢复结果（`errorCode: wait_policy_incompatible_with_action`），并在
`nextAction.arguments` 中把策略修正为 `return_early`。

Task 取消同样绑定到精确执行代次。若取消与启动并发，Winx 会等待启动发布准确执行令牌，或确认没有进程
启动。若有界握手无法完成，系统会先终止受影响的 Shell 再确认取消，因此过期中断不会命中下一条命令。

`ReadImage` 会根据内容验证 JPEG、PNG、GIF 或 WebP，而不是信任文件扩展名。源文件上限为 50 MiB，并另设
解码尺寸和内存分配限制；交付内容限制为 2 MiB、长边 2560 px，较大的输入会自动缩放和重新编码。每个会话
都有受限的内容指纹缓存，未变化的重复读取会返回紧凑的结构化引用；只有确实需要再次传输字节时才使用
`force=true`。

`EditFiles` 是强类型变更引擎唯一的公共入口。它的每文件显式模式为 `replace`、`search_replace`、`line_patch`
和 `undo`；一次 `apply` 可接收 1 到 100 个唯一目标，并在写入前验证整个批次。旧名称保留为隐藏兼容
别名，不能扩大等价 `EditFiles` 模式已授予的权限。

`EditFiles` 接受可选的 `verify_command` 与 `verify_wait_for_seconds`（默认 `15`，最大
`60`）。验证仅在提交成功后运行，并以前台命令形式遵循与 `BashCommand` 相同的模式白名单。退出码为零时组合结果成功；非零退出码保留 `isError: false`，并返回 `status: completed_with_issues`、`errorCode: verification_failed` 和
`data.edit_applied: true`。回执绑定的 `BashCommand` 下一步动作只会在修复后重新执行检查，不会重复编辑。若检查仍在运行，结果会提供标准的 `BashCommand` `status_check` 动作。验证同时需要 `EditFiles` 与 `BashCommand` 权限。

已提交的编辑会保留 30 分钟的紧凑持久化变更回执。完全相同的调用采用 single-flight，并且仅在目标哈希仍匹配时无副作用重放；若目标后来发生变化，Winx 会返回 `mutation_postcondition_changed`，不会覆盖新状态。同一会话和目标连续三次 SEARCH 冲突会升级为 `recovery_exhausted`，停止自动重试并要求代理改变策略。

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

日志文件权限严格限定为 `0600`。命令文本、文件内容、Token 及原始对话标识绝不会写入遥测日志中；日志仅记录时延、结果状态、响应大小、批次项数、worker 上限和协议元数据。可通过 `request_id` 关联 `tool_call` 与 `http_request`，从而在不暴露负载的情况下区分工具耗时和传输开销。

可使用以下命令快速查看各工具的延迟分布：

```bash
jq -s '
  def pct($p): sort | .[((length - 1) * $p | floor)];
  [.[] | select(.fields.event == "tool_call") | .fields]
  | group_by(.tool)
  | map(. as $calls | {
      tool: $calls[0].tool,
      calls: ($calls | length),
      p50_ms: ([$calls[].duration_ms] | pct(0.50)),
      p95_ms: ([$calls[].duration_ms] | pct(0.95))
    })
' ~/.local/state/winx/usage.jsonl*
```

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
