//! Rust-owned tool output handling for parser scripts.
//!
//! Mirrors the U-05 browser implementation (web/app.js, "U-05 built-in Bazel
//! activity"): ACP `content`/`rawOutput`/`terminal_output` are snapshots, only
//! `terminal_output_delta` appends, persisted sequence numbers deduplicate replay,
//! ANSI/OSC and CR/LF are handled here, and lines are bounded. The script sees
//! `on_line(ctx, line)` and mutates `ctx.state` in place. A parser failure
//! disables derived progress for that tool and leaves raw output, status and
//! exit metadata untouched.

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};

use crate::engine::{CallStats, Compiled, Diagnostic, Engine};

const MAX_LINE: usize = 16 * 1024;
/// Raw output retained per stream while a tool's command is still unknown.
const PENDING_RAW: usize = 1024 * 1024;
const MAX_COUNT: u64 = 9_007_199_254_740_991;

/// Script-owned, temporary per-stream state: plain data only. Kept in memory
/// for the stream's lifetime and rebuilt by replay; never persisted.
pub type State = serde_json::Map<String, Json>;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Counts {
    pub done: u64,
    pub total: u64,
    pub unit: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Progress {
    pub phase: String,
    pub label: Option<String>,
    pub counts: Vec<Counts>,
}

impl Progress {
    /// Host validation of an untrusted script result.
    pub fn validate(&self) -> Result<(), String> {
        if self.counts.len() > 8 {
            return Err("more than 8 count entries".into());
        }
        for c in &self.counts {
            if c.done > c.total || c.total > MAX_COUNT || c.unit.is_empty() || c.unit.len() > 32 {
                return Err(format!("invalid counts {} / {} {:?}", c.done, c.total, c.unit));
            }
        }
        if self.phase.is_empty() || self.phase.len() > 64 || self.label.as_ref().is_some_and(|l| l.len() > 256) {
            return Err("progress phase/label out of bounds".into());
        }
        Ok(())
    }
}

/// Read-only host facts for one call; mirrors the prelude's `LineContext`.
#[derive(Clone, Copy, Debug)]
pub struct LineContext<'a> {
    pub command: &'a str,
    pub stream: &'a str,
    /// The unterminated current line: state changes must not be kept.
    pub provisional: bool,
}

/// One line interpretation step. Implemented by Starlark scripts and by the
/// built-in Rust baseline used for comparison. Implementations update `state`
/// in place only for committed lines, and leave it untouched on error.
pub trait LineParser {
    /// Short display name shown before the phase, e.g. "Bazel".
    fn display_name(&self) -> &str;
    /// Host routing: whether this parser receives a tool command's output.
    fn matches(&self, command: &str) -> bool;
    fn parse(&self, ctx: &LineContext, state: &mut State, line: &str) -> Result<Option<Progress>, Diagnostic>;
}

/// Chooses the parser for a command; the first match wins.
pub trait ParserSet {
    fn select(&self, command: &str) -> Option<&dyn LineParser>;
}

impl<T: LineParser> ParserSet for T {
    fn select(&self, command: &str) -> Option<&dyn LineParser> {
        self.matches(command).then_some(self as &dyn LineParser)
    }
}

impl ParserSet for dyn LineParser + '_ {
    fn select(&self, command: &str) -> Option<&dyn LineParser> {
        self.matches(command).then_some(self)
    }
}

impl<T: LineParser> ParserSet for [T] {
    fn select(&self, command: &str) -> Option<&dyn LineParser> {
        self.iter().find(|p| p.matches(command)).map(|p| p as &dyn LineParser)
    }
}

