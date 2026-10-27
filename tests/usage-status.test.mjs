// Deterministic U-07 checks: node tests/usage-status.test.mjs (no server, no model calls).
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {emptyUsage,applyUsageEvent,usageView,countdown,STALE_MS} from '../web/usage-status.js';
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
assert.deepEqual(Object.keys(s.quota),['5h','7d']);
assert.equal(s.quota['5h'].resetsAt,1792366200000,'seconds converted to ms');
v=usageView(s,{now:1792360000000});
assert.match(v.label,/· 5h 77% left ⚠ \(resets in 1h 44m\) · 7d 90% left ⚠ \(resets in 5d 3h\)$/,'replayed quota marked stale, shown as remaining with countdown');
assert.ok(v.details.some(d=>d.startsWith('5h quota (reported): 77% remaining · status allowed · resets in 1h 44m (')&&d.includes('freshness unknown (replayed event #4')));
assert.ok(v.details.some(d=>d.startsWith('7d quota (reported): 90% remaining · resets in 5d 3h (')));
assert.equal(v.short,'⚠ ctx 1% · 5h 77% left');
assert.match(usageView(s,{now:1792366200000}).label,/5h 77% left ⚠ \(reset time passed\)/,'no countdown once the reported reset has passed');
assert.deepEqual([countdown(30e3),countdown(45*60e3),countdown(134*60e3),countdown(26*3600e3),countdown(0)],['1m','45m','2h 14m','1d 2h',null]);
// Live quota is fresh and independent of context freshness.
const live=applyUsageEvent(replay(events.slice(0,3)),events[3],1792360000000);
assert.match(usageView(live,{now:1792360000000}).label,/5h 77% left \(resets in 1h 44m\) · 7d 90% left \(resets in 5d 3h\)$/);
// Scalar utilization of unverified scale alone is not shown.
assert.deepEqual(applyUsageEvent(emptyUsage(),{kind:'acp',data:{sessionUpdate:'usage_update',used:1,size:2,_meta:{'_claude/rateLimit':{rateLimitType:'five_hour',utilization:0.4}}}}).quota,{});
// Parent/subagent-attributed usage is ignored.
s=replay(events.slice(0,5));
assert.equal(s.context.used,9000);assert.equal(s.ignored,1);
// Invalid negative/out-of-range values are rejected.
s=replay(events.slice(0,6));
assert.equal(s.context.used,9000);assert.equal(s.cost.amount,0.0421);
assert.equal(s.quota['5h'].usedPercent,23,'out-of-range fraction rejected');assert.equal(s.quota['7d'].usedPercent,10,'negative fraction rejected');
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
// size 0 means unknown, never a percentage.
s=replay(events);
assert.equal(s.context.size,undefined);assert.equal(usageView(s).label.split(' · ')[0],'⚠ 100 ctx');
assert.equal(s.quota['5h'].usedPercent,30);
// Replay is deterministic; non-usage events never change state.
assert.deepEqual(replay(events),replay(events));
const base=emptyUsage();
assert.equal(applyUsageEvent(base,{kind:'user',data:{text:'x'}}),base);
// Goose reports Codex subscription windows through the shared metadata format.
const goose=applyUsageEvent(emptyUsage(),{kind:'acp',data:{sessionUpdate:'usage_update',used:7,size:200000,_meta:{'_codex/subscriptionLimits':{version:1,limits:[{limitId:'codex',primary:{usedPercent:{value:12.5,observedAtMs:now},windowDurationMins:{value:300,observedAtMs:now}}}]}}}},now);
const gooseView=usageView(goose,{now,provider:'goose'});
assert.match(gooseView.label,/codex primary \(5h\) 88% left/);
assert.ok(gooseView.details.some(d=>d.includes('12.5% used / 87.5% remaining')));
assert.ok(!gooseView.details.some(d=>d.includes('quota: not reported')));
console.log('usage-status: all checks passed');
