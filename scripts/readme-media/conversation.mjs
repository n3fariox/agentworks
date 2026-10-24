// Scripted demo-agent turns for README media. Each prompt maps to steps replayed by
// scripts/demo-agent.mjs (AGENTWORKS_DEMO_SCRIPT); nothing here runs a model or a command.
// The capture script makes the matching file edits itself so Files and Diff show real Git state.

export const HERO_PROMPT='Add rate limiting to POST /login: 5 attempts per minute per IP. Cover it with tests.';

const tool=(toolCallId,title,kind,extra={})=>({sessionUpdate:'tool_call',toolCallId,title,kind,status:'in_progress',...extra});
const done=(toolCallId,text,extra={})=>({sessionUpdate:'tool_call_update',toolCallId,status:'completed',...(text?{content:[{type:'content',content:{type:'text',text}}]}:{}),...extra});
const term=(toolCallId,data)=>({sessionUpdate:'tool_call_update',toolCallId,_meta:{terminal_output_delta:{terminal_id:'t1',data}}});

const testOutput=[
  '> acme-api@1.4.0 test\n> vitest run\n\n',
  ' ✓ test/health.test.ts (2 tests) 4ms\n',
  ' ✓ test/users.test.ts (6 tests) 19ms\n',
  ' ✓ test/auth.test.ts (5 tests) 27ms\n',
  ' ✓ test/rate-limit.test.ts (4 tests) 11ms\n\n',
  ' Test Files  4 passed (4)\n      Tests  17 passed (17)\n   Duration  412ms\n',
];

const summary=`### Done: \`POST /login\` is rate limited

Each client IP gets **5 attempts per rolling 60 seconds**. The sixth request gets a \`429\` with a \`Retry-After\` header, and other routes aren't affected.

| File | Change |
| --- | --- |
| \`src/middleware/rate-limit.ts\` | New sliding-window limiter, in memory, keyed by IP |
| \`src/routes/auth.ts\` | Applies \`loginLimiter\` to \`POST /login\` |
| \`test/rate-limit.test.ts\` | 4 new tests covering the window, reset, 429 and per-IP isolation |

\`\`\`ts
export const loginLimiter = rateLimit({ windowMs: 60_000, max: 5 });
router.post('/login', loginLimiter, login);
\`\`\`

\`\`\`mermaid
flowchart LR
  C([Client]) -->|POST /login| L{loginLimiter}
  L -->|under 5 in 60s| A[login handler]
  L -->|limit reached| R[429 + Retry-After]
\`\`\`

All **17 tests pass**. The limiter is in memory, so if you run more than one instance you'll want a shared store such as Redis. Want me to add that?
`;

export const script={
  [HERO_PROMPT]:[
    {say:"I'll check how the auth routes are wired before adding anything.\n\n"},
    tool('read-auth','Read src/routes/auth.ts','read',{locations:[{path:'src/routes/auth.ts'}],rawInput:{path:'src/routes/auth.ts'}}),
    {wait:500},done('read-auth','28 lines · router.post(\'/login\', login)'),
    tool('grep','Search for "rateLimit|limiter" in src/','search',{rawInput:{pattern:'rateLimit|limiter',path:'src/'}}),
    {wait:450},done('grep','No matches'),
    {say:"There's no limiter yet. I'll add a small sliding-window middleware and apply it only to `POST /login`, so the other routes keep working as before.\n\n"},
    tool('edit','Create src/middleware/rate-limit.ts and update src/routes/auth.ts','edit',{status:'pending',locations:[{path:'src/middleware/rate-limit.ts'},{path:'src/routes/auth.ts'}]}),
    {approve:'edit',title:'Create src/middleware/rate-limit.ts and update src/routes/auth.ts',kind:'edit'},
    {wait:700},done('edit','src/middleware/rate-limit.ts  +34\nsrc/routes/auth.ts            +2 −1'),
    tool('tests','Create test/rate-limit.test.ts','edit',{locations:[{path:'test/rate-limit.test.ts'}]}),
    {wait:400},done('tests','test/rate-limit.test.ts  +41'),
    tool('run','npm test','execute',{rawInput:{command:'npm test'}}),
    ...testOutput.flatMap(line=>[{wait:260},term('run',line)]),
    done('run',null,{_meta:{terminal_exit:{terminal_id:'t1',exit_code:0}}}),
    {say:summary,pace:9},
  ],
};

