// Isolated ACP lifecycle checks. Demo protocol only; no accounts or model calls.
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {mkdtemp,mkdir,readFile,access} from 'node:fs/promises';
import {resolve,join} from 'node:path';
import {serverReady,stopChild,removeFixture} from '../tests/support/cleanup.mjs';

await mkdir('.cache',{recursive:true});
const fixture=await mkdtemp(resolve('.cache/acp-lifecycle-'));
const base='http://127.0.0.1:4498';
let server,log='';
const delay=ms=>new Promise(ok=>setTimeout(ok,ms));
async function until(fn){for(let i=0;i<200;i++){const result=await fn();if(result)return result;await delay(50);}throw Error('Lifecycle fixture timed out: '+log);}
async function start(overrides={}){
  server=spawn(resolve('target/debug/agentworks'),[],{env:{...process.env,AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:'4498',AGENTWORKS_ROOTS:fixture,AGENTWORKS_CATALOG_PATH:join(fixture,'catalog.json'),AGENTWORKS_DB_PATH:join(fixture,'sessions.sqlite3'),AGENTWORKS_DEMO_STATE_DIR:join(fixture,'demo'),AGENTWORKS_DEMO_LIFECYCLE:'1',...overrides},stdio:['ignore','pipe','pipe']});
  log='';server.stdout.on('data',s=>log+=s);server.stderr.on('data',s=>log+=s);
  await until(async()=>{assert.equal(server.exitCode,null,log);return serverReady(server,base);});
}
async function api(path,body,method=body===undefined?'GET':'POST',status=200){
  const response=await fetch(base+path,{method,headers:{Origin:base,'Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body),signal:AbortSignal.timeout(15000)});
  const text=await response.text();assert.equal(response.status,status,text);return status===200?JSON.parse(text):text;
}
const path=id=>'/api/sessions/'+id;
const snapshot=id=>api(path(id));
const ready=id=>until(async()=>{const s=await snapshot(id);assert.notEqual(s.session.status,'error',JSON.stringify(s.events.slice(-3)));return s.session.status==='idle'&&s;});
const native=s=>s.events.findLast(e=>e.kind==='provider_session').data.id;
const exists=file=>access(file).then(()=>true,()=>false);
const file=id=>join(fixture,'demo',id+'.json');
const trace=async()=>readFile(join(fixture,'demo','lifecycle.jsonl'),'utf8').then(s=>s.trim().split('\n').map(JSON.parse));
async function create(){const s=await api('/api/sessions',{provider:'demo',cwd:fixture,title:'ACP lifecycle fixture'});return {id:s.id,s:await ready(s.id)};}
try{
  await start();
  const first=await create(),id=first.id,nativeId=native(first.s);
  assert.deepEqual(first.s.providerCapabilities.close,{});assert.deepEqual(first.s.providerCapabilities.delete,{});
  await api(path(id)+'/suspend',{});
  assert(await exists(file(nativeId)),'close must preserve provider history');
  assert((await trace()).some(e=>e.method==='session/close'&&e.sessionId===nativeId&&e.phase==='closed'));
  await api(path(id)+'/resume',{});assert.equal(native(await ready(id)),nativeId);
  await api(path(id)+'/prompt',{text:'slow',requestId:crypto.randomUUID()});
  await until(async()=>['running','waiting'].includes((await snapshot(id)).session.status));
  await api(path(id)+'/suspend',{});
  const stopped=await snapshot(id);assert.equal(stopped.session.status,'suspended');
  assert(stopped.events.some(e=>e.kind==='provider_close'&&e.data.outcome==='closed'));
  await delay(300);assert.equal((await snapshot(id)).session.status,'suspended','late prompt completion must not undo suspension');
  const methodsBefore=(await trace()).filter(e=>['session/new','session/resume','session/prompt'].includes(e.method)).length;
  await api(path(id)+'?providerHistory=true',undefined,'DELETE');
  assert(!await exists(file(nativeId)),'explicit provider deletion must remove native history');
  await api(path(id),undefined,'GET',404);
  const events=await trace();assert.equal(events.filter(e=>['session/new','session/resume','session/prompt'].includes(e.method)).length,methodsBefore,'deletion connection must never open/resume/prompt');
  const deleted=events.findLastIndex(e=>e.method==='session/delete'&&e.sessionId===nativeId);
  assert(events.slice(0,deleted).some(e=>e.method==='session/close'&&e.sessionId===nativeId&&e.phase==='closed'));
  const local=await create(),localNative=native(local.s);
  await api(path(local.id),undefined,'DELETE');assert(await exists(file(localNative)),'ordinary Delete history must preserve provider history');
  console.log('PASS: graceful idle/active close, native resume, explicit provider delete and local-only delete');

  await stopChild(server);await start({AGENTWORKS_DEMO_CLOSE:'reject',AGENTWORKS_DEMO_DELETE:'reject'});
  const rejected=await create(),rejectedNative=native(rejected.s);
  await api(path(rejected.id)+'?providerHistory=true',undefined,'DELETE',502);
  assert(await exists(file(rejectedNative)));assert.equal((await snapshot(rejected.id)).session.status,'suspended');
  assert.equal((await snapshot(rejected.id)).session.providerHistoryDeletion,'pending');
  assert((await snapshot(rejected.id)).events.some(e=>e.kind==='provider_close'&&e.data.outcome==='failed'));
  await api(path(rejected.id)+'/resume',{},'POST',409);
  await stopChild(server);await start();
  await api(path(rejected.id)+'/resume',{},'POST',409);
  await api(path(rejected.id)+'?providerHistory=true',undefined,'DELETE');
  assert(!await exists(file(rejectedNative)));
  console.log('PASS: rejected close falls back; rejected deletion retains transcript and fences resume across restart; explicit retry succeeds');

  await stopChild(server);await start({AGENTWORKS_DEMO_DELETE:'drop'});
  const uncertain=await create(),uncertainNative=native(uncertain.s);
  await api(path(uncertain.id)+'?providerHistory=true',undefined,'DELETE',502);
  assert(!await exists(file(uncertainNative)),'fixture deletes provider history before losing the acknowledgement');
  assert.equal((await snapshot(uncertain.id)).session.providerHistoryDeletion,'pending');
  await stopChild(server);await start();
  await api(path(uncertain.id)+'/resume',{},'POST',409);
  await api(path(uncertain.id)+'?providerHistory=true',undefined,'DELETE');
  await api(path(uncertain.id),undefined,'GET',404);
  console.log('PASS: lost deletion acknowledgement retains transcript; explicit retry handles native not-found without recreating context');
  const shutdown=await create(),shutdownNative=native(shutdown.s);
  await stopChild(server,{signal:'SIGTERM',timeout:10000});
  assert((await trace()).some(e=>e.method==='session/close'&&e.sessionId===shutdownNative&&e.phase==='closed'),'normal SIGTERM must close live sessions before exiting');
  assert(await exists(file(shutdownNative)),'normal shutdown must retain provider history');
  console.log('PASS: normal SIGTERM shutdown gracefully closes the adapter and preserves history');
  await start({AGENTWORKS_DEMO_CLOSE:'hang'});
  const hung=await create(),started=Date.now();
  await api(path(hung.id)+'/suspend',{});
  assert(Date.now()-started<9000,'graceful shutdown must be bounded');
  const hungSnapshot=await snapshot(hung.id);assert.equal(hungSnapshot.session.status,'suspended');
  assert(hungSnapshot.events.some(e=>e.kind==='provider_close'&&['failed','timeout'].includes(e.data.outcome)));
  console.log('PASS: hanging close has a bounded forced-stop fallback');
  await stopChild(server);await start({AGENTWORKS_DEMO_LIFECYCLE:'0',AGENTWORKS_DEMO_TRACE:'1'});
  const unsupported=await create(),unsupportedNative=native(unsupported.s),beforeUnsupported=(await trace()).length;
  await api(path(unsupported.id)+'/suspend',{});
  assert.equal((await trace()).length,beforeUnsupported,'unadvertised close must not be sent');
  await api(path(unsupported.id)+'?providerHistory=true',undefined,'DELETE',502);
  assert(!(await trace()).slice(beforeUnsupported).some(e=>['session/close','session/delete'].includes(e.method)),'unsupported methods must not reach the adapter');
  assert(await exists(file(unsupportedNative)));assert.equal((await snapshot(unsupported.id)).session.status,'suspended');
  await api(path(unsupported.id),undefined,'DELETE');
  console.log('PASS: unsupported lifecycle methods are never sent; local-only deletion remains available');
}finally{await stopChild(server);await removeFixture(fixture);}
