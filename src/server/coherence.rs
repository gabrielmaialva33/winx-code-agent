use std::path::PathBuf;

use rmcp::model::CallToolRequestParams;

use super::principal::{canonical_workspace_identity, expected_affinity_thread_id, RequestScope};
use super::{SessionIsolation, WinxService};
use crate::config::HttpSessionAffinity;
use crate::errors::{Result, WinxError};
use crate::tool_registry::ToolKind;

/// Result recorded in usage telemetry after the request's project identity has
/// been checked. It contains no user path or transport identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum WorkspaceCoherence {
    NotRequired,
    FirstCall,
    Validated,
}

impl WorkspaceCoherence {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::NotRequired => "not_required",
            Self::FirstCall => "first_call",
            Self::Validated => "validated",
        }
    }
}

impl WinxService {
    /// Fail closed before a remote tool can select a shell or touch a file.
    ///
    /// `workspace_root` is deliberately not compared with target file paths.
    /// It is the project identity paired with `thread_id`; filesystem reach is
    /// still governed independently by `WINX_ALLOW_PATHS` and mode policy.
    pub(super) async fn validate_workspace_coherence(
        &self,
        request: &CallToolRequestParams,
        scope: &RequestScope,
        affinity: HttpSessionAffinity,
        conversation_identity: Option<&str>,
    ) -> Result<WorkspaceCoherence> {
        if self.isolation != SessionIsolation::Strict {
            return Ok(WorkspaceCoherence::NotRequired);
        }

        let arguments = request.arguments.as_ref();
        let internal_thread_id = string_argument(arguments, "thread_id").unwrap_or_default();
        let external_thread_id = scope.unscope_text(&internal_thread_id);

        if ToolKind::parse(request.name.as_ref()) == Some(ToolKind::Initialize) {
            return self
                .validate_remote_initialize(arguments, &internal_thread_id, &external_thread_id)
                .await;
        }

        let workspace_root = string_argument(arguments, "workspace_root").ok_or_else(|| {
            WinxError::WorkspaceBindingRequired { thread_id: external_thread_id.clone() }
        })?;
        if external_thread_id.is_empty() {
            return Err(WinxError::WorkspaceBindingRequired { thread_id: String::new() });
        }
        let requested_workspace = canonical_binding_root(&workspace_root)?;

        if let Some(expected_thread_id) =
            expected_affinity_thread_id(&requested_workspace, affinity, conversation_identity)
        {
            if external_thread_id != expected_thread_id {
                return Err(WinxError::WorkspaceThreadMismatch {
                    thread_id: external_thread_id,
                    workspace_root: requested_workspace,
                });
            }
        }

        let bound_workspace =
            if let Some(workspace) = self.bound_workspace(&internal_thread_id).await {
                workspace
            } else {
                // After an adapter restart the registry is empty even though the
                // session's snapshot (and possibly its guardian PTY) survived.
                // Rehydrate before rejecting so a remote tool call carrying a
                // known thread_id/workspace pair keeps working across restarts.
                drop(self.tool_session_for(&internal_thread_id).await);
                self.bound_workspace(&internal_thread_id)
                    .await
                    .ok_or(WinxError::BashStateNotInitialized)?
            };
        let bound_workspace = canonical_workspace_identity(&bound_workspace.to_string_lossy());
        if requested_workspace != bound_workspace {
            return Err(WinxError::WorkspaceBindingMismatch {
                thread_id: external_thread_id,
                requested_workspace,
                bound_workspace,
            });
        }

        Ok(WorkspaceCoherence::Validated)
    }

