use std::net::TcpListener as StdTcpListener;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const TEST_TOKEN: &str = "modern-test-token-0123456789abcdef";
const LEFT_TOKEN: &str = "left-principal-token-0123456789abcdef";
const RIGHT_TOKEN: &str = "right-principal-token-0123456789abcdef";

struct ServerProcess(Child);

impl Drop for ServerProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn_single_token_server(address: std::net::SocketAddr) -> anyhow::Result<ServerProcess> {
    let child = Command::new(env!("CARGO_BIN_EXE_winx-code-agent"))
        .args(["serve", "--http", "--bind", &address.to_string(), "--token", TEST_TOKEN])
        .env("WINX_EMBEDDED", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(ServerProcess(child))
}

async fn wait_until_listening(address: std::net::SocketAddr) -> anyhow::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if TcpStream::connect(address).await.is_ok() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    anyhow::bail!("timed out waiting for HTTP server at {address}")
}

async fn post_json(
    address: std::net::SocketAddr,
    protocol_version: &str,
    method: &str,
    body: &str,
) -> anyhow::Result<String> {
    post_json_as(address, protocol_version, method, body, TEST_TOKEN).await
}

async fn post_json_as(
    address: std::net::SocketAddr,
    protocol_version: &str,
    method: &str,
    body: &str,
    token: &str,
) -> anyhow::Result<String> {
    post_json_with_session_as(address, protocol_version, Some(method), None, body, token).await
}

async fn post_json_with_session(
    address: std::net::SocketAddr,
    protocol_version: &str,
    method: Option<&str>,
    session_id: Option<&str>,
    body: &str,
) -> anyhow::Result<String> {
    post_json_with_session_as(address, protocol_version, method, session_id, body, TEST_TOKEN).await
}

async fn post_json_with_session_as(
    address: std::net::SocketAddr,
    protocol_version: &str,
    method: Option<&str>,
    session_id: Option<&str>,
    body: &str,
    token: &str,
) -> anyhow::Result<String> {
    let mut stream = TcpStream::connect(address).await?;
    let method_header =
        method.map_or_else(String::new, |method| format!("MCP-Method: {method}\r\n"));
    let body_json = serde_json::from_str::<serde_json::Value>(body).ok();
    let name_key = match method {
        Some("tools/call" | "prompts/get") => Some("name"),
        Some("resources/read" | "resources/subscribe" | "resources/unsubscribe") => Some("uri"),
        Some("tasks/get" | "tasks/update" | "tasks/cancel") => Some("taskId"),
        _ => None,
    };
    let name_header = name_key
        .and_then(|key| body_json.as_ref()?.get("params")?.get(key)?.as_str())
        .map_or_else(String::new, |name| format!("MCP-Name: {name}\r\n"));
    let session_header =
        session_id.map_or_else(String::new, |id| format!("Mcp-Session-Id: {id}\r\n"));
    let request = format!(
        "POST /mcp HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nMCP-Protocol-Version: {protocol_version}\r\n{method_header}{name_header}{session_header}Connection: close\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).await?;
    let mut response = Vec::new();
    match tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut response)).await {
        Ok(result) => {
            result?;
        }
        Err(_) => anyhow::bail!(
            "timed out reading response for {}; partial response: {}",
            method.unwrap_or("legacy"),
            String::from_utf8_lossy(&response)
        ),
    }
    Ok(String::from_utf8_lossy(&response).into_owned())
}

async fn get_path(address: std::net::SocketAddr, path: &str) -> anyhow::Result<String> {
    let mut stream = TcpStream::connect(address).await?;
    let request = format!("GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await?;
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut response)).await??;
    Ok(String::from_utf8_lossy(&response).into_owned())
}

fn response_header(response: &str, name: &str) -> Option<String> {
    response
        .lines()
        .take_while(|line| !line.is_empty() && *line != "\r")
        .filter_map(|line| line.split_once(':'))
        .find(|(header, _)| header.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.trim().to_string())
}

fn response_json(response: &str) -> anyhow::Result<serde_json::Value> {
    let data = response
        .lines()
        .find_map(|line| line.strip_prefix("data: "))
        .ok_or_else(|| anyhow::anyhow!("response has no SSE data event: {response}"))?;
    Ok(serde_json::from_str(data)?)
}

fn modern_request_meta(client_name: &str, tasks: bool) -> serde_json::Value {
    let capabilities = if tasks {
        serde_json::json!({
            "extensions": { "io.modelcontextprotocol/tasks": {} }
        })
    } else {
        serde_json::json!({})
    };
    serde_json::json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": { "name": client_name, "version": "1" },
        "io.modelcontextprotocol/clientCapabilities": capabilities
    })
}

