# Questions and elicitation (L-02)

Agentworks advertises ACP **form** elicitation (`clientCapabilities.elicitation.form`) when it initializes a harness. That enables Claude's AskUserQuestion tool and MCP form elicitations from Claude and Codex, which arrive as `elicitation/create`. URL elicitation is not advertised: it would send people to arbitrary sites, so harnesses keep their own fallback.

## Behavior

- A pending question shares the session's approval map. Turn cancellation, Stop, Suspend, timeouts, service restart, notifications ("Waiting for your answer.") and the `waiting` status treat it like an approval. A question that arrives outside an active turn is answered `cancel` at once.
- The card renders single-select (radios, or a select above 8 options), multi-select checkboxes, text with `email`/`uri`/`date`/`date-time` inputs, numbers/integers and booleans, with titles, descriptions, option descriptions and required markers. Submit sends `accept`; **Skip** sends `decline` (the agent is told the person chose not to answer); **Cancel** sends `cancel` (the asking tool call is aborted).
- Unsupported forms (another mode, nested objects, free-form arrays) show the reason and only offer Skip or Cancel. The server also refuses `accept` for them.
- The browser checks required fields, choices, length, range, whole numbers, formats and `pattern`, then moves focus to the first invalid field and announces a summary. The server repeats every check except `pattern` (ECMAScript regex; no engine on the server; the harness validates again). Invalid answers return 400 and the question stays pending.
- Drafts are saved in browser storage for each answer, so redraws, reconnects and reloads keep typed values. A retry of an unchanged answer reuses its `submissionId`. The server acknowledges a repeated ID (`duplicate: true`) without delivering it again. Another submission for a resolved question returns 409. Drafts are removed when the question resolves.
- If the agent withdraws a question (`$/cancel_request`), the card disappears and the transcript says it was withdrawn. Every resolution leaves a transcript notice: answer sent, skipped, cancelled by you, withdrawn by the agent, or cancelled because the turn ended.

## HTTP API

`POST /api/sessions/{id}/questions/{questionId}` with `{"action":"accept"|"decline"|"cancel","content":{…},"submissionId":"…"}`. Pending questions appear in the WebSocket snapshot and `approval` events with `kind: "question"`, the ACP `request` and `unsupported` (reason or null).

## Checks

`cargo test elicitation`, `node tests/questions.test.mjs` and `npm run test:questions` (demo agent: `question`, `question url`, `question withdraw`; port 4531).
