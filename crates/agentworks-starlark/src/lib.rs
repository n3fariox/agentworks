//! Embedded Starlark progress parsers and experimental workflow decisions.

pub mod engine;
pub mod progress;
pub mod worker;
pub mod workflow;

/// Source of the bundled Bazel progress parser, seeded by the application.
pub const BAZEL_PROGRESS_SCRIPT: &str = include_str!("../scripts/bazel_progress.star");