async fn initialize_modern_as(
    address: std::net::SocketAddr,
    token: &str,
    workspace: &Path,
    thread_id: &str,
    client_name: &str,
) -> anyhow::Result<String> {
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": format!("initialize-{client_name}"),
        "method": "tools/call",
        "params": {
            "_meta": modern_request_meta(client_name, false),
            "name": "Initialize",
            "arguments": {
                "type": "first_call",
                "any_workspace_path": workspace,
                "mode_name": "wcgw",
                "thread_id": thread_id
            }
        }
    });
    post_json_as(address, "2026-07-28", "tools/call", &request.to_string(), token).await
}

async fn bash_as(
    address: std::net::SocketAddr,
    token: &str,
    thread_id: &str,
    client_name: &str,
    command: &str,
) -> anyhow::Result<String> {
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": format!("bash-{client_name}"),
        "method": "tools/call",
        "params": {
            "_meta": modern_request_meta(client_name, false),
            "name": "BashCommand",
            "arguments": {
                "action_json": {
                    "type": "command",
                    "command": command,
                    "is_background": false
                },
                "thread_id": thread_id
            }
        }
    });
    post_json_as(address, "2026-07-28", "tools/call", &request.to_string(), token).await
}

async fn pwd_as(
    address: std::net::SocketAddr,
    token: &str,
    thread_id: &str,
    client_name: &str,
) -> anyhow::Result<String> {
    bash_as(address, token, thread_id, client_name, "pwd").await
}

fn initialized_thread_id(response: &str) -> anyhow::Result<String> {
    let response = response_json(response)?;
    let text = response["result"]["content"]
        .as_array()
        .and_then(|content| content.iter().find_map(|item| item["text"].as_str()))
        .ok_or_else(|| anyhow::anyhow!("Initialize response has no text content: {response}"))?;
    text.lines()
        .find_map(|line| {
            line.strip_prefix("Use thread_id=")
                .and_then(|value| value.strip_suffix(" for all winx tool calls."))
                .map(str::to_string)
        })
        .ok_or_else(|| anyhow::anyhow!("Initialize response has no thread_id instruction: {text}"))
}

