//! Opt-in session routing at the global and project level.
//!
//! Two strategies are supported:
//! 1. `quota_round_robin`: prioritizes the candidate with the lowest observed
//!    account quota utilization plus active-session load, breaking ties via a
//!    least-recently-selected cursor so burst creation balances evenly.
//! 2. `decision_model`: sends a single-shot OpenAI-compatible HTTP chat
//!    completion request with the task context and eligible candidate pool,
//!    falling back to `quota_round_robin` on error, timeout, or invalid output.

use crate::{App, account_quota, fail};
use agentworks_session::model::RoutingDecision;
use axum::http::StatusCode;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    sync::Mutex,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub const DEFAULT_DECISION_TIMEOUT_MS: u64 = 5000;
const MIN_DECISION_TIMEOUT_MS: u64 = 500;
const MAX_DECISION_TIMEOUT_MS: u64 = 30_000;
const MAX_CANDIDATES: usize = 64;
const MAX_INSTRUCTIONS_LEN: usize = 4096;
const MAX_NOTES_LEN: usize = 500;
const RECENT_IDLE_WINDOW_MS: u64 = 5 * 60 * 1000;

/// Providers included when no explicit candidate pool is configured.
/// Excludes legacy `claude`/`codex` aliases and `demo` unless explicitly added.
const AUTO_PROVIDERS: [&str; 5] = [
    "claude-rust",
    "codex-rust",
    "antigravity",
    "opencode",
    "goose",
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Off,
    Default,
    Always,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Default => "default",
            Self::Always => "always",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Approach {
    #[default]
    QuotaRoundRobin,
    DecisionModel,
}

impl Approach {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::QuotaRoundRobin => "quota_round_robin",
            Self::DecisionModel => "decision_model",
        }
    }
}

fn default_enabled() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionProtocol {
    #[default]
    Auto,
    #[serde(alias = "openai_decisions")]
    Decisions,
    #[serde(alias = "gemini_decisions")]
    Gemini,
    Systemone,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionModelConfig {
    #[serde(default)]
    pub protocol: DecisionProtocol,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_provider_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoundRobinStrategy {
    #[default]
    QuotaAndSessions,
    LeastUsedQuota,
    StrictRoundRobin,
}

fn default_quota_limit_percent() -> u8 {
    99
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoundRobinConfig {
    #[serde(default)]
    pub strategy: RoundRobinStrategy,
    #[serde(default = "default_quota_limit_percent")]
    pub quota_limit_percent: u8,
}

impl Default for RoundRobinConfig {
    fn default() -> Self {
        Self {
            strategy: RoundRobinStrategy::default(),
            quota_limit_percent: default_quota_limit_percent(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(default)]
    pub mode: Mode,
    #[serde(default)]
    pub approach: Approach,
    #[serde(default)]
    pub candidates: Vec<Candidate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub round_robin: Option<RoundRobinConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_model: Option<DecisionModelConfig>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<Mode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approach: Option<Approach>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidates: Option<Vec<Candidate>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub round_robin: Option<RoundRobinConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_model: Option<DecisionModelConfig>,
}

/// Distinguishes omitted from explicit `null` in project update requests.
pub fn project_patch<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Option<ProjectSettings>>, D::Error> {
    Option::<ProjectSettings>::deserialize(deserializer).map(Some)
}

#[derive(Default)]
pub struct RouterState {
    seq: Mutex<(u64, HashMap<String, u64>)>,
}

impl RouterState {
    fn last_seq(&self, key: &str) -> u64 {
        self.seq.lock().unwrap().1.get(key).copied().unwrap_or(0)
    }

    fn record_pick(&self, key: &str) {
        let mut guard = self.seq.lock().unwrap();
        guard.0 += 1;
        let next = guard.0;
        guard.1.insert(key.to_owned(), next);
    }
}

/// Global disable overrides per-project routing settings.
pub fn resolve_settings(global: Option<&Settings>, project: Option<&ProjectSettings>) -> Settings {
    let base = global.cloned().unwrap_or_default();
    if base.mode == Mode::Off {
        return Settings {
            mode: Mode::Off,
            ..base
        };
    }
    let Some(proj) = project else {
        return base;
    };
    Settings {
        mode: proj.mode.unwrap_or(base.mode),
        approach: proj.approach.unwrap_or(base.approach),
        candidates: proj.candidates.clone().unwrap_or(base.candidates),
        round_robin: proj.round_robin.clone().or(base.round_robin),
        decision_model: proj.decision_model.clone().or(base.decision_model),
    }
}

pub fn effective_settings(app: &App, project_id: Option<&str>) -> Settings {
    let catalog = app.catalog.lock().unwrap();
    let global = catalog.data.settings.session_routing.as_ref();
    let project = project_id
        .and_then(|id| catalog.data.projects.iter().find(|p| p.id == id))
        .and_then(|p| p.session_routing.as_ref());
    resolve_settings(global, project)
}

pub fn effective_mode(app: &App, project_id: Option<&str>) -> Mode {
    effective_settings(app, project_id).mode
}

pub fn should_route(mode: Mode, requested_provider: &str, external_import: bool) -> bool {
    if external_import {
        return false;
    }
    match mode {
        Mode::Off => false,
        Mode::Default => requested_provider.trim().is_empty() || requested_provider == "auto",
        Mode::Always => true,
    }
}

fn validate_candidates(candidates: &mut Vec<Candidate>) -> Result<(), (StatusCode, String)> {
    if candidates.len() > MAX_CANDIDATES {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            format!("Configure at most {MAX_CANDIDATES} routing candidates"),
        ));
    }
    for c in candidates {
        c.provider = c.provider.trim().to_owned();
        if !agentworks_runtime::remote::PROVIDERS.contains(&c.provider.as_str()) {
            return Err(fail(
                StatusCode::BAD_REQUEST,
                "Unknown provider in session routing candidates",
            ));
        }
        c.model = c
            .model
            .take()
            .map(|m| m.trim().to_owned())
            .filter(|m| !m.is_empty());
        if let Some(m) = &c.model {
            if m.len() > 256 || m.chars().any(char::is_control) {
                return Err(fail(
                    StatusCode::BAD_REQUEST,
                    "Invalid model ID in session routing candidates",
                ));
            }
            if c.provider == "opencode" && !agentworks_runtime::provider::is_free_model(m) {
                return Err(fail(
                    StatusCode::BAD_REQUEST,
                    "OpenCode routing candidates support free models only",
                ));
            }
        }
        c.model_provider = c
            .model_provider
            .take()
            .map(|p| p.trim().to_owned())
            .filter(|p| !p.is_empty());
        if c.model_provider.is_some() && c.provider != "goose" {
            return Err(fail(
                StatusCode::BAD_REQUEST,
                "Model provider profiles in routing candidates require Goose",
            ));
        }
        c.notes = c
            .notes
            .take()
            .map(|n| n.trim().to_owned())
            .filter(|n| !n.is_empty());
        if c.notes
            .as_ref()
            .is_some_and(|n| n.chars().count() > MAX_NOTES_LEN)
        {
            return Err(fail(
                StatusCode::BAD_REQUEST,
                format!("Candidate notes must be at most {MAX_NOTES_LEN} characters"),
            ));
        }
    }
    Ok(())
}

fn validate_decision_model(
    cfg: &mut Option<DecisionModelConfig>,
) -> Result<(), (StatusCode, String)> {
    let Some(dm) = cfg.as_mut() else {
        return Ok(());
    };
    dm.model_provider_id = dm
        .model_provider_id
        .take()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty());
    dm.base_url = dm
        .base_url
        .take()
        .map(|s| s.trim().trim_end_matches('/').to_owned())
        .filter(|s| !s.is_empty());
    dm.model = dm
        .model
        .take()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty());
    dm.instructions = dm
        .instructions
        .take()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty());

    if let Some(url) = &dm.base_url {
        let parsed: axum::http::Uri = url
            .parse()
            .map_err(|_| fail(StatusCode::BAD_REQUEST, "Use a valid HTTP/HTTPS decision model URL"))?;
        if !matches!(parsed.scheme_str(), Some("http" | "https"))
            || parsed.host().is_none()
            || parsed.authority().is_some_and(|a| a.as_str().contains('@'))
            || parsed.query().is_some()
            || url.contains('#')
        {
            return Err(fail(
                StatusCode::BAD_REQUEST,
                "Decision model URL must be HTTP/HTTPS without credentials, query, or fragment",
            ));
        }
    }
    if let Some(id) = &dm.model_provider_id {
        let conn = agentworks_runtime::model_providers::server_connection(id)
            .map_err(|e| fail(StatusCode::BAD_REQUEST, e))?;
        if conn.profile.kind != "openai-compatible" {
            return Err(fail(
                StatusCode::BAD_REQUEST,
                "Decision model requires an OpenAI-compatible model provider",
            ));
        }
    }
    if let Some(m) = &dm.model
        && (m.len() > 256 || m.chars().any(char::is_control))
    {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            "Decision model ID is invalid",
        ));
    }
    if let Some(inst) = &dm.instructions
        && inst.chars().count() > MAX_INSTRUCTIONS_LEN
    {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            format!("Decision model instructions must be at most {MAX_INSTRUCTIONS_LEN} characters"),
        ));
    }
    if let Some(timeout) = &mut dm.timeout_ms {
        *timeout = (*timeout).clamp(MIN_DECISION_TIMEOUT_MS, MAX_DECISION_TIMEOUT_MS);
    }
    Ok(())
}

