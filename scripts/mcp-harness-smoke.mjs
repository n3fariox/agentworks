// Opt-in real inference: each selected signed-in harness sends one message through MCP.
import assert from 'node:assert/strict';
const base=process.env.AGENTWORKS_URL||'http://127.0.0.1:4323';
const harnesses=(process.env.AGENTWORKS_TEST_MCP_HARNESSES||'').split(',').filter(Boolean);
assert(harnesses.length,'Set AGENTWORKS_TEST_MCP_HARNESSES=claude,codex,opencode; this sends real prompts.');
const delay=ms=>new Promise(r=>setTimeout(r,ms));
async function api(path,body,method=body?'POST':'GET'){const r=await fetch(base+path,{method,headers:{Origin:base,'Content-Type':'application/json'},body:body?JSON.stringify(body):undefined});if(!r.ok)throw Error(await r.text());return r.json();}
const ids=[];let project;
async function idle(id){for(let i=0;i<1200;i++){const s=await api('/api/sessions/'+id);if(s.approvals.length){for(const a of s.approvals){const name=JSON.stringify(a.request.toolCall||{});assert(/agentworks|send_message/i.test(name),'Unexpected permission request '+name);const allow=a.request.options.find(o=>o.kind==='allow_once');assert(allow);await api(`/api/sessions/${id}/approvals/${a.id}`,{optionId:allow.optionId});}}if(s.session.status==='idle')return s;if(s.session.status==='error')throw Error(JSON.stringify(s.events.slice(-4)));await delay(250);}throw Error('Harness timed out');}
try{
 const {roots}=await api('/api/status');project=await api('/api/projects',{name:'MCP harness verification'});
 const peer=await api('/api/sessions',{provider:'demo',cwd:roots[0],projectId:project.id,title:'MCP verification inbox'});ids.push(peer.id);await idle(peer.id);await api(`/api/sessions/${peer.id}/agent-access`,{communicate:true},'PUT');await idle(peer.id);
 for(const provider of harnesses){
  const s=await api('/api/sessions',{provider,agentAccess:{communicate:true},cwd:roots[0],projectId:project.id,title:'MCP verification '+provider});ids.push(s.id);await idle(s.id);
  if(provider==='claude'){await api(`/api/sessions/${s.id}/prompt`,{text:'/mcp',requestId:crypto.randomUUID()});const status=await idle(s.id);const text=status.events.filter(e=>e.kind==='acp'&&e.data.sessionUpdate==='agent_message_chunk').map(e=>e.data.content?.text||'').join('');console.log(text);assert(/`agentworks`: [4-9] tools/.test(text),'MCP tools not ready; skip inference');}
  const marker='MCP_'+provider+'_'+crypto.randomUUID();
  await api(`/api/sessions/${s.id}/prompt`,{text:`Integration check: use the agentworks MCP send_message tool exactly once with recipient=${peer.id}, text=${marker}, requestId=${marker}. Do not use shell, files, other tools, or HTTP. Then reply DONE.`,requestId:crypto.randomUUID()});
  const snapshot=await idle(s.id);
  const mailbox=await api(`/api/sessions/${peer.id}/mailbox`);if(!mailbox.messages.some(m=>m.sender===s.id&&m.text===marker)){console.log(snapshot.events.filter(e=>e.kind==='acp'&&e.data.sessionUpdate==='agent_message_chunk').map(e=>e.data.content?.text||'').join(''));throw Error(provider+' did not send its MCP message');}
  const calls=snapshot.events.filter(e=>e.kind==='acp'&&['tool_call','tool_call_update'].includes(e.data.sessionUpdate));assert(calls.some(e=>/agentworks|send_message/i.test(JSON.stringify(e.data))),provider+' missing MCP tool event');
  console.log('PASS: '+provider+' actual ACP -> Agentworks MCP -> durable peer message');
 }
}finally{for(const id of ids)await api('/api/sessions/'+id,undefined,'DELETE').catch(()=>{});if(project)await api('/api/projects/'+project.id,undefined,'DELETE').catch(()=>{});}
