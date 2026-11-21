//! Script validation and interpreter metadata; storage is owned by the database crate.
use agentworks_database::Store;
pub use agentworks_database::scripts::{PRIORITY_RANGE, Removal};
use agentworks_starlark::engine::HookInfo;
use serde::Serialize;
/// Shipped scripts. A built-in follows the shipped source until a user saves
/// their own version under the same name; it can be disabled but not deleted.
pub(crate) const BUILTIN: &[(&str, &str)] = &[
    (
        "bazel_progress.star",
        agentworks_starlark::BAZEL_PROGRESS_SCRIPT,
    ),
    (
        "inbox_message.star",
        agentworks_starlark::INBOX_MESSAGE_SCRIPT,
    ),
    ("tool_header.star", agentworks_starlark::TOOL_HEADER_SCRIPT),
];

#[derive(Serialize)]
pub struct Script {
    pub name: String,
    pub capability: String,
    /// Validated installation-time registrations, in declaration order.
    pub hooks: Vec<HookInfo>,
    pub source: String,
    pub hash: String,
    pub enabled: bool,
    /// Routing priority: higher runs first, then filename, then declaration order.
    pub priority: i64,
    /// Follows the shipped source (unmodified built-in).
    pub builtin: bool,
    /// The name of a shipped script, whether or not the user modified it.
    pub shipped: bool,
    pub updated: i64,
    /// Current compile/type error for an enabled script, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub fn valid_name(name: &str) -> bool {
    let stem = name.strip_suffix(".star").unwrap_or("");
    !stem.is_empty()
        && stem.len() <= 64
        && stem
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

pub fn seed(db: &Store) -> agentworks_database::StorageResult<()> {
    db.seed_scripts(BUILTIN)
}
pub fn list(db: &Store) -> agentworks_database::StorageResult<Vec<Script>> {
    Ok(db
        .scripts()?
        .into_iter()
        .map(|r| Script {
            shipped: BUILTIN.iter().any(|(name, _)| *name == r.name),
            name: r.name,
            capability: r.capability,
            hooks: Vec::new(),
            source: r.source,
            hash: r.hash,
            enabled: r.enabled,
            priority: r.priority,
            builtin: r.builtin,
            updated: r.updated,
            error: None,
        })
        .collect())
}
pub fn save(db: &Store, name: &str, source: &str) -> agentworks_database::StorageResult<()> {
    db.save_script(name, source)
}
pub fn set_enabled(
    db: &Store,
    name: &str,
    enabled: bool,
) -> agentworks_database::StorageResult<bool> {
    db.enable_script(name, enabled)
}
pub fn set_priority(
    db: &Store,
    name: &str,
    priority: i64,
) -> agentworks_database::StorageResult<bool> {
    db.prioritize_script(name, priority)
}
pub fn delete(db: &Store, name: &str) -> agentworks_database::StorageResult<Removal> {
    db.remove_script(
        name,
        BUILTIN.iter().find(|(n, _)| *n == name).map(|(_, s)| *s),
    )
}
