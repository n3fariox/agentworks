//! Session data, queue transitions, continuation decisions, and turn timeout policies.
pub mod continuation;
pub mod lifecycle;
pub mod lineage;
pub mod model;
pub mod origins;
pub mod permissions;
pub mod queue;
pub mod timeouts;
pub use model::SessionData;

fn fail(status: http::StatusCode, message: impl Into<String>) -> (http::StatusCode, String) {
    (status, message.into())
}

pub mod async_tasks;
