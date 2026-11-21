//! SQLite schema, durable records, and atomic persistence operations.
//! No server state, routing, authorization, or interpreter execution belongs here.
mod schema;
mod store;
use serde::{Deserialize, Serialize};
pub use store::{MAX_EVENT_BYTES, MAX_EVENTS, Metadata, SCHEMA_VERSION, Store, exclusive, window};
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Access {
    #[serde(default)]
    pub communicate: bool,
    #[serde(default)]
    pub start_sessions: bool,
    #[serde(default)]
    pub prompt_sessions: bool,
    #[serde(default)]
    pub cancel_sessions: bool,
    #[serde(default)]
    pub run_automations: bool,
    #[serde(default)]
    pub restart_service: bool,
    /// The review queue was removed. Saved grants and older clients may still send this
    /// key, so it is accepted and ignored rather than rejected as unknown.
    #[serde(default, rename = "manageReviewQueue", skip_serializing)]
    pub _removed_review_queue: bool,
    /// Explicit controller grant; never inherited from a session role.
    #[serde(default)]
    pub archive_sessions: bool,
    /// Explicit opt-in to create, edit, pause and delete project automations;
    /// never part of a role preset or inherited by child sessions.
    #[serde(default)]
    pub manage_automations: bool,
}
impl Access {
    pub fn enabled(&self) -> bool {
        self.communicate
            || self.start_sessions
            || self.prompt_sessions
            || self.cancel_sessions
            || self.run_automations
            || self.restart_service
            || self.archive_sessions
            || self.manage_automations
    }
}

pub struct Ledger<'a> {
    pub actor: &'a str,
    pub request_id: &'a str,
    pub operation: &'a str,
    pub payload: &'a str,
    pub result: &'a str,
}
pub enum Finish {
    Complete(String),
    Delete,
}
pub(crate) fn content_hash(source: &str) -> String {
    blake3::hash(source.as_bytes()).to_hex().to_string()
}

pub mod scripts;

mod roles;
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

mod maintenance;
pub use maintenance::Snapshot;
pub mod inbox;
mod input_queue;
pub mod mail;

pub mod controllers;
pub mod reviews;
pub mod tracking;

mod restarts;

#[cfg(test)]
mod tests;

/// SQLite storage failures, without exposing connections or row access.
pub use rusqlite::{Error as StorageError, Result as StorageResult};
