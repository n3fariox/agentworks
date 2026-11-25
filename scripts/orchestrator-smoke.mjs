import {removeFixture} from '../tests/support/cleanup.mjs';
// Orchestrator slot over ACP -> native stdio MCP -> service bridge, with the demo
// harness and no model calls. Also screenshots the sidebar slot when SCREENSHOT is set.
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {mkdtemp, mkdir} from 'node:fs/promises';
import {resolve} from 'node:path';
import {once} from 'node:events';
const dir=await mkdtemp(resolve('.cache/orchestrator-'));
// The only root is a sibling of the orchestrator directory: nothing but the slot may use it.
await mkdir(dir+'/root');await mkdir(dir+'/orchestrator/below',{recursive:true});
const port=process.env.AGENTWORKS_ORCHESTRATOR_PORT||'4377',base=`http://127.0.0.1:${port}`;
let server;
const delay=ms=>new Promise(r=>setTimeout(r,ms));
async function api(path,body,method=body===undefined?'GET':'POST',code=200){const r=await fetch(base+path,{method,headers:{Origin:base,'Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body)});const text=await r.text();assert.equal(r.status,code,text);return code===200?JSON.parse(text):text;}
async function start(){server=spawn(resolve('target/debug/agentworks'),[],{env:{...process.env,AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:port,AGENTWORKS_CATALOG_PATH:dir+'/catalog.json',AGENTWORKS_DB_PATH:dir+'/sessions.sqlite3',AGENTWORKS_DEMO_STATE_DIR:dir+'/demo',AGENTWORKS_ROOTS:dir+'/root'},stdio:['ignore','inherit','inherit']});for(let i=0;i<80;i++){try{if((await fetch(base+'/api/status')).ok)return;}catch{}await delay(100);}throw new Error('Startup failed');}
async function stop(){const exit=once(server,'exit');server.kill('SIGINT');await exit;server=null;}
async function idle(id){for(let i=0;i<200;i++){const s=await api(`/api/sessions/${id}`);if(s.session.status==='idle')return s;assert.notEqual(s.session.status,'error',JSON.stringify(s));await delay(100);}const last=await api(`/api/sessions/${id}`);throw new Error('Idle timed out '+id+' '+last.session.status+' '+JSON.stringify(last.events.slice(-6).map(e=>[e.kind,JSON.stringify(e.data).slice(0,200)])));}
async function call(id,name,args={}){await idle(id);const old=await api(`/api/sessions/${id}`);await api(`/api/sessions/${id}/prompt`,{text:`mcp ${name} ${JSON.stringify(args)}`,requestId:crypto.randomUUID()});const s=await idle(id);const result=s.events.filter(e=>e.seq>old.seq&&e.kind==='acp'&&e.data.sessionUpdate==='tool_call_update').at(-1)?.data.rawOutput;assert(result,'Missing MCP result '+name+' '+JSON.stringify(s.events.filter(e=>e.seq>old.seq).map(e=>[e.kind,JSON.stringify(e.data).slice(0,300)])));return result;}
function value(result){assert(!result.isError,JSON.stringify(result));return JSON.parse(result.content[0].text);}
function denied(result,status=403){assert(result.isError,JSON.stringify(result));assert.equal(JSON.parse(result.content[0].text).status,status);}
const names=result=>result.tools.map(t=>t.name);
try{
  await start();const {roots}=await api('/api/status');
  const home=resolve(dir,'orchestrator');
  const alpha=await api('/api/projects',{name:'Alpha',defaultCommunicationRole:'participant'});
  const beta=await api('/api/projects',{name:'Beta',defaultCommunicationRole:'participant',tasksEnabled:true});
  const create=(title,projectId,access={communicate:true})=>api('/api/sessions',{provider:'demo',cwd:roots[0],title,projectId,agentAccess:access});
  const a=await create('Alpha worker',alpha.id),b=await create('Beta worker',beta.id),scratch=await create('Scratch',null);
  const coord=await create('Alpha coordinator',alpha.id,{communicate:true,startSessions:true,promptSessions:true});
  await Promise.all([a,b,scratch,coord].map(s=>idle(s.id)));
  assert.equal((await api('/api/orchestrator')).sessionId,null);
  denied(await call(a.id,'send_message',{recipient:b.id,text:'cross',requestId:'x0'}));

  // The orchestrator directory is closed to everyone but the slot.
  await api('/api/sessions',{provider:'demo',cwd:home,title:'Sneaky',projectId:null},'POST',403);
  denied(await call(coord.id,'start_session',{provider:'demo',cwd:home,requestId:'sneak'}));
  await api(`/api/sessions/${scratch.id}/directory`,{cwd:home},'POST',403);

  // New orchestrator: created by the slot, in its directory, with coordinator access.
  const o=await api('/api/orchestrator',{provider:'demo',requestId:'first'});
  assert.equal(o.cwd,home);assert.equal(o.projectId,null);
  assert.equal((await api('/api/orchestrator')).sessionId,o.id);
  assert.equal((await api('/api/catalog')).orchestratorId,o.id);
  assert((await api(`/api/sessions/${o.id}/agent-access`)).startSessions);
  await api('/api/orchestrator',{provider:'demo',requestId:'second'},'POST',409);
  let snap=await idle(o.id);assert(!snap.events.some(e=>e.kind==='directory_changed'),'created in place, never moved');

  // The slotted session is managed only through the slot.
  await api(`/api/sessions/${o.id}`,{archived:true},'PATCH',409);
  await api(`/api/sessions/${o.id}`,undefined,'DELETE',409);
  await api(`/api/sessions/${o.id}/fork`,{provider:'demo',requestId:'plain-fork'},'POST',409);
  await api(`/api/sessions/${o.id}/directory`,{cwd:roots[0]},'POST',409);
  await api(`/api/sessions/${o.id}/project`,{projectId:alpha.id},'POST',409);

  const tools=names(await call(o.id,'tools/list'));
  for(const t of ['list_projects','start_session','prompt_session','send_message'])assert(tools.includes(t),t);
  const listed=value(await call(o.id,'list_projects')).projects;
  assert.deepEqual(listed.map(p=>p.slug).sort(),[alpha.slug,beta.slug].sort());
  // Tool text speaks of every project, and project-scoped tools take a project.
  const catalog=(await call(o.id,'tools/list')).tools;
  for(const t of catalog)assert(!/in this project|your current project/.test(t.description),t.name+': '+t.description);
  assert(catalog.find(t=>t.name==='create_task').inputSchema.required.includes('project'));

  // Tickets in any project with tasks enabled, by project slug or task URI.
  const ticket=value(await call(o.id,'create_task',{project:beta.slug,title:'Route me',requestId:'t1'}));
  const tickets=value(await call(o.id,'list_tasks',{project:beta.slug}));
  assert(JSON.stringify(tickets).includes('Route me'));
  const uri=JSON.stringify(ticket).match(/agentworks:\/\/projects\/[^"]+\/tasks\/\d+/)[0];
  assert(JSON.stringify(value(await call(o.id,'get_task',{taskId:uri}))).includes('Route me'));
  denied(await call(o.id,'list_tasks',{}),400);

  // Automations: listed and run in every project, never ungrouped ones; not edited.
  const job=await api('/api/automations',{name:'Alpha job',provider:'demo',cwd:roots[0],projectId:alpha.id,prompt:'hello'});
  const loose=await api('/api/automations',{name:'Loose job',provider:'demo',cwd:roots[0],projectId:null,prompt:'hello'});
  const autos=value(await call(o.id,'list_automations')).automations;
  assert.equal(autos.find(d=>d.id===job.id)?.projectSlug,alpha.slug);assert(!autos.some(d=>d.id===loose.id));
  assert.deepEqual(value(await call(o.id,'list_automations',{project:beta.slug})).automations,[]);
  value(await call(o.id,'get_automation',{automationId:job.id}));
  denied(await call(o.id,'run_automation',{automationId:loose.id,requestId:'loose'}));
  denied(await call(o.id,'start_session',{provider:'demo',cwd:roots[0],project:beta.slug,inheritResumePolicy:true,requestId:'resume'}),400);

  const seen=value(await call(o.id,'list_sessions'));assert(seen.orchestrates);
  assert(seen.sessions.some(s=>s.id===scratch.id&&s.projectId===null),'ungrouped sessions are listed too');

  // It reaches every session, ungrouped included; projects reach it but not each other.
  value(await call(o.id,'send_message',{recipient:a.id,text:'Alpha task',requestId:'o-a'}));
  value(await call(o.id,'send_message',{recipient:b.id,text:'Beta task',requestId:'o-b'}));
  value(await call(o.id,'prompt_session',{sessionId:scratch.id,text:'hello',requestId:'scratch'}));await idle(scratch.id);
  assert.equal(value(await call(a.id,'list_sessions')).orchestrator.id,o.id);
  value(await call(a.id,'send_message',{recipient:o.id,text:'Done',requestId:'a-o'}));
  denied(await call(a.id,'send_message',{recipient:b.id,text:'cross',requestId:'x1'}));

  // Starts and steers work in any project, by slug.
  denied(await call(o.id,'start_session',{provider:'demo',cwd:roots[0],requestId:'no-project'}));
  const child=value(await call(o.id,'start_session',{provider:'demo',cwd:roots[0],project:beta.slug,title:'Beta child',requestId:'child'}));
  assert.equal(child.projectId,beta.id);assert.equal(child.initiatedBy.actor,o.id);await idle(child.id);
  denied(await call(a.id,'start_session',{provider:'demo',cwd:roots[0],project:beta.slug,requestId:'denied'}));

  // Hosts: listed for anyone who can start sessions, chosen by ID or name.
  const hosts=value(await call(coord.id,'list_hosts')).hosts;assert.equal(hosts[0].id,'local');assert.equal(hosts[0].status,'connected');
  denied(await call(a.id,'list_hosts'));
  denied(await call(coord.id,'start_session',{provider:'demo',cwd:roots[0],hostId:'nowhere',requestId:'bad-host'}),404);
  const onLocal=value(await call(coord.id,'start_session',{provider:'demo',cwd:roots[0],hostId:'Local',title:'Named host',requestId:'named-host'}));
  assert.equal(onLocal.hostId,'local');assert.equal(onLocal.projectId,alpha.id);await idle(onLocal.id);
  assert.equal(value(await call(o.id,'list_projects')).projects.find(p=>p.id===alpha.id).directories.find(d=>d.hostId==='local')?.hostName??'Local','Local');

  // A session that cannot message does not see the orchestrator.
  await api(`/api/sessions/${b.id}/agent-access`,{startSessions:true},'PUT');await idle(b.id);await delay(500);
  assert.equal(value(await call(b.id,'list_sessions')).orchestrator,null);

  if(process.env.SCREENSHOT){
    const {chromium}=await import('playwright');const browser=await chromium.launch();
    for(const [name,viewport] of [['desktop',{width:1280,height:800}],['phone',{width:390,height:844}]]){
      const page=await browser.newPage({viewport});await page.goto(base);
      if(name==='phone'){await page.click('#menu');await delay(300);}
      await page.waitForSelector('.orchestrator-item');await page.click('.orchestrator-more');
      await mkdir(process.env.SCREENSHOT,{recursive:true});await page.screenshot({path:`${process.env.SCREENSHOT}/orchestrator-slot-${name}.png`});
      await page.close();
    }
    await browser.close();
  }

  // The slot is locked while the orchestrator's turn runs.
  await api(`/api/sessions/${o.id}/prompt`,{text:'slow orchestration',requestId:'busy'});
  for(let i=0;i<50&&(await api(`/api/sessions/${o.id}`)).session.status!=='running';i++)await delay(100);
  await api('/api/orchestrator/fork',{provider:'demo',requestId:'busy-fork'},'POST',409);
  await api('/api/orchestrator',undefined,'DELETE',409);
  await api(`/api/sessions/${o.id}/cancel`,{});await idle(o.id);

  // Fork to another harness: the fork takes the slot and the old session is deleted.
  const forked=await api('/api/orchestrator/fork',{provider:'demo',context:'summary',summary:'Handoff.',requestId:'fork-1'});
  const fork=forked.session;assert(!forked.warning,forked.warning);
  assert.equal(fork.cwd,home);assert.equal((await api('/api/orchestrator')).sessionId,fork.id);
  await api(`/api/sessions/${o.id}`,undefined,'GET',404);
  await idle(fork.id);
  await api(`/api/sessions/${fork.id}/prompt`,{text:'hello',requestId:'absorb-fork'});await idle(fork.id);
  assert(names(await call(fork.id,'tools/list')).includes('list_projects'));
  value(await call(a.id,'send_message',{recipient:fork.id,text:'to the fork',requestId:'a-fork'}));

  // Clearing deletes the orchestrator and ends its reach.
  await api('/api/orchestrator',undefined,'DELETE');
  await api(`/api/sessions/${fork.id}`,undefined,'GET',404);
  assert.equal((await api('/api/catalog')).orchestratorId,null);
  denied(await call(a.id,'send_message',{recipient:fork.id,text:'late',requestId:'late'}),404);
  if(process.env.SCREENSHOT){
    const {chromium}=await import('playwright');const browser=await chromium.launch();
    const page=await browser.newPage({viewport:{width:1280,height:900}});await page.goto(base);
    await page.click('.orchestrator-empty');await page.waitForSelector('#create-dialog[open]');await delay(300);
    await page.screenshot({path:`${process.env.SCREENSHOT}/orchestrator-new-dialog.png`});await browser.close();
  }
  console.log('PASS: orchestrator slot create/fork/clear, slot-only directory, guards, mid-turn lock, cross-project and ungrouped reach, grant-gated visibility');
}finally{if(server)await stop();await removeFixture(dir);}
