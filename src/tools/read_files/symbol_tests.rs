#![allow(clippy::unwrap_used)]
use super::*;

fn setup(
    source: &str,
    extension: &str,
) -> (tempfile::TempDir, PathBuf, Arc<Mutex<Option<BashState>>>) {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join(format!("source.{extension}"));
    std::fs::write(&path, source).unwrap();
    let mut state = BashState::new();
    state.cwd = temp.path().to_owned();
    state.workspace_root = temp.path().to_owned();
    state.current_thread_id = "symbol-thread".into();
    state.initialized = true;
    (temp, path, Arc::new(Mutex::new(Some(state))))
}
fn args(paths: Vec<String>) -> ReadFiles {
    let paths = serde_json::to_value(paths).unwrap();
    serde_json::from_value(serde_json::json!({"file_paths":paths,"thread_id":"symbol-thread"}))
        .unwrap()
}
async fn read(state: &Arc<Mutex<Option<BashState>>>, paths: Vec<String>) -> ReadFilesOutcome {
    handle_tool_call_with_budget(
        state,
        args(paths),
        ReadResponseBudget::parse(None, None, Some("900")),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn symbol_read_returns_only_attached_definition_and_exact_edit_coverage() {
    let (_temp, path, state) = setup("fn unrelated() {}\n/// Target docs.\n#[inline]\nfn target() {\n    let value = 1;\n}\nfn private_sibling() {}\n", "rs");
    let outcome = read(&state, vec![format!("{}#target", path.display())]).await;
    assert_eq!(outcome.successful_files, 1, "{}", outcome.text);
    assert!(outcome.errors.is_empty());
    assert!(outcome.text.contains("2 /// Target docs."));
    assert!(!outcome.text.contains("private_sibling"));
    assert!(!outcome.text.contains("fn unrelated"));
    let receipt = &outcome.files[0];
    assert_eq!(receipt.visible_ranges.len(), 1);
    assert_eq!((receipt.visible_ranges[0].start_line, receipt.visible_ranges[0].end_line), (2, 6));
    let guard = state.lock().await;
    let coverage = &guard.as_ref().unwrap().whitelist_for_overwrite[&receipt.path];
    assert!(coverage.covers_range(2, 6));
    assert!(!coverage.covers_range(1, 7));
    assert_eq!(receipt.revision, revision_from_hash(&coverage.file_hash));
}

#[tokio::test]
async fn duplicate_definitions_do_not_grant_coverage_between_them() {
    let (_temp, path, state) = setup(
        "impl A {\n fn target(&self) {}\n}\nfn unseen() {}\nimpl B {\n fn target(&self) {}\n}\n",
        "rs",
    );
    let outcome = read(&state, vec![format!("{}#target", path.display())]).await;
    assert_eq!(outcome.files.len(), 2, "{}", outcome.text);
    assert_eq!(outcome.files[0].revision, outcome.files[1].revision);
    let guard = state.lock().await;
    let coverage = &guard.as_ref().unwrap().whitelist_for_overwrite[&outcome.files[0].path];
    assert!(coverage.covers_range(2, 2) && coverage.covers_range(6, 6));
    assert!(!coverage.covers_range(2, 6));
}

#[tokio::test]
async fn literal_hash_filenames_win_and_numeric_ranges_keep_their_meaning() {
    let (temp, path, state) = setup("fn target() {}\n", "rs");
    let literal = PathBuf::from(format!("{}#target", path.display()));
    std::fs::write(&literal, "literal first\nliteral second\n").unwrap();
    let outcome = read(&state, vec![literal.to_string_lossy().into_owned()]).await;
    assert_eq!(outcome.successful_files, 1);
    assert!(outcome.text.contains("literal first"));
    assert!(outcome.files[0].path.ends_with("#target"));
    let outcome = read(&state, vec![format!("{}:2-2", literal.display())]).await;
    assert!(outcome.text.contains("2 literal second"));
    assert!(!outcome.text.contains("1 literal first"));
    let hash_source = temp.path().join("part#one.py");
    std::fs::write(&hash_source, "def target():\n    return 1\n\ndef other(): pass\n").unwrap();
    let outcome = read(&state, vec![format!("{}#target", hash_source.display())]).await;
    assert_eq!(outcome.successful_files, 1, "{}", outcome.text);
    assert!(!outcome.text.contains("other"));
}

#[tokio::test]
async fn failed_resolution_never_exposes_source_or_records_coverage() {
    for (extension, source, symbol, reason) in [
        ("txt", "PRIVATE SOURCE CONTENT", "target", "symbol_language_unsupported"),
        ("rs", "fn known() {} // PRIVATE SOURCE CONTENT", "missing", "symbol_not_found"),
        ("rs", "fn broken( // PRIVATE SOURCE CONTENT", "broken", "symbol_parse_failed"),
        ("rs", "fn target() {} // PRIVATE SOURCE CONTENT", "target:1-2", "symbol_invalid_selector"),
    ] {
        let (_temp, path, state) = setup(source, extension);
        let outcome = read(&state, vec![format!("{}#{symbol}", path.display())]).await;
        assert_eq!(outcome.successful_files, 0);
        assert!(outcome.files.is_empty());
        assert!(outcome.text.contains(reason), "{}", outcome.text);
        assert!(!outcome.text.contains("PRIVATE SOURCE CONTENT"));
        assert!(state.lock().await.as_ref().unwrap().whitelist_for_overwrite.is_empty());
    }
}

#[tokio::test]
async fn symbol_reads_keep_byte_limits_continuations_and_unchanged_compaction() {
    let source = format!(
        "fn target() {{\n{}}}\nfn unseen() {{}}\n",
        "    let value = 123456789;\n".repeat(400)
    );
    let (_temp, path, state) = setup(&source, "rs");
    let request = format!("{}#target", path.display());
    let outcome = handle_tool_call_with_budget(
        &state,
        args(vec![request.clone()]),
        ReadResponseBudget::parse(Some("4096"), Some("2048"), Some("900")),
    )
    .await
    .unwrap();
    assert_eq!(outcome.files.len(), 1, "{}", outcome.text);
    let receipt = &outcome.files[0];
    assert!(receipt.truncated);
    assert!(outcome.budget.returned_bytes <= 2048);
    assert!(receipt.next_end_line.is_some_and(|end| end <= 402));
    assert!(!outcome.text.contains("fn unseen"));
    let full = read(&state, vec![request.clone()]).await;
    assert!(!full.files[0].truncated);
    let again = read(&state, vec![request]).await;
    assert!(again.files[0].unchanged);
    assert_eq!(again.files[0].revision, full.files[0].revision);
}

#[tokio::test]
async fn call_budget_defers_remaining_symbol_ranges_without_expanding_coverage() {
    let source = (0..4).fold(String::new(), |mut out, i| {
        writeln!(
            &mut out,
            "impl P{i} {{\n fn target(&self) {{\n{} }}\n}}",
            "   let value = 123456789;\n".repeat(80)
        )
        .unwrap();
        out
    });
    let (_temp, path, state) = setup(&source, "rs");
    let outcome = handle_tool_call_with_budget(
        &state,
        args(vec![format!("{}#target", path.display())]),
        ReadResponseBudget::parse(Some("2048"), Some("4096"), Some("0")),
    )
    .await
    .unwrap();
    assert!(outcome.budget.returned_bytes <= 2048);
    assert!(!outcome.budget.deferred.is_empty(), "{}", outcome.text);
    assert!(outcome.budget.deferred.iter().all(|spec| !spec.contains('#') && spec.contains(':')));
    assert_eq!(outcome.selections.symbols, 1);
    assert_eq!(outcome.selections.unknown, 0);
    let guard = state.lock().await;
    let coverage = &guard.as_ref().unwrap().whitelist_for_overwrite[&outcome.files[0].path];
    assert!(!coverage.covers_range(1, source.lines().count()));
}

#[tokio::test]
async fn completely_deferred_symbol_request_remains_unknown() {
    let source = format!(
        "// {}\nfn target() {{\n{}\n}}\n",
        "x".repeat(3500),
        " let value = 123456789;\n".repeat(120)
    );
    let (_temp, path, state) = setup(&source, "rs");
    let outcome = handle_tool_call_with_budget(
        &state,
        args(vec![format!("{}:1-1", path.display()), format!("{}#target", path.display())]),
        ReadResponseBudget::parse(Some("4096"), Some("4096"), Some("0")),
    )
    .await
    .unwrap();
    assert_eq!(outcome.files.len(), 1);
    assert_eq!(outcome.selections.bounded_ranges, 1);
    // The deferred spec names the canonical path, which differs from the temp
    // path when the temp root is reached through an alias (e.g. /proc fds).
    let expected_tail = format!("source.rs:2-{}", source.lines().count());
    assert_eq!(outcome.budget.deferred.len(), 1, "{:?}", outcome.budget.deferred);
    assert!(outcome.budget.deferred[0].ends_with(&expected_tail), "{:?}", outcome.budget.deferred);
    assert!(!outcome.budget.deferred[0].contains('#'));
    assert_eq!(outcome.selections.symbols, 0);
    assert_eq!(outcome.selections.unknown, 1);
}

#[tokio::test]
async fn binary_unsupported_and_invalid_utf8_sources_have_symbol_recovery() {
    for (ext, reason) in [("bin", "symbol_language_unsupported"), ("rs", "symbol_parse_failed")] {
        let (_temp, path, state) = setup("", ext);
        std::fs::write(&path, [0xff, 0xfe, 0]).unwrap();
        let result = read(&state, vec![format!("{}#target", path.display())]).await;
        assert!(result.text.contains(reason), "{}", result.text);
        assert!(matches!(result.errors.first(), Some(WinxError::SymbolReadRejected { .. })));
        assert!(result.files.is_empty());
    }
}

#[tokio::test]
async fn changed_source_re_resolves_positions_and_never_reuses_stale_delivery() {
    let source = "fn before() {}\nfn target() {}\n";
    let (_temp, path, state) = setup(source, "rs");
    let request = format!("{}#target", path.display());
    let first = read(&state, vec![request.clone()]).await;
    std::fs::write(&path, format!("// shifted\n{source}")).unwrap();
    let second = read(&state, vec![request]).await;
    assert_eq!(second.files[0].visible_ranges[0].start_line, 3);
    assert!(!second.files[0].unchanged);
    assert_ne!(first.files[0].revision, second.files[0].revision);
    let guard = state.lock().await;
    let coverage = &guard.as_ref().unwrap().whitelist_for_overwrite[&second.files[0].path];
    assert!(coverage.covers_range(3, 3));
    assert!(!coverage.covers_range(2, 2));
}
