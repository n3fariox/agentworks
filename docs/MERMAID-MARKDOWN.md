# Mermaid Markdown

Chat assistant Markdown, the Files workspace preview, and the beside-chat file preview share `web/mermaid.js`. Only fenced `mermaid` blocks (case insensitive) are rendered. The server assigns a fresh, unpredictable fence class in each Markdown response; raw HTML with a `language-mermaid` class remains literal. Full-file Source views and normal code fences retain existing behavior.

Diagrams render by default. The compact Show source / Show diagram button appears on desktop hover or keyboard focus and stays visible on touch devices. Source is the original code text, inserted as text. Parse, unsupported syntax, configuration and load errors show source with a readable status message.

Rendering runs in an opaque `sandbox="allow-scripts"` iframe with no same-origin, navigation, popup or form permissions. Its CSP blocks network resources except the embedded local script, including images, fonts, nested frames and connections. Mermaid strict mode disables click bindings and HTML labels. DOMPurify sanitizes generated SVG; additional filtering removes links, images, foreign objects, scripts, animation, event attributes and external references. SVG and diagram CSS remain inside the sandbox. Source never becomes HTML, and no arbitrary DOM/HTML is submitted to Mermaid.

The application does not support diagram configuration directives/front matter, interactive links, external images, or externally loaded layouts. Each diagram is limited to 24,000 characters and 300 edges. Rendering failures/timeouts (15 seconds) retain source. Diagram frames scroll for tall output (height capped at 1,600 px). Mermaid layout runs on the browser main thread inside the frame; the timeout cannot preempt synchronous layout work. Input and edge limits reduce that risk.

## Offline build

Pinned Mermaid, DOMPurify and esbuild dependencies live in `web/package.json` and `web/package-lock.json`. To regenerate:

```
npm ci --prefix web
npm run --prefix web build:mermaid
```

Commit `web/mermaid-frame.bundle.js` and generated `web/mermaid-LICENSES.txt` along with source changes. The Rust binary embeds all renderer assets; deployment requires neither Node nor network access. The bundle is fetched only when a Mermaid fence is encountered and contains every supported built-in diagram implementation in one local file. No CDN or runtime package loading is used.

Browser coverage: `tests/mermaid.spec.mjs` owns a dedicated fixture server on localhost port 4416. It exercises chat, disk preview, source-only preview, hover, keyboard, touch, malformed and unknown diagram types, source preservation, regular code fences, raw HTML exclusion and SVG safety.

## Validation

- `cargo build --locked --offline` and `cargo test --locked --offline`: pass (115 Rust tests).
- Targeted Mermaid, display-preference and hosting browser tests: pass.
- `AGENTWORKS_VERIFY_PORT=4496 npm run verify`: pass, including smoke checks, 178 passing browser tests and graceful shutdown; the existing opt-in OpenCode browser test is skipped.
- `cargo fmt --check`, staged whitespace checks, dependency audits and byte-for-byte bundle regeneration: pass.

Validation used private fixture services only. No deployment or live-service restart was performed.