fn validate_round_robin(cfg: &mut Option<RoundRobinConfig>) {
    if let Some(rr) = cfg.as_mut() {
        rr.quota_limit_percent = rr.quota_limit_percent.clamp(1, 100);
    }
}

pub fn validate_settings(settings: &mut Settings) -> Result<(), (StatusCode, String)> {
    validate_candidates(&mut settings.candidates)?;
    validate_round_robin(&mut settings.round_robin);
    validate_decision_model(&mut settings.decision_model)
}

pub fn validate_project_settings(
    settings: &mut ProjectSettings,
) -> Result<(), (StatusCode, String)> {
    if let Some(candidates) = &mut settings.candidates {
        validate_candidates(candidates)?;
    }
    validate_round_robin(&mut settings.round_robin);
    validate_decision_model(&mut settings.decision_model)
}

pub struct RouteInput<'a> {
    pub project_id: Option<&'a str>,
    pub host_id: &'a str,
    pub title: Option<&'a str>,
    pub description: Option<&'a str>,
    pub prompt: Option<&'a str>,
    pub role_name: Option<&'a str>,
    pub role_description: Option<&'a str>,
    pub requires_role_support: bool,
    pub cwd: &'a str,
}

#[derive(Clone, Debug)]
struct EligibleCandidate {
    provider: String,
    model: Option<String>,
    model_provider: Option<String>,
    notes: Option<String>,
    family: String,
    quota_used: f64,
    exhausted: bool,
    session_load: f64,
    active_sessions: usize,
    candidate_key: String,
}

