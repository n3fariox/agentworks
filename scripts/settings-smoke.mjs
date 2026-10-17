import assert from 'node:assert/strict';
import {mkdir} from 'node:fs/promises';
import {resolve} from 'node:path';
const base=process.env.AGENTWORKS_URL||'http://127.0.0.1:4319';
const headers={Origin:base,'Content-Type':'application/json'};
async function api(path,method='GET',body){const r=await fetch(base+path,{method,headers,body:body===undefined?undefined:JSON.stringify(body)});assert.equal(r.status,200,await r.clone().text());return r.json();}
async function wait(id){for(let n=0;n<100;n++){const snap=await api('/api/sessions/'+id);if(['idle','error'].includes(snap.session.status))return snap;await new Promise(r=>setTimeout(r,50));}throw new Error('Session timed out');}
const original=await api('/api/settings'),ids=[];
const cwd=resolve('.agentworks/settings-smoke-project');await mkdir(cwd,{recursive:true});
try{
  const models=await api('/api/models','POST',{provider:'demo',cwd});assert(models.options.some(o=>o.value==='demo/quick'));
  await api('/api/settings','PUT',{roots:[cwd],defaultModels:{demo:'demo/quick'}});
  const status=await api('/api/status');assert.deepEqual(status.roots,[cwd]);assert.equal(status.defaultModels.demo,'demo/quick');
  const denied=await fetch(base+'/api/sessions',{method:'POST',headers,body:JSON.stringify({provider:'demo',cwd:resolve('.')})});assert.equal(denied.status,403);
  const s=await api('/api/sessions','POST',{provider:'demo',cwd});ids.push(s.id);let snap=await wait(s.id);assert.equal(snap.models.currentValue,'demo/quick');
  assert(snap.commands.some(c=>c.name==='$demo-skill'));
  const explicit=await api('/api/sessions','POST',{provider:'demo',cwd,model:''});ids.push(explicit.id);assert.equal((await wait(explicit.id)).models.currentValue,'demo/steady');
  const stale=await api('/api/sessions','POST',{provider:'demo',cwd,model:'removed-model'});ids.push(stale.id);assert.equal((await wait(stale.id)).session.status,'error');
  const invalid=await fetch(base+'/api/settings',{method:'PUT',headers,body:JSON.stringify({roots:['/does-not-exist-agentworks'],defaultModels:{}})});assert.equal(invalid.status,400);
  assert.deepEqual((await api('/api/settings')).roots,[cwd]);
  await api('/api/settings','PUT',original);
  assert.equal((await api('/api/sessions/'+s.id)).models.currentValue,'demo/quick');
  console.log('PASS: model discovery, saved defaults, explicit overrides, stale-model failure, commands snapshot, root validation and enforcement');
}finally{for(const id of ids)await api('/api/sessions/'+id,'DELETE');await api('/api/settings','PUT',original);}
