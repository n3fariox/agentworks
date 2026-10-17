// Opt-in: one short prompt per provider using the host's existing agent sign-in.
import assert from 'node:assert/strict';
import {randomUUID} from 'node:crypto';
import {mkdir} from 'node:fs/promises';
import {resolve} from 'node:path';
const base=process.env.AGENTWORKS_URL||'http://127.0.0.1:4318';
const headers={'Content-Type':'application/json',Origin:base};
async function api(path,method='GET',body){const r=await fetch(base+path,{method,headers,body:body===undefined?undefined:JSON.stringify(body)});assert.equal(r.status,200,await r.clone().text());return r.json();}
async function wait(check,seconds=90){const until=Date.now()+seconds*1000;while(Date.now()<until){const value=await check();if(value)return value;await new Promise(r=>setTimeout(r,200));}throw new Error('Sign-in test timed out');}
const cwd=resolve('.agentworks/signin-smoke-project');await mkdir(cwd,{recursive:true});
let failed=false;
for(const provider of ['claude','codex']){
  const s=await api('/api/sessions','POST',{provider,cwd,title:'Sign-in smoke test'});const path='/api/sessions/'+s.id;
  try{
    async function inspect(){const state=await api(path);if(state.session.status==='error')throw new Error(state.events.filter(e=>e.kind==='error').map(e=>e.data.message).join('\n'));return state;}
    await wait(async()=>{const state=await inspect();return state.session.status==='idle'&&state;});
    await api(path+'/prompt','POST',{text:'Reply with exactly SIGNIN_OK. Do not use tools, read files, or run commands.',requestId:randomUUID()});
    const done=await wait(async()=>{const state=await inspect();return state.session.status==='idle'&&state.events.some(e=>e.kind==='turn_end')&&state;});
    const errors=done.events.filter(e=>e.kind==='error');assert.equal(errors.length,0,errors.map(e=>e.data.message).join('\n'));
    const reply=done.events.filter(e=>e.kind==='acp'&&e.data.sessionUpdate==='agent_message_chunk'&&e.data.content?.type==='text').map(e=>e.data.content.text).join('');
    assert(reply.includes('SIGNIN_OK'),'Expected a model reply; received '+reply.slice(0,200));
    console.log(`PASS: ${provider} reused existing host sign-in and streamed SIGNIN_OK`);
  }catch(error){failed=true;console.error(`${provider}: ${error.message.slice(0,2000)}`);}
  finally{await api(path,'DELETE');}
}
if(failed)process.exitCode=1;
