use std::fmt::Write as _;
use std::hint::black_box;
use std::sync::Arc;
use std::time::Duration;

use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput};
use tokio::sync::Mutex;

use winx_code_agent::state::bash_state::BashState;
use winx_code_agent::types::ReadFiles;

fn or_abort<T, E>(result: Result<T, E>) -> T {
    match result {
        Ok(value) => value,
        Err(_) => std::process::abort(),
    }
}

fn generated_lines(count: usize, mut append: impl FnMut(&mut String, usize)) -> String {
    (0..count).fold(String::new(), |mut output, index| {
        append(&mut output, index);
        output
    })
}

fn benchmark_thread_ids(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("thread_id_normalization");
    for length in [16_usize, 96, 512, 4_096] {
        let input = format!("client-session-{}", "á_".repeat(length));
        group.throughput(Throughput::Bytes(u64::try_from(input.len()).unwrap_or(u64::MAX)));
        group.bench_with_input(BenchmarkId::from_parameter(length), &input, |bencher, input| {
            bencher.iter(|| winx_code_agent::types::normalize_thread_id(black_box(input)));
        });
    }
    group.finish();
}

fn benchmark_terminal_rendering(criterion: &mut Criterion) {
    let clean = generated_lines(1_000, |output, index| {
        let _ = writeln!(output, "line {index}");
    });
    let ansi = generated_lines(1_000, |output, index| {
        let _ = writeln!(output, "\x1b[2K\r\x1b[32mprogress {index}\x1b[0m");
    });

    let mut group = criterion.benchmark_group("terminal_rendering");
    for (name, input) in [("clean", clean), ("ansi_redraw", ansi)] {
        group.throughput(Throughput::Bytes(u64::try_from(input.len()).unwrap_or(u64::MAX)));
        group.bench_with_input(BenchmarkId::from_parameter(name), &input, |bencher, input| {
            bencher.iter(|| {
                winx_code_agent::state::terminal::render_terminal_output(black_box(input))
            });
        });
    }
    group.finish();
}

