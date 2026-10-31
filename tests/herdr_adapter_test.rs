#![cfg(unix)]
#![allow(clippy::unwrap_used)]

//! The Herdr adapter against an in-process fake that speaks Herdr 0.9.5's socket
//! protocol: one newline-delimited JSON request per connection, answered by one
//! response line that echoes the request `id`.

use serde_json::{json, Value};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::{fs, time::Instant};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use winx_code_agent::tools::herdr::{
    HerdrAction, HerdrAdapter, HerdrContext, HerdrError, HerdrRequest, HerdrStatus,
};

enum Reply {
    Json(Value),
    Raw(Vec<u8>),
    Delayed(Duration, Value),
    Close,
    Hang,
}

type Script = dyn Fn(&Value) -> Reply + Send + Sync;
type ReplyFn = Box<dyn Fn() -> Reply + Send + Sync>;

struct Fixture {
    dir: TempDir,
    config: PathBuf,
    socket: PathBuf,
    requests: Arc<Mutex<Vec<Value>>>,
    connections: Arc<AtomicUsize>,
}

impl Fixture {
    fn new(script: impl Fn(&Value) -> Reply + Send + Sync + 'static, deadline_ms: u64) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("herdr.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let connections = Arc::new(AtomicUsize::new(0));
        let script: Arc<Script> = Arc::new(script);
        let (log, count) = (requests.clone(), connections.clone());
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                count.fetch_add(1, Ordering::SeqCst);
                let (log, script) = (log.clone(), script.clone());
                tokio::spawn(async move { serve(stream, &log, &*script).await });
            }
        });
        let config = dir.path().join("operator.json");
        let root = fs::canonicalize(dir.path()).unwrap();
        fs::write(
            &config,
            serde_json::to_vec(&json!({"socket_path": socket, "deadline_ms": deadline_ms,
                "bindings": [{"principal_key": "local", "workspace_root": root,
                    "targets": {"writer": "terminal-1"}}]}))
            .unwrap(),
        )
        .unwrap();
        fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
        Self { dir, config, socket, requests, connections }
    }
    fn adapter(&self) -> HerdrAdapter {
        HerdrAdapter::from_config_path(&self.config).unwrap()
    }
    fn context(&self) -> HerdrContext {
        HerdrContext {
            principal_key: "local".into(),
            workspace_root: fs::canonicalize(self.dir.path()).unwrap(),
            allow_prompt: true,
        }
    }
    async fn run(&self, request: HerdrRequest) -> Result<Value, HerdrError> {
        self.adapter().run(&self.context(), request).await
    }
    fn requests(&self) -> Vec<Value> {
        self.requests.lock().unwrap().clone()
    }
    fn methods(&self) -> Vec<String> {
        self.requests().iter().map(|r| r["method"].as_str().unwrap().to_string()).collect()
    }
    fn count(&self, method: &str) -> usize {
        self.methods().iter().filter(|m| *m == method).count()
    }
    fn last(&self, method: &str) -> Value {
        self.requests().into_iter().rev().find(|r| r["method"] == method).unwrap()
    }
}

async fn serve(stream: tokio::net::UnixStream, log: &Mutex<Vec<Value>>, script: &Script) {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
        return;
    }
    let request: Value = serde_json::from_str(&line).unwrap();
    log.lock().unwrap().push(request.clone());
    let reply = match script(&request) {
        Reply::Json(body) => respond(&request, body),
        Reply::Delayed(delay, body) => {
            tokio::time::sleep(delay).await;
            respond(&request, body)
        }
        Reply::Raw(bytes) => bytes,
        Reply::Close => return,
        Reply::Hang => {
            tokio::time::sleep(Duration::from_secs(30)).await;
            return;
        }
    };
    let _ = reader.get_mut().write_all(&reply).await;
}

/// Wrap a `result` or `error` body in the envelope that echoes the request id.
fn respond(request: &Value, mut body: Value) -> Vec<u8> {
    body["id"] = request["id"].clone();
    let mut bytes = serde_json::to_vec(&body).unwrap();
    bytes.push(b'\n');
    bytes
}

fn agent() -> Value {
    json!({"terminal_id": "terminal-1", "agent_status": "idle", "workspace_id": "w1",
        "tab_id": "w1:t1", "pane_id": "w1:p1", "focused": false, "state_change_seq": 1})
}

