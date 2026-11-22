//! Installed hooks and their execution lifecycle, independent of server state.
//!
//! Registry operations own script definitions, enablement, precedence and built-in
//! upgrades in the application's SQLite transaction boundary. The host owns
//! compilation, interpreter scheduling and derived hook results. Callers bind
//! durable effects and coordinate application-specific migrations.
//!
//! Display transformations live in `agentworks-presentation`; language execution
//! and capability checks live in `agentworks-starlark`. This crate does not own
//! HTTP routes, live server sessions or authorization.
mod headers;
mod host;
mod registry;

pub use host::Hub;
pub use registry::{
    PRIORITY_RANGE, Removal, SCHEMA, Script, delete, list, save, seed, set_enabled, set_priority,
    valid_name,
};
