//! Display-only projections of authoritative Agentworks content.
//!
//! This crate owns Markdown safety, prose selection, transcript compaction and
//! bounded tool summaries. It does not execute scripts, access storage, authorize
//! requests, or own server sessions. Formatters enter through a narrow port.
pub mod chat;
pub mod markdown;
pub mod tool_call;
pub mod transcript;
