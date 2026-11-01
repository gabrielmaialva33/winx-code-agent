//! Synthetic, reproducible whole-file versus selected-definition read measurements.
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;
use winx_code_agent::config::ReadResponseBudget;
use winx_code_agent::state::bash_state::BashState;
use winx_code_agent::tools::read_files::handle_tool_call_with_budget;
use winx_code_agent::types::ReadFiles;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().canonicalize()?;
    std::env::set_var("XDG_DATA_HOME", root.join("xdg"));
    let source = (0..1_000).fold(String::new(), |mut out, id| {
        let _ = writeln!(out, "pub fn symbol_{id}() -> usize {{ {id} }}");
        out
    });
    std::fs::write(root.join("source.rs"), &source)?;
    let mut state = BashState::new();
    state.cwd = root.clone();
    state.workspace_root = root;
    let state = Arc::new(Mutex::new(Some(state)));
    let budget = ReadResponseBudget::parse(Some("262144"), Some("262144"), Some("0"));
    let mut rows = Vec::new();
    for path in ["source.rs", "source.rs#symbol_500"] {
        let request: ReadFiles =
            serde_json::from_value(serde_json::json!({"file_paths":[path],"force":true}))?;
        let mut timings = Vec::new();
        let mut source_bytes = 0;
        let mut text_bytes = 0;
        for _ in 0..101 {
            let start = Instant::now();
            let result = handle_tool_call_with_budget(&state, request.clone(), budget).await?;
            if !result.errors.is_empty() || result.files.iter().any(|file| file.truncated) {
                return Err("fixture read failed or was truncated".into());
            }
            timings.push(start.elapsed().as_nanos());
            source_bytes = result.budget.returned_bytes;
            text_bytes = result.text.len();
        }
        let cold_ns = timings.remove(0);
        timings.sort_unstable();
        rows.push(serde_json::json!({"request":path,"cold_ns":cold_ns,"p50_ns":timings[50],"p95_ns":timings[95],"rendered_source_bytes":source_bytes,"text_bytes":text_bytes}));
    }
    serde_json::to_writer_pretty(
        std::io::stdout().lock(),
        &serde_json::json!({"source_bytes":source.len(),"source_lines":source.lines().count(),"iterations":100,"results":rows}),
    )?;
    Ok(())
}
