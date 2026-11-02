//! Point a client that drives Herdr through the shell at the `Agent` tool.
//!
//! Winx shells are not Herdr panes, so the herdr CLI (or a `HERDR_ENV`
//! check) fails there, and remote models conclude Herdr is unreachable even
//! when the operator configured the `Agent` backend. This adds guidance and a
//! concrete `nextAction`; it grants nothing; the `Agent` call still goes
//! through its own binding, mode and prompt authority.

use std::sync::OnceLock;

use regex::Regex;
use rmcp::model::{CallToolResult, ContentBlock};
use serde_json::{json, Map, Value};

use crate::tools::herdr::{list_arguments, HERDR_USAGE};

/// Add the hint to a finished `BashCommand` result whose command drives
/// Herdr, when the operator configured the backend and this server is not
/// itself running inside a Herdr pane (where the CLI would work).
pub(super) fn attach(result: &mut CallToolResult, arguments: Option<&Value>) {
    if std::env::var_os("WINX_HERDR_CONFIG").is_none()
        || std::env::var("HERDR_ENV").is_ok_and(|value| value == "1")
    {
        return;
    }
    apply(result, arguments);
}

fn apply(result: &mut CallToolResult, arguments: Option<&Value>) {
    let Some(command) = command_text(arguments) else { return };
    if !drives_herdr(command) {
        return;
    }
    let Some(Value::Object(envelope)) = result.structured_content.as_mut() else { return };
    if envelope.get("status").and_then(Value::as_str) == Some("running") {
        return;
    }
    result.content.push(ContentBlock::text(format!("[winx] {HERDR_USAGE}")));
    let data = envelope.entry("data").or_insert_with(|| Value::Object(Map::new()));
    if let Value::Object(data) = data {
        data.insert("herdr_access".to_string(), json!({ "tool": "Agent", "backend": "herdr" }));
    }
    if envelope.get("nextAction").is_none_or(Value::is_null) {
        let mut next = list_arguments();
        for key in ["thread_id", "workspace_root"] {
            if let Some(value) = arguments.and_then(|arguments| arguments.get(key)) {
                next[key] = value.clone();
            }
        }
        envelope.insert(
            "nextAction".to_string(),
            json!({ "tool": "Agent", "instruction": HERDR_USAGE, "arguments": next }),
        );
    }
}

fn command_text(arguments: Option<&Value>) -> Option<&str> {
    let arguments = arguments?;
    arguments
        .get("action_json")
        .and_then(|action| action.get("command"))
        .or_else(|| arguments.get("command"))
        .and_then(Value::as_str)
}

/// Whether `command` runs the herdr CLI in command position (optionally after
/// env assignments or a path) or inspects Herdr's pane environment. A mere
/// argument (`rg herdr src`, `cd tmp/herdr`) does not count.
fn drives_herdr(command: &str) -> bool {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    let pattern = PATTERN.get_or_init(|| {
        Regex::new(
            r"(?m)(?:^|[;&|(`]|\$\()\s*(?:(?:env|command|exec)\s+)?(?:[A-Za-z_][A-Za-z0-9_]*=\S*\s+)*(?:\S*/)?herdr(?:\s|$|[;&|)])|\bHERDR_(?:ENV|SOCKET_PATH|SESSION)\b",
        )
        .unwrap_or_else(|_| unreachable!("static pattern"))
    });
    pattern.is_match(command)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn herdr_in_command_position_or_pane_env_checks_count() {
        for command in [
            "herdr agent list",
            "  herdr --skill",
            "cd /x && herdr agent read wA8:p1 --lines 50",
            "test \"${HERDR_ENV:-}\" = 1 && herdr agent list",
            "printf 'HERDR_ENV=%s\\n' \"${HERDR_ENV:-unset}\"",
            "FOO=1 /usr/bin/herdr agent list | jq .",
            "echo $(herdr agent list)",
            "env herdr agent list",
            "true\nherdr agent list",
        ] {
            assert!(drives_herdr(command), "{command}");
        }
    }

    #[test]
    fn herdr_as_a_mere_argument_does_not_count() {
        for command in [
            "rg -n herdr src",
            "cd tmp/herdr && cargo build",
            "ls ~/.config/herdr",
            "git log --grep herdr",
            "cat docs/herdr.md",
        ] {
            assert!(!drives_herdr(command), "{command}");
        }
    }

    fn bash_result(status: &str, next_action: &Value) -> CallToolResult {
        let mut result = CallToolResult::success(vec![ContentBlock::text("HERDR_ENV=unset")]);
        result.structured_content = Some(json!({
            "status": status, "output": "HERDR_ENV=unset", "nextAction": next_action, "data": {}
        }));
        result
    }

    #[test]
    fn a_finished_herdr_command_points_at_the_agent_tool() {
        let arguments = json!({
            "thread_id": "t1", "workspace_root": "/w",
            "action_json": {"command": "test \"${HERDR_ENV:-}\" = 1 && herdr agent list"}
        });
        let mut result = bash_result("completed", &Value::Null);
        apply(&mut result, Some(&arguments));
        let envelope = result.structured_content.as_ref().unwrap_or(&Value::Null);
        assert_eq!(envelope["nextAction"]["tool"], "Agent");
        assert_eq!(
            envelope["nextAction"]["arguments"],
            json!({"backend": "herdr", "action": "list", "thread_id": "t1", "workspace_root": "/w"})
        );
        assert_eq!(envelope["data"]["herdr_access"]["tool"], "Agent");
        assert!(result.content.iter().any(|block| block
            .as_text()
            .is_some_and(|text| text.text.contains("HERDR_ENV and the herdr CLI are not needed"))));
    }

    #[test]
    fn running_unrelated_or_already_guided_results_are_left_alone() {
        let herdr = json!({"action_json": {"command": "herdr agent list"}});
        let mut running = bash_result("running", &Value::Null);
        apply(&mut running, Some(&herdr));
        assert_eq!(running.content.len(), 1);

        let unrelated = json!({"action_json": {"command": "rg -n herdr src"}});
        let mut plain = bash_result("completed", &Value::Null);
        apply(&mut plain, Some(&unrelated));
        assert_eq!(plain.content.len(), 1);

        let existing = json!({"tool": "BashCommand", "instruction": "status_check"});
        let mut guided = bash_result("completed", &existing);
        apply(&mut guided, Some(&herdr));
        let envelope = guided.structured_content.as_ref().unwrap_or(&Value::Null);
        assert_eq!(envelope["nextAction"], existing, "an existing next action wins");
        assert_eq!(guided.content.len(), 2, "the guidance text is still added");
    }

    #[test]
    fn command_text_reads_both_argument_shapes() {
        let nested = json!({"action_json": {"command": "herdr agent list"}});
        assert_eq!(command_text(Some(&nested)), Some("herdr agent list"));
        let flat = json!({"command": "herdr --skill"});
        assert_eq!(command_text(Some(&flat)), Some("herdr --skill"));
        assert_eq!(command_text(Some(&json!({"action_json": {"type": "status_check"}}))), None);
    }
}
