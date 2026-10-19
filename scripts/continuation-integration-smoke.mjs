// Batch 007: exercise the three schedulers together using isolated demo state.
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {mkdtemp} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';
import {once} from 'node:events';
const dir=await mkdtemp(join(tmpdir(),'agentworks-continuation-integration-'));
const base='http://127.0.0.1:4488';
const delay=ms=>new Promise(r=>setTimeout(r,ms));
let server;
async function api(path,body,method=body===undefined?'GET':'POST',code=200){
  const r=await fetch(base+path,{method,headers:{Origin:base,'Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body),signal:AbortSignal.timeout(12000)});
  const text=await r.text();assert.equal(r.status,code,text);return code===200?JSON.parse(text):text;
}
async function until(fn){for(let i=0;i<300;i++){const value=await fn();if(value)return value;await delay(100);}throw Error('Combined continuation fixture timed out');}
const path=id=>`/api/sessions/${id}`;
const snap=id=>api(path(id));
const idle=id=>until(async()=>{const s=await snap(id);assert.notEqual(s.session.status,'error',JSON.stringify(s.events.slice(-5)));return s.session.status==='idle'&&s;});
const prompt=(id,text)=>api(path(id)+'/prompt',{text,requestId:crypto.randomUUID()});
const queue=(id,text)=>api(path(id)+'/queue',{text,requestId:crypto.randomUUID()});
const users=s=>s.events.filter(e=>e.kind==='user').map(e=>e.data);
const authorize=id=>api(path(id)+'/inbox-delivery',{enabled:true,requestId:crypto.randomUUID(),deadline:Date.now()+120000,maxTurns:1,cooldownSeconds:5,maxBatch:5},'PUT');
try{
  server=spawn(resolve('target/debug/agentworks'),[],{env:{...process.env,AGENTWORKS_PORT:'4488',AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_ROOTS:dir,AGENTWORKS_CATALOG_PATH:join(dir,'catalog.json'),AGENTWORKS_DB_PATH:join(dir,'sessions.sqlite3'),AGENTWORKS_DEMO_STATE_DIR:join(dir,'demo'),AGENTWORKS_DEMO_STATUS_DELAY_MS:'1000'},stdio:['ignore','inherit','inherit']});
  await until(async()=>{if(server.exitCode!==null)throw Error('Fixture exited');try{return(await fetch(base+'/api/status')).ok;}catch{return false;}});
  const project=await api('/api/projects',{name:'Combined continuation fixture'});
  async function create(title){const s=await api('/api/sessions',{provider:'demo',cwd:dir,title,projectId:project.id,agentAccess:{communicate:true}});await idle(s.id);return s.id;}
  const sender=await create('Peer'),id=await create('Recipient');
  const send=text=>api(path(sender)+'/mailbox',{recipient:id,text,requestId:crypto.randomUUID()});
  await authorize(id);await prompt(id,'approval');
  await until(async()=>(await snap(id)).approvals.length);
  await queue(id,'First explicit input');await queue(id,'Second explicit input');await send('Peer result');
  await delay(6000);assert.equal(users(await snap(id)).length,1);
  const held=await snap(id);await api(path(id)+`/approvals/${held.approvals[0].id}`,{optionId:'allow'});
  await until(async()=>users(await snap(id)).length===4);let done=await idle(id);
  assert.deepEqual(users(done).slice(1,3).map(u=>u.text),['First explicit input','Second explicit input']);
  assert.equal(users(done)[3].initiatedBy,'human-authorized inbox delivery');
  // A paused explicit input blocks inbox wake until deliberate queue resume.
  await prompt(id,'approval');await until(async()=>(await snap(id)).approvals.length);
  await queue(id,'Explicit input after inspection');await api(path(id)+'/stop',{});await idle(id);
  const before=users(await snap(id)).length;await authorize(id);await send('Later peer result');await delay(6000);
  assert.equal(users(await snap(id)).length,before);assert.equal((await api(path(id)+'/inbox-delivery')).policy.attempts,0);
  await api(path(id)+'/queue/resume',{});await until(async()=>users(await snap(id)).length===before+2);done=await idle(id);
  assert.equal(users(done)[before].text,'Explicit input after inspection');assert.equal(users(done)[before+1].inboxDelivery,true);
  // Status reads preserve the quota timer and cannot start either pending queue.
  await api(path(id)+'/continuation',{enabled:true,maxAttempts:2},'PUT');await prompt(id,'quota known');await idle(id);
  await api(path(id)+'/continuation',{enabled:true,manualDueAt:Date.now()+60000},'PUT');
  await queue(id,'Must await quota recovery');await authorize(id);await send('Quota-held peer result');
  const due=(await snap(id)).session.continuation.dueAt;await prompt(id,'/status');await idle(id);await delay(6000);
  done=await snap(id);assert.equal(done.session.continuation.dueAt,due);assert(done.session.continuation.limited);
  assert.equal((await api(path(id)+'/inbox-delivery')).policy.attempts,0);
  assert(!users(done).some(u=>u.text==='Must await quota recovery'));
  await api(path(id)+'/queue/resume',{},'POST',409);await api(path(id)+'/stop',{});
  console.log('PASS combined FIFO-before-inbox, paused queue priority, deliberate resume, status-probe preservation and quota gates');
  console.log(`Artifacts: ${dir}`);
}finally{if(server&&server.exitCode===null){const exited=once(server,'exit');server.kill('SIGINT');await exited;}}
