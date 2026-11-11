# Patched libghostty-vt-sys

`libghostty-vt-sys-0.2.2` is the crates.io release of https://github.com/uzaaft/libghostty-rs (MIT OR Apache-2.0), applied through `[patch.crates-io]` in the workspace manifest.

The only change is in `build.rs`: for the `windows-gnu` targets, `strip_compiler_rt` deletes Zig's bundled `compiler_rt.obj` from the static Ghostty archive after the Zig build. GNU ld cannot resolve the COFF weak aliases it defines (for example `___chkstk_ms`), so cross-linking a Windows binary with MinGW failed with "relocation truncated to fit … against undefined symbol". Rust's compiler-builtins and MinGW's libgcc already provide those symbols. Other targets build exactly as upstream.

Drop this patch once upstream Ghostty or the crate stops bundling compiler_rt for windows-gnu.
