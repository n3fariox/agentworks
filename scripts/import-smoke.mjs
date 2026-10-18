import assert from 'node:assert/strict';
import {mkdir,writeFile,unlink,rm} from 'node:fs/promises';
import {resolve,join} from 'node:path';
import {randomUUID} from 'node:crypto';
const base=process.env.AGENTWORKS_URL||'http://127.0.0.1:4319',headers={Origin:base,'Content-Type':'application/json'};
async function api(path,method='GET',body){const r=await fetch(base+path,{method,headers,body:body===undefined?undefined:JSON.stringify(body)});assert.equal(r.status,200,await r.clone().text());return r.json();}
async function settled(id){for(let n=0;n<200;n++){const snap=await api('/api/sessions/'+id);if(['idle','error'].includes(snap.session.status))return snap;await new Promise(r=>setTimeout(r,50));}throw new Error('Timed out');}
const cwd=resolve('.cache/import-smoke-'+randomUUID()),stateDir=process.env.AGENTWORKS_DEMO_STATE_DIR||resolve('.agentworks/demo'),providerID=randomUUID(),created=[];
await mkdir(cwd,{recursive:true});await mkdir(stateDir,{recursive:true});
const file=join(stateDir,providerID+'.json'),fixture={cwd,turn:2,model:'demo/quick',failLoad:true,title:'External retry test',updatedAt:new Date().toISOString(),history:[{sessionUpdate:'user_message_chunk',content:{type:'text',text:'Original question'}},{sessionUpdate:'agent_message_chunk',content:{type:'text',text:'Original answer'}}]};
try{
  await writeFile(file,JSON.stringify(fixture));
  const list=await api('/api/external-sessions','POST',{provider:'demo',cwd});assert(list.sessions.some(s=>s.sessionId===providerID));
  const request={provider:'demo',providerSessionId:providerID,cwd,title:'Import smoke'};
  const s=await api('/api/sessions/import','POST',request);created.push(s.id);
  const failed=await settled(s.id);assert.equal(failed.session.status,'error');assert(!failed.events.some(e=>e.kind==='acp'&&e.data.content?.text==='Original answer'),'partial replay must not persist');
  fixture.failLoad=false;await writeFile(file,JSON.stringify(fixture));
  await api('/api/sessions/'+s.id+'/resume','POST',{});const loaded=await settled(s.id);assert.equal(loaded.session.status,'idle');
  assert.equal(loaded.events.filter(e=>e.kind==='acp'&&e.data.content?.text==='Original answer').length,1);
  assert.equal(loaded.models.currentValue,'demo/quick');
  const duplicates=await Promise.all([api('/api/sessions/import','POST',request),api('/api/sessions/import','POST',request)]);assert(duplicates.every(d=>d.id===s.id));
  const registered=await api('/api/external-sessions','POST',{provider:'demo',cwd});assert.equal(registered.sessions.find(s=>s.sessionId===providerID).agentworksId,s.id);
  const beforeMissing=await api('/api/sessions');
  const missing=await fetch(base+'/api/sessions/import',{method:'POST',headers,body:JSON.stringify({...request,providerSessionId:randomUUID()})});
  assert.equal(missing.status,400,await missing.clone().text());
  assert.equal((await api('/api/sessions')).length,beforeMissing.length,'unverifiable history must not register a replacement context');
  const bad=await fetch(base+'/api/sessions/import',{method:'POST',headers,body:JSON.stringify({...request,cwd:'/tmp'})});assert.equal(bad.status,403);
  console.log('PASS: external discovery, atomic history import, retry without duplication, model preservation, duplicate import and missing-context/root checks');
}finally{
  for(const id of created)await api('/api/sessions/'+id,'DELETE');await unlink(file).catch(()=>{});await rm(cwd,{recursive:true,force:true});
}