fn candidate_key(provider: &str, model_provider: Option<&str>, model: Option<&str>) -> String {
    format!(
        "{}|{}|{}",
        provider,
        model_provider.unwrap_or(""),
        model.unwrap_or("")
    )
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn normalize_epoch_ms(raw: u64) -> u64 {
    if raw < 1_000_000_000_000 {
        raw.saturating_mul(1000)
    } else {
        raw
    }
}

/// Extracts `(max_used_fraction, exhausted)` from an `account_quota::Hub` entry,
/// skipping windows whose reset timestamp has already elapsed.
pub fn quota_summary(entry: Option<&Value>, provider: &str, now: u64) -> (f64, bool) {
    let Some(entry) = entry else {
        return (0.0, false);
    };
    let cli = agentworks_runtime::provider::cli_provider(provider);
    let mut max_used = 0.0_f64;
    let mut exhausted = false;

    if cli == "claude"
        && let Some(windows) = entry["claude"]["windows"].as_object()
    {
        for w in windows.values() {
            if let Some(reset) = w["resetsAt"].as_u64()
                && normalize_epoch_ms(reset) <= now
            {
                continue;
            }
            if w["status"].as_str() == Some("rejected") {
                exhausted = true;
            }
            if let Some(u) = w["utilization"].as_f64()
                && (0.0..=1.0).contains(&u)
            {
                max_used = max_used.max(u);
                if u >= 0.99 {
                    exhausted = true;
                }
            }
        }
    } else if cli == "codex"
        && let Some(limits) = entry["codex"]["limits"].as_object()
    {
        for limit in limits.values() {
            for role in ["primary", "secondary"] {
                let cell = &limit[role];
                if !cell.is_object() {
                    continue;
                }
                if let Some(reset) = cell["resetsAt"]["value"].as_u64()
                    && normalize_epoch_ms(reset) <= now
                {
                    continue;
                }
                if let Some(pct) = cell["usedPercent"]["value"].as_f64()
                    && (0.0..=100.0).contains(&pct)
                {
                    let u = pct / 100.0;
                    max_used = max_used.max(u);
                    if u >= 0.99 {
                        exhausted = true;
                    }
                }
            }
        }
    } else if provider == "antigravity"
        && let Some(windows) = entry["antigravity"]["windows"].as_array()
    {
        for w in windows {
            if let Some(reset_str) = w["resetTime"].as_str()
                && let Ok(ts) = reset_str.parse::<jiff::Timestamp>()
                && (ts.as_millisecond() as u64) <= now
            {
                continue;
            }
            if let Some(rem) = w["remainingFraction"].as_f64()
                && (0.0..=1.0).contains(&rem)
            {
                let u = 1.0 - rem;
                max_used = max_used.max(u);
                if rem <= 0.01 {
                    exhausted = true;
                }
            }
        }
    }

    (max_used, exhausted)
}

fn provider_available_on_host(app: &App, host_id: &str, provider: &str, has_profile: bool) -> bool {
    if host_id == "local" {
        if !crate::installed(provider) {
            return false;
        }
        if !has_profile && crate::use_api_key() && !crate::key_present(provider) {
            return false;
        }
        true
    } else {
        app.hosts
            .live(host_id)
            .is_some_and(|live| live.has_provider(provider))
    }
}

fn build_pool(
    app: &App,
    effective: &Settings,
    host_id: &str,
    requires_role_support: bool,
    hidden_providers: &[String],
    default_models: &HashMap<String, String>,
) -> Result<Vec<EligibleCandidate>, (StatusCode, String)> {
    let raw_candidates: Vec<Candidate> = if !effective.candidates.is_empty() {
        effective
            .candidates
            .iter()
            .filter(|c| c.enabled)
            .cloned()
            .collect()
    } else {
        let mut derived = Vec::new();
        let profiles = agentworks_runtime::model_providers::list_profiles().unwrap_or_default();
        for &provider in &AUTO_PROVIDERS {
            if hidden_providers.iter().any(|h| h == provider) {
                continue;
            }
            if provider == "goose" {
                let matching: Vec<_> = profiles
                    .iter()
                    .filter(|p| p.host_ids.iter().any(|h| h == host_id))
                    .collect();
                for p in matching {
                    derived.push(Candidate {
                        provider: "goose".into(),
                        model: Some(p.default_model.clone()),
                        model_provider: Some(p.id.clone()),
                        notes: Some(p.name.clone()),
                        enabled: true,
                    });
                }
                continue;
            }
            derived.push(Candidate {
                provider: provider.into(),
                model: None,
                model_provider: None,
                notes: None,
                enabled: true,
            });
        }
        derived
    };

    if host_id != "local" && app.hosts.live(host_id).is_none() {
        return Err(fail(
            StatusCode::CONFLICT,
            "That host is not connected. Reconnect it from the Hosts list.",
        ));
    }

    let profiles = agentworks_runtime::model_providers::list_profiles().unwrap_or_default();
    let now = now_ms();

    // Snapshot active session counts and loads per (host_id, family).
    let mut family_loads: HashMap<String, (f64, usize)> = HashMap::new();
    {
        let sessions = app.sessions.lock().unwrap();
        for s in sessions.values() {
            if s.host_id != host_id {
                continue;
            }
            let d = s.data.lock().unwrap();
            if matches!(d.status.as_str(), "suspended" | "error" | "closed") {
                continue;
            }
            let fam = account_quota::family(&s.provider, d.model_provider.as_deref());
            let weight = if matches!(
                d.status.as_str(),
                "starting" | "running" | "configuring" | "resuming" | "waiting" | "cancelling"
            ) {
                0.15
            } else if now.saturating_sub(s.created) < RECENT_IDLE_WINDOW_MS {
                0.10
            } else {
                0.03
            };
            let entry = family_loads.entry(fam).or_insert((0.0, 0));
            entry.0 += weight;
            entry.1 += 1;
        }
    }

    let mut seen = HashSet::new();
    let mut pool = Vec::new();
    for c in raw_candidates {
        if !agentworks_runtime::remote::PROVIDERS.contains(&c.provider.as_str()) {
            continue;
        }
        if requires_role_support && !crate::roles::supported(&c.provider) {
            continue;
        }
        if c.provider != "goose" && c.model_provider.is_some() {
            continue;
        }
        if c.provider == "goose"
            && let Some(mp_id) = c.model_provider.as_deref()
        {
            let Some(profile) = profiles.iter().find(|p| p.id == mp_id) else {
                continue;
            };
            if !profile.host_ids.iter().any(|h| h == host_id) {
                continue;
            }
            if profile.kind == "chatgpt_codex"
                && !agentworks_runtime::codex_auth::has_credentials(mp_id)
            {
                continue;
            }
        }
        if !provider_available_on_host(app, host_id, &c.provider, c.model_provider.is_some()) {
            continue;
        }
        let resolved_model = c
            .model
            .clone()
            .or_else(|| default_models.get(&c.provider).cloned())
            .filter(|m| !m.is_empty());
        if c.provider == "opencode"
            && let Some(m) = resolved_model.as_deref()
            && !agentworks_runtime::provider::is_free_model(m)
        {
            continue;
        }
        let key = candidate_key(
            &c.provider,
            c.model_provider.as_deref(),
            resolved_model.as_deref(),
        );
        if !seen.insert(key.clone()) {
            continue;
        }
        let family = account_quota::family(&c.provider, c.model_provider.as_deref());
        let quota_entry = app.account_quota.get(host_id, &family);
        let (quota_used, exhausted) = quota_summary(quota_entry.as_ref(), &c.provider, now);
        let (session_load, active_sessions) = family_loads.get(&family).copied().unwrap_or((0.0, 0));

        pool.push(EligibleCandidate {
            provider: c.provider,
            model: resolved_model,
            model_provider: c.model_provider,
            notes: c.notes,
            family,
            quota_used,
            exhausted,
            session_load,
            active_sessions,
            candidate_key: key,
        });
    }

    if pool.is_empty() {
        return Err(fail(
            StatusCode::CONFLICT,
            "No eligible provider in the session router pool is available for this host and feature set",
        ));
    }
    Ok(pool)
}

fn select_quota_round_robin(
    app: &App,
    pool: &[EligibleCandidate],
    round_robin: Option<&RoundRobinConfig>,
    mode: Mode,
    fallback: bool,
    fallback_prefix: Option<&str>,
) -> RoutingDecision {
    let rr = round_robin.cloned().unwrap_or_default();
    let limit_fraction = f64::from(rr.quota_limit_percent.clamp(1, 100)) / 100.0;
    let mut best_idx = 0usize;
    let mut best_key = (true, i64::MAX, u64::MAX, usize::MAX);

    for (idx, c) in pool.iter().enumerate() {
        let exhausted = c.exhausted || c.quota_used >= limit_fraction;
        let score = match rr.strategy {
            RoundRobinStrategy::QuotaAndSessions => {
                ((c.quota_used + c.session_load) * 10_000.0).round() as i64
            }
            RoundRobinStrategy::LeastUsedQuota => (c.quota_used * 10_000.0).round() as i64,
            RoundRobinStrategy::StrictRoundRobin => 0,
        };
        let last_seq = app.router.last_seq(&c.candidate_key);
        let rank = (exhausted, score, last_seq, idx);
        if rank < best_key {
            best_key = rank;
            best_idx = idx;
        }
    }

    let chosen = &pool[best_idx];
    app.router.record_pick(&chosen.candidate_key);

    let summary = format!(
        "quota {:.0}%, {} active session(s)",
        chosen.quota_used * 100.0,
        chosen.active_sessions
    );
    let reason = match fallback_prefix {
        Some(prefix) => format!("{prefix}; fell back to quota round-robin ({summary})"),
        None => format!("Selected via quota round-robin ({summary})"),
    };

    RoutingDecision {
        mode: mode.as_str().into(),
        approach: Approach::QuotaRoundRobin.as_str().into(),
        provider: chosen.provider.clone(),
        model: chosen.model.clone(),
        model_provider: chosen.model_provider.clone(),
        fallback,
        reason: Some(reason),
    }
}

fn decision_endpoint_url(base_url: &str, version_prefix: &str, route: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    let suffix = format!("/{route}");
    if trimmed.ends_with(&suffix) {
        return trimmed.to_owned();
    }
    for known in ["/decisions", "/systemone"] {
        if let Some(prefix) = trimmed.strip_suffix(known) {
            return format!("{prefix}{suffix}");
        }
    }
    let version_suffix = format!("/{version_prefix}");
    if trimmed.ends_with(&version_suffix) || trimmed.contains(&format!("{version_suffix}/")) {
        format!("{trimmed}{suffix}")
    } else if let Ok(uri) = trimmed.parse::<axum::http::Uri>()
        && uri.path().trim_matches('/').is_empty()
    {
        format!("{trimmed}/{version_prefix}{suffix}")
    } else {
        format!("{trimmed}{suffix}")
    }
}

fn protocol_sequence(
    protocol: DecisionProtocol,
    base_url: &str,
    model_id: &str,
) -> Vec<DecisionProtocol> {
    match protocol {
        DecisionProtocol::Decisions => vec![DecisionProtocol::Decisions],
        DecisionProtocol::Gemini => vec![DecisionProtocol::Gemini],
        DecisionProtocol::Systemone => vec![DecisionProtocol::Systemone],
        DecisionProtocol::Auto => {
            let trimmed = base_url.trim_end_matches('/').to_ascii_lowercase();
            if trimmed.ends_with("/systemone") {
                return vec![DecisionProtocol::Systemone];
            }
            if trimmed.contains("generativelanguage.googleapis.com") || trimmed.ends_with("/v1beta") {
                return vec![DecisionProtocol::Gemini];
            }
            if trimmed.ends_with("/decisions") {
                return vec![DecisionProtocol::Decisions];
            }
            let model_lower = model_id.to_ascii_lowercase();
            if model_lower.contains("gemini") {
                vec![DecisionProtocol::Gemini, DecisionProtocol::Decisions]
            } else if trimmed.contains("api.openai.com") || model_lower.contains("luna") {
                vec![DecisionProtocol::Decisions]
            } else if ["jev", "kev", "laya", "julia", "clef", "systemone"]
                .iter()
                .any(|hint| model_lower.contains(hint))
            {
                vec![
                    DecisionProtocol::Systemone,
                    DecisionProtocol::Decisions,
                    DecisionProtocol::Gemini,
                ]
            } else {
                vec![
                    DecisionProtocol::Decisions,
                    DecisionProtocol::Systemone,
                    DecisionProtocol::Gemini,
                ]
            }
        }
    }
}

fn candidate_options(pool: &[EligibleCandidate]) -> Vec<(String, String, usize)> {
    let mut provider_counts: HashMap<&str, usize> = HashMap::new();
    for c in pool {
        *provider_counts.entry(c.provider.as_str()).or_default() += 1;
    }
    let mut used_keys: HashMap<String, usize> = HashMap::new();
    pool.iter()
        .enumerate()
        .map(|(idx, c)| {
            let base_key = if provider_counts.get(c.provider.as_str()).copied().unwrap_or(0) > 1 {
                format!(
                    "{} · {}",
                    c.provider,
                    c.model.as_deref().unwrap_or("default")
                )
            } else {
                c.provider.clone()
            };
            let count = used_keys.entry(base_key.clone()).or_default();
            *count += 1;
            let key = if *count == 1 {
                base_key
            } else {
                format!("{base_key} #{count}")
            };
            let mut parts = vec![format!("provider={}", c.provider)];
            if let Some(m) = &c.model {
                parts.push(format!("model={m}"));
            }
            if let Some(mp) = &c.model_provider {
                parts.push(format!("modelProvider={mp}"));
            }
            parts.push(format!("quotaUsed={:.0}%", c.quota_used * 100.0));
            parts.push(format!("activeSessions={}", c.active_sessions));
            if c.exhausted {
                parts.push("exhausted=true".into());
            }
            if let Some(notes) = &c.notes {
                parts.push(format!("notes={notes}"));
            }
            (key, parts.join("; "), idx)
        })
        .collect()
}

fn match_candidate_choice<'a>(
    selected: &str,
    options: &[(String, String, usize)],
    pool: &'a [EligibleCandidate],
) -> Option<&'a EligibleCandidate> {
    let needle = selected.trim();
    if let Some((_, _, idx)) = options
        .iter()
        .find(|(key, _, _)| key.eq_ignore_ascii_case(needle))
    {
        return pool.get(*idx);
    }
    pool.iter().find(|c| c.provider.eq_ignore_ascii_case(needle))
}

