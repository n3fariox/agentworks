// Opt-in integration with the installed, signed-in Antigravity CLI.
// No model calls unless --prompt is supplied. Uses isolated Agentworks state.
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {mkdtemp,mkdir} from 'node:fs/promises';
import {resolve,join} from 'node:path';
import {serverReady,stopChild,removeFixture} from '../tests/support/cleanup.mjs';
await mkdir('.cache',{recursive:true});
const fixture=await mkdtemp(resolve('.cache/antigravity-acp-'));
const base='http://127.0.0.1:4461';
const binary=resolve(process.env.AGENTWORKS_TEST_BINARY||'target/debug/agentworks');
let server,log='';
async function api(path,body){
  const response=await fetch(base+path,{method:body===undefined?'GET':'POST',headers:{Origin:base,'Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body),signal:AbortSignal.timeout(90000)});
  const result=await response.text();assert(response.ok,result);return JSON.parse(result);
}
async function until(fn){
  const deadline=Date.now()+120000;
  while(Date.now()<deadline){const value=await fn();if(value)return value;await new Promise(ok=>setTimeout(ok,200));}
  throw Error('Antigravity check timed out: '+log.slice(-4000));
}
const snapshot=id=>api('/api/sessions/'+id);
async function ready(id){return until(async()=>{const s=await snapshot(id);assert.notEqual(s.session.status,'error',JSON.stringify(s.events.slice(-5)));return s.session.status==='idle'&&s;});}
const replies=s=>s.events.filter(e=>e.kind==='acp').map(e=>e.data.update??e.data).filter(u=>u.sessionUpdate==='agent_message_chunk').map(u=>u.content?.text||'').join('');
const native=s=>s.events.findLast(e=>e.kind==='provider_session')?.data.id;
try{
  server=spawn(binary,[],{env:{...process.env,AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:'4461',AGENTWORKS_ROOTS:fixture,AGENTWORKS_CATALOG_PATH:join(fixture,'catalog.json'),AGENTWORKS_DB_PATH:join(fixture,'sessions.sqlite3')},stdio:['ignore','pipe','pipe']});
  server.stdout.on('data',b=>log+=b);server.stderr.on('data',b=>log+=b);
  await until(()=>{assert.equal(server.exitCode,null,log);return serverReady(server,base);});
  const status=await api('/api/status');
  assert(status.providers.some(p=>p.id==='antigravity'&&p.installed));
  const created=await api('/api/sessions',{provider:'antigravity',cwd:fixture,title:'Official Antigravity ACP smoke'});
  let current=await ready(created.id);const id=native(current);assert(id,'ACP must supply a native session ID');
  console.log('PASS: native ACP session created',id);
  if(process.argv.includes('--prompt')){
    await api(`/api/sessions/${created.id}/prompt`,{text:'Remember the marker amber-orchid-731. Reply with only that marker. Do not use any tools.',requestId:crypto.randomUUID()});
    current=await ready(created.id);
    assert(replies(current).includes('amber-orchid-731'),JSON.stringify(current.events.slice(-10)));
    assert(!current.events.some(e=>e.kind==='error'),JSON.stringify(current.events.slice(-10)));
    console.log('PASS: real Gemini reply');
  }
  await api(`/api/sessions/${created.id}/suspend`,{});
  assert.equal((await snapshot(created.id)).session.status,'suspended');
  await api(`/api/sessions/${created.id}/resume`,{});
  current=await ready(created.id);assert.equal(native(current),id,'Resume must retain native session ID');
  console.log('PASS: suspend/resume preserves ACP session ID');
  if(process.argv.includes('--prompt')){
    const beforeReply=replies(current).length;
    await api(`/api/sessions/${created.id}/prompt`,{text:'What marker did I ask you to remember? Reply with only the marker. Do not use tools.',requestId:crypto.randomUUID()});
    current=await ready(created.id);
    assert(replies(current).slice(beforeReply).includes('amber-orchid-731'),JSON.stringify(current.events.slice(-10)));
    assert(!current.events.some(e=>e.kind==='error'),JSON.stringify(current.events.slice(-10)));
    console.log('PASS: resumed Gemini conversation remembers prior context');
  }
}finally{await stopChild(server);await removeFixture(fixture);}
