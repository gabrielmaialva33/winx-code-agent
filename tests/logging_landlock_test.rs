#![cfg(target_os = "linux")]

use std::process::Command;

#[test]
fn usage_writer_starts_inside_landlock() -> anyhow::Result<()> {
    let Some(home) = home::home_dir() else {
        return Ok(());
    };
    let Ok(outside) = tempfile::Builder::new().prefix("winx-landlock-log-").tempdir_in(home) else {
        return Ok(());
    };
    if ["/tmp", "/var/tmp", "/dev"].iter().any(|root| outside.path().starts_with(root)) {
        return Ok(());
    }
    let workspace = tempfile::tempdir()?;
    let binary = env!("CARGO_BIN_EXE_winx-code-agent");

    let probe = Command::new(binary)
        .args(["--verbose", "doctor"])
        .current_dir(workspace.path())
        .env("WINX_SANDBOX", "1")
        .env_remove("WINX_USAGE_LOG")
        .env_remove("WINX_SANDBOX_RO_PATHS")
        .env_remove("WINX_SANDBOX_RW_PATHS")
        .output()?;
    let probe_stderr = String::from_utf8_lossy(&probe.stderr);
    if !probe_stderr.contains("FullyEnforced") && !probe_stderr.contains("PartiallyEnforced") {
        return Ok(());
    }
    assert!(probe.status.success(), "sandbox probe failed: {probe_stderr}");

    let usage_log = outside.path().join("usage.jsonl");
    let attempt = Command::new(binary)
        .arg("doctor")
        .current_dir(workspace.path())
        .env("WINX_SANDBOX", "1")
        .env("WINX_USAGE_LOG", &usage_log)
        .env("WINX_USAGE_LOG_ROTATION", "never")
        .env_remove("WINX_SANDBOX_RO_PATHS")
        .env_remove("WINX_SANDBOX_RW_PATHS")
        .output()?;
    let attempt_stderr = String::from_utf8_lossy(&attempt.stderr);
    assert!(!attempt.status.success(), "unexpected success: {attempt_stderr}");
    assert!(!usage_log.exists(), "unexpected log file: {}", usage_log.display());
    assert!(
        attempt_stderr.contains("cannot open WINX_USAGE_LOG"),
        "unexpected startup failure: {attempt_stderr}"
    );
    Ok(())
}
