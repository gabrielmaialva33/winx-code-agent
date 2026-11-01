#![allow(clippy::unwrap_used)]
#![cfg(target_os = "linux")]
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::time::Duration;

use super::runtime::Service;
use super::tests::fixture;

fn parser(dir: &std::path::Path, extra: &str) -> PathBuf {
    let path = dir.join("fake-parser");
    let script = format!(
        r#"#!/bin/sh
if [ "$1" = "--version" ]; then printf 'eulix_parser fixture\n'; exit 0; fi
out=''
while [ "$#" -gt 0 ]; do
 if [ "$1" = '--output' ]; then shift; out="$1"; fi
 shift
done
[ -z "$WINX_HTTP_TOKEN$WINX_USAGE_LOG" ] || exit 91
{extra}
cat > "$out" <<'KB'
{}
KB
"#,
        fixture()
    );
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    path
}

async fn settled(
    service: &std::sync::Arc<Service>,
    root: &std::path::Path,
) -> super::runtime::View {
    for _ in 0..200 {
        let view = service.request(root).await;
        if !service.checking(root).await {
            return view;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let view = service.request(root).await;
    assert_ne!(view.status, "building", "background build did not settle");
    view
}

#[tokio::test]
async fn unavailable_and_fake_parser_build_fresh_stale_and_coalesced() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap().join("workspace");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("a.rs"), "fn root() {}\n").unwrap();
    let absent = Service::new(None, Duration::from_secs(2), Duration::from_millis(20));
    assert_eq!(settled(&absent, &root).await.status, "unavailable");
    let executable = parser(temp.path(), "sleep 0.1");
    let service = Service::new(Some(executable), Duration::from_secs(2), Duration::from_millis(20));
    let first = service.request(&root).await;
    assert_eq!(first.status, "building");
    for _ in 0..8 {
        assert_eq!(service.request(&root).await.status, "building");
    }
    let view = settled(&service, &root).await;
    assert_eq!(view.status, "fresh");
    assert_eq!(view.index.unwrap().edge_count(), 3);
    std::fs::write(root.join("a.rs"), "fn leaf() {}\n").unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    let view = service.request(&root).await;
    assert!(view.index.is_some());
    let view = settled(&service, &root).await;
    assert_eq!(view.status, "fresh");
    assert_eq!(service.build_count(), 2);
}

#[tokio::test]
async fn parser_failure_timeout_corrupt_and_oversize_indexes_are_structured() {
    for (extra, code) in [("exit 7", "parser_failed"), ("sleep 2", "parser_timeout")] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap().join("workspace");
        std::fs::create_dir(&root).unwrap();
        let service = Service::new(
            Some(parser(temp.path(), extra)),
            Duration::from_millis(80),
            Duration::from_secs(1),
        );
        let view = settled(&service, &root).await;
        assert_eq!(view.status, "unavailable");
        assert_eq!(view.error_code.as_deref(), Some(code));
    }
    for oversized in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path().canonicalize().unwrap().join(".winx/index/eulix");
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join("index.wcg");
        std::fs::write(&path, "corrupt").unwrap();
        if oversized {
            std::fs::OpenOptions::new()
                .write(true)
                .open(path)
                .unwrap()
                .set_len(129 * 1024 * 1024)
                .unwrap();
        }
        let service = Service::new(None, Duration::from_secs(1), Duration::from_secs(1));
        let view = settled(&service, &temp.path().canonicalize().unwrap()).await;
        assert_eq!(view.status, "unavailable");
        assert_eq!(
            view.error_code.as_deref(),
            Some(if oversized { "index_oversize" } else { "index_corrupt" })
        );
    }
}

#[tokio::test]
async fn storage_and_parser_workspace_symlinks_are_refused() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap().join("workspace");
    let outside = temp.path().canonicalize().unwrap().join("outside");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, root.join(".winx")).unwrap();
    let service =
        Service::new(Some(parser(temp.path(), "")), Duration::from_secs(1), Duration::from_secs(1));
    assert_eq!(settled(&service, &root).await.error_code.as_deref(), Some("index_storage_unsafe"));
    assert_eq!(std::fs::read_dir(outside).unwrap().count(), 0);
}

#[tokio::test]
async fn failed_refresh_keeps_previous_generation_and_disk_cache_reloads() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap().join("workspace");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("a.rs"), "fn root() {}\n").unwrap();
    let executable = parser(temp.path(), "");
    let service =
        Service::new(Some(executable.clone()), Duration::from_secs(1), Duration::from_millis(10));
    let initial = settled(&service, &root).await;
    assert_eq!(initial.status, "fresh");
    let bytes = std::fs::read(root.join(".winx/index/eulix/index.wcg")).unwrap();
    let reloaded =
        Service::new(Some(executable.clone()), Duration::from_secs(1), Duration::from_millis(10));
    assert_eq!(settled(&reloaded, &root).await.status, "fresh");
    parser(temp.path(), "exit 7");
    std::fs::write(root.join("a.rs"), "fn longer_name() {}\n").unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    let stale = settled(&service, &root).await;
    assert_eq!(stale.status, "stale");
    assert_eq!(stale.error_code.as_deref(), Some("parser_failed"));
    assert!(std::sync::Arc::ptr_eq(initial.index.as_ref().unwrap(), stale.index.as_ref().unwrap()));
    assert_eq!(std::fs::read(root.join(".winx/index/eulix/index.wcg")).unwrap(), bytes);
}

#[tokio::test]
async fn configured_parser_repairs_corrupt_cache_and_refuses_oversize_kb() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap().join("workspace");
    std::fs::create_dir(&root).unwrap();
    let executable = parser(temp.path(), "");
    let directory = root.join(".winx/index/eulix");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("index.wcg"), "corrupt").unwrap();
    let service = Service::new(Some(executable), Duration::from_secs(1), Duration::from_millis(10));
    assert_eq!(settled(&service, &root).await.status, "fresh");
    let huge = parser(temp.path(), "truncate -s 536870913 \"$out\"; exit 0");
    let service = Service::new(Some(huge), Duration::from_secs(1), Duration::from_millis(10));
    let result = settled(&service, &root).await;
    assert_eq!(result.status, "stale");
    assert_eq!(result.error_code.as_deref(), Some("index_oversize"));
}