enum ProtocolError {
    UnsupportedRoute(String),
    Other(String),
}

enum AuthStyle {
    Bearer,
    GoogApiKey,
}

async fn post_decision_json(
    client: &reqwest::Client,
    url: &str,
    api_key: Option<&str>,
    auth_style: AuthStyle,
    payload: &Value,
) -> Result<Value, ProtocolError> {
    let mut req = client.post(url).json(payload);
    if let Some(key) = api_key {
        req = match auth_style {
            AuthStyle::Bearer => req.bearer_auth(key),
            AuthStyle::GoogApiKey => req.header("x-goog-api-key", key),
        };
    }
    let response = req
        .send()
        .await
        .map_err(|e| ProtocolError::Other(format!("decision request failed: {e}")))?;
    let status = response.status();
    if matches!(status.as_u16(), 404 | 405 | 501) {
        return Err(ProtocolError::UnsupportedRoute(format!(
            "decision endpoint returned HTTP {}",
            status.as_u16()
        )));
    }
    if !status.is_success() {
        return Err(ProtocolError::Other(format!(
            "decision endpoint returned HTTP {}",
            status.as_u16()
        )));
    }
    response
        .json::<Value>()
        .await
        .map_err(|_| ProtocolError::Other("invalid JSON from decision endpoint".to_owned()))
}

