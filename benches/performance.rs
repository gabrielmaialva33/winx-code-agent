use std::fmt::Write as _;
use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

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

criterion_group!(
    benches,
    benchmark_thread_ids,
    benchmark_terminal_rendering,
    benchmark_redaction,
    benchmark_output_compression
);
criterion_main!(benches);