const NOTES_DRAFT='## acme-api 1.4.0\n\n- Paginated `GET /users` (`?cursor=`)\n- Fixed duplicate webhook deliveries after a retry\n- Node 22 is now the minimum version\n';

// Agent messaging: the rate-limit session tells its release-notes peer through real Agentworks MCP calls.
// The peer has inbox delivery enabled, so the message starts its next turn; it replies the same way.
export const NOTES_TITLE='Draft v1.4 release notes';
export const MESSAGE_PROMPT='Tell the release-notes session about this so it makes v1.4.';
script[MESSAGE_PROMPT]=[
  {say:"I'll find the release-notes session in this project and message it.\n\n"},
  {mcp:'list_sessions',title:'List project sessions'},
  {mcp:'send_message',title:`Message “${NOTES_TITLE}”`,arguments:{recipient:'$peer:'+NOTES_TITLE,requestId:'readme-rate-limit-notes',
    text:'Rate limiting for POST /login landed in src/middleware/rate-limit.ts: 5 attempts per minute per IP, then 429 with Retry-After. Please add it to the v1.4 notes.'}},
  {say:"Sent to **"+NOTES_TITLE+"**. That session's work wasn't interrupted; it picks the message up from its inbox.\n"},
];
script['^## Agentworks inbox delivery']=[
  {mcp:'read_inbox',title:'Read inbox'},
  {mcp:'acknowledge_message',title:'Acknowledge message',arguments:{messageId:'$message-from:Rate-limit POST /login'}},
  {mcp:'send_message',title:'Reply to “Rate-limit POST /login”',arguments:{recipient:'$sender:Rate-limit POST /login',requestId:'readme-notes-ack',
    text:'Added under Security in the v1.4 notes. Thanks!'}},
  {say:'Added the rate limit from **Rate-limit POST /login** and replied to confirm. Updated draft:\n\n'+NOTES_DRAFT.replace('\n\n','\n\n### Security\n\n- **Login rate limiting**: `POST /login` allows 5 attempts per minute per IP, then returns `429` with `Retry-After`\n\n### Changes\n\n')},
];

// Short finished conversations that fill the sidebar and dashboard.
export const backgroundSessions=[
  {project:'acme-api',title:'Fix flaky webhook retry test',prompt:'Why does test/webhooks.test.ts fail about 1 in 20 runs on CI?',steps:[
    tool('bg1','Read test/webhooks.test.ts','read',{locations:[{path:'test/webhooks.test.ts'}]}),done('bg1','52 lines'),
    {say:'The retry test asserts on `Date.now()` while the backoff uses a real timer, so a slow CI runner can cross the 200 ms boundary. I recommend switching the test to `vi.useFakeTimers()` and advancing by exactly `backoff(attempt)`. Want me to make that change?\n'}]},
  {project:'acme-api',title:'Draft v1.4 release notes',prompt:'Summarise everything merged since v1.3.0 as release notes.',steps:[
    tool('bg2','git log v1.3.0..HEAD --oneline','execute'),done('bg2','23 commits'),
    {say:NOTES_DRAFT}]},
  {project:'acme-web',title:'Dark mode for settings page',prompt:'Make the settings page respect prefers-color-scheme.',steps:[
    tool('bg3','Read src/pages/Settings.tsx','read',{locations:[{path:'src/pages/Settings.tsx'}]}),done('bg3','140 lines'),
    {say:'Settings hard-codes `#fff` in three places. I moved them to the existing `--surface` tokens, so dark mode now follows the OS setting with no new CSS variables.\n'}]},
  {project:'acme-web',title:'Investigate bundle size jump',prompt:'The production bundle grew 180 KB last week. Find out why.',steps:[
    tool('bg4','npx vite build --mode analyze','execute'),done('bg4','dist/assets/index.js  612 KB'),
    {say:'The jump comes from `date-fns`, which is imported whole in `src/lib/format.ts`. Switching to per-function imports brings it back to **431 KB**.\n'}]},
];
for(const s of backgroundSessions)script[s.prompt]=s.steps;
