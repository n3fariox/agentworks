//! Shared embedding: one dialect, typed host preludes, separate capability globals.
//!
//! Typing is enabled at all three levels starlark-rust supports:
//! 1. `DialectTypes::Enable`: annotations are checked at runtime on every call.
//! 2. `AstModuleTypecheck::typecheck`: the whole script is linted on install
//!    against the capability's globals and prelude interface. It does not
//!    evaluate, so annotations naming loaded record types resolve to `typing.Any`.
//! 3. `Evaluator::enable_static_typechecking`: when the module is compiled, every
//!    `def` (called or not) is checked using real values, including prelude records.
//!
//! The host additionally requires the compiled entry point's type to match the
//! versioned host API exactly, so unannotated or mistyped entries are rejected.

mod registration;
pub use registration::{HookInfo, RegisteredScript};

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use starlark::environment::{FrozenModule, Globals, GlobalsBuilder, LibraryExtension, Module};
use starlark::eval::{Evaluator, ReturnFileLoader};
use starlark::starlark_module;
use starlark::syntax::{AstModule, Dialect, DialectTypes};
use starlark::typing::{AstModuleTypecheck, Interface, Ty};
use starlark::values::FrozenHeapName;
use starlark::values::OwnedFrozenValue;
use starlark::values::none::NoneOr;

pub const HOST_API_VERSION: u32 = 1;
const PROGRESS_PRELUDE: &str = "@agentworks//progress.star";
const MESSAGE_PRELUDE: &str = "@agentworks//message.star";
const WORKFLOW_PRELUDE: &str = "@agentworks//workflow.star";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Interprets tool output. Pattern helpers only; no actions.
    Parser,
    /// Proposes workflow actions as data. No pattern helpers, no I/O.
    Workflow,
    /// Formats peer message data as Markdown. No actions or I/O.
    MessageFormatter,
}

impl Capability {
    fn prelude(self) -> (&'static str, &'static str) {
        match self {
            Capability::Parser => (PROGRESS_PRELUDE, include_str!("../host/progress.star")),
            Capability::MessageFormatter => (MESSAGE_PRELUDE, include_str!("../host/message.star")),
            Capability::Workflow => (WORKFLOW_PRELUDE, include_str!("../host/workflow.star")),
        }
    }
    fn entry(self) -> &'static str {
        match self {
            Capability::Parser => "on_line",
            Capability::Workflow => "on_event",
            Capability::MessageFormatter => "format_message",
        }
    }
    /// Host API v1 entry signature, as rendered by the starlark typechecker.
    pub fn signature(self) -> &'static str {
        match self {
            Capability::Parser => "def(ctx: LineContext, line: str) -> None | Progress",
            Capability::Workflow => "def(ctx: WorkflowContext, event: Event) -> list[Action]",
            Capability::MessageFormatter => "def(ctx: MessageContext, message: PeerMessage) -> str",
        }
    }
    /// Prelude record type for the hook's `ctx` argument.
    fn context_type(self) -> &'static str {
        match self {
            Capability::Parser => "LineContext",
            Capability::Workflow => "WorkflowContext",
            Capability::MessageFormatter => "MessageContext",
        }
    }
}

/// Per-evaluation bounds. Tick and heap limits are best-effort inside the
/// process (see starlark-rust docs); `worker` adds OS limits for hard stops.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Limits {
    pub max_source_bytes: usize,
    pub max_input_bytes: usize,
    pub max_state_bytes: usize,
    pub max_output_bytes: usize,
    pub max_ticks: u64,
    pub max_heap_bytes: usize,
    pub max_callstack: usize,
    pub max_wall: Duration,
}

