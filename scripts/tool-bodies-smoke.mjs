// Browser transport for tool cards: compacted snapshot tool events, tool bodies
// on demand, reconnect resume and Starlark tool headers, end to end with the
// demo agent's recorded Bazel output. No command or model is run.
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
const sockets=[];
// Resolves with the first message (the snapshot); later messages collect in `live`.
function open(path){
  return new Promise((ok,fail)=>{
    const ws=new WebSocket(base.replace('http','ws')+path,{headers:{Origin:base}});sockets.push(ws);
    const live=[];let snapshot=null;
    ws.on('message',m=>{if(snapshot)live.push(JSON.parse(m));else{snapshot=JSON.parse(m);ok({snapshot,bytes:m.length,live});}});
    ws.once('error',fail);
  });
}
const isTool=e=>e.kind==='acp'&&['tool_call','tool_call_update'].includes(e.data.sessionUpdate);
const cwd=resolve('.agentworks/tool-bodies-smoke-project');await mkdir(cwd,{recursive:true});
const roots=(await api('/api/settings')).roots;if(!roots.includes(cwd))await api('/api/settings','PUT',{...(await api('/api/settings')),roots:[...roots,cwd]});
const header=`load("@agentworks//tool.star", "ToolCall", "ToolContext")
def bazel(ctx: ToolContext, tool: ToolCall) -> str | None:
    return "Bazel tests in " + ctx.provider
agent.register_tool_header(bazel, title_pattern = "^bazel ")
`;
const ids=[];
try{
  assert((await api('/api/scripts')).scripts.some(s=>s.name==='tool_header.star'&&s.builtin&&s.enabled&&s.hooks[0].hook==='tool_header'),'built-in header script is seeded');
  await api('/api/scripts/a_smoke_header.star','PUT',{source:header});
  const s=await api('/api/sessions','POST',{provider:'demo',cwd,title:'Tool bodies smoke'});ids.push(s.id);await idle(s.id);
  const watch=await open(`/api/sessions/${s.id}/events`);
  await api(`/api/sessions/${s.id}/prompt`,'POST',{text:'bazel demo',requestId:'tool-bodies-smoke-1'});
  const full=await idle(s.id);
  const toolEvents=full.events.filter(isTool),tool=toolEvents[0].data.toolCallId;
  assert(toolEvents.some(e=>e.data._meta?.terminal_output_delta)&&toolEvents.some(e=>e.data.rawInput),'the HTTP snapshot keeps stored bodies');
  const live=await until(()=>watch.live.find(e=>e.kind==='tool_header'));
  assert.deepEqual(live.data,{toolCallId:tool,header:'Bazel tests in demo'});
  assert(watch.live.filter(isTool).some(e=>e.data._meta?.terminal_output_delta),'live tool events are unchanged');
  console.log('PASS: header hooks publish live tool_header events; live and HTTP events keep bodies');

  const browser=await open(`/api/sessions/${s.id}/events`);
  const compact=browser.snapshot.events.filter(isTool);
  assert.equal(browser.snapshot.resumed,null);
  assert(compact.length>0&&compact.length<toolEvents.length,'output-only updates are dropped');
  for(const e of compact){
    assert(!('rawInput' in e.data)&&!('content' in e.data)&&!('rawOutput' in e.data),JSON.stringify(e));
    assert(!e.data._meta?.terminal_output_delta&&!e.data._meta?.terminal_output,JSON.stringify(e));
  }
  assert(compact.some(e=>e.data.bodyOmitted)&&compact.some(e=>e.data.status==='failed'),'final status stays');
  assert(compact.some(e=>e.data._meta?.future_fixture),'unknown metadata stays');
  assert.equal(compact.at(-1).data.header,'Bazel tests in demo','the header rides on the last tool event');
  assert(compact.some(e=>e.data._meta?.terminal_exit),'exit metadata stays in the snapshot');
  const fullBytes=JSON.stringify(full).length;
  assert(browser.bytes<fullBytes,`browser snapshot ${browser.bytes} bytes < full ${fullBytes}`);
  console.log(`PASS: browser snapshot omits tool bodies (${browser.bytes} vs ${fullBytes} bytes)`);

  const body=await api(`/api/sessions/${s.id}/tools/${encodeURIComponent(tool)}`);
  assert.deepEqual(body.events,toolEvents,'the stored body is every event of the tool, unchanged');
  const acp=toolEvents[0].data.acpSessionId;
  if(acp)assert.equal((await api(`/api/sessions/${s.id}/tools/${encodeURIComponent(tool)}?acpSession=${encodeURIComponent(acp)}`)).events.length,toolEvents.length);
  assert.equal((await call(`/api/sessions/${s.id}/tools/${encodeURIComponent(tool)}?acpSession=other`)).status,404,'other ACP sessions do not match');
  assert.equal((await call(`/api/sessions/${s.id}/tools/missing`)).status,404);
  console.log('PASS: tool bodies load on demand from storage');

  const seq=browser.snapshot.seq;
  const resumed=await open(`/api/sessions/${s.id}/events?after=${seq}`);
  assert.equal(resumed.snapshot.resumed,seq);assert.deepEqual(resumed.snapshot.events,[]);
  assert.equal(resumed.snapshot.toolHeaders[tool],'Bazel tests in demo','headers refresh even without newer events');
  assert.equal(resumed.snapshot.session.id,s.id,'session state is still sent');
  const partial=await open(`/api/sessions/${s.id}/events?after=${seq-2}`);
  assert.equal(partial.snapshot.resumed,seq-2);
  assert(partial.snapshot.events.length&&partial.snapshot.events.every(e=>e.seq>seq-2&&e.seq<=seq),'only newer events are sent');
  const ahead=await open(`/api/sessions/${s.id}/events?after=${seq+5}`);
  assert.equal(ahead.snapshot.resumed,null,'an unknown future sequence gets a full snapshot');
  const meta=await api(`/api/sessions/${s.id}?view=meta`);
  assert.equal(meta.id,s.id);assert(!('events' in meta));
  console.log('PASS: reconnects resume after the last applied event; existence checks are metadata only');

  await api('/api/scripts/a_smoke_header.star','DELETE');
  const cleared=await until(()=>watch.live.find(e=>e.kind==='tool_header'&&e.data.header===null));
  assert.equal(cleared.data.toolCallId,tool);
  const after=await open(`/api/sessions/${s.id}/events`);
  assert(!after.snapshot.events.filter(isTool).some(e=>e.data.header),'removed hooks remove headers');
  const afterResume=await open(`/api/sessions/${s.id}/events?after=${seq}`);
  assert.equal(afterResume.snapshot.resumed,seq);
  assert.deepEqual(afterResume.snapshot.toolHeaders,{},'a reconnect clears headers removed while disconnected');
  console.log('PASS: script changes recompute headers for open sessions');
}finally{
  for(const ws of sockets)ws.terminate();
  await call('/api/scripts/a_smoke_header.star','DELETE');
  for(const id of ids)await call('/api/sessions/'+id,'DELETE');
}
