// Run the shipped adapter handler in isolation: no CLI startup or Codex process.
import assert from 'node:assert/strict';
import {readFileSync,realpathSync} from 'node:fs';
import {createHash} from 'node:crypto';
import vm from 'node:vm';
import {observeSubscriptionLimits,subscriptionLimitsMeta} from '../vendor/codex-acp/dist/subscription-limits.js';
import {emptyUsage,applyUsageEvent,usageView,setupUsageStatus,STALE_MS} from '../web/usage-status.js';

const fixture=JSON.parse(readFileSync(new URL('./fixtures/codex-subscription.json',import.meta.url)));
const bundle=readFileSync(new URL('../vendor/codex-acp/dist/index.js',import.meta.url),'utf8');
const rootManifest=JSON.parse(readFileSync(new URL('../package.json',import.meta.url)));
assert.equal(rootManifest.dependencies['@agentclientprotocol/codex-acp'],'file:vendor/codex-acp');
assert.equal(realpathSync('node_modules/@agentclientprotocol/codex-acp/dist/index.js'),realpathSync(new URL('../vendor/codex-acp/dist/index.js',import.meta.url)),'installed CLI resolves tracked local fork');
// Verify that the vendored bundle differs from pinned upstream in exactly five
// places. Any accidental bundled-code change fails this provenance check.
const upstream=bundle
  .replace('  subagents: defaultOnError(object({}).nullish(), () => void 0), // Agentworks: preserve canonical subagent negotiation.\n','')
  .replace("// Modified by Agentworks: cached subscription metadata; see ../AGENTWORKS.md.\nimport {observeSubscriptionLimits, subscriptionLimitsMeta} from './subscription-limits.js';\n",'')
  .replace('  version: \"2.2.2-agentworks.1\",', '  version: \"2.2.2\",')
  .replace('      size,\n      ...(params.threadId === this.sessionState.sessionId ? subscriptionLimitsMeta(this.sessionState) : {})','      size')
  .replace('    observeSubscriptionLimits(this.sessionState, params.rateLimits);\n','');
const expectedHash=readFileSync(new URL('../vendor/codex-acp/upstream.sha256',import.meta.url),'utf8').split(' ')[0];
assert.equal(createHash('sha256').update(upstream).digest('hex'),expectedHash);
let now=fixture.observedAtMs;
const sandbox={observeSubscriptionLimits:(s,r)=>observeSubscriptionLimits(s,r,now),subscriptionLimitsMeta};
vm.createContext(sandbox);
const rateMap=bundle.slice(bundle.indexOf('function rateLimitId('),bundle.indexOf('// src/AgentFileChangeReport.ts'));
const tokenCount=bundle.slice(bundle.indexOf('function toTokenCount('),bundle.indexOf('function toPromptUsage('));
const handler=bundle.slice(bundle.indexOf('var CodexEventHandler = class'),bundle.indexOf('var NON_MODEL_OUTPUT_ITEM_TYPES'));
vm.runInContext(rateMap+tokenCount+handler,sandbox);
const adapter=Object.create(sandbox.CodexEventHandler.prototype);
adapter.sessionState={sessionId:'parent'};
adapter.subagents={isNativeSubagentThread:id=>id==='child'};
const counts={totalTokens:1234,inputTokens:1200,cachedInputTokens:200,cacheWriteInputTokens:0,outputTokens:34,reasoningOutputTokens:0};
const token=(threadId='parent',size=200000)=>({threadId,tokenUsage:{last:counts,total:counts,modelContextWindow:size}});
const emit=()=>adapter.createUpdateEvent({method:'thread/tokenUsage/updated',params:token()});
const notify=async snapshot=>{
  assert.equal(await adapter.createUpdateEvent({method:'account/rateLimits/updated',params:{rateLimits:snapshot}}),null,'no fabricated context event on quota notification');
};
const plain=x=>JSON.parse(JSON.stringify(x));
assert.deepEqual(plain(await emit()),{sessionUpdate:'usage_update',used:1234,size:200000});
assert.equal(adapter.createUsageUpdate(token('parent',null)),null,'unknown context still suppresses context event');
await notify(fixture.notifications[0]);
await notify(fixture.notifications[1]);
const first=plain(await emit());
assert.equal(first.used,1234);assert.equal(first.size,200000);
assert.equal(adapter.sessionState.rateLimits.size,2,'upstream cache still populated');
const limits=first._meta['_codex/subscriptionLimits'].limits;
assert.equal(limits[0].primary.usedPercent.value,15);
assert.equal(limits[0].primary.usedPercent.observedAtMs,now);
assert.equal(limits[1].primary.windowDurationMins.value,90);
assert.equal(limits[1].secondary.usedPercent.value,100);
assert.ok(!limits[1].primary.resetsAt,'missing reset stays missing');
assert.equal(limits[1].secondary.resetsAt.value,null);
assert.ok(!adapter.createUsageUpdate(token('child'))._meta,'never attach parent quota to child context');
assert.equal(adapter.createUsageUpdate(token('unrelated')),null);
const key=(id,role)=>JSON.stringify([id,role]);
const event=data=>({kind:'acp',seq:1,data});
let state=applyUsageEvent(emptyUsage(),event(first),now);
const context=state.context;
assert.equal(state.codexQuota[key('codex','primary')].usedPercent.value,15,'native scale, no x100');
assert.equal(state.codexQuota[key('codex','primary')].resetsAt.value,1792385436000);
let view=usageView(state,{provider:'codex',now});
assert.match(view.label,/codex primary \(5h\) 85% left/);
assert.match(view.label,/codex secondary \(7d\) 33% left/);
assert.match(view.label,/Research \(research\) primary \(90m\) 100% left/);
assert.match(view.label,/Research \(research\) secondary \(2d\) 0% left/);
assert.ok(view.details.some(t=>t.includes('15% used / 85% remaining')));
assert.ok(view.details.some(t=>t.includes('reset unknown')));