impl Limits {
    pub fn parser() -> Self {
        Limits {
            max_source_bytes: 64 * 1024,
            max_input_bytes: 16 * 1024,
            max_state_bytes: 16 * 1024,
            max_output_bytes: 16 * 1024,
            max_ticks: 20_000,
            max_heap_bytes: 8 * 1024 * 1024,
            max_callstack: 32,
            max_wall: Duration::from_millis(50),
        }
    }
    pub fn workflow() -> Self {
        Limits {
            max_source_bytes: 64 * 1024,
            max_input_bytes: 64 * 1024,
            max_state_bytes: 64 * 1024,
            max_output_bytes: 64 * 1024,
            max_ticks: 50_000,
            max_heap_bytes: 16 * 1024 * 1024,
            max_callstack: 32,
            max_wall: Duration::from_millis(200),
        }
    }
    pub fn for_capability(capability: Capability) -> Self {
        match capability {
            Capability::Parser => Self::parser(),
            Capability::Workflow => Self::workflow(),
            Capability::MessageFormatter => Self::parser(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticKind {
    Install,
    Syntax,
    Type,
    Capability,
    Runtime,
    Limit,
    InvalidResult,
    Panic,
    Worker,
}

/// Attributable failure: which installed script, which version, what went wrong.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub script: String,
    pub hash: String,
    pub kind: DiagnosticKind,
    pub message: String,
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let short = &self.hash[..self.hash.len().min(12)];
        write!(f, "{}@{} {:?}: {}", self.script, short, self.kind, self.message)
    }
}

/// Bound diagnostic text, keeping the start and the final error of long tracebacks.
fn bounded(message: impl std::fmt::Display) -> String {
    let text = message.to_string();
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= 2000 {
        return text;
    }
    let head: String = chars[..600].iter().collect();
    let tail: String = chars[chars.len() - 1200..].iter().collect();
    format!("{head}\n…\n{tail}")
}

pub fn content_hash(source: &str) -> String {
    blake3::hash(source.as_bytes()).to_hex().to_string()
}

/// An explicit installation record. Scripts are never discovered implicitly:
/// the host compiles only enabled installations whose content hash matches.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Installation {
    pub name: String,
    pub capability: Capability,
    pub hash: String,
    pub enabled: bool,
}

impl Installation {
    pub fn of(name: &str, capability: Capability, source: &str) -> Self {
        Installation {
            name: name.into(),
            capability,
            hash: content_hash(source),
            enabled: true,
        }
    }
}

pub fn dialect() -> Dialect {
    Dialect {
        enable_types: DialectTypes::Enable,
        enable_f_strings: true,
        ..Dialect::Standard
    }
}

#[starlark_module]
fn parser_helpers(builder: &mut GlobalsBuilder) {
    /// Host pattern matching: capture groups of the first match, or None.
    /// Uses the linear-time `regex` crate; compiled patterns are cached and bounded.
    fn regex_match(
        #[starlark(require = pos)] pattern: &str,
        #[starlark(require = pos)] text: &str,
    ) -> anyhow::Result<NoneOr<Vec<String>>> {
        let regex = cached_regex(pattern)?;
        Ok(match regex.captures(text) {
            None => NoneOr::None,
            Some(c) => NoneOr::Other(
                c.iter()
                    .skip(1)
                    .map(|m| m.map(|m| m.as_str().to_owned()).unwrap_or_default())
                    .collect(),
            ),
        })
    }
}

/// Shared through `Arc`: cloning a `Regex` would give each call a cold match cache.
fn cached_regex(pattern: &str) -> anyhow::Result<Arc<regex::Regex>> {
    static CACHE: Mutex<Option<HashMap<String, Arc<regex::Regex>>>> = Mutex::new(None);
    anyhow::ensure!(pattern.len() <= 512, "regex_match pattern exceeds 512 bytes");
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let cache = cache.get_or_insert_with(HashMap::new);
    if let Some(r) = cache.get(pattern) {
        return Ok(r.clone());
    }
    let regex = Arc::new(regex::RegexBuilder::new(pattern).size_limit(1 << 20).build()?);
    if cache.len() >= 64 {
        cache.clear();
    }
    cache.insert(pattern.to_owned(), regex.clone());
    Ok(regex)
}

fn globals(capability: Capability) -> Globals {
    // No print/debug/json/breakpoint: scripts have no output channel but their return value.
    let base = [
        LibraryExtension::RecordType,
        LibraryExtension::EnumType,
        LibraryExtension::Typing,
        LibraryExtension::Map,
        LibraryExtension::Filter,
    ];
    let builder = GlobalsBuilder::extended_by(&base);
    match capability {
        Capability::Parser => builder
            .with(parser_helpers)
            .with_namespace("agent", registration::registration_helpers)
            .build(),
        Capability::MessageFormatter => builder
            .with_namespace("agent", registration::registration_helpers)
            .build(),
        Capability::Workflow => builder.build(),
    }
}

struct Prelude {
    id: &'static str,
    module: FrozenModule,
    interface: Interface,
    context: OwnedFrozenValue,
    /// Builds the workflow `Event` record from JSON; parsers receive a plain `str`.
    event: Option<OwnedFrozenValue>,
}

/// A compiled, frozen script. Values are immutable and shared across calls;
/// each call gets its own temporary heap, so no interpreter state accumulates.
pub struct Compiled {
    pub name: String,
    pub hash: String,
    pub capability: Capability,
    entry: OwnedFrozenValue,
    context: OwnedFrozenValue,
    event: Option<OwnedFrozenValue>,
    /// Parser scripts: `DISPLAY_NAME`, shown before the phase. Workflows: the script name.
    pub display_name: String,
    /// Parser scripts: `COMMAND_PATTERN`. Rust routes only matching commands' output.
    pub command_pattern: Option<Arc<regex::Regex>>,
    pub text_pattern: Option<Arc<regex::Regex>>,
    pub message_type: Option<String>,
    pub compile_time: Duration,
}

impl Compiled {
    fn diagnostic(&self, kind: DiagnosticKind, message: impl std::fmt::Display) -> Diagnostic {
        Diagnostic {
            script: self.name.clone(),
            hash: self.hash.clone(),
            kind,
            message: bounded(message),
        }
    }
}

/// Metrics for one evaluation.
#[derive(Clone, Copy, Debug, Default)]
pub struct CallStats {
    pub ticks: u64,
    pub elapsed: Duration,
}

pub struct Engine {
    parser_globals: Globals,
    workflow_globals: Globals,
    message_globals: Globals,
    parser_prelude: Prelude,
    workflow_prelude: Prelude,
    message_prelude: Prelude,
    cache: Mutex<HashMap<String, Arc<Compiled>>>,
    hook_globals: Globals,
    hook_cache: Mutex<HashMap<(String, String), Arc<RegisteredScript>>>,
}

impl Engine {
    pub fn new() -> Result<Self, String> {
        let parser_globals = globals(Capability::Parser);
        let workflow_globals = globals(Capability::Workflow);
        let message_globals = globals(Capability::MessageFormatter);
        let message_prelude = prelude(Capability::MessageFormatter, &message_globals)?;
        let parser_prelude = prelude(Capability::Parser, &parser_globals)?;
        let workflow_prelude = prelude(Capability::Workflow, &workflow_globals)?;
        Ok(Engine {
            parser_globals,
            workflow_globals,
            message_globals,
            message_prelude,
            parser_prelude,
            workflow_prelude,
            cache: Mutex::default(),
            hook_globals: globals(Capability::Parser),
            hook_cache: Mutex::default(),
        })
    }