pub struct StarlarkParser {
    pub engine: Arc<Engine>,
    pub script: Arc<Compiled>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CallResult {
    result: Option<Progress>,
    state: Option<State>,
    #[serde(default, rename = "badgeEffects")]
    badge_effects: Vec<Json>,
}

impl StarlarkParser {
    pub fn parse_with_stats(
        &self,
        ctx: &LineContext,
        state: &mut State,
        line: &str,
    ) -> Result<(Option<Progress>, CallStats), Diagnostic> {
        let input = json!({"command": ctx.command, "stream": ctx.stream, "provisional": ctx.provisional,
            "state": Json::Object(state.clone())});
        let (value, stats) = self.engine.call(&self.script, &input, &Json::String(line.to_owned()))?;
        let result: CallResult = serde_json::from_value(value)
            .map_err(|e| Engine::invalid(&self.script, format!("result does not match host API v1: {e}")))?;
        if let Some(p) = &result.result {
            p.validate().map_err(|e| Engine::invalid(&self.script, e))?;
        }
        if let Some(next) = result.state {
            *state = next;
        }
        self.engine.publish_badge_effects(&self.script, &result.badge_effects);
        Ok((result.result, stats))
    }
}

impl LineParser for StarlarkParser {
    fn display_name(&self) -> &str {
        &self.script.display_name
    }
    fn matches(&self, command: &str) -> bool {
        self.script
            .command_pattern
            .as_ref()
            .is_some_and(|r| r.is_match(command))
    }
    fn parse(&self, ctx: &LineContext, state: &mut State, line: &str) -> Result<Option<Progress>, Diagnostic> {
        self.parse_with_stats(ctx, state, line).map(|(p, _)| p)
    }
}

/// Comparison baseline: the same rules as built-in Rust code with no interpreter.
pub struct BuiltinBazel;

struct BuiltinPatterns {
    phase: regex::Regex,
    actions: regex::Regex,
    tests: regex::Regex,
}

fn builtin_patterns() -> &'static BuiltinPatterns {
    static P: OnceLock<BuiltinPatterns> = OnceLock::new();
    P.get_or_init(|| {
        let n = r"(\d{1,3}(?:,\d{3})+|\d+)";
        BuiltinPatterns {
            phase: regex::Regex::new(
                r"(?i)^(?:INFO:\s*)?(Loading|Analyzing|Fetching|Building|Testing|Computing (?:main repo|repository) mapping):",
            )
            .unwrap(),
            actions: regex::Regex::new(&format!(r"^\s*\[{n}\s*/\s*{n}\](.*)$")).unwrap(),
            tests: regex::Regex::new(&format!(r"(?i)(?:^|[;\]\s]){n}\s*/\s*{n}\s+tests\b")).unwrap(),
        }
    })
}

