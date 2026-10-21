#![allow(dead_code)]
use std::sync::Arc;

use agentworks_starlark::engine::{Capability, Compiled, Diagnostic, Engine, Installation};

pub const BAZEL: &str = include_str!("../../scripts/bazel_progress.star");
pub const WORKFLOW: &str = include_str!("../../scripts/check_then_fix.star");
pub const PARSER_HEAD: &str = "load(\"@agentworks//progress.star\", \"Counts\", \"LineContext\", \"Progress\")\n";
pub const WORKFLOW_HEAD: &str =
    "load(\"@agentworks//workflow.star\", \"Decision\", \"Event\", \"decide\", \"start_session\")\n";

pub fn engine() -> Arc<Engine> {
    Arc::new(Engine::new().expect("host preludes compile"))
}

pub fn install(engine: &Engine, name: &str, capability: Capability, source: &str) -> Result<Arc<Compiled>, Diagnostic> {
    engine.compile(&Installation::of(name, capability, source), source)
}

/// A parser whose entry runs `body` (indented) under the standard signature.
pub fn parser_source(body: &str) -> String {
    format!("{PARSER_HEAD}def on_line(ctx: LineContext, line: str) -> Progress | None:\n{body}")
}
