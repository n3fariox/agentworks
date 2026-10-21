//! Supervised worker process: hard stops the in-process limits cannot promise.
mod common;
use std::time::{Duration, Instant};

use agentworks_starlark::engine::{Capability, DiagnosticKind, Installation};
use agentworks_starlark::worker::{WorkerClient, WorkerLimits};
use common::*;
use serde_json::json;

const WORKER: &str = env!("CARGO_BIN_EXE_agentworks-starlark-worker");

fn state() -> serde_json::Value {
    json!({"phase": null, "counts": []})
}

#[test]
fn worker_parses_and_reuses_its_compiled_script() {
    let mut worker = WorkerClient::new(WORKER, WorkerLimits::default());
    let install = Installation::of("bazel_progress.star", Capability::Parser, BAZEL);
    for line in [
        "Loading: 1 packages",
        "[1 / 4] Compiling a.cc",
        "[2 / 4] 1 / 3 tests; Testing //a",
    ] {
        let (value, _) = worker.call(&install, BAZEL, &state(), &json!(line)).unwrap();
        assert!(value["progress"]["phase"].is_string(), "{value}");
    }
    assert_eq!(worker.spawns, 1);
}

#[test]
fn allocation_past_the_os_limit_kills_only_the_worker() {
    // One native string repeat allocates ~1 GiB before any heap-limit check runs.
    let body = "    big = line * (1024 * 1024 * 1024)\n    return ParseResult(state = state, progress = Progress(phase = big[:4], label = None, counts = []))\n";
    let source = parser_source(body);
    let install = Installation::of("alloc.star", Capability::Parser, &source);
    let limits = WorkerLimits {
        address_space_bytes: 256 * 1024 * 1024,
        ..WorkerLimits::default()
    };
    let mut worker = WorkerClient::new(WORKER, limits);
    let d = worker.call(&install, &source, &state(), &json!("x")).unwrap_err();
    assert_eq!(d.kind, DiagnosticKind::Worker, "{d}");
    assert!(d.message.contains("worker exited"), "{d}");
    assert_eq!(d.script, "alloc.star");
    // The host continues; a fresh worker serves the next request.
    let good = Installation::of("bazel_progress.star", Capability::Parser, BAZEL);
    worker.call(&good, BAZEL, &state(), &json!("[1 / 2] ok")).unwrap();
    assert_eq!(worker.spawns, 2);
}

#[test]
fn wall_clock_deadline_kills_a_stuck_worker() {
    // A blocking or very slow evaluation the in-process checks do not interrupt
    // in time: simulated by a long string build exceeding the worker deadline.
    let body = "    s = \"\"\n    for i in range(5000):\n        s = (s + line)[-100000:] + line * 100000\n    return ParseResult(state = state, progress = None)\n";
    let source = parser_source(body);
    let install = Installation::of("slow.star", Capability::Parser, &source);
    let limits = WorkerLimits {
        wall: Duration::from_millis(20),
        ..WorkerLimits::default()
    };
    let mut worker = WorkerClient::new(WORKER, limits);
    let started = Instant::now();
    let d = worker.call(&install, &source, &state(), &json!("x")).unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(d.kind, DiagnosticKind::Worker, "{d}");
    assert!(d.message.contains("wall time and was killed"), "{d}");
}
