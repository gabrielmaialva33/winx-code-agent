use std::fmt::Write as _;
use std::hint::black_box;
use std::sync::Arc;
use std::time::Duration;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
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

criterion_group!(
    benches,
    benchmark_thread_ids,
    benchmark_terminal_rendering,
    benchmark_redaction,
    benchmark_output_compression,
    benchmark_read_files_batch
);
criterion_main!(benches);
