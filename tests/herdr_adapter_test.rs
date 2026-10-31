#![cfg(unix)]
#![allow(clippy::unwrap_used)]

use serde_json::{json, Value};
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};
use tempfile::TempDir;
use winx_code_agent::tools::herdr::{
    HerdrAction, HerdrAdapter, HerdrContext, HerdrError, HerdrRequest, HerdrStatus,
};

struct Fixture {
    dir: TempDir,
    config: PathBuf,
    calls: PathBuf,
}
impl Fixture {
    fn new(body: &str, deadline_ms: u64) -> Self {
        Self::with_setup("", body, deadline_ms)
    }
    /// `setup` edits the live `agent` before the shared CLI behaviour runs.
    fn with_setup(setup: &str, body: &str, deadline_ms: u64) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("herdr-fixture");
        let calls = dir.path().join("calls");
        let script = format!("#!/usr/bin/env python3\nimport os,sys,json,time\na=sys.argv[1:]\nwith open({},'a') as f: f.write(json.dumps(a)+'\\n')\nassert set(k for k in os.environ if k.startswith('HERDR_'))=={{'HERDR_SOCKET_PATH'}}\nagent={{'terminal_id':'terminal-1','agent_status':'idle','workspace_id':'w','tab_id':'t','pane_id':'p','focused':False,'revision':1}}\n{}\n{}\n{}\n", serde_json::to_string(&calls.to_string_lossy()).unwrap(), setup, REAL_CLI, body);
        fs::write(&exe, script).unwrap();
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o700)).unwrap();
        let config = dir.path().join("operator.json");
        fs::write(&config, serde_json::to_vec(&json!({"executable":exe,"socket_path":dir.path().join("test.sock"),"deadline_ms":deadline_ms,"bindings":[{"principal_key":"local","workspace_root":fs::canonicalize(dir.path()).unwrap(),"targets":{"writer":"terminal-1"}}]})).unwrap()).unwrap();
        fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
        Self { dir, config, calls }
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
    fn calls(&self) -> Vec<Value> {
        fs::read_to_string(&self.calls)
            .unwrap_or_default()
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect()
    }
}
/// Real Herdr CLI behaviour every fixture inherits: `agent list` reports live
/// agents, and targeted agent commands accept a pane ID or agent name but never a
/// terminal ID. `FOREIGN` is a live agent the operator did not configure.
const REAL_CLI: &str = r"
foreign={'terminal_id':'terminal-9','agent_status':'idle','workspace_id':'w','tab_id':'t','pane_id':'q','focused':True,'revision':1}
if a[1]=='list':
    print(json.dumps({'id':'cli:agent:list','result':{'type':'agent_list','agents':[foreign,agent]}}));sys.exit(0)
target=a[2] if len(a)>2 else ''
if target not in (agent['pane_id'],foreign['pane_id']):
    print(json.dumps({'id':'cli:agent:'+a[1],'error':{'code':'agent_not_found','message':'agent target '+target+' not found'}}));sys.exit(1)
";
const NORMAL: &str = r"
if a[1]=='explain': print(json.dumps({'agent':'claude','state':'idle','manifest_source':'builtin'}))
elif a[1]=='read': print('literal output λ',end='')
else: print(json.dumps({'id':'x','result':{'type':'agent_prompted' if a[1]=='prompt' else 'agent_info','agent':agent}}))
";
// Generous budget for fixtures whose assertions do not depend on the deadline:
// CI runners stall, and a timeout there would masquerade as a wrong variant.
const BUDGET_MS: u64 = 5_000;
fn req(action: HerdrAction) -> HerdrRequest {
    HerdrRequest { action, target: Some("writer".into()), ..Default::default() }
}

