import {test} from 'node:test';
import assert from 'node:assert/strict';
import {sessionState, counts, usageMeters} from '../dashboard.js';
import {emptyUsage, applyUsageEvent} from '../usage-status.js';

const event = data => ({kind:'acp',seq:2,data:{sessionUpdate:'usage_update',...data}});
test('session states retain waiting, stopped and unknown rather than assuming idle', () => {
  for (const status of ['running','starting','resuming','configuring','cancelling']) assert.equal(sessionState({status}), 'running');
  for (const status of ['closed','suspended','error','stopped']) assert.equal(sessionState({status}), 'stopped');
  assert.equal(sessionState({status:'waiting'}), 'waiting');
  assert.equal(sessionState({status:'running'}, {pendingApprovals:1}), 'waiting');
  assert.equal(sessionState({status:'idle'}), 'idle');
  assert.equal(sessionState({status:'unrecognized'}), 'unknown');
  assert.deepEqual(counts([{id:'a',status:'running'},{id:'b',status:'suspended',archived:true},{id:'c',status:'idle'}],new Map([['a',{pendingApprovals:1}]])),{running:0,waiting:1,idle:1,stopped:1,unknown:0});
});
test('quota meters accept only reported scales and keep source freshness', () => {
  const now = Date.now();
  let state = applyUsageEvent(emptyUsage(), event({used:10,size:100,_meta:{'_claude/rateLimit':{utilization:40,unifiedWindows:{five_hour:{utilization:.25},seven_day:{utilization:1.2}}}}}));
  assert.deepEqual(usageMeters(state, now).map(m => m.percent), [10,25]);
  assert.match(usageMeters(state, now)[1].detail,/Freshness unknown/);
  state = applyUsageEvent(emptyUsage(), event({_meta:{'_codex/subscriptionLimits':{version:1,limits:[{limitId:'codex',primary:{usedPercent:{value:31,observedAtMs:now-700000},windowDurationMins:{value:300,observedAtMs:now},resetsAt:{value:now/1000+600,observedAtMs:now}},secondary:{usedPercent:{value:80}}}]}}}));
  const meters = usageMeters(state, now);
  assert.equal(meters.length,2); assert.equal(meters[0].percent,31); assert.equal(meters[1].percent,80);
  assert.match(meters[0].detail,/stale/); assert.match(meters[1].detail,/Freshness unknown/);
  assert.deepEqual(usageMeters(emptyUsage()),[]);
});
test('subagent observations and unverified scalar utilization cannot supply meters', () => {
  assert.deepEqual(usageMeters(applyUsageEvent(emptyUsage(),event({used:20,size:100,_meta:{subagent:'other'}}))),[]);
  assert.deepEqual(usageMeters(applyUsageEvent(emptyUsage(),event({_meta:{'_claude/rateLimit':{utilization:.5}}}))),[]);
});
