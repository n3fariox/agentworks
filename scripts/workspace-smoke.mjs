import assert from 'node:assert/strict';
import {mkdir,writeFile,symlink,rm} from 'node:fs/promises';
import {resolve,join} from 'node:path';
import {randomUUID} from 'node:crypto';
const base=process.env.AGENTWORKS_URL||'http://127.0.0.1:4319',headers={Origin:base,'Content-Type':'application/json'};
async function api(path,method='GET',body){const r=await fetch(base+path,{method,headers,body:body===undefined?undefined:JSON.stringify(body)});assert.equal(r.status,200,await r.clone().text());return r.json();}
async function until(check){for(let n=0;n<300;n++){const value=await check();if(value)return value;await new Promise(r=>setTimeout(r,50));}throw new Error('Timed out');}
const originalSettings=await api('/api/settings');
const cwd=resolve('.cache/workspace-smoke-'+randomUUID()),created=[],defs=[];let project;
await mkdir(join(cwd,'docs'),{recursive:true});await writeFile(join(cwd,'docs/note.md'),'# Workspace\n\nA local note.');await writeFile(join(cwd,'.hidden'),'hidden');await writeFile(join(cwd,'sample.pdf'),'%PDF-example');await writeFile(join(cwd,'code.ts'),'export const message = "hello";');await symlink('/etc/passwd',join(cwd,'outside'));
try{
  project=await api('/api/projects','POST',{name:'Workspace smoke '+randomUUID()});
  const location=await api('/api/locations','POST',{cwd,projectId:project.id});
  assert((await api('/api/catalog')).projects.find(p=>p.id===project.id).locationIds.includes(location.id));
  const listing=await api('/api/locations/'+location.id+'/files');assert.equal(listing.entries[0].name,'docs');assert(!listing.entries.some(e=>e.name==='.hidden'));assert.equal(listing.entries.find(e=>e.name==='outside').kind,'blocked');
  assert((await api('/api/locations/'+location.id+'/files?hidden=true')).entries.some(e=>e.name==='.hidden'));
  const file=await fetch(base+'/api/locations/'+location.id+'/file?path=code.ts');assert(file.headers.get('content-type').startsWith('text/'));assert((await file.text()).includes('export const'));
  for(const path of ['/etc/passwd','../','outside']){const r=await fetch(base+'/api/locations/'+location.id+'/file?path='+encodeURIComponent(path));assert.equal(r.status,403);}
  const pdf=await fetch(base+'/api/locations/'+location.id+'/file?path=sample.pdf');assert.equal(pdf.headers.get('content-disposition'),'attachment');
  await api('/api/settings','PUT',{...originalSettings,roots:[join(cwd,'docs')]});
  assert.equal((await fetch(base+'/api/locations/'+location.id+'/files')).status,403);
  assert.equal((await fetch(base+'/api/locations/'+location.id+'/file?path=code.ts')).status,403);
  await api('/api/settings','PUT',originalSettings);
  const input={name:'Repeatable demo',provider:'demo',model:'demo/quick',cwd,projectId:project.id,prompt:'approval',enabled:true};
  const def=await api('/api/automations','POST',input);defs.push(def.id);
  const request={requestId:randomUUID()};const results=await Promise.all([api('/api/automations/'+def.id+'/run','POST',request),api('/api/automations/'+def.id+'/run','POST',request)]);
  assert.equal(results[0].session.id,results[1].session.id);const session=results[0].session;created.push(session.id);assert.equal(session.automation.automationId,def.id);
  const waiting=await until(async()=>{const s=await api('/api/sessions/'+session.id);return s.approvals.length&&s;});
  assert.equal(waiting.models.currentValue,'demo/quick');assert.equal(waiting.events.filter(e=>e.kind==='user').length,1);
  const concurrent=await fetch(base+'/api/automations/'+def.id+'/run',{method:'POST',headers,body:JSON.stringify({requestId:randomUUID()})});assert.equal(concurrent.status,409);
  await api('/api/sessions/'+session.id+'/approvals/'+waiting.approvals[0].id,'POST',{optionId:'allow'});
  await until(async()=>{const s=await api('/api/sessions/'+session.id);return s.session.status==='idle';});
  let runs=await api('/api/automations/'+def.id+'/runs');assert.equal(runs.length,1);assert.equal(runs[0].status,'completed');assert.equal(runs[0].sessionId,session.id);
  await api('/api/automations/'+def.id,'PUT',{...input,enabled:false});const paused=await fetch(base+'/api/automations/'+def.id+'/run',{method:'POST',headers,body:JSON.stringify({requestId:randomUUID()})});assert.equal(paused.status,409);
  await api('/api/projects/'+project.id,'DELETE');project=null;assert.equal((await api('/api/automations')).find(d=>d.id===def.id).projectId,null);
  await api('/api/automations/'+def.id,'DELETE');assert((await api('/api/sessions')).some(s=>s.id===session.id));
  await api('/api/sessions/'+session.id,'DELETE');created.length=0;runs=await api('/api/automations/'+def.id+'/runs');assert.equal(runs[0].status,'deleted');assert.equal(runs[0].sessionId,null);
  const repeated=await fetch(base+'/api/automations/'+def.id+'/run',{method:'POST',headers,body:JSON.stringify(request)});assert.equal(repeated.status,410);
  console.log('PASS: independent directory browsing, previews/downloads and boundaries, durable manual definitions, exactly-once runs, approval, concurrency and retained run history');
}finally{
  await api('/api/settings','PUT',originalSettings);
  for(const id of created)await api('/api/sessions/'+id,'DELETE');for(const id of defs)await api('/api/automations/'+id,'DELETE');if(project)await api('/api/projects/'+project.id,'DELETE');await rm(cwd,{recursive:true,force:true});
}
