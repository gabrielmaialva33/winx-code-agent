use std::path::Path;
use std::process::Command;

fn git_output(manifest_dir: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git").arg("-C").arg(manifest_dir).args(args).output().ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn emit_build_identity(revision: &str, dirty: bool) {
    let package_version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "unknown".into());
    let identity = if revision == "package" {
        format!("{package_version}+package")
    } else {
        let dirty_suffix = if dirty { ".dirty" } else { "" };
        format!("{package_version}+g{revision}{dirty_suffix}")
    };
    println!("cargo:rustc-env=WINX_BUILD_REVISION={revision}");
    println!("cargo:rustc-env=WINX_BUILD_DIRTY={dirty}");
    println!("cargo:rustc-env=WINX_BUILD_IDENTITY={identity}");
}

fn main() {
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=Cargo.lock");
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/index");

    let Some(manifest_dir) = std::env::var_os("CARGO_MANIFEST_DIR").map(std::path::PathBuf::from)
    else {
        emit_build_identity("package", false);
        return;
    };
    let manifest_canonical = manifest_dir.canonicalize().ok();
    let git_root = git_output(&manifest_dir, &["rev-parse", "--show-toplevel"])
        .and_then(|root| std::path::PathBuf::from(root).canonicalize().ok());
    if manifest_canonical != git_root {
        emit_build_identity("package", false);
        return;
    }

    let revision = git_output(&manifest_dir, &["rev-parse", "--short=12", "HEAD"])
        .filter(|revision| !revision.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    // Untracked Rust/build files participate in the compiled artifact too, so
    // excluding them could advertise a falsely clean executable identity.
    let dirty = git_output(&manifest_dir, &["status", "--porcelain", "--untracked-files=normal"])
        .is_some_and(|status| !status.is_empty());
    emit_build_identity(&revision, dirty);
}
