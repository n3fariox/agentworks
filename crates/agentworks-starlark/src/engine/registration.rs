//! Installation-time hook declarations. Runtime calls never have a registry.
use super::*;
use starlark::values::Value;
use starlark::values::list::{AllocList, ListRef, UnpackList};
use starlark::values::none::NoneType;
use starlark::values::tuple::{AllocTuple, TupleRef};

#[derive(Clone, Debug, Serialize)]
pub struct HookInfo {
    pub hook: &'static str,
    /// Absent for a formatter without `text_pattern`, which sees every message or line.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub events: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger: Option<Json>,
}

pub struct RegisteredScript {
    pub handlers: Vec<Arc<Compiled>>,
    pub hooks: Vec<HookInfo>,
}

fn register<'v>(
    eval: &mut Evaluator<'v, '_, '_>,
    function: Value<'v>,
    kind: &str,
    pattern: Option<&str>,
    label: &str,
    message_type: &str,
    events: Option<Vec<String>>,
) -> anyhow::Result<NoneType> {
    let registry = eval
        .module()
        .extra_value()
        .and_then(ListRef::from_value)
        .ok_or_else(|| anyhow::anyhow!("hooks can only be registered during script installation"))?;
    anyhow::ensure!(registry.len() < 32, "at most 32 hook registrations per script");
    let heap = eval.heap();
    let row = heap.alloc(AllocTuple([
        heap.alloc(kind),
        function,
        pattern.map_or(Value::new_none(), |p| heap.alloc(p)),
        heap.alloc(label),
        heap.alloc(message_type),
        events.map_or(Value::new_none(), |events| heap.alloc(AllocList(events))),
    ]));
    let mut entries: Vec<_> = registry.iter().collect();
    entries.push(row);
    eval.module().set_extra_value(heap.alloc(AllocList(entries)));
    Ok(NoneType)
}

fn badge_effect<'v>(eval: &mut Evaluator<'v, '_, '_>, effect: serde_json::Value) -> anyhow::Result<NoneType> {
    let extra = eval
        .module()
        .extra_value()
        .and_then(TupleRef::from_value)
        .ok_or_else(|| anyhow::anyhow!("badge setters are only available inside a running hook"))?;
    anyhow::ensure!(
        extra.content()[0].unpack_str() == Some("badge_effects"),
        "missing badge effect buffer"
    );
    let values = ListRef::from_value(extra.content()[1]).unwrap();
    anyhow::ensure!(values.len() < 16, "at most 16 badge changes per hook call");
    let heap = eval.heap();
    let mut next: Vec<_> = values.iter().collect();
    next.push(heap.alloc(effect));
    eval.module()
        .set_extra_value(heap.alloc(AllocTuple([heap.alloc("badge_effects"), heap.alloc(AllocList(next))])));
    Ok(NoneType)
}