/// A live agent the operator did not configure.
fn foreign() -> Value {
    json!({"terminal_id": "terminal-9", "agent_status": "idle", "workspace_id": "w1",
        "tab_id": "w1:t1", "pane_id": "w1:p9", "focused": true, "state_change_seq": 1})
}

fn ok(result: Value) -> Reply {
    Reply::Json(Value::Object([("result".to_string(), result)].into_iter().collect()))
}

fn error(code: &str, message: &str) -> Reply {
    Reply::Json(json!({ "error": { "code": code, "message": message } }))
}

/// Herdr 0.9.5 behaviour over `agent`'s current state: agent methods resolve a
/// public pane ID hosting an agent, never a terminal ID.
fn herdr_with(agent: Value) -> impl Fn(&Value) -> Reply + Send + Sync {
    move |request| {
        let params = &request["params"];
        let method = request["method"].as_str().unwrap();
        match method {
            "ping" => return ok(json!({"type": "pong", "version": "0.9.5", "protocol": 22})),
            "agent.list" => {
                return ok(json!({"type": "agent_list", "agents": [foreign(), agent.clone()]}));
            }
            _ => {}
        }
        let Some(found) =
            [agent.clone(), foreign()].into_iter().find(|a| a["pane_id"] == params["target"])
        else {
            return error("agent_not_found", "agent target not found");
        };
        match method {
            "agent.get" | "agent.wait" => ok(json!({"type": "agent_info", "agent": found})),
            "agent.prompt" => ok(json!({"type": "agent_prompted", "agent": found})),
            "agent.explain" => ok(json!({"type": "agent_explain",
                "explain": {"agent": "claude", "state": "idle", "manifest_source": "builtin"}})),
            "agent.read" => ok(json!({"type": "pane_read", "read": {"pane_id": found["pane_id"],
                "workspace_id": "w1", "tab_id": "w1:t1", "source": "recent", "format": "text",
                "text": "literal output λ", "revision": 0, "truncated": false}})),
            _ => error("invalid_request", "unknown method"),
        }
    }
}

fn herdr() -> impl Fn(&Value) -> Reply + Send + Sync {
    herdr_with(agent())
}

/// Real Herdr behaviour except for `method`, which `reply` answers.
fn herdr_except(
    method: &'static str,
    reply: impl Fn() -> Reply + Send + Sync + 'static,
) -> impl Fn(&Value) -> Reply + Send + Sync {
    let real = herdr();
    move |request| if request["method"] == method { reply() } else { real(request) }
}

// Generous budget for fixtures whose assertions do not depend on the deadline:
// CI runners stall, and a timeout there would masquerade as a wrong variant.
const BUDGET_MS: u64 = 5_000;

fn req(action: HerdrAction) -> HerdrRequest {
    HerdrRequest { action, target: Some("writer".into()), ..Default::default() }
}

fn prompt(text: &str) -> HerdrRequest {
    HerdrRequest { text: Some(text.into()), ..req(HerdrAction::Prompt) }
}

#[tokio::test]
async fn disabled_and_binding_denials_never_connect() {
    let f = Fixture::new(herdr(), BUDGET_MS);
    assert!(matches!(
        HerdrAdapter::disabled().run(&f.context(), req(HerdrAction::Get)).await,
        Err(HerdrError::Disabled)
    ));
    let a = f.adapter();
    let mut c = f.context();
    c.principal_key = "http:other".into();
    assert!(matches!(a.run(&c, req(HerdrAction::Get)).await, Err(HerdrError::Denied(_))));
    c = f.context();
    c.workspace_root = PathBuf::from("/tmp");
    assert!(matches!(a.run(&c, req(HerdrAction::Read)).await, Err(HerdrError::Denied(_))));
    for name in ["current", "focused", "unknown", "--session=evil", "w1:p9", "terminal-9"] {
        let mut r = req(HerdrAction::Get);
        r.target = Some(name.into());
        assert!(matches!(a.run(&f.context(), r).await, Err(HerdrError::Denied(_))), "{name}");
    }
    c = f.context();
    c.allow_prompt = false;
    assert!(matches!(a.run(&c, prompt("deny")).await, Err(HerdrError::Denied(_))));
    assert_eq!(f.connections.load(Ordering::SeqCst), 0, "no denial reached the socket");
    assert!(a.run(&c, req(HerdrAction::Get)).await.is_ok());
}

