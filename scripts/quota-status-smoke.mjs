import {serverReady,removeFixture} from '../tests/support/cleanup.mjs';
// B-02 isolated ACP fixtures. No provider inference, live catalog or live database.
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {mkdtemp} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';
import {once} from 'node:events';
const dir=await mkdtemp(join(tmpdir(),'agentworks-quota-status-'));
const port=process.env.AGENTWORKS_QUOTA_STATUS_PORT||'4493',base=`http://127.0.0.1:${port}`;
let server,log='';
const delay=ms=>new Promise(r=>setTimeout(r,ms));
async function api(path,body,method=body===undefined?'GET':'POST') {
 const r=await fetch(base+path,{method,headers:{Origin:base,'Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body)});
 const text=await r.text();assert.equal(r.status,200,text);return JSON.parse(text);
}
async function start(){
 server=spawn(resolve('target/debug/agentworks'),[],{env:{...process.env,AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:port,AGENTWORKS_ROOTS:resolve('.'),AGENTWORKS_CATALOG_PATH:dir+'/catalog.json',AGENTWORKS_DB_PATH:dir+'/sessions.sqlite3',AGENTWORKS_DEMO_STATE_DIR:dir+'/demo',AGENTWORKS_DEMO_STATUS_DELAY_MS:'1800'},stdio:['ignore','pipe','pipe']});
 server.stdout.on('data',b=>log+=b);server.stderr.on('data',b=>log+=b);
 for(let i=0;i<100;i++){if(server.exitCode!==null)throw Error(log);if(await serverReady(server,base))return;await delay(80);}throw Error('Startup failed '+log);
}
async function stop(signal='SIGINT'){if(server.exitCode!==null){server=null;return;}const exited=once(server,'exit');server.kill(signal);await exited;server=null;}
const snapshot=id=>api(`/api/sessions/${id}`);
async function wait(id,test){for(let i=0;i<300;i++){const s=await snapshot(id);if(test(s))return s;await delay(60);}throw Error('Timed out '+JSON.stringify((await snapshot(id)).session));}
const idle=id=>wait(id,s=>s.session.status==='idle');
const policy=(id,p)=>api(`/api/sessions/${id}/continuation`,p,'PUT');
const send=(id,text,requestId=crypto.randomUUID())=>api(`/api/sessions/${id}/prompt`,{text,requestId});
async function prompt(id,text){await send(id,text);return idle(id);}
const count=s=>s.events.filter(e=>e.kind==='user'&&e.data.initiatedBy==='usage-limit scheduler').length;
const native=s=>s.events.filter(e=>e.kind==='provider_session').at(-1)?.data.id;
function bound(p){return [p.enabled,p.authorization,p.generation,p.deadline,p.maxAttempts,p.attempts];}
try {
 await start();
 const s=await api('/api/sessions',{provider:'demo',cwd:resolve('.'),title:'B-02 fixture',resumePolicy:{enabled:true,maxAttempts:3}});const id=s.id;await idle(id);
 let paused=await prompt(id,'quota known');const context=native(paused),original=paused.session.continuation;assert.equal(typeof context,'string');
 await policy(id,{enabled:true,manualDueAt:Date.now()+800});paused=await snapshot(id);const saved=paused.session.continuation;
 const request='b02-status-exactly-once';await send(id,'/status',request);await send(id,'/status',request);
 await delay(1100);let checking=await snapshot(id);assert.equal(checking.session.status,'running');assert.equal(count(checking),0);assert.equal(checking.session.continuation.dueAt,saved.dueAt);assert.deepEqual(bound(checking.session.continuation),bound(saved));
 let resumed=await wait(id,x=>count(x)===1&&x.session.status==='idle');assert.equal(native(resumed),context);assert.equal(resumed.session.continuation.attempts,1);assert.equal(resumed.events.filter(e=>e.kind==='user'&&e.data.requestId===request).length,1);assert.match(resumed.events.find(e=>e.kind==='user'&&e.data.initiatedBy==='usage-limit scheduler').data.text,/Inspect.*tool results/);await delay(1100);assert.equal(count(await snapshot(id)),1);
 // A crash abandons the local check, opens the same context and continues assignment once.
 await policy(id,{enabled:true,maxAttempts:3});await prompt(id,'quota known');await policy(id,{enabled:true,manualDueAt:Date.now()+800});await send(id,'/status','b02-crashed-probe');await delay(200);await stop('SIGKILL');await delay(900);await start();
 resumed=await wait(id,x=>count(x)===2&&x.session.status==='idle');assert.equal(native(resumed),context);assert.equal(resumed.events.filter(e=>e.kind==='user'&&e.data.requestId==='b02-crashed-probe').length,1);assert.equal(resumed.session.continuation.attempts,1);
 // Unknown/stale status evidence cannot fabricate a new reset or erase the known reset.
 for(const mode of ['unknown','stale']) {
  await policy(id,{enabled:true});paused=await prompt(id,'quota known probe-'+mode);const before=paused.session.continuation;
  const checked=await prompt(id,'/usage');assert.equal(checked.session.continuation.dueAt,before.dueAt);assert.equal(checked.session.continuation.source,before.source);assert.deepEqual(bound(checked.session.continuation),bound(before));await api(`/api/sessions/${id}/cancel`,{});
 }
 // Fresh structured exhaustion from a successful read may move reset, never bounds.
 await policy(id,{enabled:true});paused=await prompt(id,'quota known probe-exhausted');let checked=await prompt(id,'/status');assert(checked.session.continuation.dueAt>paused.session.continuation.dueAt);assert.deepEqual(bound(checked.session.continuation),bound(paused.session.continuation));await api(`/api/sessions/${id}/cancel`,{});
 // Human terminal actions win while an exhausted probe is outstanding.
 for(const action of ['stop','disable']) {
  await policy(id,{enabled:true});await prompt(id,'quota known probe-exhausted');await send(id,'/status');await delay(200);
  if(action==='stop')await api(`/api/sessions/${id}/cancel`,{});else await policy(id,{enabled:false});
  checked=await idle(id);assert.equal(checked.session.continuation.enabled,false);assert.equal(checked.session.continuation.dueAt,null);assert.equal(count(checked),2);
 }
 // Deliberate replacement slash text is normal work, not a status exemption.
 await policy(id,{enabled:true});await prompt(id,'quota known');checked=await prompt(id,'/status continue my task');assert.equal(checked.session.continuation.dueAt,null);assert.equal(checked.session.continuation.limited,false);
 // A fresh assignment can still schedule with existing explicit authorization.
 paused=await prompt(id,'quota known');assert(paused.session.continuation.dueAt);await api(`/api/sessions/${id}/cancel`,{});checked=await prompt(id,'/status');assert.equal(checked.session.continuation.enabled,false);assert.equal(checked.session.continuation.dueAt,null);
 // A dead read transport cannot mark the later original-assignment turn as another probe.
 await stop();await start();
 const transport=await api('/api/sessions',{provider:'demo',cwd:resolve('.'),title:'B-02 dead read transport',resumePolicy:{enabled:true}});await idle(transport.id);
 paused=await prompt(transport.id,'quota known probe-transport');const transportContext=native(paused);await send(transport.id,'/status');
 resumed=await wait(transport.id,x=>count(x)===1&&x.session.status==='idle');assert.equal(native(resumed),transportContext);assert.equal(resumed.session.continuation.limited,false);assert.equal(resumed.session.continuation.dueAt,null);
 assert.equal(original.attempts,0);
 console.log('PASS B-02: timer due during probe; duplicate request; exactly-once same native context; crash during probe without replay; stale/unknown/fresh evidence; concurrent Stop/disable; replacement input; fresh quota error; no auto-enable; dead read transport');
} finally {if(server)await stop();await removeFixture(dir);}