#[starlark_module]
pub(super) fn registration_helpers(builder: &mut GlobalsBuilder) {
    /// Set/update this script's named badge on the current hook's session.
    fn set_badge<'v>(
        #[starlark(require = pos)] id: &str,
        #[starlark(require = pos)] label: &str,
        #[starlark(require = named, default = "neutral")] tone: &str,
        #[starlark(require = named, default = "")] tooltip: &str,
        #[starlark(require = named, default = NoneOr::None)] url: NoneOr<&str>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        badge_effect(
            eval,
            serde_json::json!({"op":"set","id":id,"label":label,"tone":tone,"tooltip":tooltip,"url":url.into_option()}),
        )
    }
    fn clear_badge<'v>(
        #[starlark(require = pos)] id: &str,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        badge_effect(eval, serde_json::json!({"op":"clear","id":id}))
    }

    fn register_progress<'v>(
        #[starlark(require = pos)] function: Value<'v>,
        #[starlark(require = named)] command_pattern: &str,
        #[starlark(require = named, default = "Progress")] display_name: &str,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        register(
            eval,
            function,
            "progress",
            Some(command_pattern),
            display_name,
            "",
            None,
        )
    }

    fn register_formatter<'v>(
        #[starlark(require = pos)] function: Value<'v>,
        #[starlark(require = named)] message_type: &str,
        #[starlark(require = named, default = NoneOr::None)] text_pattern: NoneOr<&str>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        // Without a pattern no regex runs; the function sees everything.
        register(
            eval,
            function,
            "formatter",
            text_pattern.into_option(),
            "",
            message_type,
            None,
        )
    }

    fn register_github_trigger<'v>(
        #[starlark(require = pos)] function: Value<'v>,
        #[starlark(require = named)] host: &str,
        #[starlark(require = named)] repo: &str,
        #[starlark(require = named)] account: &str,
        #[starlark(require = named)] events: UnpackList<String>,
        #[starlark(require = named, default = "default")] name: &str,
        #[starlark(require = named, default = false)] include_existing: bool,
        #[starlark(require = named, default = NoneOr::None)] settings: NoneOr<Value<'v>>,
        #[starlark(require = named, default = NoneOr::None)] instructions: NoneOr<&str>,
        #[starlark(require = named, default = NoneOr::None)] steps: NoneOr<UnpackList<String>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        anyhow::ensure!(
            !name.is_empty()
                && name.len() <= 32
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-')),
            "invalid trigger name"
        );
        anyhow::ensure!(
            !events.items.is_empty()
                && events.items.len() <= 2
                && events
                    .items
                    .iter()
                    .all(|s| matches!(s.as_str(), "pr.assigned_to_me" | "pr.review_requested")),
            "events must select pr.assigned_to_me and/or pr.review_requested"
        );
        anyhow::ensure!(
            [host, repo, account].iter().all(|s| !s.is_empty() && s.len() <= 256),
            "invalid GitHub subscription"
        );
        let settings = settings.into_option().map(|s| s.to_json_value()).transpose()?;
        let descriptor = serde_json::json!({"name":name,"host":host,"repo":repo,"account":account,"events":events.items,"includeExisting":include_existing,"settings":settings,"instructions":instructions.into_option(),"steps":steps.into_option().map(|s|s.items)}).to_string();
        register(eval, function, "github_trigger", None, &descriptor, "", None)
    }

    fn register_github<'v>(
        #[starlark(require = pos)] function: Value<'v>,
        #[starlark(require = named, default = NoneOr::None)] events: NoneOr<UnpackList<String>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let events = events.into_option().map(|events| events.items);
        if let Some(events) = &events {
            anyhow::ensure!(
                !events.is_empty() && events.len() <= GITHUB_EVENT_KINDS.len(),
                "events must contain 1 to 6 GitHub event names; omit it to receive all events"
            );
            anyhow::ensure!(
                events.iter().all(|kind| GITHUB_EVENT_KINDS.contains(&kind.as_str())),
                "unknown GitHub event name; supported: {}",
                GITHUB_EVENT_KINDS.join(", ")
            );
        }
        register(eval, function, "github", None, "", "", events)
    }

    fn register_tool_header<'v>(
        #[starlark(require = pos)] function: Value<'v>,
        #[starlark(require = named, default = ".*")] title_pattern: &str,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        register(eval, function, "tool_header", Some(title_pattern), "", "", None)
    }
}

impl Engine {
    /// Inspect a previously validated installation without evaluating disabled source.
    pub fn cached_registered(&self, name: &str, hash: &str) -> Option<Arc<RegisteredScript>> {
        self.hook_cache
            .lock()
            .unwrap()
            .get(&(name.to_owned(), hash.to_owned()))
            .cloned()
            .filter(|script| script.handlers.iter().all(|h| h.capability != Capability::Workflow))
    }

