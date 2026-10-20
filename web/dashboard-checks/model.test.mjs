import {test} from 'node:test';
import assert from 'node:assert/strict';
import {sessionState, counts, usageMeters, activelyTurning, latestQuota, quotaAvailability} from '../dashboard.js';
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
test('dashboard activity excludes archived or non-turning sessions and picks latest quota without summing sources', () => {
  assert.equal(activelyTurning({turnRunning:true}),true);
  assert.equal(activelyTurning({turnRunning:true,archived:true}),false);
  assert.equal(activelyTurning({status:'running'}),false);
  const first = {id:'old',provider:'codex',created:1}, second = {id:'new',provider:'codex',created:2};
  const oldState = applyUsageEvent(emptyUsage(),event({_meta:{'_codex/subscriptionLimits':{version:1,limits:[{limitId:'codex',primary:{usedPercent:{value:30,observedAtMs:1000}}}]}}}));
  const newState = applyUsageEvent(emptyUsage(),event({_meta:{'_codex/subscriptionLimits':{version:1,limits:[{limitId:'codex',primary:{usedPercent:{value:60,observedAtMs:2000}}}]}}}));
  const latest = latestQuota([first,second],new Map([[first.id,{state:oldState}],[second.id,{state:newState}]]),'codex');
  assert.equal(latest.state.codexQuota['["codex","primary"]'].usedPercent.value,60);
  assert.equal(latest.sources,1);
});

test('missing fields and enterprise labels never imply an unmetered plan or account USD spend', () => {
  const state = applyUsageEvent(emptyUsage(), event({cost:{amount:42,currency:'USD'},_meta:{
    '_codex/subscriptionLimits':{version:1,limits:[{limitId:'enterprise',planType:'enterprise',credits:{unlimited:true},individualLimit:{used:'50',limit:'100'}}]},
    '_claude/rateLimit':{status:'allowed',plan:'enterprise'}
  }}));
  assert.equal(quotaAvailability(state,{readable:1}).label,'No cap reported');
  assert.match(quotaAvailability(state,{readable:1}).detail,/USD is unavailable/);
  for (const coverage of [{},{readable:1,failed:1},{readable:1,truncated:1}]) {
    assert.equal(quotaAvailability(state,coverage).label,'Quota unavailable');
  }
  assert.deepEqual(usageMeters(state),[]);
  const capped = applyUsageEvent(state,event({_meta:{'_claude/rateLimit':{unifiedWindows:{five_hour:{utilization:0}}}}}));
  assert.equal(quotaAvailability(capped,{readable:1}).label,'Reported quota windows');
  assert.equal(usageMeters(capped)[0].percent,0);
});
test('latest Codex window uses percentage observation time ahead of session creation time', () => {
  const sessions = [{id:'older-session',provider:'codex',created:1},{id:'newer-session',provider:'codex',created:2}];
  const observation = (value, at) => ({state:applyUsageEvent(emptyUsage(),event({_meta:{'_codex/subscriptionLimits':{version:1,limits:[{limitId:'codex',primary:{usedPercent:{value,observedAtMs:at}}}]}}}))});
  const result = latestQuota(sessions,new Map([['older-session',observation(60,2000)],['newer-session',observation(30,1000)]]),'codex');
  assert.equal(Object.values(result.state.codexQuota)[0].usedPercent.value,60);
});

test('quota reset times show local timezone, countdown and independent field freshness', () => {
  const now = Date.UTC(2026,9,20,12), reset = now + 90*60000;
  const date = new Date(reset).toLocaleString(undefined,{timeZoneName:'short'});
  for (const resetsAt of [reset/1000,reset]) {
    const state = applyUsageEvent(emptyUsage(),event({_meta:{'_claude/rateLimit':{unifiedWindows:{five_hour:{utilization:.25,resetsAt}}}}}));
    const detail = usageMeters(state,now)[0].detail;
    assert.ok(detail.includes(date));
    assert.match(detail,/Resets in 1h 30m/);
    assert.match(detail,/Freshness unknown/);
  }
  let state = applyUsageEvent(emptyUsage(),event({_meta:{'_codex/subscriptionLimits':{version:1,limits:[{limitId:'codex',primary:{usedPercent:{value:25,observedAtMs:now-700000},resetsAt:{value:reset/1000,observedAtMs:now}}}]}}}));
  const detail = usageMeters(state,now)[0].detail;
  assert.ok(detail.includes(date));
  assert.match(detail,/Resets in 1h 30m/);
  assert.doesNotMatch(detail.split('Usage freshness:')[0],/stale/);
  assert.match(detail.split('Usage freshness:')[1],/stale/);
  // Replaying or refreshing after the reset cannot fabricate a fresh quota.
  assert.match(usageMeters(state,reset)[0].detail,/Reported reset time passed.*awaiting a new provider report/);
  assert.equal(usageMeters(state,reset)[0].percent,25);
  state = applyUsageEvent(state,event({_meta:{'_codex/subscriptionLimits':{version:1,limits:[{limitId:'codex',primary:{resetsAt:{value:0,observedAtMs:now}}}]}}}));
  assert.match(usageMeters(state,now)[0].detail,/Reported reset time passed/);
});
test('missing or invalid reset timestamps stay unavailable rather than inventing reset times', () => {
  for (const resetsAt of [undefined,null,-1,Infinity,1e20,'tomorrow']) {
    const state = applyUsageEvent(emptyUsage(),event({_meta:{'_claude/rateLimit':{unifiedWindows:{five_hour:{utilization:.25,resetsAt}}}}}));
    assert.match(usageMeters(state)[0].detail,/Reset unavailable/);
    assert.doesNotMatch(usageMeters(state)[0].detail,/Invalid Date|Resets in/);
  }
  const state = applyUsageEvent(emptyUsage(),event({_meta:{'_codex/subscriptionLimits':{version:1,limits:[{limitId:'codex',primary:{usedPercent:{value:25},resetsAt:{value:null}}}]}}}));
  assert.match(usageMeters(state)[0].detail,/Reset unavailable/);
});
