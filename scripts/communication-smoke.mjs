import {removeFixture} from '../tests/support/cleanup.mjs';
// Exercises ACP -> native stdio MCP -> private service bridge, without model calls.
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {mkdtemp} from 'node:fs/promises';
import {resolve} from 'node:path';
import {once} from 'node:events';
const dir=await mkdtemp(resolve('.cache/communication-'));
const port=process.env.AGENTWORKS_COMMUNICATION_PORT||'4322',base=`http://127.0.0.1:${port}`;
let server;
const delay=ms=>new Promise(r=>setTimeout(r,ms));
async function api(path,body,method=body===undefined?'GET':'POST',code=200){const r=await fetch(base+path,{method,headers:{Origin:base,'Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body)});const text=await r.text();assert.equal(r.status,code,text);return code===200?JSON.parse(text):text;}
async function start(){server=spawn(resolve('target/debug/agentworks'),[],{env:{...process.env,AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:port,AGENTWORKS_CATALOG_PATH:dir+'/catalog.json',AGENTWORKS_DB_PATH:dir+'/sessions.sqlite3',AGENTWORKS_DEMO_STATE_DIR:dir+'/demo'},stdio:['ignore','inherit','inherit']});for(let i=0;i<80;i++){try{if((await fetch(base+'/api/status')).ok)return;}catch{}await delay(100);}throw new Error('Startup failed');}
async function stop(){const exit=once(server,'exit');server.kill('SIGINT');await exit;server=null;}
async function idle(id){for(let i=0;i<200;i++){const s=await api(`/api/sessions/${id}`);if(s.session.status==='idle')return s;assert.notEqual(s.session.status,'error',JSON.stringify(s));await delay(100);}throw new Error('Idle timed out '+id);}
async function enable(id,access={communicate:true}){await idle(id);await api(`/api/sessions/${id}/agent-access`,access,'PUT');await idle(id);}
async function call(id,name,args={}){await idle(id);const old=await api(`/api/sessions/${id}`);await api(`/api/sessions/${id}/prompt`,{text:`mcp ${name} ${JSON.stringify(args)}`,requestId:crypto.randomUUID()});const s=await idle(id);const result=s.events.filter(e=>e.seq>old.seq&&e.kind==='acp'&&e.data.sessionUpdate==='tool_call_update').at(-1)?.data.rawOutput;assert(result,'Missing MCP result '+name);return result;}
function value(result){assert(!result.isError,JSON.stringify(result));return JSON.parse(result.content[0].text);}
function denied(result,status=403){assert(result.isError,JSON.stringify(result));assert.equal(JSON.parse(result.content[0].text).status,status);}
try{
 await start();const settings=await api('/api/settings');await api('/api/settings',{...settings,defaultCommunicationRole:'off'},'PUT');const {roots}=await api('/api/status');const project=await api('/api/projects',{name:'Communication tests',defaultCommunicationRole:'participant'});const other=await api('/api/projects',{name:'Other project'});
 const create=(title,projectId=project.id)=>api('/api/sessions',{provider:'demo',cwd:roots[0],title,projectId,agentAccess:{}});
 const a=await create('Sender'),b=await create('Recipient'),c=await create('Outside',other.id);await Promise.all([idle(a.id),idle(b.id),idle(c.id)]);
 assert.deepEqual((await call(a.id,'tools/list')).tools,[]);denied(await call(a.id,'read_inbox'));
 await enable(a.id);await enable(b.id);await enable(c.id);
 const peers=value(await call(a.id,'list_sessions'));assert.deepEqual(peers.sessions.map(s=>s.id),[b.id]);assert.equal(peers.sessions[0].uri,'agentworks://sessions/'+b.id);assert.equal(peers.selfUri,'agentworks://sessions/'+a.id);
 denied(await call(a.id,'send_message',{sender:c.id,recipient:b.id,text:'Forged sender',requestId:'spoof'}),400);
 const sent=value(await call(a.id,'send_message',{recipient:b.id,text:'Durable peer message',requestId:'first'}));
 const duplicate=value(await call(a.id,'send_message',{recipient:b.id,text:'Durable peer message',requestId:'first'}));assert.equal(duplicate.id,sent.id);assert(duplicate.duplicate);
 denied(await call(a.id,'send_message',{recipient:b.id,text:'Changed payload',requestId:'first'}),409);
 // agentworks://sessions/<id> URIs are equivalent to bare IDs, including for retry deduplication and project scoping.
 assert.equal(value(await call(a.id,'send_message',{recipient:'agentworks://sessions/'+b.id,text:'Durable peer message',requestId:'first'})).id,sent.id);
 denied(await call(a.id,'send_message',{recipient:'agentworks://sessions/'+c.id,text:'No crossing projects',requestId:'outside-uri'}));
 denied(await call(a.id,'send_message',{recipient:c.id,text:'No crossing projects',requestId:'outside'}));
 let inbox=value(await call(b.id,'read_inbox'));assert.equal(inbox.messages.length,1);assert.equal(inbox.messages[0].sender,a.id);
 denied(await call(c.id,'acknowledge_message',{messageId:sent.id}),404);
 assert.equal(sent.uri,'agentworks://messages/'+sent.id);assert.equal(inbox.messages[0].uri,sent.uri);assert.equal(inbox.messages[0].senderUri,'agentworks://sessions/'+a.id);
 value(await call(b.id,'acknowledge_message',{messageId:sent.uri}));
 value(await call(b.id,'send_message',{recipient:a.id,text:'Received',requestId:'reply',replyTo:sent.uri}));denied(await call(b.id,'send_message',{recipient:a.id,text:'Bad ref',requestId:'reply-bad',replyTo:'agentworks://messages/x'}),400);
 denied(await call(a.id,'start_session',{provider:'demo',cwd:roots[0],requestId:'ungranted'}));
 denied(await call(a.id,'prompt_session',{sessionId:b.id,text:'Unwanted work',requestId:'ungranted'}));
 denied(await call(a.id,'cancel_session',{sessionId:b.id}));
 denied(await call(a.id,'set_access',{sessionId:a.id,communicate:true,startSessions:true}));
 await enable(a.id,{communicate:true,startSessions:true,promptSessions:true,cancelSessions:true,runAutomations:true});
 const child=value(await call(a.id,'start_session',{provider:'demo',cwd:roots[0],title:'Created by coordinator',requestId:'child'}));await idle(child.id);
 assert(value(await call(a.id,'list_sessions')).sessions.some(s=>s.id===child.id&&s.status==='idle'));
 assert.equal(child.initiatedBy.actor,a.id);assert.equal(child.projectId,project.id);assert.deepEqual((await api(`/api/sessions/${child.id}/agent-access`)),{communicate:true,startSessions:false,promptSessions:false,cancelSessions:false,runAutomations:false,restartService:false,manageReviewQueue:false,archiveSessions:false});
 const childTools=(await call(child.id,'tools/list')).tools.map(t=>t.name);assert(childTools.includes('send_message'));assert(childTools.includes('report_completion'));assert(!childTools.includes('start_session'));assert(!childTools.includes('prompt_session'));assert(!childTools.includes('cancel_session'));
 const same=value(await call(a.id,'start_session',{provider:'demo',cwd:roots[0],title:'Created by coordinator',requestId:'child'}));assert.equal(same.id,child.id);
 denied(await call(a.id,'start_session',{provider:'demo',cwd:roots[0],title:'Changed title',requestId:'child'}),409);
 denied(await call(a.id,'prompt_session',{sessionId:c.id,text:'Cross project',requestId:'cross'}));
 value(await call(a.id,'prompt_session',{sessionId:child.id,text:'slow coordinated work',requestId:'work'}));
 const before=(await api(`/api/sessions/${child.id}`)).events.filter(e=>e.kind==='user').length;
 value(await call(a.id,'prompt_session',{sessionId:child.id,text:'slow coordinated work',requestId:'work'}));
 assert.equal((await api(`/api/sessions/${child.id}`)).events.filter(e=>e.kind==='user').length,before);
 value(await call(a.id,'cancel_session',{sessionId:child.id}));await idle(child.id);
 await api(`/api/sessions/${b.id}/prompt`,{text:'slow approval',requestId:'grant-during-turn'});
 for(let i=0;i<50;i++){if((await api(`/api/sessions/${b.id}`)).session.status==='waiting')break;await delay(100);}
 await api(`/api/sessions/${b.id}/agent-access`,{},'PUT');await api(`/api/sessions/${b.id}/cancel`,{});await idle(b.id);await delay(800);await idle(b.id);
 assert.deepEqual((await call(b.id,'tools/list')).tools,[]);await enable(b.id);

 const automation=await api('/api/automations',{name:'MCP run',provider:'demo',cwd:roots[0],projectId:project.id,prompt:'An automated hello',enabled:true});
 assert(value(await call(a.id,'list_automations')).automations.some(d=>d.id===automation.id));
 const run=value(await call(a.id,'run_automation',{automationId:automation.id,requestId:'auto'}));await idle(run.session.id);
 assert.equal((await api(`/api/sessions/${run.session.id}/agent-access`)).communicate,false);
 const repeated=value(await call(a.id,'run_automation',{automationId:automation.id,requestId:'auto'}));assert.equal(repeated.session.id,run.session.id);
 await api(`/api/sessions/${a.id}/agent-access`,{},'PUT');denied(await call(a.id,'send_message',{recipient:b.id,text:'Revoked',requestId:'revoked'}));denied(await call(a.id,'start_session',{provider:'demo',cwd:roots[0],requestId:'revoked'}));
 const audit=await api(`/api/sessions/${a.id}/agent-audit`);assert(audit.events.some(e=>e.action==='start_session'&&e.outcome==='accepted'));assert(audit.events.some(e=>e.outcome==='rejected'));
 await api(`/api/sessions/${b.id}/project`,{projectId:other.id});denied(await call(b.id,'send_message',{recipient:child.id,text:'Moved away',requestId:'moved'}));await api(`/api/sessions/${b.id}/project`,{projectId:project.id});
 const snapshot=JSON.stringify(await api(`/api/sessions/${a.id}`));assert(!snapshot.includes('AGENTWORKS_MCP_TOKEN'));
 await stop();await start();
 assert.equal((await api(`/api/sessions/${b.id}/agent-access`)).communicate,true);
 inbox=await api(`/api/sessions/${b.id}/mailbox`);assert.equal(inbox.messages.length,2);assert(inbox.messages[0].acknowledged);
 await api(`/api/sessions/${b.id}/resume`,{});await idle(b.id);assert.equal(value(await call(b.id,'read_inbox')).messages.length,2);
 await api(`/api/sessions/${a.id}` ,undefined,'DELETE');inbox=value(await call(b.id,'read_inbox'));assert.equal(inbox.messages[0].senderTitle,'Sender');
 console.log('PASS: native MCP messaging, grants, project isolation, denied controls, coordinator work, deduplication, revocation, persistence and sender tombstones');
}finally{if(server)await stop();await removeFixture(dir);}