#[tokio::test]
async fn disabled_and_binding_denials_never_spawn() {
    let f = Fixture::new(NORMAL, BUDGET_MS);
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
    for name in ["current", "focused", "unknown", "--session=evil"] {
        let mut r = req(HerdrAction::Get);
        r.target = Some(name.into());
        assert!(matches!(a.run(&f.context(), r).await, Err(HerdrError::Denied(_))));
    }
    c = f.context();
    c.allow_prompt = false;
    let mut r = req(HerdrAction::Prompt);
    r.text = Some("deny".into());
    assert!(matches!(a.run(&c, r).await, Err(HerdrError::Denied(_))));
    assert_eq!(f.calls(), Vec::<Value>::new());
    assert!(a.run(&c, req(HerdrAction::Get)).await.is_ok());
}

#[tokio::test]
async fn list_filters_configured_agents_and_real_cli_shapes_parse() {
    let f = Fixture::new(NORMAL, BUDGET_MS);
    let a = f.adapter();
    let c = f.context();
    let list = a.run(&c, HerdrRequest::default()).await.unwrap();
    assert_eq!(
        list["agents"],
        json!([{"terminal_id":"terminal-1","agent_status":"idle",
        "workspace_id":"w","tab_id":"t","pane_id":"p","focused":false,"revision":1}]),
        "the unconfigured agent is never listed"
    );
    assert_eq!(f.calls(), vec![json!(["agent", "list"])]);
    assert_eq!(
        a.run(&c, req(HerdrAction::Get)).await.unwrap()["agent"]["terminal_id"],
        "terminal-1"
    );
    assert_eq!(a.run(&c, req(HerdrAction::Explain)).await.unwrap()["explain"]["state"], "idle");
    let mut r = req(HerdrAction::Read);
    r.lines = Some(17);
    assert_eq!(a.run(&c, r).await.unwrap()["text"], "literal output λ");
    let mut r = req(HerdrAction::Wait);
    r.timeout_ms = Some(123);
    r.until = vec![HerdrStatus::Idle, HerdrStatus::Done];
    assert!(a.run(&c, r).await.is_ok());
    let calls = f.calls();
    assert!(calls.contains(&json!(["agent", "get", "p"])));
    assert!(calls.contains(&json!(["agent", "explain", "p", "--json"])));
    assert!(calls.contains(&json!([
        "agent", "read", "p", "--source", "recent", "--lines", "17", "--format", "text"
    ])));
    assert!(calls.contains(&json!([
        "agent",
        "wait",
        "p",
        "--timeout",
        "123",
        "--until",
        "idle",
        "--until",
        "done"
    ])));
}

#[tokio::test]
async fn prompt_is_literal_and_session_option_cannot_escape() {
    let f = Fixture::new(NORMAL, BUDGET_MS);
    let text = "--session evil; $(touch /tmp/never-herdr) `echo nope`\n'\" & | \\";
    let mut r = req(HerdrAction::Prompt);
    r.text = Some(text.into());
    assert!(f.adapter().run(&f.context(), r).await.is_ok());
    assert_eq!(
        f.calls(),
        vec![
            json!(["agent", "list"]),
            json!(["agent", "get", "p"]),
            json!(["agent", "prompt", "p", text])
        ]
    );
}

#[tokio::test]
async fn wrong_identity_blocks_read_explain_and_prompt() {
    let f = Fixture::new("agent['terminal_id']='foreign'\nprint(json.dumps({'id':'x','result':{'type':'agent_info','agent':agent}}))", BUDGET_MS);
    for action in [HerdrAction::Get, HerdrAction::Read, HerdrAction::Explain, HerdrAction::Prompt] {
        let mut r = req(action);
        if action == HerdrAction::Prompt {
            r.text = Some("must not send".into());
        }
        assert!(matches!(
            f.adapter().run(&f.context(), r).await,
            Err(HerdrError::InvalidResponse(_))
        ));
    }
    assert!(f.calls().iter().all(|c| c[1] == "list" || c[1] == "get"));
}

