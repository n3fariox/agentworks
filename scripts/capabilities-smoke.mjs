// Opt-in: initialize signed-in harnesses and run their local status/usage commands. No inference prompts.
import assert from 'node:assert/strict';
import {randomUUID} from 'node:crypto';
import {mkdir} from 'node:fs/promises';
import {resolve} from 'node:path';
const base=process.env.AGENTWORKS_URL||'http://127.0.0.1:4320';
const headers={Origin:base,'Content-Type':'application/json'};
async function api(path,method='GET',body){const r=await fetch(base+path,{method,headers,body:body===undefined?undefined:JSON.stringify(body)});assert.equal(r.status,200,await r.clone().text());return r.json();}
async function until(check){for(let n=0;n<450;n++){const v=await check();if(v)return v;await new Promise(r=>setTimeout(r,200));}throw new Error('Timed out');}
const cwd=resolve('.agentworks/capabilities-smoke-project');await mkdir(cwd,{recursive:true});
for(const provider of ['claude','codex']){
  const models=await api('/api/models','POST',{provider,cwd});assert(models.options.length>0,provider+' must advertise models');
  console.log(`${provider}: ${models.options.length} advertised models; default ${models.currentValue}`);
  const s=await api('/api/sessions','POST',{provider,cwd,model:models.currentValue,title:'Capability smoke test'});const path='/api/sessions/'+s.id;
  try{
    async function inspect(){const snap=await api(path);if(snap.session.status==='error')throw new Error(snap.events.filter(e=>e.kind==='error').map(e=>e.data.message).join('\n'));return snap;}
    const ready=await until(async()=>{const snap=await inspect();return snap.session.status==='idle'&&snap.commands.length&&snap;});
    assert.equal(ready.models.currentValue,models.currentValue);
    const command=provider==='claude'?'usage':'status';assert(ready.commands.some(c=>c.name===command));
    await api(path+'/prompt','POST',{text:'/'+command,requestId:randomUUID()});
    const done=await until(async()=>{const snap=await inspect();return snap.session.status==='idle'&&snap.events.some(e=>e.kind==='turn_end')&&snap;});
    assert(!done.events.some(e=>e.kind==='error'));
    const text=done.events.filter(e=>e.kind==='acp'&&e.data.sessionUpdate==='agent_message_chunk'&&e.data.content?.type==='text').map(e=>e.data.content.text).join('');
    // Do not print account identity or tokens from status output.
    const windows=text.split('\n').filter(line=>/5.?h|5.hour|7.?d|7.day|weekly|limits.*available/i.test(line));
    console.log(`${provider}: /${command} succeeded; ${ready.commands.length} commands; quota windows: ${windows.join(' ').slice(0,700)||'no matching windows reported'}`);
  }finally{await api(path,'DELETE');}
}