#[tokio::test(flavor = "multi_thread")]
async fn oauth_well_known_probes_return_404_without_bearer_auth() -> anyhow::Result<()> {
    let port = StdTcpListener::bind("127.0.0.1:0")?.local_addr()?.port();
    let address: std::net::SocketAddr = format!("127.0.0.1:{port}").parse()?;
    let _server = spawn_single_token_server(address)?;
    wait_until_listening(address).await?;

    for path in ["/.well-known/oauth-protected-resource", "/.well-known/openid-configuration"] {
        let response = get_path(address, path).await?;
        assert!(response.starts_with("HTTP/1.1 404"), "{path}: {response}");
    }
    let protected = get_path(address, "/mcp").await?;
    assert!(protected.starts_with("HTTP/1.1 401"), "{protected}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn modern_client_can_discover_server_before_initialization() -> anyhow::Result<()> {
    let port = StdTcpListener::bind("127.0.0.1:0")?.local_addr()?.port();
    let address: std::net::SocketAddr = format!("127.0.0.1:{port}").parse()?;
    let _server = spawn_single_token_server(address)?;
    wait_until_listening(address).await?;

    let response = post_json(
        address,
        "2026-07-28",
        "server/discover",
        r#"{"jsonrpc":"2.0","id":"discover-test","method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientInfo":{"name":"winx-test","version":"1"},"io.modelcontextprotocol/clientCapabilities":{}}}}"#,
    )
    .await?;

    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.contains("supportedVersions"), "{response}");
    assert!(response.contains("2026-07-28"), "{response}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn modern_stateless_tools_list_exposes_bash_command() -> anyhow::Result<()> {
    let port = StdTcpListener::bind("127.0.0.1:0")?.local_addr()?.port();
    let address: std::net::SocketAddr = format!("127.0.0.1:{port}").parse()?;
    let _server = spawn_single_token_server(address)?;
    wait_until_listening(address).await?;

    let response = post_json(
        address,
        "2026-07-28",
        "tools/list",
        r#"{"jsonrpc":"2.0","id":"tools-test","method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientInfo":{"name":"winx-test","version":"1"},"io.modelcontextprotocol/clientCapabilities":{}}}}"#,
    )
    .await?;

    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert_eq!(response.matches(r#""name":"BashCommand""#).count(), 1, "{response}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn legacy_initialize_session_still_exposes_bash_command() -> anyhow::Result<()> {
    let port = StdTcpListener::bind("127.0.0.1:0")?.local_addr()?.port();
    let address: std::net::SocketAddr = format!("127.0.0.1:{port}").parse()?;
    let _server = spawn_single_token_server(address)?;
    wait_until_listening(address).await?;

    let initialize = post_json_with_session(
        address,
        "2025-11-25",
        None,
        None,
        r#"{"jsonrpc":"2.0","id":"legacy-init","method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"legacy-test","version":"1"}}}"#,
    )
    .await?;
    assert!(initialize.starts_with("HTTP/1.1 200"), "{initialize}");
    let session_id = response_header(&initialize, "mcp-session-id")
        .ok_or_else(|| anyhow::anyhow!("initialize response has no session id: {initialize}"))?;

    let initialized = post_json_with_session(
        address,
        "2025-11-25",
        None,
        Some(&session_id),
        r#"{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}"#,
    )
    .await?;
    assert!(initialized.starts_with("HTTP/1.1 202"), "{initialized}");

    let tools = post_json_with_session(
        address,
        "2025-11-25",
        None,
        Some(&session_id),
        r#"{"jsonrpc":"2.0","id":"legacy-tools","method":"tools/list","params":{}}"#,
    )
    .await?;
    assert!(tools.starts_with("HTTP/1.1 200"), "{tools}");
    assert_eq!(tools.matches(r#""name":"BashCommand""#).count(), 1, "{tools}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn modern_bash_command_task_completes_through_tasks_get() -> anyhow::Result<()> {
    let workspace = tempfile::tempdir()?;
    let port = StdTcpListener::bind("127.0.0.1:0")?.local_addr()?.port();
    let address: std::net::SocketAddr = format!("127.0.0.1:{port}").parse()?;
    let _server = spawn_single_token_server(address)?;
    wait_until_listening(address).await?;

    let request_meta = modern_request_meta("task-test", true);
    let initialize = serde_json::json!({
        "jsonrpc": "2.0",
        "id": "tool-init",
        "method": "tools/call",
        "params": {
            "_meta": request_meta,
            "name": "Initialize",
            "arguments": {
                "type": "first_call",
                "any_workspace_path": workspace.path(),
                "mode_name": "wcgw",
                "thread_id": "modern_task_test"
            }
        }
    });
    let initialize_response =
        post_json(address, "2026-07-28", "tools/call", &initialize.to_string())
            .await
            .map_err(|error| anyhow::anyhow!("Initialize tool call failed: {error}"))?;
    assert!(initialize_response.starts_with("HTTP/1.1 200"), "{initialize_response}");
    let initialized_thread = initialized_thread_id(&initialize_response)?;

    let call = serde_json::json!({
        "jsonrpc": "2.0",
        "id": "task-call",
        "method": "tools/call",
        "params": {
            "_meta": request_meta,
            "name": "BashCommand",
            "arguments": {
                "action_json": {
                    "type": "command",
                    "command": "printf modern-task-output",
                    "is_background": false
                },
                "thread_id": initialized_thread
            }
        }
    });
    let call_response = post_json(address, "2026-07-28", "tools/call", &call.to_string())
        .await
        .map_err(|error| anyhow::anyhow!("BashCommand task call failed: {error}"))?;
    let call_json = response_json(&call_response)?;
    assert_eq!(call_json["result"]["resultType"], "task", "{call_response}");
    let task_id = call_json["result"]["taskId"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("task response has no taskId: {call_response}"))?;

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let get_task = serde_json::json!({
            "jsonrpc": "2.0",
            "id": "task-get",
            "method": "tasks/get",
            "params": {
                "_meta": request_meta,
                "taskId": task_id
            }
        });
        let response = post_json(address, "2026-07-28", "tasks/get", &get_task.to_string()).await?;
        let response = response_json(&response)?;
        match response["result"]["status"].as_str() {
            Some("completed") => {
                let rendered = response["result"]["result"].to_string();
                assert!(rendered.contains("modern-task-output"), "{response}");
                break;
            }
            Some("working") if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            status => anyhow::bail!("task did not complete successfully ({status:?}): {response}"),
        }
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn http_workspace_affinity_reuses_one_session_for_unstable_thread_ids() -> anyhow::Result<()>
{
    let workspace = tempfile::tempdir()?;
    let nested = workspace.path().join("nested");
    std::fs::create_dir_all(&nested)?;
    let port = StdTcpListener::bind("127.0.0.1:0")?.local_addr()?.port();
    let address: std::net::SocketAddr = format!("127.0.0.1:{port}").parse()?;
    let _server = spawn_single_token_server(address)?;
    wait_until_listening(address).await?;

    let first = initialize_modern_as(
        address,
        TEST_TOKEN,
        workspace.path(),
        "release_02333",
        "workspace-affinity-first",
    )
    .await?;
    let first_thread = initialized_thread_id(&first)?;
    assert!(first_thread.starts_with("ws_"), "{first_thread}");

    let changed = bash_as(
        address,
        TEST_TOKEN,
        &first_thread,
        "workspace-affinity-cd",
        &format!("cd {}", nested.display()),
    )
    .await?;
    assert!(changed.starts_with("HTTP/1.1 200"), "{changed}");

    let second = initialize_modern_as(
        address,
        TEST_TOKEN,
        workspace.path(),
        "release_0_2_333",
        "workspace-affinity-second",
    )
    .await?;
    let second_thread = initialized_thread_id(&second)?;
    assert_eq!(first_thread, second_thread);
    assert!(second.contains("Attached to the existing durable session"), "{second}");

    let pwd = pwd_as(address, TEST_TOKEN, &second_thread, "workspace-affinity-pwd").await?;
    assert!(pwd.contains(&nested.display().to_string()), "{pwd}");
    Ok(())
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn http_principals_isolate_the_same_external_thread_id() -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    let left_workspace = tempfile::tempdir()?;
    let right_workspace = tempfile::tempdir()?;
    let credentials = tempfile::tempdir()?;
    let left_token_file = credentials.path().join("left-token");
    let right_token_file = credentials.path().join("right-token");
    std::fs::write(&left_token_file, LEFT_TOKEN)?;
    std::fs::write(&right_token_file, RIGHT_TOKEN)?;
    std::fs::set_permissions(&left_token_file, std::fs::Permissions::from_mode(0o600))?;
    std::fs::set_permissions(&right_token_file, std::fs::Permissions::from_mode(0o600))?;

    let principal_config = credentials.path().join("principals.toml");
    std::fs::write(
        &principal_config,
        format!(
            "[[principals]]\nname = \"left\"\ntoken_file = {left_token_file:?}\n\n[[principals]]\nname = \"right\"\ntoken_file = {right_token_file:?}\n"
        ),
    )?;

    let port = StdTcpListener::bind("127.0.0.1:0")?.local_addr()?.port();
    let address: std::net::SocketAddr = format!("127.0.0.1:{port}").parse()?;
    let child = Command::new(env!("CARGO_BIN_EXE_winx-code-agent"))
        .args([
            "serve",
            "--http",
            "--bind",
            &address.to_string(),
            "--session-affinity",
            "thread",
            "--principal-config",
        ])
        .arg(&principal_config)
        .env("WINX_EMBEDDED", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let _server = ServerProcess(child);
    wait_until_listening(address).await?;

    let shared_thread = "shared_external_thread";
    let left_initialize = initialize_modern_as(
        address,
        LEFT_TOKEN,
        left_workspace.path(),
        shared_thread,
        "left-client",
    )
    .await?;
    let right_initialize = initialize_modern_as(
        address,
        RIGHT_TOKEN,
        right_workspace.path(),
        shared_thread,
        "right-client",
    )
    .await?;
    assert!(left_initialize.starts_with("HTTP/1.1 200"), "{left_initialize}");
    assert!(right_initialize.starts_with("HTTP/1.1 200"), "{right_initialize}");
    assert!(left_initialize.contains(shared_thread), "{left_initialize}");
    assert!(right_initialize.contains(shared_thread), "{right_initialize}");

    let left_pwd = pwd_as(address, LEFT_TOKEN, shared_thread, "left-client").await?;
    let right_pwd = pwd_as(address, RIGHT_TOKEN, shared_thread, "right-client").await?;
    let left_path = left_workspace.path().display().to_string();
    let right_path = right_workspace.path().display().to_string();
    assert!(left_pwd.contains(&left_path), "left principal lost its workspace: {left_pwd}");
    assert!(!left_pwd.contains(&right_path), "left principal reached right workspace: {left_pwd}");
    assert!(right_pwd.contains(&right_path), "right principal lost its workspace: {right_pwd}");
    assert!(!right_pwd.contains(&left_path), "right principal reached left workspace: {right_pwd}");
    Ok(())
}
