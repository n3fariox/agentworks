use serde::{Deserialize, Serialize};
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Metadata {
    pub title: Option<String>,
    pub archived: bool,
    pub suspended: bool,
    /// Released by the idle sweep, not a human: input and peer mail wake it.
    #[serde(default)]
    pub asleep: bool,
}