async fn call_openai_decisions_api(
    client: &reqwest::Client,
    base_url: &str,
    api_key: Option<&str>,
    decision_model_id: &str,
    input_text: &str,
    instructions: &str,
    options: &[(String, String, usize)],
    pool: &[EligibleCandidate],
    default_models: &HashMap<String, String>,
    mode: Mode,
) -> Result<RoutingDecision, ProtocolError> {
    let choices: Vec<Value> = options
        .iter()
        .map(|(key, desc, _)| json!({"value": key, "description": desc}))
        .collect();
    let url = decision_endpoint_url(base_url, "v1", "decisions");
    let body = post_decision_json(
        client,
        &url,
        api_key,
        AuthStyle::Bearer,
        &json!({
            "model": decision_model_id,
            "input": input_text,
            "questions": [{
                "type": "choice",
                "name": "candidate",
                "instructions": instructions,
                "choices": choices,
            }],
        }),
    )
    .await?;

    let answers = body["answers"]
        .as_array()
        .ok_or_else(|| ProtocolError::Other("decisions response missing answers array".into()))?;
    let answer = answers
        .iter()
        .find(|a| a["name"] == "candidate")
        .or_else(|| answers.first())
        .ok_or_else(|| ProtocolError::Other("decisions response returned empty answers".into()))?;
    if answer["type"] == "refusal" {
        return Err(ProtocolError::Other("decision model refused evaluation".into()));
    }
    let selected = answer["selected"]
        .as_str()
        .or_else(|| answer["choice"].as_str())
        .ok_or_else(|| ProtocolError::Other("decisions answer missing selected choice".into()))?;
    let candidate = match_candidate_choice(selected, options, pool).ok_or_else(|| {
        ProtocolError::Other(format!(
            "decisions model chose ineligible option `{selected}`"
        ))
    })?;
    let confidence = answer["confidence"].as_f64();
    let reason = confidence.map(|c| {
        format!(
            "OpenAI Decisions selected `{selected}` (confidence {:.0}%)",
            c * 100.0
        )
    });
    let model = candidate
        .model
        .clone()
        .or_else(|| default_models.get(&candidate.provider).cloned())
        .filter(|m| !m.is_empty());

    Ok(RoutingDecision {
        mode: mode.as_str().into(),
        approach: Approach::DecisionModel.as_str().into(),
        provider: candidate.provider.clone(),
        model,
        model_provider: candidate.model_provider.clone(),
        fallback: false,
        reason,
    })
}

/// Shared caller for criteria-map decision APIs:
/// - Gemini Decisions (`/v1beta/decisions` using `input` + `x-goog-api-key`)
/// - Jev / llama.cpp System One (`/v1/systemone` using `state` + Bearer)
async fn call_criteria_decision_api(
    client: &reqwest::Client,
    url: &str,
    api_key: Option<&str>,
    auth_style: AuthStyle,
    context_field: &str,
    label: &str,
    decision_model_id: &str,
    context_text: &str,
    instructions: &str,
    options: &[(String, String, usize)],
    pool: &[EligibleCandidate],
    default_models: &HashMap<String, String>,
    mode: Mode,
) -> Result<RoutingDecision, ProtocolError> {
    let mut criteria = serde_json::Map::new();
    for (key, desc, _) in options {
        criteria.insert(key.clone(), json!(desc));
    }
    let mut payload = json!({
        "model": decision_model_id,
        "questions": {
            "candidate": {
                "type": "choice",
                "instructions": instructions,
                "criteria": criteria,
            }
        },
    });
    payload[context_field] = json!(context_text);

    let body = post_decision_json(client, url, api_key, auth_style, &payload).await?;

    let answer = &body["answers"]["candidate"];
    if !answer.is_object() {
        return Err(ProtocolError::Other(format!(
            "{label} response missing answers.candidate"
        )));
    }
    let selected = answer["choice"]
        .as_str()
        .or_else(|| answer["selected"].as_str())
        .ok_or_else(|| ProtocolError::Other(format!("{label} answer missing choice")))?;
    let candidate = match_candidate_choice(selected, options, pool).ok_or_else(|| {
        ProtocolError::Other(format!("{label} model chose ineligible option `{selected}`"))
    })?;
    let prob = answer["probabilities"][selected].as_f64();
    let confidence = answer["confidence"].as_f64();
    let reason = match (prob, confidence) {
        (Some(p), Some(c)) => Some(format!(
            "{label} selected `{selected}` (p={:.0}%, confidence {:.0}%)",
            p * 100.0,
            c * 100.0
        )),
        (Some(p), None) => Some(format!(
            "{label} selected `{selected}` (p={:.0}%)",
            p * 100.0
        )),
        (None, Some(c)) => Some(format!(
            "{label} selected `{selected}` (confidence {:.0}%)",
            c * 100.0
        )),
        (None, None) => Some(format!("{label} selected `{selected}`")),
    };
    let model = candidate
        .model
        .clone()
        .or_else(|| default_models.get(&candidate.provider).cloned())
        .filter(|m| !m.is_empty());

    Ok(RoutingDecision {
        mode: mode.as_str().into(),
        approach: Approach::DecisionModel.as_str().into(),
        provider: candidate.provider.clone(),
        model,
        model_provider: candidate.model_provider.clone(),
        fallback: false,
        reason,
    })
}