impl LineParser for BuiltinBazel {
    fn display_name(&self) -> &str {
        "Bazel"
    }
    fn matches(&self, command: &str) -> bool {
        is_bazel(command)
    }
    fn parse(&self, ctx: &LineContext, state: &mut State, line: &str) -> Result<Option<Progress>, Diagnostic> {
        let p = builtin_patterns();
        let mut scratch;
        let state = if ctx.provisional {
            scratch = state.clone();
            &mut scratch
        } else {
            state
        };
        let mut label = None;
        let counts = |d: &str, t: &str| {
            let d: u64 = d.replace(',', "").parse().ok()?;
            let t: u64 = t.replace(',', "").parse().ok()?;
            (d <= t && t <= MAX_COUNT).then(|| json!({"done": d, "total": t}))
        };
        if let Some(m) = p.phase.captures(line) {
            state.insert("phase".into(), json!(&m[1]));
        }
        if let Some(m) = p.actions.captures(line)
            && let Some(c) = counts(&m[1], &m[2])
        {
            state.insert("actions".into(), c);
            state.insert("phase".into(), json!("Executing"));
            let l: String = m[3].trim().chars().take(200).collect();
            label = (!l.is_empty()).then_some(l);
        }
        if let Some(m) = p.tests.captures(line)
            && let Some(c) = counts(&m[1], &m[2])
        {
            state.insert("tests".into(), c);
            state.insert("phase".into(), json!("Testing"));
        }
        let Some(phase) = state.get("phase").and_then(Json::as_str) else {
            return Ok(None);
        };
        let counts = ["actions", "tests"]
            .iter()
            .filter_map(|u| {
                let c = state.get(*u)?;
                Some(Counts {
                    done: c["done"].as_u64()?,
                    total: c["total"].as_u64()?,
                    unit: (*u).into(),
                })
            })
            .collect();
        Ok(Some(Progress {
            phase: phase.into(),
            label,
            counts,
        }))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(clippy::enum_variant_names)]
enum Escape {
    #[default]
    None,
    Esc,
    Csi,
    Osc,
    OscEscape,
}

#[derive(Clone, Debug, Default)]
pub struct Stream {
    /// Exact output as received; never stripped or rewritten.
    pub raw: String,
    line: String,
    escape: Escape,
    overflow: bool,
    committed: State,
    pub progress: Option<Progress>,
    version: u64,
}

/// Derived progress for one tool call, rebuilt from persisted events.
#[derive(Debug, Default)]
pub struct ToolProgress {
    pub streams: BTreeMap<String, Stream>,
    pub status: Option<String>,
    pub exit: Option<Json>,
    pub command: Option<String>,
    /// Set on the first parser failure; progress is then withheld for this tool.
    pub parser_failure: Option<Diagnostic>,
    /// Display name of the parser handling this tool, once its command matched.
    pub parser: Option<String>,
    /// Keep raw output only until the command is known. The service sets this:
    /// it already persists raw events, so derived state needs no second copy.
    pub drop_raw: bool,
    seq: Option<i64>,
    version: u64,
    pub calls: u64,
}

pub fn is_bazel(command: &str) -> bool {
    static R: OnceLock<regex::Regex> = OnceLock::new();
    R.get_or_init(|| regex::Regex::new(r#"(?:^|[\s;&|"'])(?:[^\s;&|"']*/)?bazel(?:isk)?(?:[\s"']|$)"#).unwrap())
        .is_match(command)
}

fn output_text(content: &Json) -> Option<String> {
    match content {
        Json::String(s) => Some(s.clone()),
        Json::Array(items) => {
            let texts: Vec<&str> = items
                .iter()
                .filter(|c| c["type"] == "content" && c["content"]["type"] == "text")
                .filter_map(|c| c["content"]["text"].as_str())
                .collect();
            (!texts.is_empty()).then(|| texts.join("\n"))
        }
        _ => None,
    }
}

impl ToolProgress {
    /// Derived state for a service that persists raw events itself (`drop_raw`).
    pub fn without_raw() -> Self {
        ToolProgress {
            drop_raw: true,
            ..ToolProgress::default()
        }
    }

    /// Apply one persisted ACP tool update. Returns false for duplicates.
    pub fn apply<P: ParserSet + ?Sized>(&mut self, parsers: &P, update: &Json, seq: Option<i64>) -> bool {
        if let (Some(seq), Some(last)) = (seq, self.seq)
            && seq <= last
        {
            return false;
        }
        if seq.is_some() {
            self.seq = seq;
        }
        let was_known = self.command.is_some();
        if let Some(s) = update.get("status").and_then(Json::as_str) {
            self.status = Some(s.to_owned());
        }
        if let Some(input) = update.get("rawInput") {
            let command = input
                .as_str()
                .or_else(|| input["command"].as_str())
                .or_else(|| input["cmd"].as_str());
            if let Some(c) = command {
                self.command = Some(c.to_owned());
            }
        }
        if self.command.is_none()
            && let Some(t) = update.get("title").and_then(Json::as_str)
        {
            self.command = Some(t.to_owned());
        }
        if !was_known && self.command.is_some() {
            match self.active(parsers) {
                Some(parser) => {
                    self.parser = Some(parser.display_name().to_owned());
                    self.reparse(parser);
                }
                // No parser handles this command: nothing derived, nothing to keep.
                None if self.drop_raw => self.streams.clear(),
                None => {}
            }
        }
        if let Some(content) = update.get("content") {
            self.streams.remove("content");
            if let Some(text) = output_text(content) {
                self.write(parsers, "content", &text, false);
            }
        }
        if let Some(raw) = update.get("rawOutput") {
            self.streams.remove("rawOutput");
            let text = match raw {
                Json::String(s) => Some(s.clone()),
                Json::Object(o) => Some(
                    ["stdout", "stderr", "output"]
                        .iter()
                        .filter_map(|k| o.get(*k).and_then(Json::as_str))
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
                _ => None,
            };
            if let Some(text) = text {
                self.write(parsers, "rawOutput", &text, false);
            }
        }
        let meta = update.get("_meta");
        for (key, delta) in [("terminal_output", false), ("terminal_output_delta", true)] {
            let value = meta.and_then(|m| m.get(key));
            if let (Some(id), Some(data)) = (
                value.and_then(|v| v["terminal_id"].as_str()),
                value.and_then(|v| v["data"].as_str()),
            ) {
                self.write(parsers, &format!("terminal:{id}"), data, delta);
            }
        }
        if let Some(exit) = meta.and_then(|m| m.get("terminal_exit"))
            && exit["terminal_id"].is_string()
        {
            self.exit = Some(exit.clone());
        }
        true
    }

    fn active<'p, P: ParserSet + ?Sized>(&self, parsers: &'p P) -> Option<&'p dyn LineParser> {
        if self.parser_failure.is_some() {
            return None;
        }
        parsers.select(self.command.as_deref()?)
    }

    /// Output can arrive before the tool call names its command. Once it is
    /// recognised, rebuild derived state from the retained raw output.
    fn reparse(&mut self, parser: &dyn LineParser) {
        let keys: Vec<String> = self.streams.keys().cloned().collect();
        for key in keys {
            let old = self.streams.remove(&key).unwrap();
            let mut stream = Stream {
                raw: if self.drop_raw { String::new() } else { old.raw.clone() },
                version: old.version,
                ..Stream::default()
            };
            let result = self.feed(parser, &key, &mut stream, &old.raw);
            self.streams.insert(key, stream);
            if let Err(d) = result {
                for s in self.streams.values_mut() {
                    s.progress = None;
                }
                self.parser_failure = Some(d);
                return;
            }
        }
    }

    fn write<P: ParserSet + ?Sized>(&mut self, parsers: &P, key: &str, data: &str, delta: bool) {
        let parser = self.active(parsers);
        if self.drop_raw && parser.is_none() && (self.command.is_some() || self.parser_failure.is_some()) {
            return;
        }
        self.version += 1;
        let version = self.version;
        let mut stream = match self.streams.remove(key) {
            Some(s) if delta => s,
            _ => Stream::default(),
        };
        if !self.drop_raw {
            stream.raw.push_str(data);
        } else if parser.is_none() && stream.raw.len() + data.len() <= PENDING_RAW {
            // Needed to parse output that arrives before the command is known.
            stream.raw.push_str(data);
        }
        stream.version = version;
        let mut failure = None;
        if let Some(parser) = parser
            && let Err(d) = self.feed(parser, key, &mut stream, data)
        {
            failure = Some(d);
        }
        self.streams.insert(key.to_owned(), stream);
        if let Some(d) = failure {
            // Isolation: drop derived progress for this tool, keep everything else.
            for s in self.streams.values_mut() {
                s.progress = None;
            }
            self.parser_failure = Some(d);
        }
    }

    fn feed(&mut self, parser: &dyn LineParser, key: &str, stream: &mut Stream, data: &str) -> Result<(), Diagnostic> {
        let command = self.command.clone().unwrap_or_default();
        let ctx = |provisional| LineContext {
            command: &command,
            stream: key,
            provisional,
        };
        for c in data.chars() {
            match stream.escape {
                Escape::Osc => {
                    stream.escape = match c {
                        '\x07' => Escape::None,
                        '\x1b' => Escape::OscEscape,
                        _ => Escape::Osc,
                    };
                    continue;
                }
                Escape::OscEscape => {
                    stream.escape = if c == '\\' { Escape::None } else { Escape::Osc };
                    continue;
                }
                Escape::Csi => {
                    if ('@'..='~').contains(&c) {
                        stream.escape = Escape::None;
                    }
                    continue;
                }
                Escape::Esc => {
                    stream.escape = match c {
                        '[' => Escape::Csi,
                        ']' => Escape::Osc,
                        _ => Escape::None,
                    };
                    continue;
                }
                Escape::None => {}
            }
            match c {
                '\x1b' => stream.escape = Escape::Esc,
                '\r' | '\n' => {
                    if !stream.overflow {
                        self.calls += 1;
                        parser.parse(&ctx(false), &mut stream.committed, &stream.line)?;
                    }
                    stream.line.clear();
                    stream.overflow = false;
                }
                c if c >= ' ' || c == '\t' => {
                    if stream.line.len() + c.len_utf8() <= MAX_LINE {
                        stream.line.push(c);
                    } else {
                        stream.overflow = true;
                    }
                }
                _ => {}
            }
        }
        // Provisional progress includes the unterminated line without committing it.
        self.calls += 1;
        let line = if stream.overflow { "" } else { stream.line.as_str() };
        stream.progress = parser.parse(&ctx(true), &mut stream.committed, line)?;
        Ok(())
    }

    /// Most recently updated stream's progress, if derived progress is available.
    pub fn progress(&self) -> Option<&Progress> {
        if self.parser.is_none() || self.parser_failure.is_some() {
            return None;
        }
        self.streams
            .values()
            .filter(|s| s.progress.is_some())
            .max_by_key(|s| s.version)?
            .progress
            .as_ref()
    }

    pub fn raw(&self, key: &str) -> Option<&str> {
        self.streams.get(key).map(|s| s.raw.as_str())
    }

    /// Compact text in the U-05 format. Tool status and exit stay authoritative.
    pub fn view(&self) -> String {
        let mut parts = Vec::new();
        let stopped =
            self.exit.is_some() || !matches!(self.status.as_deref().unwrap_or("pending"), "pending" | "in_progress");
        match self.progress() {
            Some(p) => {
                parts.push(format!("{} · {}", self.parser.as_deref().unwrap_or_default(), p.phase));
                for unit in ["actions", "tests"] {
                    if let Some(c) = p.counts.iter().find(|c| c.unit == unit) {
                        parts.push(format!("{} / {} {unit} reported", group(c.done), group(c.total)));
                    }
                }
            }
            None if !stopped => parts.push("In progress".into()),
            None => {}
        }
        if let Some(exit) = &self.exit {
            if let Some(code) = exit["exit_code"].as_i64() {
                parts.push(format!("Exit {code}"));
            }
            if let Some(signal) = exit["signal"].as_str() {
                parts.push(format!("Signal {signal}"));
            }
        }
        parts.join(" · ")
    }

    pub fn snapshot(&self) -> Json {
        json!({"status": self.status, "progress": self.progress(), "exit": self.exit,
            "parserFailure": self.parser_failure.as_ref().map(|d| d.to_string())})
    }

    /// Derived view published to browsers: parser name, progress and any
    /// attributable parser failure. Tool status and exit come from raw events.
    pub fn derived(&self) -> Json {
        json!({"parser": self.parser, "progress": self.progress(),
            "parserFailure": self.parser_failure.as_ref().map(|d| d.to_string())})
    }
}

fn group(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}