#[tokio::test]
async fn malformed_json_plaintext_failure_and_missing_binary_are_clean_errors() {
    let malformed = Fixture::new("print('not json')", BUDGET_MS);
    assert!(matches!(
        malformed.adapter().run(&malformed.context(), req(HerdrAction::Get)).await,
        Err(HerdrError::InvalidResponse(_))
    ));
    let failure = Fixture::new("sys.stderr.write('not connected');sys.exit(9)", BUDGET_MS);
    assert!(matches!(
        failure.adapter().run(&failure.context(), req(HerdrAction::Get)).await,
        Err(HerdrError::Failed(_))
    ));
    let f = Fixture::new(NORMAL, BUDGET_MS);
    let mut config: Value = serde_json::from_slice(&fs::read(&f.config).unwrap()).unwrap();
    config["executable"] = json!(f.dir.path().join("missing"));
    fs::write(&f.config, serde_json::to_vec(&config).unwrap()).unwrap();
    assert!(matches!(HerdrAdapter::from_config_path(&f.config), Err(HerdrError::Unavailable(_))));
    fs::write(&f.config, b"{").unwrap();
    assert!(matches!(HerdrAdapter::from_config_path(&f.config), Err(HerdrError::Configuration(_))));
    assert!(matches!(
        HerdrAdapter::from_config_path(&f.dir.path().join("absent")),
        Err(HerdrError::Configuration(_))
    ));
}

#[tokio::test]
async fn prompt_timeout_is_uncertain_without_retry() {
    let f = Fixture::new("if a[1]=='prompt': time.sleep(4)\nelse: print(json.dumps({'id':'x','result':{'type':'agent_info','agent':agent}}))", 1000);
    let mut r = req(HerdrAction::Prompt);
    r.text = Some("once".into());
    assert!(matches!(
        f.adapter().run(&f.context(), r).await,
        Err(HerdrError::Timeout { uncertain: true })
    ));
    assert_eq!(f.calls().iter().filter(|c| c[1] == "prompt").count(), 1);
}

#[tokio::test]
async fn deadline_and_both_output_pipes_are_bounded() {
    let slow = Fixture::new("time.sleep(4)", 80);
    assert!(matches!(
        slow.adapter().run(&slow.context(), req(HerdrAction::Get)).await,
        Err(HerdrError::Timeout { uncertain: false })
    ));
    for body in ["sys.stdout.write('x'*100000)", "sys.stderr.write('x'*100000)"] {
        let f = Fixture::new(body, BUDGET_MS);
        assert!(matches!(
            f.adapter().run(&f.context(), req(HerdrAction::Get)).await,
            Err(HerdrError::OutputLimit)
        ));
    }
}

#[tokio::test]
async fn invalid_prompt_and_wait_limits_fail_before_spawn() {
    let f = Fixture::new(NORMAL, BUDGET_MS);
    for text in ["has\0nul".into(), "a".repeat(16385), String::new()] {
        let mut r = req(HerdrAction::Prompt);
        r.text = Some(text);
        assert!(matches!(
            f.adapter().run(&f.context(), r).await,
            Err(HerdrError::InvalidRequest(_))
        ));
    }
    let mut r = req(HerdrAction::Wait);
    r.timeout_ms = Some(30001);
    assert!(matches!(f.adapter().run(&f.context(), r).await, Err(HerdrError::InvalidRequest(_))));
    assert_eq!(f.calls(), Vec::<Value>::new());
}

