use super::*;
use serde_json::{json, Value};

async fn configured_server(
    config: Option<PathBuf>,
) -> anyhow::Result<(std::net::SocketAddr, ServerProcess)> {
    spawn_server_on_free_port(move |address| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_winx-code-agent"));
        command
            .args(["serve", "--http", "--bind", &address.to_string(), "--token", TEST_TOKEN])
            .env("WINX_EMBEDDED", "1")
            .env_remove("WINX_HERDR_CONFIG")
            .env_remove("WINX_USAGE_LOG")
            .env_remove("WINX_USAGE_LOG_ROTATION")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(path) = &config {
            command.env("WINX_HERDR_CONFIG", path);
        }
        Ok(ServerProcess(command.spawn()?))
    })
    .await
}

async fn initialize_and_status_hints(
    address: std::net::SocketAddr,
    workspace: &Path,
) -> anyhow::Result<(String, Value, Value)> {
    let response =
        initialize_modern_as(address, TEST_TOKEN, workspace, "backend-hint", "backend-hint-client")
            .await?;
    let thread = initialized_thread_id(&response)?;
    let initialized = response_json(&response)?;
    let status = call_tool_json(
        address,
        &modern_request_meta("backend-hint-client", false),
        "backend-status",
        "Initialize",
        json!({
            "type":"status", "thread_id":thread, "any_workspace_path":workspace
        }),
    )
    .await?;
    assert_eq!(status["result"]["structuredContent"]["status"], "completed");
    Ok((
        thread,
        initialized["result"]["structuredContent"]["data"]["agent_backends"]["herdr"].clone(),
        status["result"]["structuredContent"]["data"]["agent_backends"]["herdr"].clone(),
    ))
}

#[tokio::test]
async fn initialize_reports_disabled_herdr_without_enabling_it() -> anyhow::Result<()> {
    let workspace = tempfile::tempdir()?;
    let (address, _server) = configured_server(None).await?;
    let (thread, initial, status) = initialize_and_status_hints(address, workspace.path()).await?;
    for hint in [initial, status] {
        assert_eq!(hint["configuration_present"], false);
        assert_eq!(hint["availability"], "disabled");
        assert_eq!(hint["reason"], "configuration_missing");
    }
    let result = call_tool_json(
        address,
        &modern_request_meta("backend-hint-client", false),
        "disabled-get",
        "Agent",
        json!({"thread_id":thread, "backend":"herdr", "action":"get", "target":"writer"}),
    )
    .await?;
    let envelope = &result["result"]["structuredContent"];
    assert_eq!(envelope["errorCode"], "herdr_disabled");
    assert_eq!(envelope["data"]["backend_availability"], "disabled");
    assert_eq!(envelope["data"]["operator_action_required"], true);
    assert_eq!(envelope["data"]["input_sent"], false);
    assert_eq!(envelope["data"]["automatic_resend_allowed"], false);
    Ok(())
}

#[tokio::test]
async fn initialize_never_claims_an_unvalidated_config_is_available() -> anyhow::Result<()> {
    let workspace = tempfile::tempdir()?;
    let config = workspace.path().join("SECRET-missing-herdr-config.json");
    let (address, _server) = configured_server(Some(config.clone())).await?;
    let (_, initial, status) = initialize_and_status_hints(address, workspace.path()).await?;
    for hint in [initial, status] {
        assert_eq!(hint["configuration_present"], true);
        assert_eq!(hint["availability"], "unknown");
        assert_eq!(hint["reason"], "configuration_not_validated");
        assert!(!hint.to_string().contains("SECRET"));
    }
    assert!(!config.exists(), "discovery must not create configuration");
    Ok(())
}
