// Private server + deterministic demo ACP. No live service, model calls or restart.
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {once} from 'node:events';
import {mkdtemp,readFile} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';
const dir=await mkdtemp(join(tmpdir(),'agentworks-composer-'));
const base='http://127.0.0.1:4531',env={...process.env,AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:'4531',AGENTWORKS_ROOTS:dir,AGENTWORKS_CATALOG_PATH:join(dir,'catalog.json'),AGENTWORKS_DB_PATH:join(dir,'sessions.sqlite3'),AGENTWORKS_DEMO_STATE_DIR:join(dir,'demo')};
const server=spawn(resolve('target/debug/agentworks'),[],{env,stdio:['ignore','inherit','inherit']});
let exited=false;server.once('exit',()=>exited=true);
const delay=ms=>new Promise(ok=>setTimeout(ok,ms));
async function until(fn){for(let i=0;i<180;i++){if(exited)throw Error('Fixture server exited');const value=await fn();if(value)return value;await delay(50);}throw Error('Fixture timed out');}
async function api(path,body,status=200){const r=await fetch(base+path,{method:body===undefined?'GET':'POST',headers:{Origin:base,'Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body)});const text=await r.text();assert.equal(r.status,status,text);return status===200?JSON.parse(text):text;}
try{
  await until(async()=>{try{return (await fetch(base+'/api/status')).ok;}catch{return false;}});
  const session=await api('/api/sessions',{provider:'demo',cwd:dir,title:'Composer cancellation fixture'}),path='/api/sessions/'+session.id;
  const snapshot=()=>api(path),idle=()=>until(async()=>{const s=await snapshot();assert.notEqual(s.session.status,'error',JSON.stringify(s.events.slice(-3)));return s.session.status==='idle'&&s;});
  await idle();const text='  slow\n\nexact submitted prompt  \n',requestId='original';
  await api(path+'/prompt',{text,requestId});
  let s=await until(async()=>{const s=await snapshot();return s.events.some(e=>e.kind==='acp'&&e.data.sessionUpdate==='agent_message_chunk')&&s;});
  const user=s.events.findLast(e=>e.kind==='user'),before=s.events.filter(e=>e.kind==='acp');assert(before.length);
  await api(path+'/cancel?userSeq='+(user.seq+1),{},409);assert.equal((await snapshot()).session.status,'running');
  assert(!(await snapshot()).events.some(e=>e.kind==='stop_requested'));
  await api(path+'/cancel?userSeq='+user.seq,{});s=await idle();
  const marker=s.events.findLast(e=>e.kind==='stop_requested');assert.equal(marker.data.text,text);assert.equal(marker.data.requestId,requestId);assert.equal(marker.data.historyRewound,false);
  assert.equal(s.session.lastUserSeq,user.seq);assert.equal(s.events.filter(e=>e.kind==='user').length,1);
  assert(s.events.some(e=>e.kind==='turn_end'&&e.data.stopReason==='cancelled'));for(const event of before)assert(s.events.some(e=>e.seq===event.seq));
  const native=s.events.find(e=>e.kind==='provider_session').data.id;
  const provider=JSON.parse(await readFile(join(dir,'demo',native+'.json'),'utf8'));
  assert(provider.history.some(e=>e.sessionUpdate==='user_message_chunk'&&e.content.text===text));assert(provider.history.some(e=>e.sessionUpdate==='agent_message_chunk'));
  // A Stop arriving after completion records that fact without another submission.
  await api(path+'/cancel?userSeq='+user.seq,{});s=await snapshot();assert.equal(s.events.findLast(e=>e.kind==='stop_requested').data.alreadyFinished,true);
  await api(path+'/prompt',{text:'slow deliberate retry',requestId:'retry'});
  s=await snapshot();const retry=s.events.findLast(e=>e.kind==='user');assert.notEqual(retry.seq,user.seq);
  await api(path+'/cancel?userSeq='+user.seq,{},409);assert.equal((await snapshot()).session.status,'running');
  // Legacy Stop and approval cancellation remain supported.
  await api(path+'/stop',{});await idle();await api(path+'/prompt',{text:'approval',requestId:'approval'});
  await until(async()=>(await snapshot()).approvals.length);await api(path+'/stop',{});s=await idle();assert.equal(s.approvals.length,0);
  assert.equal(s.events.filter(e=>e.kind==='user').length,3);assert.equal(s.session.continuation.enabled,false);
  console.log('PASS: targeted/stale Stop, exact durable recovery marker, completion race, retained provider history, deliberate retry and approval cancellation');
}finally{
  if(!exited){const done=once(server,'exit');server.kill('SIGTERM');const timeout=setTimeout(()=>server.kill('SIGKILL'),5000);await done;clearTimeout(timeout);}
}