    fn parts(&self, capability: Capability) -> (&Globals, &Prelude) {
        match capability {
            Capability::Parser => (&self.parser_globals, &self.parser_prelude),
            Capability::Workflow => (&self.workflow_globals, &self.workflow_prelude),
            Capability::MessageFormatter => (&self.message_globals, &self.message_prelude),
        }
    }

    /// Compile an explicitly installed script. Cached by content hash.
    pub fn compile(&self, install: &Installation, source: &str) -> Result<Arc<Compiled>, Diagnostic> {
        let fail = |kind, message: String| Diagnostic {
            script: install.name.clone(),
            hash: install.hash.clone(),
            kind,
            message: bounded(message),
        };
        if !install.enabled {
            return Err(fail(
                DiagnosticKind::Install,
                "script is installed but not enabled".into(),
            ));
        }
        if content_hash(source) != install.hash {
            return Err(fail(
                DiagnosticKind::Install,
                "source does not match the installed content hash".into(),
            ));
        }
        if let Some(c) = self.cache.lock().unwrap().get(&install.hash)
            && c.capability == install.capability
            && c.name == install.name
        {
            return Ok(c.clone());
        }
        let limits = Limits::for_capability(install.capability);
        if source.len() > limits.max_source_bytes {
            return Err(fail(
                DiagnosticKind::Install,
                format!("source exceeds {} bytes", limits.max_source_bytes),
            ));
        }
        let started = Instant::now();
        let (globals, prelude) = self.parts(install.capability);
        let ast = AstModule::parse(&install.name, source.to_owned(), &dialect())
            .map_err(|e| fail(DiagnosticKind::Syntax, e.to_string()))?;
        for load in ast.loads() {
            if load.module_id != prelude.id {
                return Err(fail(
                    DiagnosticKind::Capability,
                    format!(
                        "{} scripts may only load {:?}, not {:?}",
                        cap_name(install.capability),
                        prelude.id,
                        load.module_id
                    ),
                ));
            }
        }
        // Static typecheck of the whole module, including functions never called.
        let loads = HashMap::from([(prelude.id.to_owned(), prelude.interface.clone())]);
        let (errors, _, interface, _) = ast.clone().typecheck(globals, &loads);
        if !errors.is_empty() {
            let text: Vec<String> = errors.iter().take(5).map(|e| e.to_string()).collect();
            return Err(fail(DiagnosticKind::Type, text.join("\n")));
        }
        if interface.get(install.capability.entry()).is_none() {
            return Err(fail(
                DiagnosticKind::Type,
                format!("missing entry point `{}`", install.capability.entry()),
            ));
        }
        let entry_name = install.capability.entry();
        let modules = HashMap::from([(prelude.id, &prelude.module)]);
        let loader = ReturnFileLoader { modules: &modules };
        let module = Module::with_temp_heap(|module| -> Result<FrozenModule, Diagnostic> {
            module.set_extra_value(
                module
                    .heap()
                    .alloc(starlark::values::list::AllocList(Vec::<starlark::values::Value>::new())),
            );
            {
                let mut eval = Evaluator::new(&module);
                eval.set_loader(&loader);
                eval.enable_static_typechecking(true);
                eval.set_max_tick_count(limits.max_ticks)
                    .map_err(|e| fail(DiagnosticKind::Install, e.to_string()))?;
                eval.eval_module(ast, globals)
                    .map_err(|e| fail(DiagnosticKind::Type, e.to_string()))?;
            }
            module
                .freeze_named(FrozenHeapName::User(Box::new(install.name.clone())))
                .map_err(|e| fail(DiagnosticKind::Install, format!("{e:?}")))
        })?;
        let entry = module
            .get(entry_name)
            .map_err(|e| fail(DiagnosticKind::Type, e.to_string()))?;
        // The entry contract uses the compiled function's type: the standalone checker
        // renders loaded record annotations as `typing.Any`, compiled values do not.
        let declared = Ty::of_value(entry.value()).to_string();
        if declared != install.capability.signature() {
            return Err(fail(
                DiagnosticKind::Type,
                format!(
                    "entry point `{entry_name}` must be declared as `{}`; found `{declared}`",
                    install.capability.signature()
                ),
            ));
        }
        let (display_name, command_pattern) = match install.capability {
            Capability::Parser => {
                let constant = |name: &str| -> Result<String, Diagnostic> {
                    let value = module.get_option(name).ok().flatten().ok_or_else(|| {
                        fail(
                            DiagnosticKind::Type,
                            format!("parser scripts must define `{name}: str`"),
                        )
                    })?;
                    value
                        .unpack_str()
                        .map(str::to_owned)
                        .ok_or_else(|| fail(DiagnosticKind::Type, format!("`{name}` must be a str")))
                };
                let display = constant("DISPLAY_NAME")?;
                if display.is_empty() || display.len() > 32 {
                    return Err(fail(DiagnosticKind::Type, "`DISPLAY_NAME` must be 1..=32 bytes".into()));
                }
                let pattern = constant("COMMAND_PATTERN")?;
                let regex = (pattern.len() <= 512)
                    .then(|| regex::RegexBuilder::new(&pattern).size_limit(1 << 20).build().ok())
                    .flatten()
                    .ok_or_else(|| {
                        fail(
                            DiagnosticKind::Type,
                            "`COMMAND_PATTERN` must be a valid regex of at most 512 bytes".into(),
                        )
                    })?;
                (display, Some(Arc::new(regex)))
            }
            Capability::Workflow | Capability::MessageFormatter => (install.name.clone(), None),
        };
        let compiled = Arc::new(Compiled {
            name: install.name.clone(),
            hash: install.hash.clone(),
            capability: install.capability,
            entry,
            context: prelude.context.clone(),
            event: prelude.event.clone(),
            display_name,
            command_pattern,
            text_pattern: None,
            message_type: None,
            compile_time: started.elapsed(),
        });
        self.cache
            .lock()
            .unwrap()
            .insert(install.hash.clone(), compiled.clone());
        Ok(compiled)
    }