    /// Compile one explicitly supplied module and validate all registrations
    /// atomically. Both presentation APIs are available, with no workflow I/O.
    /// Hook scripts may only register presentation/observation handlers.
    pub fn compile_registered(&self, name: &str, source: &str) -> Result<Arc<RegisteredScript>, Diagnostic> {
        self.compile_kind(name, source, false)
    }
    /// Automation files may only register automation handlers. Helpers are fine;
    /// hiding another kind of registered effect in this file is rejected.
    pub fn compile_automation(&self, name: &str, source: &str) -> Result<Arc<RegisteredScript>, Diagnostic> {
        self.compile_kind(name, source, true)
    }
    fn compile_kind(&self, name: &str, source: &str, automation: bool) -> Result<Arc<RegisteredScript>, Diagnostic> {
        let script = self.compile_registrations(name, source)?;
        if script
            .handlers
            .iter()
            .any(|h| (h.capability == Capability::Workflow) != automation)
        {
            return Err(Diagnostic {
                script: name.into(),
                hash: content_hash(source),
                kind: DiagnosticKind::Capability,
                message: if automation {
                    "Automation scripts can only register automation handlers; move hooks to a separate hook script."
                } else {
                    "Hook scripts cannot register automation handlers; import this file through Automations."
                }
                .into(),
            });
        }
        if automation {
            let mut names = std::collections::HashSet::new();
            if script
                .handlers
                .iter()
                .any(|h| !names.insert(h.automation_trigger.as_ref().unwrap()["name"].as_str().unwrap()))
            {
                return Err(Diagnostic {
                    script: name.into(),
                    hash: content_hash(source),
                    kind: DiagnosticKind::Install,
                    message: "Trigger names must be unique within one automation file".into(),
                });
            }
        }
        Ok(script)
    }
    fn compile_registrations(&self, name: &str, source: &str) -> Result<Arc<RegisteredScript>, Diagnostic> {
        let hash = content_hash(source);
        let key = (name.to_owned(), hash.clone());
        if let Some(script) = self.hook_cache.lock().unwrap().get(&key) {
            return Ok(script.clone());
        }
        let fail = |kind, message: String| Diagnostic {
            script: name.to_owned(),
            hash: hash.clone(),
            kind,
            message: bounded(message),
        };
        let result = catch_unwind(AssertUnwindSafe(|| self.compile_registered_inner(name, source, &hash)));
        let compiled = match result {
            Ok(result) => Arc::new(result?),
            Err(_) => {
                return Err(fail(
                    DiagnosticKind::Panic,
                    "interpreter panicked during installation".into(),
                ));
            }
        };
        self.hook_cache.lock().unwrap().insert(key, compiled.clone());
        Ok(compiled)
    }

