//! Session secrets without the service around them: the encrypted key file and value
//! sealing, names and assignment rules, their tables, redaction of delivered values,
//! and running a command with secret templates on the session's host. HTTP routes,
//! authorization, approvals and live session state belong to the service.
pub mod exec;
pub mod model;
pub mod redact;
#[cfg(feature = "storage")]
pub mod store;
#[cfg(feature = "storage")]
pub mod vault;
