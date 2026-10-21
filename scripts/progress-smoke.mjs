// U-05/E-01: server-side Bazel progress from the built-in Starlark parser script,
// the script API and restart replay are covered by the Rust tests; this checks the
// running service end to end with the demo agent. No command or model is run.
import assert from 'node:assert/strict';
import {mkdir} from 'node:fs/promises';
import {resolve} from 'node:path';
import WebSocket from 'ws';
const base=process.env.AGENTWORKS_URL||'http://127.0.0.1:4319';
const headers={Origin:base,'Content-Type':'application/json'};
async function call(path,method='GET',body){const r=await fetch(base+path,{method,headers,body:body===undefined?undefined:JSON.stringify(body)});const raw=await r.text();let parsed=raw;try{parsed=JSON.parse(raw);}catch{}return {status:r.status,body:parsed};}
async function api(path,method,body){const r=await call(path,method,body);assert.equal(r.status,200,JSON.stringify(r.body));return r.body;}
const delay=ms=>new Promise(r=>setTimeout(r,ms));
async function idle(id){for(let n=0;n<200;n++){const snap=await api('/api/sessions/'+id);if(['idle','error'].includes(snap.session.status))return snap;await delay(50);}throw new Error('Session timed out');}
async function until(fn){for(let n=0;n<100;n++){const v=await fn();if(v)return v;await delay(50);}throw new Error('Condition timed out');}
const text=v=>{const p=v.progress;return p?[v.parser+' · '+p.phase,...p.counts.map(c=>`${c.done} / ${c.total} ${c.unit}`)].join(' · '):null;};
const cwd=resolve('.agentworks/progress-smoke-project');await mkdir(cwd,{recursive:true});
const roots=(await api('/api/settings')).roots;if(!roots.includes(cwd))await api('/api/settings','PUT',{...(await api('/api/settings')),roots:[...roots,cwd]});
const ids=[];let socket;
try{
  const scripts=(await api('/api/scripts')).scripts;
  const builtin=scripts.find(s=>s.name==='bazel_progress.star');assert(builtin?.builtin&&builtin.enabled,'built-in Bazel parser is seeded and enabled');
  const s=await api('/api/sessions','POST',{provider:'demo',cwd,title:'Progress smoke'});ids.push(s.id);await idle(s.id);
  const live=[];socket=new WebSocket(base.replace('http','ws')+`/api/sessions/${s.id}/events`,{headers:{Origin:base}});
  socket.on('message',m=>{const e=JSON.parse(m);if(e.kind==='tool_progress')live.push(e);});
  await new Promise(r=>socket.once('open',r));
  await api(`/api/sessions/${s.id}/prompt`,'POST',{text:'bazel demo',requestId:'progress-smoke-1'});
  let snap=await idle(s.id);
  await until(()=>live.some(e=>text(e.data)==='Bazel · Testing · 4 / 9 actions · 3 / 5 tests'));
  assert(live.some(e=>text(e.data)==='Bazel · Testing · 2 / 6 actions · 1 / 3 tests'),'intermediate progress is published live');
  const acpSeqs=new Set(snap.events.filter(e=>e.kind==='acp').map(e=>e.seq));
  assert(live.every(e=>acpSeqs.has(e.seq)),'views carry their source event seq');
  assert(!snap.events.some(e=>e.kind==='tool_progress'),'derived progress is not stored in history');
  snap=await until(async()=>{const v=await api('/api/sessions/'+s.id);return Object.keys(v.toolProgress).length&&v;});
  const [view]=Object.values(snap.toolProgress);assert.equal(text(view),'Bazel · Testing · 4 / 9 actions · 3 / 5 tests');
  console.log('PASS: built-in Starlark parser publishes live progress and snapshot views; raw history unchanged');

  // Invalid scripts are rejected with an attributable diagnostic and never stored.
  const bad=await call('/api/scripts/bad.star','PUT',{source:'load("@agentworks//progress.star", "LineContext", "Progress")\nCOMMAND_PATTERN = "x"\nDISPLAY_NAME = "X"\ndef on_line(ctx: LineContext, line: str) -> Progress | None:\n    return ctx.missing\n'});
  assert.equal(bad.status,400);assert.match(JSON.stringify(bad.body),/bad\.star.*missing/);
  assert(!(await api('/api/scripts')).scripts.some(x=>x.name==='bad.star'));
  assert.equal((await call('/api/scripts/bazel_progress.star','DELETE')).status,409);
  // A disabled parser stops new derived progress; raw output still arrives.
  await api('/api/scripts/bazel_progress.star/enabled','PUT',{enabled:false});
  const before=live.length;
  await api(`/api/sessions/${s.id}/prompt`,'POST',{text:'bazel demo',requestId:'progress-smoke-2'});await idle(s.id);await delay(300);
  assert.equal(live.length,before,'no progress while the parser is disabled');
  await api('/api/scripts/bazel_progress.star/enabled','PUT',{enabled:true});
  // The Settings starter template must install as written.
  const panel=await (await fetch(base+'/parser-scripts.js')).text();
  const template=/String\.raw`([\s\S]*?)`;/.exec(panel)[1];
  await api('/api/scripts/smoke_template.star','PUT',{source:template});
  assert((await api('/api/scripts')).scripts.some(x=>x.name==='smoke_template.star'&&x.enabled&&!x.error));
  await api('/api/scripts/smoke_template.star','DELETE');
  console.log('PASS: script validation, built-in protection, disable/enable and the starter template');
}finally{
  socket?.terminate();
  await call('/api/scripts/bazel_progress.star/enabled','PUT',{enabled:true});
  await call('/api/scripts/smoke_template.star','DELETE');
  for(const id of ids)await call('/api/sessions/'+id,'DELETE');
}