async fn call_decision_model(
    effective: &Settings,
    input: &RouteInput<'_>,
    project_name: Option<&str>,
    pool: &[EligibleCandidate],
    default_models: &HashMap<String, String>,
    host_id: &str,
) -> Result<RoutingDecision, String> {
    let dm = effective
        .decision_model
        .as_ref()
        .ok_or_else(|| "Decision model is not configured".to_owned())?;

    let (base_url, api_key, default_dm_model) = if let Some(id) = dm.model_provider_id.as_deref() {
        let conn = agentworks_runtime::model_providers::server_connection(id)?;
        if conn.profile.kind != "openai-compatible" {
            return Err("Decision model requires an OpenAI-compatible provider".into());
        }
        (
            conn.profile.base_url,
            conn.api_key,
            Some(conn.profile.default_model),
        )
    } else if let Some(url) = dm.base_url.as_deref() {
        (url.to_owned(), None, None)
    } else {
        return Err("Decision model has no provider or URL configured".into());
    };

    let decision_model_id = dm
        .model
        .as_deref()
        .filter(|m| !m.is_empty())
        .or(default_dm_model.as_deref())
        .filter(|m| !m.is_empty())
        .ok_or_else(|| "Decision model ID is not configured".to_owned())?;

    let candidates_json: Vec<Value> = pool
        .iter()
        .map(|c| {
            json!({
                "provider": c.provider,
                "defaultModel": c.model,
                "modelProvider": c.model_provider,
                "quotaFamily": c.family,
                "notes": c.notes,
                "quotaUsedPercent": (c.quota_used * 100.0).round(),
                "exhausted": c.exhausted,
                "activeSessions": c.active_sessions,
            })
        })
        .collect();

    let clip = |s: &str, max: usize| -> String {
        let trimmed = s.trim();
        if trimmed.chars().count() <= max {
            trimmed.to_owned()
        } else {
            trimmed.chars().take(max).collect::<String>() + "…"
        }
    };

    let task_context = json!({
        "title": input.title.map(|s| clip(s, 200)),
        "description": input.description.map(|s| clip(s, 2000)),
        "prompt": input.prompt.map(|s| clip(s, 4000)),
        "project": project_name,
        "cwd": input.cwd,
        "hostId": host_id,
        "role": input.role_name.map(|name| json!({
            "name": name,
            "description": input.role_description,
        })),
        "candidates": candidates_json,
    });

    let mut base_instructions = String::from(
        "Choose the best eligible agent/model candidate for this new session based on the task title, description/prompt, role, candidate notes, quota usage, and active sessions. Prefer non-exhausted candidates and balance load when options are otherwise comparable.",
    );
    if let Some(extra) = dm.instructions.as_deref()
        && !extra.trim().is_empty()
    {
        base_instructions.push_str(" Operator instructions: ");
        base_instructions.push_str(extra.trim());
    }

    let options = candidate_options(pool);
    let state_text = serde_json::to_string_pretty(&task_context)
        .unwrap_or_else(|_| task_context.to_string());

    let timeout_ms = dm
        .timeout_ms
        .unwrap_or(DEFAULT_DECISION_TIMEOUT_MS)
        .clamp(MIN_DECISION_TIMEOUT_MS, MAX_DECISION_TIMEOUT_MS);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(timeout_ms))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| "Could not construct HTTP client".to_owned())?;

    let sequence = protocol_sequence(dm.protocol, &base_url, decision_model_id);
    let mut last_err = String::from("no decision protocol succeeded");
    for proto in sequence {
        let attempt = match proto {
            DecisionProtocol::Decisions => {
                call_openai_decisions_api(
                    &client,
                    &base_url,
                    api_key.as_deref(),
                    decision_model_id,
                    &state_text,
                    &base_instructions,
                    &options,
                    pool,
                    default_models,
                    effective.mode,
                )
                .await
            }
            DecisionProtocol::Gemini => {
                let url = decision_endpoint_url(&base_url, "v1beta", "decisions");
                call_criteria_decision_api(
                    &client,
                    &url,
                    api_key.as_deref(),
                    AuthStyle::GoogApiKey,
                    "input",
                    "Gemini Decisions",
                    decision_model_id,
                    &state_text,
                    &base_instructions,
                    &options,
                    pool,
                    default_models,
                    effective.mode,
                )
                .await
            }
            DecisionProtocol::Systemone => {
                let url = decision_endpoint_url(&base_url, "v1", "systemone");
                call_criteria_decision_api(
                    &client,
                    &url,
                    api_key.as_deref(),
                    AuthStyle::Bearer,
                    "state",
                    "System One",
                    decision_model_id,
                    &state_text,
                    &base_instructions,
                    &options,
                    pool,
                    default_models,
                    effective.mode,
                )
                .await
            }
            DecisionProtocol::Auto => continue,
        };
        match attempt {
            Ok(decision) => return Ok(decision),
            Err(ProtocolError::UnsupportedRoute(msg)) => {
                last_err = msg;
                continue;
            }
            Err(ProtocolError::Other(msg)) => return Err(msg),
        }
    }
    Err(last_err)
}

