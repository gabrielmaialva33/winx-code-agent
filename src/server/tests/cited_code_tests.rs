//! A completed command whose output cites `path:line` carries the cited code,
//! read through the ordinary `ReadFiles` path, on a real PTY.

use std::path::PathBuf;

use anyhow::{ensure, Context};
use rmcp::model::CallToolRequestParams;
use serde_json::{json, Value};

use crate::runtime::ShellActionOptions;
use crate::server::WinxService;
use crate::tool_policy::ToolPolicy;

struct Session {
    service: WinxService,
    _directory: tempfile::TempDir,
    root: PathBuf,
    thread: String,
}

impl Session {
    async fn new() -> anyhow::Result<Self> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().canonicalize()?;
        std::fs::create_dir(root.join("src"))?;
        let body = (1..=40).map(|n| format!("line {n}\n")).collect::<Vec<_>>().concat();
        std::fs::write(root.join("src/lib.rs"), body)?;
        let mut session = Self {
            service: WinxService::new(),
            _directory: directory,
            root,
            thread: format!("cited_code_{:032x}", rand::random::<u128>()),
        };
        let initialized = session
            .call(
                "Initialize",
                json!({"type": "first_call", "any_workspace_path": session.root, "mode_name": "wcgw"}),
                false,
            )
            .await?;
        ensure!(initialized["status"] == "completed", "{initialized}");
        session.thread =
            initialized["data"]["thread_id"].as_str().context("thread ID")?.to_string();
        Ok(session)
    }

    async fn call(&self, tool: &str, mut arguments: Value, cited: bool) -> anyhow::Result<Value> {
        arguments["thread_id"] = json!(self.thread);
        arguments["workspace_root"] = json!(self.root);
        let request = CallToolRequestParams::new(tool.to_string())
            .with_arguments(arguments.as_object().context("arguments")?.clone());
        let options = ShellActionOptions { attach_cited_code: cited, ..Default::default() };
        self.service
            .execute_tool_call(request, options, ToolPolicy::default().edit_permissions())
            .await?
            .result
            .structured_content
            .context("structured result")
    }

    async fn command(&self, command: &str, cited: bool) -> anyhow::Result<Value> {
        let arguments = json!({
            "action_json": {"type": "command", "command": command},
            "wait_for_seconds": 5
        });
        self.call("BashCommand", arguments, cited).await
    }

    async fn close(self) -> anyhow::Result<()> {
        self.service.shell_runtime.terminate_session(&self.thread).await?;
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cited_code_arms_a_line_patch_without_another_read() -> anyhow::Result<()> {
    let session = Session::new().await?;
    let result = session.command("printf 'src/lib.rs:20:line 20\\n'", true).await?;
    ensure!(result["status"] == "completed", "{result}");
    let output = result["output"].as_str().context("output")?;
    ensure!(output.contains("[cited code"), "{output}");
    ensure!(output.contains("line 12") && output.contains("line 28"), "{output}");
    ensure!(!output.contains("line 29\n"), "excerpt exceeds its context: {output}");
    let files = &result["data"]["cited_code"]["files"];
    ensure!(files[0]["path"].as_str().is_some_and(|p| p.ends_with("src/lib.rs")), "{result}");

    let edited = session
        .call(
            "EditFiles",
            json!({"files": [{
                "file_path": session.root.join("src/lib.rs"),
                "mode": "line_patch",
                "expected_revision": files[0]["revision"],
                "patches": [{"start_line": 20, "delete_lines": 1, "replacement": "line twenty\n"}]
            }]}),
            false,
        )
        .await?;
    ensure!(edited["status"] == "completed", "{edited}");
    let content = std::fs::read_to_string(session.root.join("src/lib.rs"))?;
    ensure!(content.contains("line 19\nline twenty\nline 21\n"), "{content}");
    session.close().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cited_code_is_opt_in_bounded_by_policy_and_redacted() -> anyhow::Result<()> {
    let session = Session::new().await?;
    // Disabled: the result is exactly an ordinary command result.
    let plain = session.command("printf 'src/lib.rs:20:x\\n'", false).await?;
    ensure!(plain["data"].get("cited_code").is_none(), "{plain}");
    ensure!(!plain["output"].as_str().unwrap_or("").contains("[cited code"), "{plain}");

    // Cited code never reaches further than a plain ReadFiles of the same
    // file (whose reach depends on the operator's WINX_ALLOW_PATHS).
    let outside = tempfile::tempdir()?;
    let secret = outside.path().canonicalize()?.join("outside.rs");
    std::fs::write(&secret, "outside workspace\n")?;
    let plain_read = session.call("ReadFiles", json!({"file_paths": [secret]}), false).await?;
    let readable = plain_read["status"] == "completed";
    let cited = session.command(&format!("printf '{}:1:x\\n'", secret.display()), true).await?;
    ensure!(cited["status"] == "completed", "{cited}");
    ensure!(cited["data"].get("cited_code").is_some() == readable, "{plain_read}\n{cited}");
    let shown = cited["output"].as_str().unwrap_or("").contains("outside workspace");
    ensure!(shown == readable, "{plain_read}\n{cited}");

    // Cited source goes through the same redaction as every other result.
    let key = format!("sk-proj-{}", "a1B2c3D4e5F6g7H8i9J0".repeat(2));
    std::fs::write(session.root.join("src/keys.py"), format!("API_KEY = \"{key}\"\n"))?;
    let redacted = session.command("printf 'src/keys.py:1:API_KEY\\n'", true).await?;
    let text = redacted.to_string();
    ensure!(redacted["data"]["cited_code"]["files"][0]["path"].is_string(), "{redacted}");
    ensure!(!text.contains(&key), "secret leaked through cited code: {text}");

    // A running command attaches nothing; only the finished output is scanned.
    let running = session.command("sleep 30; printf 'src/lib.rs:20:x\\n'", true).await?;
    ensure!(running["status"] != "completed", "{running}");
    ensure!(running["data"].get("cited_code").is_none(), "{running}");
    session.close().await
}