    async fn validate_remote_initialize(
        &self,
        arguments: Option<&rmcp::model::JsonObject>,
        internal_thread_id: &str,
        external_thread_id: &str,
    ) -> Result<WorkspaceCoherence> {
        let kind = string_argument(arguments, "type").unwrap_or_else(|| "first_call".to_string());
        let workspace = string_argument(arguments, "any_workspace_path").unwrap_or_default();

        if kind == "user_asked_change_workspace" {
            let workspace_root = if workspace.is_empty() {
                PathBuf::new()
            } else {
                canonical_workspace_identity(&workspace)
            };
            return Err(WinxError::WorkspaceChangeRequiresNewSession { workspace_root });
        }

        // An empty first-call path intentionally creates a per-thread scratch
        // playground. There is no canonical input root to compare before it is
        // created; Initialize returns the concrete binding for later calls.
        if kind == "first_call" && workspace.trim().is_empty() {
            return Ok(WorkspaceCoherence::FirstCall);
        }

        let requested_workspace = canonical_initialize_workspace(&workspace)?;
        let mut recovering_missing_session = false;
        if let Some(bound_workspace) = self.bound_workspace(internal_thread_id).await {
            let bound_workspace = canonical_workspace_identity(&bound_workspace.to_string_lossy());
            if requested_workspace != bound_workspace {
                return Err(WinxError::WorkspaceBindingMismatch {
                    thread_id: external_thread_id.to_string(),
                    requested_workspace,
                    bound_workspace,
                });
            }
        } else if kind != "first_call" {
            if matches!(kind.as_str(), "reset_shell" | "user_asked_mode_change") {
                recovering_missing_session = true;
            } else {
                return Err(WinxError::BashStateNotInitialized);
            }
        }

        Ok(if kind == "first_call" || recovering_missing_session {
            WorkspaceCoherence::FirstCall
        } else {
            WorkspaceCoherence::Validated
        })
    }
}

fn canonical_binding_root(workspace: &str) -> Result<PathBuf> {
    let supplied = absolute_workspace_path(workspace, "workspace_root")?;
    if supplied.is_file() {
        return Err(WinxError::ParameterValidationError {
            field: "workspace_root".to_string(),
            message: "must be the workspace directory returned by Initialize, not a file path"
                .to_string(),
        });
    }
    let path = canonical_workspace_identity(workspace);
    Ok(path)
}

fn canonical_initialize_workspace(workspace: &str) -> Result<PathBuf> {
    let _ = absolute_workspace_path(workspace, "any_workspace_path")?;
    // Initialize intentionally accepts either a directory or a file hint; a
    // file resolves to its parent exactly as the tool implementation does.
    Ok(canonical_workspace_identity(workspace))
}

fn absolute_workspace_path(workspace: &str, field: &str) -> Result<PathBuf> {
    if workspace.trim().is_empty() {
        return Err(WinxError::ParameterValidationError {
            field: field.to_string(),
            message: "must be a non-empty absolute workspace path".to_string(),
        });
    }
    let expanded = crate::utils::path::expand_user(workspace);
    let supplied = PathBuf::from(expanded);
    if !supplied.is_absolute() {
        return Err(WinxError::ParameterValidationError {
            field: field.to_string(),
            message: "must be an absolute path for a remote session".to_string(),
        });
    }
    Ok(supplied)
}

