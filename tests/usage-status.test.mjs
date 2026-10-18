// Deterministic U-07 checks: node tests/usage-status.test.mjs (no server, no model calls).
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {emptyUsage,applyUsageEvent,usageView,STALE_MS} from '../web/usage-status.js';
const events=JSON.parse(readFileSync(new URL('./fixtures/usage-events.json',import.meta.url)));
const replay=(list,at=null)=>list.reduce((s,e)=>applyUsageEvent(s,e,at),emptyUsage());
let s=emptyUsage();
assert.equal(usageView(s).label,'Context unknown');
assert.ok(usageView(s).details.includes('5h quota: not reported'));
// Replayed (timestamp-less) events: values shown, freshness explicitly unknown, never "now".
s=replay(events.slice(0,2));
let v=usageView(s);
assert.equal(v.label,'⚠ 53k/200k · 27%');
assert.ok(v.stale);
assert.ok(v.details[0].includes('freshness unknown (replayed event #2'));
assert.ok(v.details.some(d=>d.startsWith('Provider-reported cost: 0.0421 USD · scope and account billing not verified')));
assert.ok(v.details.some(d=>d.startsWith('Reported model: claude-opus-5-5')));
assert.ok(!v.details.some(d=>/compact|API-equivalent/i.test(d)),'no inferred cause or billing claim');
s=replay(events.slice(0,3));
assert.equal(s.context.dropped,true);
assert.ok(usageView(s).details.includes('Occupancy decreased since the previous report (cause not reported)'));
s=replay(events.slice(0,4));
assert.equal(s.context.sizeChanged,true);
assert.deepEqual(Object.keys(s.quota),['5h']);
assert.ok(usageView(s).details.some(d=>d.startsWith('5h quota (reported): status allowed_warning · utilization 42')));
assert.ok(usageView(s).details.includes('7d quota: not reported'));
// Parent/subagent-attributed usage is ignored.
s=replay(events.slice(0,5));
assert.equal(s.context.used,9000);assert.equal(s.ignored,1);
// Invalid negative/out-of-range values are rejected.
s=replay(events.slice(0,6));
assert.equal(s.context.used,9000);assert.equal(s.cost.amount,0.0421);
assert.equal(s.quota['7d'].utilization,undefined);
// Live partial cost-only update does not freshen old context.
const now=1760000000000;
s=applyUsageEvent(replay(events.slice(0,6)),events[6],now);
assert.equal(s.cost.at,now);assert.equal(s.cost.currency,null);
assert.equal(s.context.at,null,'context freshness unchanged by cost-only update');
v=usageView(s,{now});
assert.ok(v.stale);
assert.ok(v.details.some(d=>d.includes('0.5000 (currency unknown)')));
// Live context goes stale after STALE_MS.
s=applyUsageEvent(emptyUsage(),events[1],now);
assert.ok(!usageView(s,{now}).stale);
assert.ok(usageView(s,{now:now+STALE_MS+1}).stale);
// Replay is deterministic; non-usage events never change state.
assert.deepEqual(replay(events),replay(events));
const base=emptyUsage();
assert.equal(applyUsageEvent(base,{kind:'user',data:{text:'x'}}),base);
console.log('usage-status: all checks passed');