now+=STALE_MS+1;
const repeated=plain(await emit());
assert.deepEqual(repeated,first,'token events never restamp cached quota');
state=applyUsageEvent(state,event(repeated),now);
assert.equal(state.context.at,now);
assert.equal(state.codexQuota[key('codex','primary')].usedPercent.at,fixture.observedAtMs);
assert.match(usageView(state,{provider:'codex',now}).label,/85% left ⚠/);
state=applyUsageEvent(state,event({sessionUpdate:'usage_update',used:1500,size:200000}),now);
assert.equal(state.codexQuota[key('codex','primary')].usedPercent.at,fixture.observedAtMs);

await notify(fixture.notifications[2]); // sparse reset only
const sparse=plain(await emit());
state=applyUsageEvent(state,event(sparse),now);
const primary=state.codexQuota[key('codex','primary')];
assert.equal(primary.usedPercent.at,fixture.observedAtMs,'sparse reset cannot freshen percent');
assert.equal(primary.resetsAt.at,now);assert.equal(primary.resetsAt.value,1792385500000);
assert.equal(primary.windowDurationMins.at,fixture.observedAtMs);
assert.equal(state.codexQuota[key('codex','secondary')].usedPercent.at,fixture.observedAtMs);
assert.equal(limits[0].primary.resetsAt.value,1792385436,'old emitted event is immutable');
await notify(fixture.notifications[3]);
state=applyUsageEvent(state,event(plain(await emit())),now);
assert.equal(state.codexQuota[key('research','primary')].usedPercent.value,25);
assert.equal(state.codexQuota[key('research','primary')].windowDurationMins.value,90);
await notify(fixture.notifications[4]);
await notify(fixture.notifications[5]);
const cleared=plain(await emit());
state=applyUsageEvent(state,event(cleared),now);
assert.equal(state.codexQuota[key('codex','primary')].usedPercent.value,undefined,'explicit null is unknown, not zero');
assert.ok(!state.codexQuota[key('research','secondary')],'explicit null clears window');
assert.equal(state.context.used,context.used,'metadata never changes context');