#[tokio::test]
async fn invalid_prompt_response_is_uncertain_after_exactly_one_submission() {
    for body in [
        "if a[1]=='prompt': print('invalid json')\nelse: print(json.dumps({'id':'x','result':{'type':'agent_info','agent':agent}}))",
        "if a[1]=='prompt': sys.stderr.write('arbitrary sensitive diagnostic');sys.exit(2)\nelse: print(json.dumps({'id':'x','result':{'type':'agent_info','agent':agent}}))",
        "if a[1]=='prompt': sys.stdout.write('x'*100000)\nelse: print(json.dumps({'id':'x','result':{'type':'agent_info','agent':agent}}))",
    ] {
        let f = Fixture::new(body, BUDGET_MS);
        let mut r = req(HerdrAction::Prompt); r.text = Some("only once".into());
        let error = f.adapter().run(&f.context(), r).await.unwrap_err();
        assert!(matches!(error, HerdrError::DeliveryUncertain(_)), "{body}: {error:?}");
        assert!(!error.to_string().contains("arbitrary sensitive diagnostic"));
        assert_eq!(f.calls().iter().filter(|c| c[1] == "prompt").count(), 1);
    }
}

#[test]
fn rejects_public_config_ambiguous_aliases_and_uncanonical_workspaces() {
    let f = Fixture::new(NORMAL, BUDGET_MS);
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
    assert_eq!(f.calls(), Vec::<Value>::new());
}

#[tokio::test]
async fn stable_id_is_accepted_and_stdin_is_closed() {
    let f = Fixture::new(&format!("assert sys.stdin.read()==''\n{NORMAL}"), BUDGET_MS);
    let mut r = req(HerdrAction::Get);
    r.target = Some("terminal-1".into());
    assert_eq!(
        f.adapter().run(&f.context(), r).await.unwrap()["agent"]["terminal_id"],
        "terminal-1"
    );
}

#[tokio::test]
async fn operation_deadline_includes_identity_preflight() {
    let f = Fixture::new("time.sleep(0.6)\nprint(json.dumps({'id':'x','result':{'type':'agent_prompted' if a[1]=='prompt' else 'agent_info','agent':agent}}))", 1000);
    let mut r = req(HerdrAction::Prompt);
    r.text = Some("one submission".into());
    assert!(matches!(
        f.adapter().run(&f.context(), r).await,
        Err(HerdrError::Timeout { uncertain: true })
    ));
    assert_eq!(f.calls().len(), 3);
}

#[tokio::test]
async fn irrelevant_fields_are_rejected_instead_of_silently_ignored() {
    let f = Fixture::new(NORMAL, BUDGET_MS);
    let mut r = req(HerdrAction::Get);
    r.text = Some("unused".into());
    assert!(matches!(f.adapter().run(&f.context(), r).await, Err(HerdrError::InvalidRequest(_))));
    let mut r = req(HerdrAction::Get);
    r.lines = Some(10);
    assert!(matches!(f.adapter().run(&f.context(), r).await, Err(HerdrError::InvalidRequest(_))));
    for action in [HerdrAction::Get, HerdrAction::Prompt] {
        let mut r = req(action);
        r.timeout_ms = Some(100);
        r.text = if action == HerdrAction::Prompt { Some("submit".into()) } else { None };
        assert!(matches!(
            f.adapter().run(&f.context(), r).await,
            Err(HerdrError::InvalidRequest(_))
        ));
    }
    assert_eq!(f.calls(), Vec::<Value>::new());
}

// This is the relevant external CLI contract (Herdr v0.9.5), not the adapter's
// argv builder. main.rs runs session::configure_from_args and
// remote::extract_remote_args over the whole argv (each stops at `--`, which it
// keeps), spec::print_requested_help scans for `--help`/`-h`, then
// cli/agent.rs `agent_prompt` takes args[0] as TARGET and args[1] verbatim as
// TEXT, parsing only the remaining arguments as options.
fn parse_herdr_prompt_contract(args: &[String]) -> anyhow::Result<(String, String, bool)> {
    use anyhow::{bail, ensure, Context};

    for token in args.iter().take_while(|token| token.as_str() != "--") {
        if matches!(token.as_str(), "--session" | "--remote" | "--handoff" | "--remote-keybindings")
            || ["--session=", "--remote=", "--remote-keybindings="]
                .iter()
                .any(|prefix| token.starts_with(prefix))
        {
            bail!("a global Herdr scanner consumed {token:?}");
        }
    }
    if let Some(help) = args.iter().position(|arg| matches!(arg.as_str(), "--help" | "-h")) {
        ensure!(args[..help].iter().any(|arg| arg == "--"), "help would be printed instead");
    }
    ensure!(args.first().map(String::as_str) == Some("agent"));
    ensure!(args.get(1).map(String::as_str) == Some("prompt"));
    let target = args.get(2).context("prompt target")?.clone();
    let text = args.get(3).context("prompt text")?.clone();
    let mut wait = false;
    for option in &args[4..] {
        match option.as_str() {
            "--wait" => wait = true,
            other => bail!("unknown option: {other}"),
        }
    }
    Ok((target, text, wait))
}

