# Source and diff highlighting (U-09)

Source views in Files and beside chat share the existing browser-local highlighting
and wrapping preferences. Known language extensions use the conservative tokenizer
in `web/display-preferences.js` (JS/TS, Rust, Python, shell, Go, C-family, SQL,
JSON and configuration aliases). Unknown extensions stay monospace plain text.
Source rows have line numbers; unified patches have old/new line numbers, hunk
markers and green/red addition/removal backgrounds independent of syntax colors.
Turning syntax highlighting off retains patch colors and gutters.

The tokenizer handles comments, strings, numbers and keywords, not full language
semantics. Each side of each diff hunk is tokenized separately, preserving multiline
comments within the available context. Omitted context cannot establish lexer state.
Decoration is limited to 60,000 UTF-16 code units and 3,000 rows; larger text,
NUL-containing input and binary patch summaries remain plain. The existing source
preview cap (100,000 code units), file endpoint cap (10 MB), patch cap (256 KiB)
and binary download/summary behavior are retained.

Review of rendering/security constraints:

- Source text, patch headers, patch bodies and tokens use text nodes/textContent.
  Language lookup uses own properties, including for unknown names such as
  `constructor`. No grammar downloads, new dependencies or source-as-HTML path.
- Markdown Preview continues using the backend's Ammonia-sanitized HTML; Source
  shows the literal Markdown. Highlighting does not change sanitization or links.
- Gutters use CSS generated content; the DOM text remains the original preview or
  patch. Copy actions read captured original strings rather than rendered markup
  or line-number gutters. Source Copy includes the complete loaded file even when
  its displayed preview is shortened. Download remains available for binary files.
- Existing clipboard failure handling exposes a selected read-only raw-text field.
  Highlighting preferences apply immediately to open source and patch views.
- Diff API root/ref validation, bounded Git inspection, and suppression of external
  diff/textconv helpers are unchanged.

Validation: locked offline build; 107 Rust tests; 15 focused browser tests in
`tests/display-preferences.spec.mjs` and `tests/diff.spec.mjs`; Rust formatting,
changed-JavaScript syntax and Git whitespace checks. Browser fixtures run their own
loopback servers/catalogs/databases and Git repositories. Coverage includes source
and diff token categories, multiline comments, exact DOM/copy reconstruction,
CRLF/tabs/trailing spaces, shortened source copying, unknown/large/binary fallback,
diff backgrounds/gutters, preference updates and desktop/mobile containment.