    fn compile_registered_inner(&self, name: &str, source: &str, hash: &str) -> Result<RegisteredScript, Diagnostic> {
        let fail = |kind, message: String| Diagnostic {
            script: name.to_owned(),
            hash: hash.to_owned(),
            kind,
            message: bounded(message),
        };
        let limits = Limits::parser();
        if source.len() > limits.max_source_bytes {
            return Err(fail(DiagnosticKind::Install, "script exceeds source size limit".into()));
        }
        let started = Instant::now();
        let ast = AstModule::parse(name, source.to_owned(), &dialect())
            .map_err(|e| fail(DiagnosticKind::Syntax, e.to_string()))?;
        let preludes = [
            &self.parser_prelude,
            &self.message_prelude,
            &self.tool_prelude,
            &self.chat_prelude,
            &self.github_prelude,
            &self.workflow_prelude,
        ];
        for load in ast.loads() {
            if !preludes.iter().any(|p| load.module_id == p.id) {
                return Err(fail(
                    DiagnosticKind::Capability,
                    format!(
                        "hook scripts may only load progress.star, message.star, tool.star chat.star and github.star, not {:?}",
                        load.module_id
                    ),
                ));
            }
        }
        let loads = preludes
            .iter()
            .map(|p| (p.id.to_owned(), p.interface.clone()))
            .collect();
        let (errors, ..) = ast.clone().typecheck(&self.hook_globals, &loads);
        if !errors.is_empty() {
            return Err(fail(
                DiagnosticKind::Type,
                errors
                    .iter()
                    .take(5)
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("\n"),
            ));
        }
        let modules = preludes.iter().map(|p| (p.id, &p.module)).collect();
        let loader = ReturnFileLoader { modules: &modules };
        let module = Module::with_temp_heap(|module| -> Result<FrozenModule, Diagnostic> {
            module.set_extra_value(module.heap().alloc(AllocList(Vec::<Value>::new())));
            {
                let mut eval = Evaluator::new(&module);
                eval.set_loader(&loader);
                eval.enable_static_typechecking(true);
                eval.set_max_tick_count(limits.max_ticks)
                    .map_err(|e| fail(DiagnosticKind::Install, e.to_string()))?;
                eval.set_max_heap_size(limits.max_heap_bytes)
                    .map_err(|e| fail(DiagnosticKind::Install, e.to_string()))?;
                eval.set_max_callstack_size(limits.max_callstack)
                    .map_err(|e| fail(DiagnosticKind::Install, e.to_string()))?;
                let deadline = Instant::now() + limits.max_wall;
                eval.set_check_cancelled(Box::new(move || Instant::now() > deadline));
                eval.eval_module(ast, &self.hook_globals)
                    .map_err(|e| fail(DiagnosticKind::Type, e.to_string()))?;
            }
            module
                .freeze_named(FrozenHeapName::User(Box::new(name.to_owned())))
                .map_err(|e| fail(DiagnosticKind::Install, format!("{e:?}")))
        })?;
        let registry = module.owned_extra_value().expect("host registry initialized");
        let entries = ListRef::from_value(registry.value()).expect("host registry is a list");
        if entries.is_empty() {
            return Err(fail(
                DiagnosticKind::Install,
                "no handlers registered; call the appropriate agent.register_* function at module scope".into(),
            ));
        }
        let mut handlers = Vec::new();
        let mut hooks = Vec::new();
        for (index, row) in entries.iter().enumerate() {
            let row = TupleRef::from_value(row).expect("host registration tuple").content();
            let (kind, pattern, label, message_type) = (
                row[0].unpack_str().unwrap(),
                row[2].unpack_str(),
                row[3].unpack_str().unwrap(),
                row[4].unpack_str().unwrap(),
            );
            if kind == "formatter" && !["inbox.peer", "chat.agent", "chat.user"].contains(&message_type) {
                return Err(fail(
                    DiagnosticKind::Type,
                    format!("unknown message_type {message_type:?}; supported: inbox.peer, chat.agent, chat.user"),
                ));
            }
            let events: Option<Vec<String>> = ListRef::from_value(row[5])
                .map(|values| values.iter().map(|v| v.unpack_str().unwrap().to_owned()).collect());
            let capability = match kind {
                "progress" => Capability::Parser,
                "tool_header" => Capability::ToolHeader,
                "github" => Capability::Github,
                "github_trigger" => Capability::Workflow,
                _ if message_type.starts_with("chat.") => Capability::ChatFormatter,
                _ => Capability::MessageFormatter,
            };
            let signature = Ty::of_value(row[1]).to_string();
            if signature != capability.signature() {
                return Err(fail(
                    DiagnosticKind::Type,
                    format!(
                        "{kind} registration {} must have signature `{}`; found `{signature}`",
                        index + 1,
                        capability.signature()
                    ),
                ));
            }
            if pattern.is_some_and(|p| p.len() > 512) {
                return Err(fail(DiagnosticKind::Type, "hook pattern exceeds 512 bytes".into()));
            }
            let regex = pattern
                .map(|pattern| {
                    regex::RegexBuilder::new(pattern)
                        .size_limit(1 << 20)
                        .build()
                        .map(Arc::new)
                        .map_err(|e| fail(DiagnosticKind::Type, format!("invalid {kind} pattern: {e}")))
                })
                .transpose()?;
            if kind == "progress" && (label.is_empty() || label.len() > 32) {
                return Err(fail(
                    DiagnosticKind::Type,
                    "progress display_name must be 1..=32 bytes".into(),
                ));
            }
            let entry = registry.map(|root| {
                let rows = ListRef::from_value(root.to_value()).unwrap();
                TupleRef::from_value(rows.content()[index]).unwrap().content()[1]
                    .unpack_frozen()
                    .unwrap()
            });
            let (_, prelude) = self.parts(capability);
            handlers.push(Arc::new(Compiled {
                name: name.to_owned(),
                hash: hash.to_owned(),
                capability,
                entry,
                context: prelude.context.clone(),
                event: prelude.event.clone(),
                display_name: if kind == "progress" {
                    label.to_owned()
                } else {
                    name.to_owned()
                },
                command_pattern: regex.clone().filter(|_| kind == "progress"),
                // Formatters route on message text, tool headers on the tool title.
                text_pattern: regex.filter(|_| kind != "progress"),
                message_type: (kind == "formatter").then(|| message_type.to_owned()),
                github_events: events.clone(),
                automation_trigger: if kind == "github_trigger" {
                    Some(
                        serde_json::from_str(label)
                            .map_err(|e| fail(DiagnosticKind::Install, format!("invalid trigger: {e}")))?,
                    )
                } else {
                    None
                },
                compile_time: started.elapsed(),
            }));
            hooks.push(HookInfo {
                hook: match kind {
                    "progress" => "progress",
                    "tool_header" => "tool_header",
                    "github" => "github",
                    "github_trigger" => "github_trigger",
                    _ => "formatter",
                },
                pattern: pattern.map(str::to_owned),
                message_type: (kind == "formatter").then(|| message_type.to_owned()),
                display_name: (kind == "progress").then(|| label.to_owned()),
                events,
                trigger: if kind == "github_trigger" {
                    serde_json::from_str(label).ok()
                } else {
                    None
                },
            });
        }
        Ok(RegisteredScript { handlers, hooks })
    }
}
