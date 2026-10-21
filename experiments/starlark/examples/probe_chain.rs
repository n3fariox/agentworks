//! E-01 evidence: why parser state is copied rather than kept as frozen values.
//! Chains each call's returned state through `Module::freeze`. The call heap must
//! reference the previous state's frozen heap, so every state keeps all its
//! predecessors alive: RSS grows linearly and dropping the chain overflows the
//! stack. Run: cargo run --release -p agentworks-starlark --example probe_chain [reuse]
use starlark::environment::{FrozenModule, Globals, GlobalsBuilder, LibraryExtension, Module};
use starlark::eval::Evaluator;
use starlark::syntax::{AstModule, Dialect, DialectTypes};
use starlark::typing::Ty;
use starlark::values::OwnedFrozenValue;

fn rss() -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap();
    s.lines()
        .find(|l| l.starts_with("VmRSS:"))
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap()
}

fn main() {
    let reuse = std::env::args().nth(1).as_deref() == Some("reuse");
    let src = r#"
S = record(n = int, items = list[str], label = str)
def init() -> S:
    return S(n = 0, items = ["a", "b"], label = "x")
def on_line(state: S, line: str) -> (S, str | None):
    if REUSE:
        return (S(n = state.n + 1, items = state.items, label = state.label), None)
    return (S(n = state.n + 1, items = [line], label = line), None)
"#
    .replace("REUSE", if reuse { "True" } else { "False" });
    let dialect = Dialect {
        enable_types: DialectTypes::Enable,
        ..Dialect::Standard
    };
    let globals: Globals =
        GlobalsBuilder::extended_by(&[LibraryExtension::RecordType, LibraryExtension::Typing]).build();
    let ast = AstModule::parse("p.star", src, &dialect).unwrap();
    let script: FrozenModule = Module::with_temp_heap(|m| {
        {
            let mut e = Evaluator::new(&m);
            e.eval_module(ast, &globals).unwrap();
        }
        m.freeze().unwrap()
    });
    let on_line = script.get("on_line").unwrap();
    println!("on_line type: {}", Ty::of_value(on_line.value()));
    let init = script.get("init").unwrap();
    let mut state: OwnedFrozenValue = Module::with_temp_heap(|m| {
        let f = unsafe { init.owned_frozen_value(m.frozen_heap()) }.to_value();
        let v = {
            let mut e = Evaluator::new(&m);
            e.eval_function(f, &[], &[]).unwrap()
        };
        m.set("state", v);
        m.freeze().unwrap().get("state").unwrap()
    });
    let base = rss();
    let t = std::time::Instant::now();
    for i in 0..300_000 {
        state = Module::with_temp_heap(|m| {
            let (f, s) = unsafe {
                (
                    on_line.owned_frozen_value(m.frozen_heap()).to_value(),
                    state.owned_frozen_value(m.frozen_heap()).to_value(),
                )
            };
            let r = {
                let mut e = Evaluator::new(&m);
                e.eval_function(f, &[s, m.heap().alloc(format!("line {i}"))], &[])
                    .unwrap()
            };
            let new_state = r.iterate(m.heap()).unwrap().next().unwrap();
            m.set("state", new_state);
            m.freeze().unwrap().get("state").unwrap()
        });
        if i == 29_999 || i == 299_999 {
            println!(
                "reuse={reuse} after {}: RSS +{} KiB, {:?}/call",
                i + 1,
                rss() as i64 - base as i64,
                t.elapsed() / (i + 1)
            );
        }
    }
    // Dropping a 300k-long heap chain recurses and overflows the stack.
    std::mem::forget(state);
}
