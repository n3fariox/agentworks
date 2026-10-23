//! Installation-time hook declarations. Runtime calls never have a registry.
use super::*;
use starlark::values::Value;
use starlark::values::list::{AllocList, ListRef};
use starlark::values::none::NoneType;
use starlark::values::tuple::{AllocTuple, TupleRef};

#[derive(Clone, Debug, Serialize)]
pub struct HookInfo {
    pub hook: &'static str,
    pub pattern: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

pub struct RegisteredScript {
    pub handlers: Vec<Arc<Compiled>>,
    pub hooks: Vec<HookInfo>,
}

fn register<'v>(
    eval: &mut Evaluator<'v, '_, '_>,
    function: Value<'v>,
    kind: &str,
    pattern: &str,
    label: &str,
    message_type: &str,
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
        heap.alloc(pattern),
        heap.alloc(label),
        heap.alloc(message_type),
    ]));
    let mut entries: Vec<_> = registry.iter().collect();
    entries.push(row);
    eval.module().set_extra_value(heap.alloc(AllocList(entries)));
    Ok(NoneType)
}

#[starlark_module]
pub(super) fn registration_helpers(builder: &mut GlobalsBuilder) {
    fn register_progress<'v>(
        #[starlark(require = pos)] function: Value<'v>,
        #[starlark(require = named)] command_pattern: &str,
        #[starlark(require = named, default = "Progress")] display_name: &str,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        register(eval, function, "progress", command_pattern, display_name, "")
    }

    fn register_formatter<'v>(
        #[starlark(require = pos)] function: Value<'v>,
        #[starlark(require = named)] message_type: &str,
        #[starlark(require = named, default = ".*")] text_pattern: &str,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        register(eval, function, "formatter", text_pattern, "", message_type)
    }

    fn register_tool_header<'v>(
        #[starlark(require = pos)] function: Value<'v>,
        #[starlark(require = named, default = ".*")] title_pattern: &str,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        register(eval, function, "tool_header", title_pattern, "", "")
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
    }

    /// Compile one explicitly supplied module and validate all registrations
    /// atomically. Both presentation APIs are available, with no workflow I/O.
    pub fn compile_registered(&self, name: &str, source: &str) -> Result<Arc<RegisteredScript>, Diagnostic> {
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
        let preludes = [&self.parser_prelude, &self.message_prelude, &self.tool_prelude];
        for load in ast.loads() {
            if !preludes.iter().any(|p| load.module_id == p.id) {
                return Err(fail(
                    DiagnosticKind::Capability,
                    format!(
                        "hook scripts may only load progress.star, message.star and tool.star, not {:?}",
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
            return Err(fail(DiagnosticKind::Install, "no hooks registered; call agent.register_progress(...), agent.register_formatter(...) or agent.register_tool_header(...) at module scope".into()));
        }
        let mut handlers = Vec::new();
        let mut hooks = Vec::new();
        for (index, row) in entries.iter().enumerate() {
            let row = TupleRef::from_value(row).expect("host registration tuple").content();
            let (kind, pattern, label, message_type) = (
                row[0].unpack_str().unwrap(),
                row[2].unpack_str().unwrap(),
                row[3].unpack_str().unwrap(),
                row[4].unpack_str().unwrap(),
            );
            let capability = match kind {
                "progress" => Capability::Parser,
                "tool_header" => Capability::ToolHeader,
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
            if pattern.len() > 512 {
                return Err(fail(DiagnosticKind::Type, "hook pattern exceeds 512 bytes".into()));
            }
            let regex = Arc::new(
                regex::RegexBuilder::new(pattern)
                    .size_limit(1 << 20)
                    .build()
                    .map_err(|e| fail(DiagnosticKind::Type, format!("invalid {kind} pattern: {e}")))?,
            );
            if kind == "progress" && (label.is_empty() || label.len() > 32) {
                return Err(fail(
                    DiagnosticKind::Type,
                    "progress display_name must be 1..=32 bytes".into(),
                ));
            }
            if kind == "formatter" && message_type != "inbox.peer" {
                return Err(fail(
                    DiagnosticKind::Type,
                    format!("unknown message_type {message_type:?}; supported: inbox.peer"),
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
                command_pattern: (kind == "progress").then(|| regex.clone()),
                // Formatters route on message text, tool headers on the tool title.
                text_pattern: (kind != "progress").then(|| regex.clone()),
                message_type: (kind == "formatter").then(|| message_type.to_owned()),
                compile_time: started.elapsed(),
            }));
            hooks.push(HookInfo {
                hook: match kind {
                    "progress" => "progress",
                    "tool_header" => "tool_header",
                    _ => "formatter",
                },
                pattern: pattern.to_owned(),
                message_type: (kind == "formatter").then(|| message_type.to_owned()),
                display_name: (kind == "progress").then(|| label.to_owned()),
            });
        }
        Ok(RegisteredScript { handlers, hooks })
    }
}