// Restart/replay: original source observations survive, browser receipt does not
// count as a new quota observation. A different session starts with no cache.
const replay=applyUsageEvent(emptyUsage(),event(sparse),null);
assert.equal(replay.codexQuota[key('codex','primary')].usedPercent.at,fixture.observedAtMs);
assert.ok(usageView(replay,{provider:'codex',now}).details.some(t=>t.includes('(replayed)')));
assert.match(usageView(replay,{provider:'codex',now}).label,/85% left ⚠/);
assert.deepEqual(subscriptionLimitsMeta({sessionId:'other'}),{});
// The same percentage reported again by the provider is a new observation;
// receiving only another limit does not freshen this one.
now+=1000;
await notify({limitId:'codex',primary:{usedPercent:15}});
const newObservation=applyUsageEvent(state,event(plain(await emit())),now);
assert.equal(newObservation.codexQuota[key('codex','primary')].usedPercent.at,now);
assert.equal(newObservation.codexQuota[key('codex','secondary')].usedPercent.at,fixture.observedAtMs);
assert.equal(newObservation.codexQuota[key('codex','primary')].resetsAt.at,now-1000);
const unknown={};
observeSubscriptionLimits(unknown,{primary:{usedPercent:0}},now);
const unknownState=applyUsageEvent(emptyUsage(),event({sessionUpdate:'usage_update',...subscriptionLimitsMeta(unknown)}),now);
assert.match(usageView(unknownState,{provider:'codex',now}).label,/codex primary \(duration unknown\) 100% left/);
assert.ok(usageView(unknownState,{provider:'codex',now}).details.some(t=>t.includes('reset unknown')));
const noTime=applyUsageEvent(emptyUsage(),event({sessionUpdate:'usage_update',_meta:{'_codex/subscriptionLimits':{version:1,limits:[{limitId:'custom',primary:{usedPercent:{value:5}}}]}}}),now);
assert.equal(noTime.codexQuota[key('custom','primary')].usedPercent.at,null);
assert.match(usageView(noTime,{provider:'codex',now}).label,/95% left ⚠/);
assert.ok(usageView(noTime,{provider:'codex',now}).details.some(t=>t.includes('freshness unknown (no source observation time)')));
const zeroReset={};observeSubscriptionLimits(zeroReset,{primary:{usedPercent:0,resetsAt:0}},now);
assert.ok(usageView(applyUsageEvent(emptyUsage(),event({sessionUpdate:'usage_update',...subscriptionLimitsMeta(zeroReset)}),now),{provider:'codex',now}).details.some(t=>t.includes('reset time passed')));
assert.deepEqual(applyUsageEvent(emptyUsage(),event({...first,_meta:{...first._meta,parentToolUseId:'child'}}),now).codexQuota,{});
for(const bad of [-1,101,'15',NaN,Infinity]){
  const badState={};observeSubscriptionLimits(badState,{primary:{usedPercent:bad}},now);
  const s=applyUsageEvent(emptyUsage(),event({sessionUpdate:'usage_update',...subscriptionLimitsMeta(badState)}),now);
  assert.equal(s.codexQuota[key('codex','primary')].usedPercent.value,undefined);
  assert.equal(s.context,null,'quota-only fixture does not fabricate context');
}
assert.equal(applyUsageEvent(emptyUsage(),event({sessionUpdate:'usage_update',_meta:{'_codex/subscriptionLimits':{version:99,limits}}}),now).context,null);

// UI controller reset is the actual session-switch/snapshot-replacement path.
// Minimal DOM stand-in lets this deterministic check run without a browser.
const node=()=>({hidden:false,textContent:'',classList:{toggle(){}},setAttribute(){},replaceChildren(){}});
const root={firstChild:node(),lastChild:node()};
globalThis.document={createElement:node};
const ui=setupUsageStatus({root,getProvider:()=> 'codex'});
ui.consume(event(first));ui.render();assert.match(root.firstChild.textContent,/85% left/);
ui.reset();assert.equal(root.firstChild.textContent,'Context unknown');assert.deepEqual(ui.state.codexQuota,{});
ui.consume(event({sessionUpdate:'usage_update',used:2,size:10}));ui.render();assert.doesNotMatch(root.firstChild.textContent,/85% left/);
ui.reset();ui.consume(event(sparse));ui.render();assert.match(root.firstChild.textContent,/85% left/);
assert.equal(ui.state.codexQuota[key('codex','primary')].usedPercent.at,fixture.observedAtMs);
delete globalThis.document;
console.log('codex-subscription: adapter provenance/handler, sparse observations, UI, replay and switching passed');
