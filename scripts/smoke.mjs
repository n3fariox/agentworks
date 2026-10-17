import assert from 'node:assert/strict';
import {randomUUID} from 'node:crypto';
import WebSocket from 'ws';
const base=process.env.AGENTWORKS_URL||'http://127.0.0.1:4317';
const headers={'Content-Type':'application/json',Origin:base};
async function request(path,method='GET',data){const response=await fetch(base+path,{method,headers,body:data===undefined?undefined:JSON.stringify(data)});assert.equal(response.status,200,await response.clone().text());return response.json();}
async function until(check,timeout=12000){const end=Date.now()+timeout;while(Date.now()<end){const value=await check();if(value)return value;await new Promise(r=>setTimeout(r,60));}throw new Error('Timed out waiting for session state');}
const status=await request('/api/status');
const session=await request('/api/sessions','POST',{provider:'demo',cwd:status.roots[0],title:'Smoke test'});
const path='/api/sessions/'+session.id;
let ws;
try{
  await until(async()=>{const s=await request(path);return s.session.status==='idle'&&s;});
  let received=[];
  ws=new WebSocket(base.replace('http','ws')+path+'/events',{headers:{Origin:base}});
  ws.on('message',data=>received.push(JSON.parse(data.toString())));
  await until(()=>received.some(e=>e.kind==='snapshot'));
  const turn={text:'hello',requestId:randomUUID()};
  await request(path+'/prompt','POST',turn);
  await until(()=>received.some(e=>e.kind==='acp'&&e.data.sessionUpdate==='agent_message_chunk'));
  // Disconnecting a browser must leave the agent running.
  ws.close();
  const first=await until(async()=>{const s=await request(path);return s.events.some(e=>e.kind==='turn_end')&&s.session.status==='idle'&&s;});
  assert(first.events.some(e=>e.kind==='acp'&&JSON.stringify(e.data).includes('Rust')));
  await request(path+'/prompt','POST',turn);
  assert.equal((await request(path)).events.filter(e=>e.kind==='user').length,1,'duplicate turn must not execute twice');
  await request(path+'/prompt','POST',{text:'approval',requestId:randomUUID()});
  const waiting=await until(async()=>{const s=await request(path);return s.approvals.length&&s;});
  assert.equal(waiting.session.status,'waiting');
  const activity=await request('/api/activity');
  assert.equal(activity.safeToClose,false);
  assert(activity.activeTurns>=1);
  const ownActivity=activity.sessions.find(s=>s.id===session.id);
  assert.equal(ownActivity.turnRunning,true);assert.equal(ownActivity.canClose,false);assert.equal(ownActivity.pendingApprovals,1);
  const approval=waiting.approvals[0];
  const bad=await fetch(base+path+'/approvals/'+approval.id,{method:'POST',headers,body:JSON.stringify({optionId:'invented'})});assert.equal(bad.status,400);
  await request(path+'/approvals/'+approval.id,'POST',{optionId:'allow'});
  const second=await until(async()=>{const s=await request(path);return s.events.filter(e=>e.kind==='turn_end').length===2&&s.session.status==='idle'&&s;});
  assert(second.events.some(e=>e.kind==='acp'&&e.data.status==='completed'));
  const stale=await fetch(base+path+'/approvals/'+approval.id,{method:'POST',headers,body:JSON.stringify({optionId:'allow'})});assert.equal(stale.status,409);
  await request(path+'/prompt','POST',{text:'slow',requestId:randomUUID()});
  await until(async()=>{const s=await request(path);return s.events.filter(e=>e.kind==='acp').length>second.events.filter(e=>e.kind==='acp').length;});
  await request(path+'/cancel','POST',{});
  const cancelled=await until(async()=>{const s=await request(path);return s.events.some(e=>e.kind==='turn_end'&&e.data.stopReason==='cancelled')&&s.session.status==='idle'&&s;});
  assert(cancelled);
  const afterCancel=(await request('/api/activity')).sessions.find(s=>s.id===session.id);
  assert.equal(afterCancel.turnRunning,false);assert.equal(afterCancel.canClose,true);
  // Cancellation must also resolve an outstanding ACP permission request.
  await request(path+'/prompt','POST',{text:'approval slow',requestId:randomUUID()});
  await until(async()=>{const s=await request(path);return s.approvals.length&&s;});
  await request(path+'/cancel','POST',{});
  await until(async()=>{const s=await request(path);return s.session.status==='idle'&&!s.approvals.length;});
  const markdown=await request('/api/markdown','POST',{text:'# Test\n<script>alert(1)</script>\n[bad](javascript:alert(1))'});
  assert(markdown.html.includes('<h1>Test</h1>'));assert(!markdown.html.includes('<script'));assert(!markdown.html.includes('javascript:'));
  const traversal=await fetch(base+path+'/file?path='+encodeURIComponent('/etc/passwd'));assert.equal(traversal.status,403);
  const wrongRoot=await fetch(base+'/api/sessions',{method:'POST',headers,body:JSON.stringify({provider:'demo',cwd:'/etc'})});assert.equal(wrongRoot.status,403);
  const crossOrigin=await fetch(base+path+'/cancel',{method:'POST',headers:{...headers,Origin:'http://untrusted.invalid'},body:'{}'});assert.equal(crossOrigin.status,403);
  const missingOrigin=await fetch(base+path+'/cancel',{method:'POST',headers:{'Content-Type':'application/json'},body:'{}'});assert.equal(missingOrigin.status,403);
  console.log('PASS: ACP multi-turn sessions, streaming, disconnect survival, idempotency, approval validation, cancellation, Markdown sanitization, activity/safe-to-close status, root and origin checks');
}finally{ws?.close();await request(path,'DELETE');}
