//! Embedded Starlark progress parsers, message formatters and experimental workflows.

pub mod engine;
pub mod progress;
pub mod worker;
pub mod workflow;

/// Source of the bundled Bazel progress parser, seeded by the application.
pub const BAZEL_PROGRESS_SCRIPT: &str = include_str!("../scripts/bazel_progress.star");

/// Source of the bundled inbox message formatter.
pub const INBOX_MESSAGE_SCRIPT: &str = include_str!("../scripts/inbox_message.star");

/// Source of the bundled tool card header script.
pub const TOOL_HEADER_SCRIPT: &str = include_str!("../scripts/tool_header.star");