#[tokio::test]
async fn protocol_mismatch_is_unavailable_before_any_agent_request() {
    for pong in [
        json!({"type": "pong", "version": "0.10.0", "protocol": 23}),
        json!({"type": "pong", "version": "0.8.0"}),
    ] {
        let f = Fixture::new(move |_| ok(pong.clone()), BUDGET_MS);
        for action in [HerdrAction::List, HerdrAction::Prompt] {
            let r = if action == HerdrAction::List { HerdrRequest::default() } else { prompt("x") };
            let error = f.run(r).await.unwrap_err();
            assert!(
                matches!(&error, HerdrError::Unavailable(m) if m.contains("protocol [22]")),
                "{error:?}"
            );
        }
        assert_eq!(f.methods(), vec!["ping", "ping"], "no agent.* request was sent");
    }
    let f = Fixture::new(|_| ok(json!({"type": "pong", "version": "0.10.0", "protocol": 23})), 500);
    let message = f.run(HerdrRequest::default()).await.unwrap_err().to_string();
    assert!(message.contains("0.10.0") && message.contains("protocol 23"), "{message}");
}

#[tokio::test]
async fn list_returns_only_configured_terminals_from_one_agent_list() {
    let f = Fixture::new(herdr(), BUDGET_MS);
    let list = f.run(HerdrRequest::default()).await.unwrap();
    assert_eq!(list["agents"], json!([agent()]), "the unconfigured agent is never listed");
    assert_eq!(f.methods(), vec!["ping", "agent.list"]);
    let ids: Vec<_> = f.requests().iter().map(|r| r["id"].clone()).collect();
    assert_ne!(ids[0], ids[1], "request ids are unique");
}

#[tokio::test]
async fn targeted_actions_resolve_the_terminal_to_its_pane_and_verify_identity() {
    let f = Fixture::new(herdr(), BUDGET_MS);
    assert_eq!(f.run(req(HerdrAction::Get)).await.unwrap()["agent"], agent());
    assert_eq!(f.methods(), vec!["ping", "agent.list", "agent.get"]);
    assert_eq!(f.last("agent.get")["params"], json!({"target": "w1:p1"}));

    let explain = f.run(req(HerdrAction::Explain)).await.unwrap();
    assert_eq!(explain["explain"]["state"], "idle");
    assert_eq!(f.last("agent.explain")["params"], json!({"target": "w1:p1"}));

    let read = f.run(HerdrRequest { lines: Some(17), ..req(HerdrAction::Read) }).await.unwrap();
    assert_eq!(read["text"], "literal output λ");
    assert_eq!(read["terminal_id"], "terminal-1");
    assert_eq!(
        f.last("agent.read")["params"],
        json!({"target": "w1:p1", "source": "recent", "lines": 17, "format": "text"})
    );

    let wait = HerdrRequest {
        timeout_ms: Some(123),
        until: vec![HerdrStatus::Idle, HerdrStatus::Done],
        ..req(HerdrAction::Wait)
    };
    assert_eq!(f.run(wait).await.unwrap()["agent"]["terminal_id"], "terminal-1");
    assert_eq!(
        f.last("agent.wait")["params"],
        json!({"target": "w1:p1", "until": ["idle", "done"], "timeout_ms": 123})
    );

    let mut by_id = req(HerdrAction::Get);
    by_id.target = Some("terminal-1".into());
    assert_eq!(f.run(by_id).await.unwrap()["agent"]["terminal_id"], "terminal-1");
}

#[tokio::test]
async fn wrong_identity_blocks_read_explain_and_prompt() {
    let real = herdr();
    let f = Fixture::new(
        move |r| {
            if r["method"] == "agent.get" {
                let mut impostor = agent();
                impostor["terminal_id"] = json!("foreign");
                ok(json!({"type": "agent_info", "agent": impostor}))
            } else {
                real(r)
            }
        },
        BUDGET_MS,
    );
    for action in [HerdrAction::Get, HerdrAction::Read, HerdrAction::Explain, HerdrAction::Prompt] {
        let r = if action == HerdrAction::Prompt { prompt("must not send") } else { req(action) };
        assert!(matches!(f.run(r).await, Err(HerdrError::InvalidResponse(_))), "{action:?}");
    }
    assert!(f.methods().iter().all(|m| matches!(m.as_str(), "ping" | "agent.list" | "agent.get")));
}

