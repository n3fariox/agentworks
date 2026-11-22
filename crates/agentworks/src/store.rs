//! Server-facing typed adapters for durable records. SQL lives in agentworks-database.
use crate::automations;
pub use agentworks_database::{Metadata, SCHEMA_VERSION, exclusive, window};
use std::path::Path;
type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
pub struct Store(pub agentworks_database::Store);
impl std::ops::Deref for Store {
    type Target = agentworks_database::Store;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl std::ops::DerefMut for Store {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
impl Store {
    pub fn apply_compaction(
        &mut self,
        id: &str,
        plan: &crate::event_compaction::Plan,
        through: u64,
    ) -> Result<()> {
        self.0
            .apply_compaction(id, &plan.remove, &plan.replace, through)
    }

    pub fn open(path: &Path) -> Result<Self> {
        Ok(Self(agentworks_database::Store::open(path)?))
    }
    pub fn automations(&self) -> Result<Vec<automations::Definition>> {
        self.0.automations()
    }
    pub fn usable_automations(&self) -> Result<Vec<automations::Definition>> {
        self.0.usable_automations()
    }
    pub fn automation(&self, id: &str) -> Result<Option<automations::Definition>> {
        self.0.automation(id)
    }
    pub fn terminal(&self, id: &str) -> Result<Option<crate::terminal::Frame>> {
        self.0.terminal(id)
    }
    pub fn save_automation(
        &mut self,
        definition: &automations::Definition,
        arm: Option<i64>,
        ledger: Option<&automations::Ledger>,
    ) -> Result<()> {
        self.save_automation_source(definition, arm, ledger, None)
    }
    pub fn save_automation_source(
        &mut self,
        definition: &automations::Definition,
        arm: Option<i64>,
        ledger: Option<&automations::Ledger>,
        source: Option<&str>,
    ) -> Result<()> {
        self.0
            .save_automation_source(&serde_json::to_string(definition)?, arm, ledger, source)
    }
    #[allow(clippy::too_many_arguments)]
    pub fn claim_slot(
        &mut self,
        id: &str,
        slot: i64,
        request_id: &str,
        outcome: &str,
        detail: Option<&str>,
        now: i64,
        finish: Option<&automations::Finish>,
    ) -> Result<()> {
        let finish = finish
            .map(|f| {
                Ok::<_, serde_json::Error>(match f {
                    automations::Finish::Delete => agentworks_database::Finish::Delete,
                    automations::Finish::Complete(def) => {
                        agentworks_database::Finish::Complete(serde_json::to_string(def)?)
                    }
                })
            })
            .transpose()?;
        self.0
            .claim_slot(id, slot, request_id, outcome, detail, now, finish.as_ref())
    }
}