    /// Evaluate `entry(ctx, input)` in a fresh heap with limits. `first` holds the
    /// `ctx` fields, including `state`; `second` is the line (parser) or event JSON
    /// (workflow). Returns `{result, state}`; `state` is null for provisional calls.
    /// Never panics; all failures become attributable diagnostics.
    pub fn call(&self, script: &Compiled, first: &Json, second: &Json) -> Result<(Json, CallStats), Diagnostic> {
        let limits = Limits::for_capability(script.capability);
        let (state, input) = (&first["state"], second);
        let state_len = serde_json::to_string(state).map(|s| s.len()).unwrap_or(usize::MAX);
        if state_len > limits.max_state_bytes {
            return Err(script.diagnostic(
                DiagnosticKind::Limit,
                format!("state exceeds {} bytes", limits.max_state_bytes),
            ));
        }
        let input_len = serde_json::to_string(input).map(|s| s.len()).unwrap_or(usize::MAX);
        if input_len > limits.max_input_bytes {
            return Err(script.diagnostic(
                DiagnosticKind::Limit,
                format!("input exceeds {} bytes", limits.max_input_bytes),
            ));
        }
        let outcome = catch_unwind(AssertUnwindSafe(|| self.call_inner(script, &limits, first, second)));
        match outcome {
            Ok(result) => result,
            Err(_) => Err(script.diagnostic(DiagnosticKind::Panic, "interpreter panicked; evaluation discarded")),
        }
    }