#[tokio::test]
async fn moved_pane_still_works_and_absent_terminal_is_unavailable() {
    let mut moved = agent();
    moved["pane_id"] = json!("w2:p4");
    let f = Fixture::new(herdr_with(moved), BUDGET_MS);
    let read = f.run(HerdrRequest { lines: Some(5), ..req(HerdrAction::Read) }).await.unwrap();
    assert_eq!(read["terminal_id"], "terminal-1");
    assert_eq!(f.last("agent.get")["params"]["target"], "w2:p4");
    assert_eq!(f.last("agent.read")["params"]["target"], "w2:p4");

    let mut replaced = agent();
    replaced["terminal_id"] = json!("replaced");
    let gone = Fixture::new(herdr_with(replaced), BUDGET_MS);
    assert!(matches!(gone.run(prompt("must not send")).await, Err(HerdrError::Unavailable(_))));
    assert_eq!(gone.methods(), vec!["ping", "agent.list"], "nothing targeted is sent");
}

#[tokio::test]
async fn prompt_text_is_delivered_verbatim_in_json() {
    let f = Fixture::new(herdr(), BUDGET_MS);
    for text in [
        "ordinary prompt",
        "--session=x",
        "--session",
        "--remote=host",
        "--handoff",
        "--help",
        "-h",
        "--",
        "--wait",
        "- [ ] markdown task",
        "\u{1}leading control byte",
        "line one\nline two\r\n\ttabbed",
        "literal; $(touch /tmp/never-herdr) `echo nope`\n'\" & | \\ {\"id\":\"x\"}",
    ] {
        let result = f.run(prompt(text)).await.unwrap();
        assert_eq!(result["agent"]["terminal_id"], "terminal-1");
        let sent = f.last("agent.prompt");
        assert_eq!(sent["params"]["text"].as_str(), Some(text));
        assert_eq!(sent["params"]["target"], "w1:p1", "prompts address the verified pane");
        assert!(sent["params"].get("wait").is_none(), "a plain prompt does not wait");
    }
    assert_eq!(f.count("agent.prompt"), 13);
}

#[tokio::test]
async fn prompt_with_wait_fields_submits_and_waits_atomically() {
    let f = Fixture::new(herdr(), BUDGET_MS);
    let r = HerdrRequest {
        timeout_ms: Some(1500),
        until: vec![HerdrStatus::Done, HerdrStatus::Blocked],
        ..prompt("build it")
    };
    assert!(f.run(r).await.is_ok());
    let wait = &f.last("agent.prompt")["params"]["wait"];
    assert_eq!(wait["until"], json!(["done", "blocked"]));
    assert_eq!(wait["timeout_ms"], 1500);
    assert_eq!(f.count("agent.wait"), 0, "no separate, racy wait request");

    // A wait longer than the operation deadline is capped so Herdr answers first.
    let short = Fixture::new(herdr(), 1000);
    let r = HerdrRequest { timeout_ms: Some(30_000), ..prompt("build it") };
    assert!(short.run(r).await.is_ok());
    let timeout = short.last("agent.prompt")["params"]["wait"]["timeout_ms"].as_u64().unwrap();
    assert!((1..1000).contains(&timeout), "{timeout}");
}

#[tokio::test]
async fn pre_delivery_prompt_rejections_are_definite_not_uncertain() {
    let blocked = Fixture::new(
        herdr_except("agent.prompt", || error("agent_blocked", "agent requires input")),
        BUDGET_MS,
    );
    assert!(matches!(blocked.run(prompt("hello")).await, Err(HerdrError::AgentBlocked)));
    assert_eq!(blocked.count("agent.prompt"), 1);
    for code in ["agent_not_found", "agent_not_ready", "agent_target_ambiguous"] {
        let f = Fixture::new(
            herdr_except("agent.prompt", move || error(code, "candidates: terminal_id=secret")),
            BUDGET_MS,
        );
        let error = f.run(prompt("hello")).await.unwrap_err();
        assert!(matches!(&error, HerdrError::Rejected { code: c } if c == code), "{error:?}");
        assert!(!error.to_string().contains("secret"), "Herdr messages are not forwarded");
        assert_eq!(f.count("agent.prompt"), 1);
    }
}

