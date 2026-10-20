// Real-PTY contract for the PowerShell shell family: the same completion
// marker, exit codes and cwd tracking as bash, driven through `pwsh`.
//
// Run with: cargo test --test powershell_pty_test -- --ignored --test-threads=1
// The test is a no-op when `pwsh` is not installed. It sets WINX_SHELL for
// the whole process, so it must stay the only test in this binary.
#![cfg(unix)]
#![allow(clippy::print_stderr)]

use std::path::Path;

use tempfile::TempDir;
use winx_code_agent::state::pty::{configured_shell_kind, PtyShell};
use winx_code_agent::utils::executable::{powershell_executable, ShellKind};

fn run(shell: &mut PtyShell, command: &str) -> anyhow::Result<(String, bool)> {
    shell.send_command(command)?;
    shell.read_output(15.0)
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires real PTY and pwsh; run in the PTY CI job"]
async fn powershell_shell_speaks_the_same_completion_contract() -> anyhow::Result<()> {
    if powershell_executable().is_none() {
        eprintln!("pwsh not installed; skipping");
        return Ok(());
    }
    std::env::set_var("WINX_SHELL", "pwsh");
    assert_eq!(configured_shell_kind(false), ShellKind::PowerShell);

    let temp_dir = TempDir::new()?;
    let mut shell = PtyShell::new(temp_dir.path(), false)?;

    // A cmdlet pipeline completes with exit code 0 and its output.
    let (output, complete) = run(&mut shell, "Write-Output 'hello from pwsh'")?;
    assert!(complete, "{output}");
    assert!(output.contains("hello from pwsh"), "{output}");
    assert_eq!(shell.last_exit_code, Some(0), "{output}");

    // A native process exit code is reported as-is.
    let (output, complete) = run(&mut shell, "sh -c 'exit 3'")?;
    assert!(complete, "{output}");
    assert_eq!(shell.last_exit_code, Some(3), "{output}");

    // The next successful cmdlet is not tainted by the stale native code.
    let (output, complete) = run(&mut shell, "Write-Output ok")?;
    assert!(complete, "{output}");
    assert_eq!(shell.last_exit_code, Some(0), "{output}");

    // A failing cmdlet reports 1.
    let (output, complete) =
        run(&mut shell, "Get-Item /definitely/not/here -ErrorAction SilentlyContinue")?;
    assert!(complete, "{output}");
    assert_eq!(shell.last_exit_code, Some(1), "{output}");

    // cd persists and shows up in the prompt marker as a native path.
    let nested = temp_dir.path().join("nested");
    std::fs::create_dir(&nested)?;
    let (output, complete) = run(&mut shell, &format!("Set-Location '{}'", nested.display()))?;
    assert!(complete, "{output}");
    let (output, complete) = run(&mut shell, "Get-Location | Select-Object -ExpandProperty Path")?;
    assert!(complete, "{output}");
    let canonical = nested.canonicalize()?;
    assert!(output.contains(canonical.to_str().unwrap_or("")), "{output}");
    assert!(
        output
            .lines()
            .any(|line| line.starts_with("◉ ") && line.contains(canonical.to_str().unwrap_or("?"))),
        "prompt marker must carry the cwd: {output}"
    );

    // Restricted mode never gets PowerShell.
    assert!(
        PtyShell::new(Path::new("/tmp"), true).is_err()
            || configured_shell_kind(true) != ShellKind::PowerShell
    );
    Ok(())
}