fn string_argument(arguments: Option<&rmcp::model::JsonObject>, key: &str) -> Option<String> {
    arguments?
        .get(key)?
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;
    use std::path::Path;

    use crate::state::BashState;
    use serde_json::Value;

    fn request(name: &str, value: &Value) -> CallToolRequestParams {
        CallToolRequestParams::new(name.to_string())
            .with_arguments(value.as_object().expect("request object").clone())
    }

    async fn bind(service: &WinxService, thread_id: &str, workspace: &Path) {
        let (slot, guard) = service.session_for(thread_id).await;
        let mut state = BashState::new();
        state.current_thread_id = thread_id.to_string();
        state.workspace_root = workspace.to_path_buf();
        state.cwd = workspace.to_path_buf();
        state.initialized = true;
        *slot.lock().await = Some(state);
        drop(guard);
    }

    #[tokio::test]
    async fn strict_remote_binding_allows_targets_outside_the_workspace() {
        let workspace = tempfile::tempdir().expect("workspace");
        let external = tempfile::tempdir().expect("external target");
        let service = WinxService::with_isolation(SessionIsolation::Strict);
        bind(&service, "thread", workspace.path()).await;
        let request = request(
            "ReadFiles",
            &serde_json::json!({
                "file_paths": [external.path().join("outside.txt")],
                "thread_id": "thread",
                "workspace_root": workspace.path()
            }),
        );

        let result = service
            .validate_workspace_coherence(
                &request,
                &RequestScope::default(),
                HttpSessionAffinity::Thread,
                None,
            )
            .await;
        assert_eq!(result.expect("coherent binding"), WorkspaceCoherence::Validated);
    }

    #[tokio::test]
    async fn strict_remote_binding_rejects_another_projects_thread() {
        let intended = tempfile::tempdir().expect("intended workspace");
        let wrong = tempfile::tempdir().expect("wrong workspace");
        let service = WinxService::with_isolation(SessionIsolation::Strict);
        bind(&service, "wrong_thread", wrong.path()).await;
        let request = request(
            "BashCommand",
            &serde_json::json!({
                "action_json": {"type": "command", "command": "pwd"},
                "thread_id": "wrong_thread",
                "workspace_root": intended.path()
            }),
        );

        let error = service
            .validate_workspace_coherence(
                &request,
                &RequestScope::default(),
                HttpSessionAffinity::Thread,
                None,
            )
            .await
            .expect_err("cross-project pair must fail");
        assert!(matches!(error, WinxError::WorkspaceBindingMismatch { .. }));
    }

    #[tokio::test]
    async fn strict_remote_binding_requires_workspace_root() {
        let workspace = tempfile::tempdir().expect("workspace");
        let service = WinxService::with_isolation(SessionIsolation::Strict);
        bind(&service, "thread", workspace.path()).await;
        let request = request(
            "ReadFiles",
            &serde_json::json!({"file_paths": [workspace.path()], "thread_id": "thread"}),
        );

        let error = service
            .validate_workspace_coherence(
                &request,
                &RequestScope::default(),
                HttpSessionAffinity::Thread,
                None,
            )
            .await
            .expect_err("missing binding must fail closed");
        assert!(matches!(error, WinxError::WorkspaceBindingRequired { .. }));
    }

    #[tokio::test]
    async fn strict_remote_initialize_accepts_a_file_as_the_workspace_hint() {
        let workspace = tempfile::tempdir().expect("workspace");
        let file = workspace.path().join("request.md");
        std::fs::write(&file, "request").expect("workspace file");
        let service = WinxService::with_isolation(SessionIsolation::Strict);
        let request = request(
            "Initialize",
            &serde_json::json!({
                "type": "first_call",
                "any_workspace_path": file,
                "mode_name": "wcgw",
                "thread_id": "thread"
            }),
        );

        let result = service
            .validate_workspace_coherence(
                &request,
                &RequestScope::default(),
                HttpSessionAffinity::Thread,
                None,
            )
            .await;
        assert_eq!(result.expect("file hint"), WorkspaceCoherence::FirstCall);
    }

    #[tokio::test]
    async fn strict_remote_initialize_recovers_safe_transitions_after_state_loss() {
        let workspace = tempfile::tempdir().expect("workspace");
        let service = WinxService::with_isolation(SessionIsolation::Strict);

        for (kind, thread_id) in
            [("reset_shell", "reset-thread"), ("user_asked_mode_change", "mode-thread")]
        {
            let request = request(
                "Initialize",
                &serde_json::json!({
                    "type": kind,
                    "any_workspace_path": workspace.path(),
                    "mode_name": "wcgw",
                    "thread_id": thread_id
                }),
            );
            let result = service
                .validate_workspace_coherence(
                    &request,
                    &RequestScope::default(),
                    HttpSessionAffinity::Thread,
                    None,
                )
                .await;
            assert_eq!(result.expect("missing session recovery"), WorkspaceCoherence::FirstCall);
        }
    }

    #[tokio::test]
    async fn strict_remote_initialize_keeps_workspace_change_fail_closed_after_state_loss() {
        let workspace = tempfile::tempdir().expect("workspace");
        let service = WinxService::with_isolation(SessionIsolation::Strict);
        let request = request(
            "Initialize",
            &serde_json::json!({
                "type": "user_asked_change_workspace",
                "any_workspace_path": workspace.path(),
                "mode_name": "wcgw",
                "thread_id": "workspace-change-thread"
            }),
        );

        let error = service
            .validate_workspace_coherence(
                &request,
                &RequestScope::default(),
                HttpSessionAffinity::Thread,
                None,
            )
            .await
            .expect_err("workspace changes must still require a separate session");
        assert!(matches!(error, WinxError::WorkspaceChangeRequiresNewSession { .. }));
    }
}
