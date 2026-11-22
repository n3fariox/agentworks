//! Session secrets without the service around them: the encrypted key file and value
//! sealing, names and assignment rules, redaction of delivered values, and running a
//! command with secret templates on the session's host. Tables live in
//! agentworks-database; HTTP routes, authorization, approvals and live session state
//! belong to the service.
pub mod exec;
pub mod model;
pub mod redact;
#[cfg(feature = "vault")]
pub mod vault;