fn benchmark_terminal_rendering_cold(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("terminal_rendering_cold");
    for (name, input) in [
        ("short", "build complete\n".to_string()),
        ("wrapped_ascii", "Q".repeat(2_400)),
        ("colored_unicode", format!("\x1b[32m{}\x1b[0m", "界 e\u{301} ".repeat(400))),
        ("cursor_scroll", "\x1b[999B\nFINAL".to_string()),
        ("over_limit", "Q".repeat(100_200)),
    ] {
        group.throughput(Throughput::Bytes(u64::try_from(input.len()).unwrap_or(u64::MAX)));
        let mut nonce = 0_u64;
        group.bench_with_input(BenchmarkId::from_parameter(name), &input, |bencher, input| {
            bencher.iter_batched(
                || {
                    nonce = nonce.wrapping_add(1);
                    // Invisible, fixed-width title changes prevent cache hits.
                    format!("\x1b]0;{nonce:016x}\x07{input}")
                },
                |input| winx_code_agent::state::terminal::render_terminal_output(black_box(&input)),
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

fn benchmark_redaction(criterion: &mut Criterion) {
    let clean = "ordinary build output without credentials\n".repeat(5_000);
    let secrets = generated_lines(1_000, |output, index| {
        let _ = writeln!(
            output,
            "request {index}: Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJ1c2VyIn0.signature"
        );
    });

    let mut group = criterion.benchmark_group("redaction");
    for (name, input) in [("clean", clean), ("credential_heavy", secrets)] {
        group.throughput(Throughput::Bytes(u64::try_from(input.len()).unwrap_or(u64::MAX)));
        group.bench_with_input(BenchmarkId::from_parameter(name), &input, |bencher, input| {
            bencher.iter(|| winx_code_agent::utils::redact::redact(black_box(input)));
        });
    }
    group.finish();
}

fn benchmark_output_compression(criterion: &mut Criterion) {
    let repeated = "compiling dependency\n".repeat(20_000);
    let unique = generated_lines(20_000, |output, index| {
        let _ = writeln!(output, "event {index}");
    });

    let mut group = criterion.benchmark_group("output_compression");
    for (name, input) in [("repeated", repeated), ("unique", unique)] {
        group.throughput(Throughput::Bytes(u64::try_from(input.len()).unwrap_or(u64::MAX)));
        group.bench_with_input(BenchmarkId::from_parameter(name), &input, |bencher, input| {
            bencher.iter(|| {
                winx_code_agent::utils::output_compress::compress_output(black_box(input))
            });
        });
    }
    group.finish();
}

fn benchmark_read_files_batch(criterion: &mut Criterion) {
    let workspace = or_abort(tempfile::tempdir());
    let root = or_abort(workspace.path().canonicalize());
    let payload = generated_lines(4_000, |output, index| {
        let _ = writeln!(
            output,
            "pub fn generated_{index}() -> usize {{ {index} }} // deterministic benchmark payload"
        );
    });
    let mut file_paths = Vec::new();
    for index in 0..8 {
        let path = root.join(format!("batch-{index}.rs"));
        or_abort(std::fs::write(&path, &payload));
        file_paths.push(path.to_string_lossy().into_owned());
    }

    // Keep benchmark-only stats out of the user's persistent XDG directory and
    // avoid per-file truncation so the whole ordered batch is measured.
    std::env::set_var("XDG_DATA_HOME", root.join("xdg"));
    std::env::set_var("WINX_CODING_TOKEN_BUDGET", "1000000");

    let mut bash_state = BashState::new();
    bash_state.cwd.clone_from(&root);
    bash_state.workspace_root.clone_from(&root);
    let bash_state = Arc::new(Mutex::new(Some(bash_state)));
    let request = ReadFiles {
        force: false,
        start_line_nums: vec![None; file_paths.len()],
        end_line_nums: vec![None; file_paths.len()],
        file_paths,
        thread_id: "benchmark".to_string(),
    };
    let runtime = or_abort(
        tokio::runtime::Builder::new_multi_thread().worker_threads(4).enable_all().build(),
    );

    let mut group = criterion.benchmark_group("read_files_batch");
    group.sample_size(20);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(3));
    group.throughput(Throughput::Elements(8));
    for workers in [1_usize, 4] {
        std::env::set_var("WINX_READ_PARALLELISM", workers.to_string());
        group.bench_with_input(BenchmarkId::new("workers", workers), &workers, |bencher, _| {
            bencher.iter(|| {
                let outcome = or_abort(runtime.block_on(
                    winx_code_agent::tools::read_files::handle_tool_call_detailed(
                        &bash_state,
                        request.clone(),
                    ),
                ));
                black_box((outcome.successful_files, outcome.text.len()))
            });
        });
    }
    group.finish();
}

fn history_capture(
    id: &str,
    output: &str,
    now: u64,
    exited: bool,
) -> winx_code_agent::state::execution_history::ExecutionCapture {
    use winx_code_agent::state::execution_history::{CapturedOutput, ExecutionCapture};
    use winx_code_agent::tools::bash_command::{BashCommandState, BashProcessStatus};
    ExecutionCapture {
        execution_id: id.to_string(),
        execution_token: Some(winx_code_agent::runtime::ShellExecutionToken {
            guardian_epoch: "guardian".to_string(),
            session_epoch: "session".to_string(),
            generation: 1,
        }),
        command_generation: Some(1),
        state: BashCommandState {
            process_status: if exited {
                BashProcessStatus::Exited
            } else {
                BashProcessStatus::Running
            },
            background_id: None,
            running_for_seconds: None,
            exit_code: exited.then_some(0),
            cwd: std::path::PathBuf::from("/workspace"),
            turn_state: None,
        },
        output: CapturedOutput::Snapshot(output.to_string()),
        screen: None,
        explanation: None,
        prompt_receipt: None,
        activity_seq: Some(now),
        observed_at_unix_ms: now,
        label: Some("cargo test --all-features --locked".to_string()),
        output_truncated: false,
        dropped_output_file: None,
    }
}

/// One `BashCommand` performs three history transactions: the pre-launch
/// reconciliation scan, the launch record, and the post-inspect update. The
/// catalog is pre-filled to its 128-record cap like a long-lived session.
fn benchmark_execution_history(criterion: &mut Criterion) {
    use winx_code_agent::state::execution_history::{
        ExecutionCatalog, ExecutionScope, HistoryLimits,
    };
    // CARGO_TARGET_TMPDIR is on the real filesystem, unlike a tmpfs /tmp.
    let workspace = or_abort(tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")));
    let catalog = ExecutionCatalog::new(workspace.path().join("history"), HistoryLimits::default());
    let scope = ExecutionScope {
        thread_id: "benchmark".to_string(),
        workspace_root: workspace.path().to_path_buf(),
    };
    let output = "test result: ok. 42 passed; 0 failed\n".repeat(64);
    for index in 0..128_u64 {
        or_abort(catalog.record(
            &scope,
            history_capture(&format!("seed-{index}"), &output, 1_000 + index, true),
        ));
    }

    let mut group = criterion.benchmark_group("execution_history");
    group.sample_size(30);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(3));
    group.bench_function("list_full_index", |bencher| {
        bencher.iter(|| black_box(or_abort(catalog.list(&scope, 10_000)).executions.len()));
    });
    group.bench_function("pending_foreground_scan", |bencher| {
        bencher.iter(|| black_box(or_abort(catalog.pending_foreground(&scope, 10_000)).len()));
    });
    let mut sequence = 0_u64;
    group.bench_function("bash_command_cycle", |bencher| {
        bencher.iter(|| {
            sequence += 1;
            let now = 20_000 + sequence * 10;
            let id = format!("run-{sequence}");
            black_box(or_abort(catalog.pending_foreground(&scope, now)).len());
            or_abort(catalog.record(&scope, history_capture(&id, &output, now, false)));
            black_box(or_abort(
                catalog.update_existing(&scope, history_capture(&id, &output, now + 1, true)),
            ));
        });
    });
    group.finish();
}

fn benchmark_tool_catalog(criterion: &mut Criterion) {
    use rmcp::ServerHandler as _;
    let service = winx_code_agent::WinxService::new();
    let mut group = criterion.benchmark_group("tool_catalog");
    group.bench_function("build_uncached", |bencher| {
        bencher.iter(|| black_box(winx_code_agent::server::build_tool_catalog_uncached().len()));
    });
    group.bench_function("get_tool_lookup", |bencher| {
        bencher.iter(|| black_box(service.get_tool(black_box("BashCommand")).is_some()));
    });
    group.finish();
}

criterion_group!(
    benches,
    benchmark_thread_ids,
    benchmark_terminal_rendering,
    benchmark_terminal_rendering_cold,
    benchmark_redaction,
    benchmark_output_compression,
    benchmark_read_files_batch,
    benchmark_execution_history,
    benchmark_tool_catalog
);
criterion_main!(benches);