    fn call_inner(
        &self,
        script: &Compiled,
        limits: &Limits,
        first: &Json,
        second: &Json,
    ) -> Result<(Json, CallStats), Diagnostic> {
        let started = Instant::now();
        let deadline = started + limits.max_wall;
        Module::with_temp_heap(|module| {
            let mut eval = Evaluator::new(&module);
            let setup = |e: anyhow::Error| script.diagnostic(DiagnosticKind::Runtime, e);
            eval.set_max_tick_count(limits.max_ticks).map_err(setup)?;
            eval.set_max_heap_size(limits.max_heap_bytes).map_err(setup)?;
            eval.set_max_callstack_size(limits.max_callstack).map_err(setup)?;
            eval.set_check_cancelled(Box::new(move || Instant::now() > deadline));
            let heap = module.heap();
            let classify = |e: starlark::Error| {
                let mut text = e.to_string();
                if text.contains("Evaluation cancelled") {
                    text = format!("{text}: wall-clock limit of {:?} exceeded", limits.max_wall);
                }
                let kind = if text.contains("Heap memory limit of")
                    || text.contains("Execution duration limit of")
                    || text.contains("wall-clock limit of")
                    || text.contains("Starlark call stack overflow")
                {
                    DiagnosticKind::Limit
                } else if text.contains("does not match the type annotation") || text.contains("type annotation") {
                    DiagnosticKind::Type
                } else {
                    DiagnosticKind::Runtime
                };
                script.diagnostic(kind, text)
            };
            // SAFETY: the call module's frozen heap now references the script's heap,
            // so these frozen values outlive every use inside this temporary module.
            let (context, entry, event) = unsafe {
                (
                    script.context.owned_frozen_value(module.frozen_heap()).to_value(),
                    script.entry.owned_frozen_value(module.frozen_heap()).to_value(),
                    script
                        .event
                        .as_ref()
                        .map(|e| e.owned_frozen_value(module.frozen_heap()).to_value()),
                )
            };
            // `ctx.state` is a fresh mutable copy of plain data. Nothing from a previous
            // call's heap is referenced, so states never chain heaps across calls.
            let state = heap.alloc(first["state"].clone());
            let mut fields = vec![("state", state)];
            for (key, value) in first.as_object().into_iter().flatten() {
                if key != "state" {
                    fields.push((key.as_str(), heap.alloc(value.clone())));
                }
            }
            let ctx = eval.eval_function(context, &[], &fields).map_err(classify)?;
            let input = match event {
                Some(event) => eval
                    .eval_function(event, &[heap.alloc(second.clone())], &[])
                    .map_err(classify)?,
                None => heap.alloc(second.clone()),
            };
            let result = eval.eval_function(entry, &[ctx, input], &[]).map_err(classify)?;
            let result = result.to_json_value().map_err(|e| {
                script.diagnostic(
                    DiagnosticKind::InvalidResult,
                    format!("result is not JSON-compatible: {e}"),
                )
            })?;
            let size = serde_json::to_string(&result).map(|s| s.len()).unwrap_or(usize::MAX);
            if size > limits.max_output_bytes {
                return Err(script.diagnostic(
                    DiagnosticKind::Limit,
                    format!("result exceeds {} bytes", limits.max_output_bytes),
                ));
            }
            // Provisional parser calls never commit, so their state is not even copied out.
            let committed = if first["provisional"] == true {
                Json::Null
            } else {
                let committed = state.to_json_value().map_err(|e| {
                    script.diagnostic(
                        DiagnosticKind::InvalidResult,
                        format!("ctx.state must hold only plain data: {e}"),
                    )
                })?;
                let size = serde_json::to_string(&committed).map(|s| s.len()).unwrap_or(usize::MAX);
                if size > limits.max_state_bytes {
                    return Err(script.diagnostic(
                        DiagnosticKind::Limit,
                        format!("ctx.state exceeds {} bytes", limits.max_state_bytes),
                    ));
                }
                committed
            };
            let json = serde_json::json!({"result": result, "state": committed});
            Ok((
                json,
                CallStats {
                    ticks: eval.get_total_tick_count(),
                    elapsed: started.elapsed(),
                },
            ))
        })
    }

