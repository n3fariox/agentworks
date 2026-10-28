//! Bounded Git operations and owned worktree preparation, inspection, and cleanup.
//! Callers supply filesystem policy and durable mutation reservations through `worktrees::Context`.
pub mod cleanup;
pub mod config;
mod cow;
pub mod diff;
pub mod directories;
pub mod git;
pub mod hooks;
pub mod worktrees;

// Shared with application credential digests; retains the existing implementation and test vectors.
pub mod sha256;

/// Operation failure with the existing API status classification.
pub type Error = (http::StatusCode, String);
fn fail(status: http::StatusCode, message: impl Into<String>) -> Error {
    (status, message.into())
}