#[tokio::test]
async fn failures_after_the_prompt_line_is_written_are_uncertain_and_never_retried() {
    let cases: Vec<(&str, ReplyFn)> = vec![
        ("close", Box::new(|| Reply::Close)),
        ("malformed", Box::new(|| Reply::Raw(b"not json\n".to_vec()))),
        ("oversized", Box::new(|| Reply::Raw([vec![b'x'; 100_000], vec![b'\n']].concat()))),
        ("timeout code", Box::new(|| error("timeout", "timed out"))),
        ("prompt failed", Box::new(|| error("agent_prompt_failed", "pty actor closed"))),
        ("unknown code", Box::new(|| error("Something Else!", "?"))),
        ("wrong id", Box::new(|| Reply::Raw(b"{\"id\":\"other\",\"result\":{}}\n".to_vec()))),
    ];
    for (name, reply) in cases {
        let f = Fixture::new(herdr_except("agent.prompt", reply), BUDGET_MS);
        let error = f.run(prompt("only once")).await.unwrap_err();
        assert!(matches!(error, HerdrError::DeliveryUncertain(_)), "{name}: {error:?}");
        assert_eq!(f.count("agent.prompt"), 1, "{name}");
    }
    let hang = Fixture::new(herdr_except("agent.prompt", || Reply::Hang), 400);
    assert!(matches!(
        hang.run(prompt("only once")).await,
        Err(HerdrError::Timeout { uncertain: true })
    ));
    assert_eq!(hang.count("agent.prompt"), 1);
}

#[tokio::test]
async fn non_prompt_failures_are_clean_and_definite() {
    let malformed = Fixture::new(herdr_except("agent.get", || Reply::Raw(b"nope\n".into())), 900);
    assert!(matches!(
        malformed.run(req(HerdrAction::Get)).await,
        Err(HerdrError::InvalidResponse(_))
    ));
    let closed = Fixture::new(herdr_except("agent.get", || Reply::Close), BUDGET_MS);
    assert!(matches!(closed.run(req(HerdrAction::Get)).await, Err(HerdrError::Failed(_))));
    let huge = Fixture::new(
        herdr_except("agent.list", || Reply::Raw([vec![b'x'; 100_000], vec![b'\n']].concat())),
        BUDGET_MS,
    );
    assert!(matches!(huge.run(HerdrRequest::default()).await, Err(HerdrError::OutputLimit)));
    let wait = Fixture::new(herdr_except("agent.wait", || error("timeout", "")), BUDGET_MS);
    let r = HerdrRequest { timeout_ms: Some(50), ..req(HerdrAction::Wait) };
    assert!(matches!(wait.run(r).await, Err(HerdrError::Rejected { code }) if code == "timeout"));
}

#[tokio::test]
async fn deadline_bounds_a_silent_server_and_includes_identity_preflight() {
    let silent = Fixture::new(|_| Reply::Hang, 150);
    let started = Instant::now();
    assert!(matches!(
        silent.run(req(HerdrAction::Get)).await,
        Err(HerdrError::Timeout { uncertain: false })
    ));
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(silent.methods(), vec!["ping"]);

    // The deadline covers ping, list and get, so it expires before the prompt.
    let real = herdr();
    let slow = Fixture::new(
        move |r| match real(r) {
            Reply::Json(body) => Reply::Delayed(Duration::from_millis(400), body),
            other => other,
        },
        1000,
    );
    assert!(matches!(
        slow.run(prompt("one submission")).await,
        Err(HerdrError::Timeout { uncertain: false })
    ));
    assert_eq!(slow.count("agent.prompt"), 0);
}