#[tokio::test]
async fn prompt_arguments_preserve_text_through_the_herdr_parser() -> anyhow::Result<()> {
    use anyhow::Context;

    let fixture = Fixture::new(NORMAL, BUDGET_MS);
    let adapter = fixture.adapter();
    let context = fixture.context();
    for text in [
        "ordinary prompt",
        "--session foreign is just words",
        "--wait",
        "--until",
        "--timeout=5",
        "--",
        "- [ ] markdown task",
        "\u{1}leading control byte",
        "literal; $(no shell) `no command`\n'\" & | \\",
    ] {
        let mut request = req(HerdrAction::Prompt);
        request.text = Some(text.to_string());
        adapter.run(&context, request).await?;
        let calls = fixture.calls();
        let argv: Vec<String> = calls
            .last()
            .context("prompt subprocess")?
            .as_array()
            .context("argv array")?
            .iter()
            .map(|argument| argument.as_str().context("string argument").map(str::to_string))
            .collect::<anyhow::Result<_>>()?;
        let (target, parsed_text, wait) = parse_herdr_prompt_contract(&argv)?;
        assert_eq!(target, "p", "prompts address the verified hosting pane");
        assert_eq!(parsed_text, text);
        assert!(!wait, "literal prompt must not enable a CLI wait option");
    }
    Ok(())
}

#[tokio::test]
async fn prompts_herdr_would_consume_are_rejected_before_spawn() {
    let fixture = Fixture::new(NORMAL, BUDGET_MS);
    for text in [
        "--session",
        "--session=foreign",
        "--remote",
        "--remote=host",
        "--handoff",
        "--remote-keybindings=server",
        "--help",
        "-h",
    ] {
        let mut request = req(HerdrAction::Prompt);
        request.text = Some(text.into());
        assert!(
            matches!(
                fixture.adapter().run(&fixture.context(), request).await,
                Err(HerdrError::InvalidRequest(_))
            ),
            "{text}"
        );
    }
    assert_eq!(fixture.calls(), Vec::<Value>::new(), "nothing was spawned");
}

#[tokio::test]
async fn stable_terminal_id_follows_its_pane_and_absent_terminal_is_unavailable() {
    // The pane moved: the configured terminal now lives in pane `moved`.
    let f = Fixture::with_setup("agent['pane_id']='moved'", NORMAL, BUDGET_MS);
    let mut r = req(HerdrAction::Read);
    r.lines = Some(5);
    assert_eq!(f.adapter().run(&f.context(), r).await.unwrap()["terminal_id"], "terminal-1");
    assert!(f.calls().contains(&json!(["agent", "get", "moved"])));

    let gone = Fixture::with_setup("agent['terminal_id']='replaced'", NORMAL, BUDGET_MS);
    let mut r = req(HerdrAction::Prompt);
    r.text = Some("must not send".into());
    assert!(matches!(
        gone.adapter().run(&gone.context(), r).await,
        Err(HerdrError::Unavailable(_))
    ));
    assert_eq!(gone.calls(), vec![json!(["agent", "list"])], "nothing targeted is spawned");
}