    /// Construct a diagnostic for a host-side validation failure of a result.
    pub fn invalid(script: &Compiled, message: impl std::fmt::Display) -> Diagnostic {
        script.diagnostic(DiagnosticKind::InvalidResult, message)
    }
}

fn cap_name(c: Capability) -> &'static str {
    match c {
        Capability::Parser => "parser",
        Capability::Workflow => "workflow",
        Capability::MessageFormatter => "message_formatter",
    }
}

fn prelude(capability: Capability, globals: &Globals) -> Result<Prelude, String> {
    let (id, source) = capability.prelude();
    let ast = AstModule::parse(id, source.to_owned(), &dialect()).map_err(|e| e.to_string())?;
    let (errors, ..) = ast.clone().typecheck(globals, &HashMap::new());
    if let Some(e) = errors.first() {
        return Err(format!("host prelude {id} failed typecheck: {e}"));
    }
    let module = Module::with_temp_heap(|module| -> Result<FrozenModule, String> {
        {
            let mut eval = Evaluator::new(&module);
            eval.enable_static_typechecking(true);
            eval.eval_module(ast, globals).map_err(|e| e.to_string())?;
        }
        module
            .freeze_named(FrozenHeapName::User(Box::new(id.to_owned())))
            .map_err(|e| format!("{e:?}"))
    })?;
    let context = module.get(capability.context_type()).map_err(|e| e.to_string())?;
    let event = match capability {
        Capability::Parser => None,
        Capability::MessageFormatter => Some(module.get("message_from_json").map_err(|e| e.to_string())?),
        Capability::Workflow => Some(module.get("event_from_json").map_err(|e| e.to_string())?),
    };
    // The standalone typechecker does not evaluate `record(...)`, so names bound to
    // record types would be `typing.Any`. Describe the prelude from its frozen values.
    let mut bindings: HashMap<String, Ty> = HashMap::new();
    for name in module.names() {
        let value = module.get(name.as_str()).map_err(|e| e.to_string())?;
        bindings.insert(name.as_str().to_owned(), Ty::of_value(value.value()));
    }
    let interface = Interface::new(bindings);
    Ok(Prelude {
        id,
        module,
        interface,
        context,
        event,
    })
}
