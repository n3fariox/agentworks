//! E-01 runtime measurements. Run with:
//! cargo run --release -p agentworks-starlark --example measure
use std::sync::Arc;
use std::time::{Duration, Instant};

use agentworks_starlark::engine::{Capability, Engine, Installation};
use agentworks_starlark::progress::{BuiltinBazel, LineContext, LineParser, StarlarkParser, State, ToolProgress};

const CTX: LineContext = LineContext {
    command: "bazel build //...",
    stream: "t",
    provisional: false,
};
use agentworks_starlark::worker::{WorkerClient, WorkerLimits};
use serde_json::json;

const BAZEL: &str = include_str!("../scripts/bazel_progress.star");
const WORKFLOW: &str = include_str!("../scripts/check_then_fix.star");

fn rss_kib() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    status
        .lines()
        .find(|l| l.starts_with("VmRSS:"))
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap()
        .parse()
        .unwrap()
}

fn line(i: usize) -> String {
    match i % 4 {
        0 => format!(
            "[{} / 120,000] Compiling src/module_{i}.cc; 3s remote-cache",
            i % 120_000
        ),
        1 => format!(
            "[{} / 120,000] {} / 400 tests; Testing //pkg:test_{i}",
            i % 120_000,
            i % 400
        ),
        2 => "INFO: From Compiling external/zlib/inflate.c: warning: unused variable".to_owned(),
        _ => format!(
            "Analyzing: {i} targets ({} packages loaded, 12000 targets configured)",
            i / 10
        ),
    }
}

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    sorted[((sorted.len() as f64 - 1.0) * p) as usize]
}

fn bench(name: &str, parser: &dyn LineParser, n: usize) {
    let mut state = State::new();
    let mut samples = Vec::with_capacity(n);
    let started = Instant::now();
    for i in 0..n {
        let l = line(i);
        let t = Instant::now();
        parser.parse(&CTX, &mut state, &l).unwrap();
        samples.push(t.elapsed());
    }
    let total = started.elapsed();
    samples.sort();
    println!(
        "{name}: {n} lines in {total:?}; mean {:?}, p50 {:?}, p99 {:?}, max {:?}",
        total / n as u32,
        percentile(&samples, 0.5),
        percentile(&samples, 0.99),
        samples[n - 1]
    );
}

fn main() {
    let t = Instant::now();
    let engine = Arc::new(Engine::new().unwrap());
    println!("engine + preludes: {:?}", t.elapsed());
    let parser = engine
        .compile(
            &Installation::of("bazel_progress.star", Capability::Parser, BAZEL),
            BAZEL,
        )
        .unwrap();
    let workflow = engine
        .compile(
            &Installation::of("check_then_fix.star", Capability::Workflow, WORKFLOW),
            WORKFLOW,
        )
        .unwrap();
    println!(
        "compile+typecheck parser: {:?}, workflow: {:?}",
        parser.compile_time, workflow.compile_time
    );

    let starlark = StarlarkParser {
        engine: Arc::clone(&engine),
        script: Arc::clone(&parser),
    };
    bench("builtin Rust", &BuiltinBazel, 200_000);
    bench("starlark", &starlark, 200_000);
    // Fixed embedding cost: fresh heap, JSON->record state, call, record->JSON, validation.
    let noop_src = "load(\"@agentworks//progress.star\", \"LineContext\", \"Progress\")\ndef on_line(ctx: LineContext, line: str) -> Progress | None:\n    return None\n";
    let noop = engine
        .compile(&Installation::of("noop.star", Capability::Parser, noop_src), noop_src)
        .unwrap();
    bench(
        "starlark no-op entry",
        &StarlarkParser {
            engine: Arc::clone(&engine),
            script: noop,
        },
        200_000,
    );

    // Ticks per call for limit calibration.
    let (_, stats) = starlark.parse_with_stats(&CTX, &mut State::new(), &line(1)).unwrap();
    println!("starlark ticks for one test-count line: {}", stats.ticks);

    // Interpreter memory over a long stream: per-call heaps must not accumulate.
    let mut state = State::new();
    let base = rss_kib();
    for i in 0..1_000_000 {
        starlark.parse(&CTX, &mut state, &line(i)).unwrap();
        if i == 99_999 || i == 999_999 {
            println!(
                "RSS after {} lines: {} KiB (+{} KiB from start)",
                i + 1,
                rss_kib(),
                rss_kib() as i64 - base as i64
            );
        }
    }

    // End-to-end stream host: 20-line deltas, including the provisional parse.
    let mut tool = ToolProgress::default();
    tool.apply(
        &starlark,
        &json!({"rawInput": {"command": "bazel build //..."}, "status": "in_progress"}),
        Some(0),
    );
    let chunks: Vec<String> = (0..5_000)
        .map(|c| (0..20).map(|i| line(c * 20 + i) + "\r").collect())
        .collect();
    let t = Instant::now();
    for (seq, chunk) in chunks.iter().enumerate() {
        tool.apply(
            &starlark,
            &json!({"_meta": {"terminal_output_delta": {"terminal_id": "t", "data": chunk}}}),
            Some(seq as i64 + 1),
        );
    }
    println!(
        "stream host: 100000 lines in 5000 deltas: {:?} ({} parser calls)",
        t.elapsed(),
        tool.calls
    );

    // Worker process overhead.
    let install = Installation::of("bazel_progress.star", Capability::Parser, BAZEL);
    let program = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("agentworks-starlark-worker");
    let mut worker = WorkerClient::new(&program, WorkerLimits::default());
    let t = Instant::now();
    worker
        .call(
            &install,
            BAZEL,
            &json!({"command": "bazel build", "stream": "t", "provisional": false, "state": {}}),
            &json!(line(0)),
        )
        .unwrap();
    println!("worker spawn + compile + first call: {:?}", t.elapsed());
    let t = Instant::now();
    for i in 0..1000 {
        worker
            .call(
                &install,
                BAZEL,
                &json!({"command": "bazel build", "stream": "t", "provisional": false, "state": {}}),
                &json!(line(i)),
            )
            .unwrap();
    }
    println!(
        "worker steady state: {:?} per call (1000 calls, includes source in each request)",
        t.elapsed() / 1000
    );

    // Workflow decision cost without SQLite.
    let t = Instant::now();
    for _ in 0..10_000 {
        engine.call(&workflow, &json!({"workflow": "wf", "owner": "coordinator", "state": {"project": "demo", "step": "check"}}),
            &json!({"id": "e", "kind": "action_completed", "action_key": "check", "outcome": "failed", "data": {"summary": "x"}})).unwrap();
    }
    println!("workflow decision: {:?} per event", t.elapsed() / 10_000);
}