pub async fn route(
    app: &App,
    input: &RouteInput<'_>,
) -> Result<RoutingDecision, (StatusCode, String)> {
    let (effective, hidden_providers, default_models, project_name) = {
        let catalog = app.catalog.lock().unwrap();
        let global = catalog.data.settings.session_routing.as_ref();
        let project = input
            .project_id
            .and_then(|id| catalog.data.projects.iter().find(|p| p.id == id));
        let effective = resolve_settings(
            global,
            project.and_then(|p| p.session_routing.as_ref()),
        );
        (
            effective,
            catalog
                .data
                .settings
                .hidden_providers
                .clone()
                .unwrap_or_default(),
            catalog.data.settings.default_models.clone(),
            project.map(|p| p.name.clone()),
        )
    };

    let pool = build_pool(
        app,
        &effective,
        input.host_id,
        input.requires_role_support,
        &hidden_providers,
        &default_models,
    )?;

    match effective.approach {
        Approach::QuotaRoundRobin => Ok(select_quota_round_robin(
            app,
            &pool,
            effective.round_robin.as_ref(),
            effective.mode,
            false,
            None,
        )),
        Approach::DecisionModel => {
            match call_decision_model(
                &effective,
                input,
                project_name.as_deref(),
                &pool,
                &default_models,
                input.host_id,
            )
            .await
            {
                Ok(decision) => Ok(decision),
                Err(err) => {
                    tracing::warn!(%err, "Decision model routing failed; falling back to quota round-robin");
                    Ok(select_quota_round_robin(
                        app,
                        &pool,
                        effective.round_robin.as_ref(),
                        effective.mode,
                        true,
                        Some(&format!("Decision model unavailable ({err})")),
                    ))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_disable_overrides_project_settings() {
        let global = Settings {
            mode: Mode::Off,
            approach: Approach::DecisionModel,
            ..Default::default()
        };
        let project = ProjectSettings {
            mode: Some(Mode::Always),
            approach: Some(Approach::QuotaRoundRobin),
            ..Default::default()
        };
        let resolved = resolve_settings(Some(&global), Some(&project));
        assert_eq!(resolved.mode, Mode::Off);
    }

    #[test]
    fn project_overrides_when_global_enabled() {
        let global = Settings {
            mode: Mode::Default,
            approach: Approach::QuotaRoundRobin,
            ..Default::default()
        };
        let project = ProjectSettings {
            mode: Some(Mode::Always),
            approach: Some(Approach::DecisionModel),
            ..Default::default()
        };
        let resolved = resolve_settings(Some(&global), Some(&project));
        assert_eq!(resolved.mode, Mode::Always);
        assert_eq!(resolved.approach, Approach::DecisionModel);

        let proj_off = ProjectSettings {
            mode: Some(Mode::Off),
            ..Default::default()
        };
        assert_eq!(resolve_settings(Some(&global), Some(&proj_off)).mode, Mode::Off);
    }

    #[test]
    fn should_route_respects_modes_and_imports() {
        assert!(!should_route(Mode::Off, "", false));
        assert!(!should_route(Mode::Off, "auto", false));
        assert!(should_route(Mode::Default, "", false));
        assert!(should_route(Mode::Default, "auto", false));
        assert!(!should_route(Mode::Default, "claude-rust", false));
        assert!(should_route(Mode::Always, "claude-rust", false));
        assert!(should_route(Mode::Always, "", false));
        assert!(!should_route(Mode::Always, "claude-rust", true));
    }

    #[test]
    fn quota_summary_ignores_expired_windows() {
        let now = 1_700_000_000_000_u64;
        let claude = json!({
            "claude": {
                "windows": {
                    "five_hour": {"utilization": 0.95, "resetsAt": 1_699_999_000_u64, "status": "rejected"},
                    "seven_day": {"utilization": 0.35, "resetsAt": 1_700_100_000_u64, "status": "allowed"}
                }
            }
        });
        let (used, exhausted) = quota_summary(Some(&claude), "claude-rust", now);
        assert!((used - 0.35).abs() < 1e-6);
        assert!(!exhausted);
    }

    #[test]
    fn quota_summary_reads_codex_and_antigravity() {
        let now = 1_700_000_000_000_u64;
        let codex = json!({
            "codex": {
                "limits": {
                    "codex": {
                        "primary": {"usedPercent": {"value": 42.0}, "resetsAt": {"value": 1_700_100_000_u64}},
                        "secondary": {"usedPercent": {"value": 15.0}, "resetsAt": {"value": 1_700_200_000_u64}}
                    }
                }
            }
        });
        let (used, exhausted) = quota_summary(Some(&codex), "codex-rust", now);
        assert!((used - 0.42).abs() < 1e-6);
        assert!(!exhausted);

        let agy = json!({
            "antigravity": {
                "windows": [
                    {"id": "w1", "group": "g", "remainingFraction": 0.2, "resetTime": "2099-01-01T00:00:00Z"}
                ]
            }
        });
        let (used, exhausted) = quota_summary(Some(&agy), "antigravity", now);
        assert!((used - 0.8).abs() < 1e-6);
        assert!(!exhausted);
    }

    struct TestApp {
        app: App,
        root: std::path::PathBuf,
    }

    impl Drop for TestApp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn test_app() -> TestApp {
        let root = std::env::temp_dir().join(format!("agentworks-routing-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let store = std::sync::Arc::new(Mutex::new(
            crate::store::Store::open(&root.join("db.sqlite3")).unwrap(),
        ));
        let catalog = std::sync::Arc::new(Mutex::new(
            crate::Catalog::open(root.join("catalog.json")).unwrap(),
        ));
        let app = App {
            sessions: Default::default(),
            roots: std::sync::Arc::new(vec![root.clone()]),
            port: 4512,
            bind: "127.0.0.1".parse().unwrap(),
            catalog: catalog.clone(),
            discovery: std::sync::Arc::new(tokio::sync::Semaphore::new(1)),
            store,
            run_lock: Default::default(),
            workspace_lock: Default::default(),
            communication_lock: Default::default(),
            mcp_endpoint: "127.0.0.1:4512".parse().unwrap(),
            restart: std::sync::Arc::new(crate::service_restart::Manager::new()),
            notifications: crate::notifications::Hub::new(catalog),
            hosts: crate::hosts::Hosts::new(),
            host_usage: std::sync::Arc::new(crate::host_usage::Hub::default()),
            account_quota: std::sync::Arc::default(),
            router: std::sync::Arc::default(),
            github: std::sync::Arc::new(crate::github::Hub::default()),
            web_auth: Default::default(),
            progress: crate::tool_progress::Hub::empty(),
        };
        TestApp { app, root }
    }

    #[tokio::test]
    async fn quota_round_robin_prioritizes_least_used_and_filters_by_role() {
        let f = test_app();
        f.app.account_quota.observe(
            "local",
            "claude",
            "s-claude",
            now_ms(),
            &json!({
                "sessionUpdate": "usage_update",
                "_meta": {
                    "_claude/rateLimit": {
                        "status": "allowed",
                        "rateLimitType": "five_hour",
                        "unifiedWindows": {
                            "five_hour": {"utilization": 0.75, "resetsAt": 4_000_000_000_u64}
                        }
                    }
                }
            }),
        );
        f.app.account_quota.observe(
            "local",
            "codex",
            "s-codex",
            now_ms(),
            &json!({
                "sessionUpdate": "usage_update",
                "_meta": {
                    "_codex/subscriptionLimits": {
                        "version": 1,
                        "limits": [{
                            "limitId": "codex",
                            "primary": {"usedPercent": {"value": 10.0, "observedAtMs": now_ms()}}
                        }]
                    }
                }
            }),
        );

        let pool = vec![
            EligibleCandidate {
                provider: "claude-rust".into(),
                model: Some("claude-sonnet".into()),
                model_provider: None,
                notes: None,
                family: "claude".into(),
                quota_used: 0.75,
                exhausted: false,
                session_load: 0.0,
                active_sessions: 0,
                candidate_key: candidate_key("claude-rust", None, Some("claude-sonnet")),
            },
            EligibleCandidate {
                provider: "codex-rust".into(),
                model: Some("codex-mini".into()),
                model_provider: None,
                notes: None,
                family: "codex".into(),
                quota_used: 0.10,
                exhausted: false,
                session_load: 0.0,
                active_sessions: 0,
                candidate_key: candidate_key("codex-rust", None, Some("codex-mini")),
            },
        ];
        let picked = select_quota_round_robin(&f.app, &pool, None, Mode::Default, false, None);
        assert_eq!(picked.provider, "codex-rust");

        // Strict round-robin rotates evenly when neither candidate exceeds the quota limit.
        let strict = RoundRobinConfig {
            strategy: RoundRobinStrategy::StrictRoundRobin,
            quota_limit_percent: 90,
        };
        let first = select_quota_round_robin(&f.app, &pool, Some(&strict), Mode::Default, false, None);
        let second = select_quota_round_robin(&f.app, &pool, Some(&strict), Mode::Default, false, None);
        assert_ne!(first.provider, second.provider);

        // Quota limit threshold marks claude-rust exhausted once quota >= 70%.
        let tight = RoundRobinConfig {
            strategy: RoundRobinStrategy::StrictRoundRobin,
            quota_limit_percent: 70,
        };
        let capped_a = select_quota_round_robin(&f.app, &pool, Some(&tight), Mode::Default, false, None);
        let capped_b = select_quota_round_robin(&f.app, &pool, Some(&tight), Mode::Default, false, None);
        assert_eq!(capped_a.provider, "codex-rust");
        assert_eq!(capped_b.provider, "codex-rust");

        // Role requirement filters out opencode while keeping demo.
        {
            let mut catalog = f.app.catalog.lock().unwrap();
            let mut data = catalog.data.clone();
            data.settings.session_routing = Some(Settings {
                mode: Mode::Default,
                approach: Approach::QuotaRoundRobin,
                candidates: vec![
                    Candidate {
                        provider: "opencode".into(),
                        model: None,
                        model_provider: None,
                        notes: None,
                        enabled: true,
                    },
                    Candidate {
                        provider: "demo".into(),
                        model: Some("demo-role-model".into()),
                        model_provider: None,
                        notes: None,
                        enabled: true,
                    },
                ],
                round_robin: None,
                decision_model: None,
            });
            catalog.save(data).unwrap();
        }
        let decision = route(
            &f.app,
            &RouteInput {
                project_id: None,
                host_id: "local",
                title: Some("Role test"),
                description: None,
                prompt: None,
                role_name: Some("Reviewer"),
                role_description: None,
                requires_role_support: true,
                cwd: &f.root.to_string_lossy(),
            },
        )
        .await
        .unwrap();
        assert_eq!(decision.provider, "demo");
        assert_eq!(decision.model.as_deref(), Some("demo-role-model"));
    }

    #[test]
    fn decision_endpoints_and_auto_protocol_selection() {
        assert_eq!(
            decision_endpoint_url("https://api.openai.com/v1", "v1", "decisions"),
            "https://api.openai.com/v1/decisions"
        );
        assert_eq!(
            decision_endpoint_url("https://generativelanguage.googleapis.com", "v1beta", "decisions"),
            "https://generativelanguage.googleapis.com/v1beta/decisions"
        );
        assert_eq!(
            decision_endpoint_url("https://generativelanguage.googleapis.com/v1beta", "v1beta", "decisions"),
            "https://generativelanguage.googleapis.com/v1beta/decisions"
        );
        assert_eq!(
            decision_endpoint_url("http://127.0.0.1:8080", "v1", "systemone"),
            "http://127.0.0.1:8080/v1/systemone"
        );
        assert_eq!(
            decision_endpoint_url("http://127.0.0.1:8080/v1", "v1", "systemone"),
            "http://127.0.0.1:8080/v1/systemone"
        );
        assert_eq!(
            decision_endpoint_url("http://127.0.0.1:8080/v1/decisions", "v1", "systemone"),
            "http://127.0.0.1:8080/v1/systemone"
        );
        assert_eq!(
            protocol_sequence(DecisionProtocol::Auto, "https://api.openai.com/v1", "gpt-6-luna")[0],
            DecisionProtocol::Decisions
        );
        assert_eq!(
            protocol_sequence(
                DecisionProtocol::Auto,
                "https://generativelanguage.googleapis.com/v1beta",
                "gemini-decision-1"
            )[0],
            DecisionProtocol::Gemini
        );
        assert_eq!(
            protocol_sequence(DecisionProtocol::Auto, "http://127.0.0.1:8080", "kev-4b")[0],
            DecisionProtocol::Systemone
        );
    }
}
