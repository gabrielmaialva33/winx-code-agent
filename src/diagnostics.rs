//! Read-only runtime diagnostics shared by the CLI and future health endpoints.

use serde_json::{json, Value};

/// Build a redacted snapshot of configuration and live runtime topology.
pub async fn doctor_report() -> Value {
    let runtime = crate::runtime::configured_runtime_mode().map_or_else(
        |error| format!("invalid: {error}"),
        |mode| format!("{mode:?}").to_ascii_lowercase(),
    );
    let build = crate::build_info::BuildIdentity::current();
    let mut report = json!({
        "version": crate::build_info::package_version(),
        "build": build,
        "platform": {
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH
        },
        "cwd": std::env::current_dir().ok().map(|path| path.display().to_string()),
        "runtime": runtime,
        "environment": {
            "sandbox_requested": crate::config::env_flag("WINX_SANDBOX"),
            "redaction_disabled": crate::config::env_flag("WINX_NO_REDACT"),
            "compression_disabled": crate::config::env_flag("WINX_NO_COMPRESS"),
            "http_token_configured": crate::config::env_text("WINX_HTTP_TOKEN").is_some(),
            "usage_log_configured": crate::config::env_text("WINX_USAGE_LOG").is_some()
        }
    });

    let allowed_roots = crate::utils::path::configured_allowed_roots();
    let unconfined =
        cfg!(unix) && allowed_roots.iter().any(|root| root == std::path::Path::new("/"));
    let containment_mode = if unconfined {
        "unconfined"
    } else if allowed_roots.is_empty() {
        "workspace"
    } else {
        "extended"
    };
    report["file_tool_containment"] = json!({
        "mode": containment_mode,
        "extra_roots": allowed_roots
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
    });

    #[cfg(unix)]
    add_unix_runtime_report(&mut report).await;
    report
}

#[cfg(unix)]
async fn add_unix_runtime_report(report: &mut Value) {
    use crate::daemon::{socket_candidates, DaemonClient};

    let expected = crate::build_info::BuildIdentity::current();
    let candidates = socket_candidates();
    let selected = candidates
        .iter()
        .find(|candidate| candidate.selected)
        .map(|candidate| candidate.path.display().to_string());
    let mut entries = Vec::with_capacity(candidates.len());
    let mut reachable = 0_u64;
    let mut compatible = 0_u64;
    let mut mixed_guardians = 0_u64;

    for candidate in candidates {
        let client = DaemonClient::new(&candidate.path);
        let entry = match client.hello().await {
            Ok(hello) => {
                reachable += 1;
                let build_compatible = hello.build_matches(&expected);
                compatible += u64::from(build_compatible);
                let sessions = match client.list_sessions().await {
                    Ok(sessions) => sessions,
                    Err(error) => {
                        entries.push(json!({
                            "path": candidate.path.display().to_string(),
                            "sources": candidate.sources,
                            "selected": candidate.selected,
                            "reachable": true,
                            "build_compatible": build_compatible,
                            "hello": hello,
                            "sessions_error": error.to_string()
                        }));
                        continue;
                    }
                };
                let session_entries = sessions
                    .into_iter()
                    .map(|session| {
                        let guardian_build_compatible = session
                            .runtime
                            .as_ref()
                            .is_some_and(|runtime| runtime.build_matches(&expected));
                        if session.runtime.is_some() && !guardian_build_compatible {
                            mixed_guardians += 1;
                        }
                        json!({
                            "thread_id": session.thread_id,
                            "cwd": session.cwd,
                            "running": session.running,
                            "guardian_build_compatible": guardian_build_compatible,
                            "runtime": session.runtime
                        })
                    })
                    .collect::<Vec<_>>();
                json!({
                    "path": candidate.path.display().to_string(),
                    "sources": candidate.sources,
                    "selected": candidate.selected,
                    "reachable": true,
                    "build_compatible": build_compatible,
                    "hello": hello,
                    "session_count": session_entries.len(),
                    "sessions": session_entries
                })
            }
            Err(error) => json!({
                "path": candidate.path.display().to_string(),
                "sources": candidate.sources,
                "selected": candidate.selected,
                "reachable": false,
                "error": error.to_string()
            }),
        };
        entries.push(entry);
    }

    report["daemon_topology"] = json!({
        "selected_socket": selected,
        "reachable_count": reachable,
        "compatible_count": compatible,
        "split_brain": reachable > 1,
        "mixed_guardian_builds": mixed_guardians,
        "healthy": reachable == 1 && mixed_guardians == 0 && compatible == reachable,
        "candidates": entries
    });
    report["daemon_binary"] = match crate::runtime::configured_daemon_binary() {
        Ok(path) => json!({"available": true, "path": path.display().to_string()}),
        Err(error) => json!({"available": false, "error": error.to_string()}),
    };
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn doctor_report_never_exposes_secret_values() {
        let report = super::doctor_report().await.to_string();
        assert!(!report.contains("WINX_HTTP_TOKEN="));
        assert!(report.contains("http_token_configured"));
        assert!(report.contains("daemon_topology"));
    }
}
