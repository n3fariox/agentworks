import assert from 'node:assert/strict';
import {randomUUID} from 'node:crypto';
const base=process.env.AGENTWORKS_URL||'http://127.0.0.1:4319';
const headers={Origin:base,'Content-Type':'application/json'};
async function api(path,method='GET',body){const r=await fetch(base+path,{method,headers,body:body===undefined?undefined:JSON.stringify(body)});assert.equal(r.status,200,await r.clone().text());return r.json();}
async function until(check){for(let n=0;n<200;n++){const result=await check();if(result)return result;await new Promise(r=>setTimeout(r,50));}throw new Error('Timed out');}
const createdProjects=[],createdSessions=[];
try{
  const config=await api('/api/status');
  const first=await api('/api/projects','POST',{name:'Smoke project '+randomUUID()});createdProjects.push(first.id);
  const second=await api('/api/projects','POST',{name:'Smoke other '+randomUUID()});createdProjects.push(second.id);
  const s=await api('/api/sessions','POST',{provider:'demo',cwd:config.roots[0],projectId:first.id,hostId:'local'});createdSessions.push(s.id);
  assert.equal(s.projectId,first.id);assert.equal(s.hostId,'local');assert(s.locationId);
  await until(async()=>{const snap=await api('/api/sessions/'+s.id);return snap.session.status==='idle';});
  const path='/api/sessions/'+s.id;
  await api(path+'/prompt','POST',{text:'approval',requestId:randomUUID()});
  const waiting=await until(async()=>{const snap=await api(path);return snap.approvals.length&&snap;});
  const moved=await api(path+'/project','POST',{projectId:second.id});
  assert.equal(moved.locationId,s.locationId);assert.equal(moved.cwd,s.cwd);assert.equal(moved.projectId,second.id);
  const afterMove=await api(path);assert.equal(afterMove.session.status,'waiting');assert.equal(afterMove.approvals[0].id,waiting.approvals[0].id);
  await api('/api/projects/'+second.id,'PATCH',{name:'Smoke renamed '+randomUUID()});
  await api('/api/projects/'+second.id,'DELETE');
  const afterDelete=await api(path);assert.equal(afterDelete.session.projectId,null);assert.equal(afterDelete.approvals.length,1);
  await api(path+'/approvals/'+waiting.approvals[0].id,'POST',{optionId:'deny'});
  await until(async()=>{const snap=await api(path);return snap.session.status==='idle';});
  const invalid=await fetch(base+path+'/project',{method:'POST',headers,body:JSON.stringify({projectId:'not-a-project'})});assert.equal(invalid.status,400);
  console.log('PASS: logical projects, stable host locations, rename, reassignment during approval, deleting groups without stopping sessions');
}finally{
  for(const id of createdSessions)await api('/api/sessions/'+id,'DELETE');
  const catalog=await api('/api/catalog');
  for(const id of createdProjects)if(catalog.projects.some(p=>p.id===id))await api('/api/projects/'+id,'DELETE');
}