#[tokio::test]
async fn unusable_socket_paths_fail_cleanly_without_hanging() {
    let f = Fixture::new(herdr(), BUDGET_MS);
    fs::set_permissions(&f.socket, fs::Permissions::from_mode(0o666)).unwrap();
    let error = f.run(req(HerdrAction::Get)).await.unwrap_err();
    assert!(matches!(&error, HerdrError::Unavailable(m) if m.contains("private")), "{error:?}");
    fs::remove_file(&f.socket).unwrap();
    let error = f.run(req(HerdrAction::Get)).await.unwrap_err();
    assert!(matches!(&error, HerdrError::Unavailable(m) if m.contains("does not exist")));
    fs::write(&f.socket, b"").unwrap();
    fs::set_permissions(&f.socket, fs::Permissions::from_mode(0o600)).unwrap();
    let error = f.run(req(HerdrAction::Get)).await.unwrap_err();
    assert!(matches!(&error, HerdrError::Unavailable(m) if m.contains("not a socket")));
    assert_eq!(f.connections.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn invalid_requests_fail_before_connecting() {
    let f = Fixture::new(herdr(), BUDGET_MS);
    for text in ["has\0nul".into(), "a".repeat(16385), String::new()] {
        let r = HerdrRequest { text: Some(text), ..req(HerdrAction::Prompt) };
        assert!(matches!(f.run(r).await, Err(HerdrError::InvalidRequest(_))));
    }
    let mut invalid = vec![
        HerdrRequest { timeout_ms: Some(30_001), ..req(HerdrAction::Wait) },
        HerdrRequest { timeout_ms: Some(30_001), ..prompt("x") },
        HerdrRequest { until: vec![HerdrStatus::Idle; 6], ..prompt("x") },
        HerdrRequest { text: Some("unused".into()), ..req(HerdrAction::Get) },
        HerdrRequest { lines: Some(10), ..req(HerdrAction::Get) },
        HerdrRequest { lines: Some(1001), ..req(HerdrAction::Read) },
        HerdrRequest { timeout_ms: Some(100), ..req(HerdrAction::Read) },
        HerdrRequest { until: vec![HerdrStatus::Idle], ..req(HerdrAction::Explain) },
    ];
    invalid.push(HerdrRequest { target: Some("writer".into()), ..HerdrRequest::default() });
    invalid.push(HerdrRequest { target: None, ..req(HerdrAction::Get) });
    for r in invalid {
        let debug = format!("{r:?}");
        assert!(matches!(f.run(r).await, Err(HerdrError::InvalidRequest(_))), "{debug}");
    }
    assert_eq!(f.connections.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn configuration_without_executable_loads_and_legacy_executable_is_validated() {
    let f = Fixture::new(herdr(), BUDGET_MS);
    assert!(f.run(req(HerdrAction::Get)).await.is_ok(), "socket-only configuration works");
    let original: Value = serde_json::from_slice(&fs::read(&f.config).unwrap()).unwrap();
    let write = |config: &Value| fs::write(&f.config, serde_json::to_vec(config).unwrap()).unwrap();

    let mut config = original.clone();
    config["executable"] = json!("/bin/sh");
    write(&config);
    assert!(HerdrAdapter::from_config_path(&f.config).is_ok(), "legacy field is accepted");
    config["executable"] = json!(f.dir.path().join("missing"));
    write(&config);
    assert!(matches!(HerdrAdapter::from_config_path(&f.config), Err(HerdrError::Unavailable(_))));
    config["executable"] = json!("relative/herdr");
    write(&config);
    assert!(matches!(HerdrAdapter::from_config_path(&f.config), Err(HerdrError::Configuration(_))));
    let mut config = original.clone();
    config["socket_path"] = json!("relative.sock");
    write(&config);
    assert!(matches!(HerdrAdapter::from_config_path(&f.config), Err(HerdrError::Configuration(_))));
    let mut config = original;
    config["unknown"] = json!(true);
    write(&config);
    assert!(matches!(HerdrAdapter::from_config_path(&f.config), Err(HerdrError::Configuration(_))));
    fs::write(&f.config, b"{").unwrap();
    assert!(matches!(HerdrAdapter::from_config_path(&f.config), Err(HerdrError::Configuration(_))));
    assert!(matches!(
        HerdrAdapter::from_config_path(&f.dir.path().join("absent")),
        Err(HerdrError::Configuration(_))
    ));
}

#[tokio::test]
async fn rejects_public_config_ambiguous_aliases_and_uncanonical_workspaces() {
    let f = Fixture::new(herdr(), BUDGET_MS);
    fs::set_permissions(&f.config, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(HerdrAdapter::from_config_path(&f.config), Err(HerdrError::Configuration(_))));
    fs::set_permissions(&f.config, fs::Permissions::from_mode(0o600)).unwrap();
    let original: Value = serde_json::from_slice(&fs::read(&f.config).unwrap()).unwrap();
    for targets in
        [json!({"current":"terminal-1"}), json!({"writer":"terminal-1", "terminal-1":"terminal-2"})]
    {
        let mut config = original.clone();
        config["bindings"][0]["targets"] = targets;
        fs::write(&f.config, serde_json::to_vec(&config).unwrap()).unwrap();
        assert!(matches!(
            HerdrAdapter::from_config_path(&f.config),
            Err(HerdrError::Configuration(_))
        ));
    }
    let mut config = original;
    config["bindings"][0]["workspace_root"] = json!(f.dir.path().join(".."));
    fs::write(&f.config, serde_json::to_vec(&config).unwrap()).unwrap();
    assert!(matches!(HerdrAdapter::from_config_path(&f.config), Err(HerdrError::Configuration(_))));
    assert_eq!(f.connections.load(Ordering::SeqCst), 0);
}
