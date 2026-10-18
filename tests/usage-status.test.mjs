// Deterministic U-07 checks: node tests/usage-status.test.mjs (no server, no model calls).
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {emptyUsage,applyUsageEvent,usageView,STALE_MS} from '../web/usage-status.js';
const events=JSON.parse(readFileSync(new URL('./fixtures/usage-events.json',import.meta.url)));
const replay=list=>list.reduce((s,e)=>applyUsageEvent(s,e),emptyUsage());
let s=emptyUsage();
assert.equal(usageView(s).label,'Context unknown');
assert.ok(usageView(s).details.some(d=>d.includes('5h quota: not reported')));
s=replay(events.slice(0,2));
assert.equal(s.events,1);
assert.equal(usageView(s,{now:1760000001000}).label,'53k/200k · 27%');
assert.ok(usageView(s,{now:1760000001000}).details.some(d=>d.includes('Reported session cost')&&d.includes('0.0421')));
s=replay(events.slice(0,3));
assert.equal(s.resetReason,'context compacted');
assert.equal(s.cost.amount,0.0421,'cost persists independently of context');
s=replay(events.slice(0,4));
assert.equal(s.resetReason,'model or context window changed');
assert.deepEqual(Object.keys(s.quota),['5h'],'window without duration is not labelled 5h/7d');
const v=usageView(s,{now:1760000003000});
assert.match(v.label,/9\.0k\/1\.0M · 1% · 5h 42%/);
assert.ok(v.details.some(d=>d.startsWith('5h quota: 42% used / 58% remaining · resets')));
assert.ok(v.details.includes('7d quota: not reported'));
s=replay(events);
assert.equal(s.context.used,9000,'empty update keeps last reported context');
assert.equal(s.events,4);
assert.ok(usageView(s,{now:1760000004000+STALE_MS+1}).stale);
assert.ok(usageView(s,{now:1760000004000+STALE_MS+1}).label.startsWith('⚠ '));
// Replay of the same stored events reproduces identical state (reconnect safety).
assert.deepEqual(replay(events),replay(events));
// Non-usage and non-ACP events never change state.
const base=emptyUsage();
assert.equal(applyUsageEvent(base,{kind:'user',data:{text:'x'}}),base);
console.log('usage-status: all checks passed');
