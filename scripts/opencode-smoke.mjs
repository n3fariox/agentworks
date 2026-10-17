// Opt-in real inference check: one short prompt, using only an ACP-advertised free model.
import assert from 'node:assert/strict';
import {randomUUID} from 'node:crypto';
import {mkdir} from 'node:fs/promises';
import {resolve} from 'node:path';
const base=process.env.AGENTWORKS_URL||'http://127.0.0.1:4318';
const headers={'Content-Type':'application/json',Origin:base};
async function api(path,method='GET',body){const r=await fetch(base+path,{method,headers,body:body===undefined?undefined:JSON.stringify(body)});assert.equal(r.status,200,await r.clone().text());return r.json();}
async function wait(check,seconds=80){const until=Date.now()+seconds*1000;while(Date.now()<until){const value=await check();if(value)return value;await new Promise(r=>setTimeout(r,150));}throw new Error('OpenCode check timed out');}
const cwd=resolve('.agentworks/opencode-smoke-project');await mkdir(cwd,{recursive:true});
const s=await api('/api/sessions','POST',{provider:'opencode',cwd,title:'OpenCode smoke test'});const path='/api/sessions/'+s.id;
async function inspect(){const state=await api(path);if(state.session.status==='error')throw new Error(JSON.stringify(state.events.filter(e=>e.kind==='error')));return state;}
try{
  const ready=await wait(async()=>{const state=await inspect();return state.session.status==='idle'&&state;});
  assert(ready.models.options.length>0);console.log('Advertised free models:',ready.models.options.map(o=>o.value).join(', '));
  const chosen=ready.models.options.find(o=>o.value==='opencode/big-pickle')||ready.models.options[0];
  await api(path+'/model','POST',{model:chosen.value});
  await wait(async()=>{const state=await inspect();return state.session.status==='idle'&&state.models.currentValue===chosen.value;});
  const invalid=await fetch(base+path+'/model',{method:'POST',headers,body:JSON.stringify({model:'openai/paid-model'})});assert.equal(invalid.status,400);
  await api(path+'/prompt','POST',{text:'Reply with exactly ACP_OK. Do not use tools, read files, or run commands.',requestId:randomUUID()});
  const done=await wait(async()=>{const state=await inspect();return state.session.status==='idle'&&state.events.some(e=>e.kind==='turn_end')&&state;});
  const errors=done.events.filter(e=>e.kind==='error');assert.equal(errors.length,0,JSON.stringify(errors));
  const reply=done.events.filter(e=>e.kind==='acp'&&e.data.sessionUpdate==='agent_message_chunk'&&e.data.content?.type==='text').map(e=>e.data.content.text).join('');
  assert(reply.includes('ACP_OK'),'Expected a real model reply; received '+reply.slice(0,300));
  console.log('PASS: native OpenCode ACP session, advertised free-model selection, paid-model rejection, real streamed reply:',reply.trim());
}finally{await api(path,'DELETE');}
