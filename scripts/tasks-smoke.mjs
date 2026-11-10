// Isolated HTTP + native MCP task contract tests; no model or live service required.
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {mkdtemp,writeFile,readFile} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {resolve,join} from 'node:path';
import {randomUUID} from 'node:crypto';
import {Client} from '@modelcontextprotocol/sdk/client/index.js';
import {StdioClientTransport} from '@modelcontextprotocol/sdk/client/stdio.js';
import {removeFixture,stopChild,serverReady} from '../tests/support/cleanup.mjs';
const dir=await mkdtemp(join(tmpdir(),'agentworks-tasks-'));
const binary=resolve(process.env.AGENTWORKS_BINARY||'target/debug/agentworks');
const port=process.env.AGENTWORKS_TASKS_PORT||'4591',base=`http://127.0.0.1:${port}`;
const env={...process.env,AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:port,AGENTWORKS_ROOTS:dir,AGENTWORKS_CATALOG_PATH:join(dir,'catalog.json'),AGENTWORKS_DB_PATH:join(dir,'sessions.sqlite3'),AGENTWORKS_DEMO_STATE_DIR:join(dir,'demo'),AGENTWORKS_SUPERVISOR_DIR:'',AGENTWORKS_SUPERVISOR_TOKEN:'',AGENTWORKS_RESTART_REQUEST:''};
let server;const clients=[];const delay=ms=>new Promise(r=>setTimeout(r,ms));
async function api(path,body,method=body===undefined?'GET':'POST',status=200){const r=await fetch(base+path,{method,headers:{Origin:base,'Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body)});const text=await r.text();assert.equal(r.status,status,`${method} ${path}: ${text}`);return status===200?JSON.parse(text):text;}
async function start(){server=spawn(binary,[],{env,stdio:['ignore','ignore','inherit']});for(let i=0;i<200;i++){assert.equal(server.exitCode,null,'Server exited');if(await serverReady(server,base))return;await delay(100);}throw Error('Server startup timed out');}
async function idle(id){for(let i=0;i<200;i++){const s=await api(`/api/sessions/${id}`);assert.notEqual(s.session.status,'error',JSON.stringify(s));if(s.session.status==='idle')return s;await delay(50);}throw Error('Session idle timed out');}
async function call(id,name,args={}){await idle(id);const old=await api(`/api/sessions/${id}`);await api(`/api/sessions/${id}/prompt`,{text:`mcp ${name} ${JSON.stringify(args)}`,requestId:randomUUID()});const s=await idle(id);const out=s.events.filter(e=>e.seq>old.seq&&e.kind==='acp'&&e.data.sessionUpdate==='tool_call_update').at(-1)?.data.rawOutput;assert(out,'Missing native MCP result: '+name);return out;}
function value(r){assert(!r.isError,JSON.stringify(r));return JSON.parse(r.content[0].text);}
function denied(r,status=403){assert(r.isError,JSON.stringify(r));assert.equal(JSON.parse(r.content[0].text).status,status);}
async function controller(token){const file=join(dir,randomUUID()+'.token');await writeFile(file,token,{mode:0o600});const transport=new StdioClientTransport({command:binary,args:['mcp'],env:{PATH:process.env.PATH,AGENTWORKS_MCP_ENDPOINT_FILE:join(dir,'sessions.mcp-endpoint'),AGENTWORKS_MCP_TOKEN_FILE:file},stderr:'pipe'});const c=new Client({name:'tasks-smoke',version:'1'});await c.connect(transport);clients.push(c);return c;}
try{
 await start();
 const p=await api('/api/projects',{name:'Tickets',slug:'TIX'}),q=await api('/api/projects',{name:'Other tickets'}),path=`/api/projects/${p.id}/tasks`;
 assert.match(q.slug,/^[A-Z][A-Z0-9]{1,19}$/);
 for(const slug of ['a','1ABC','lowercase','TIX'])await api('/api/projects',{name:'Invalid',slug},'POST',400);
 const req={title:'First ticket',body:'**Markdown**',status:'open',requestId:'human-create'};
 const first=await api(path,req);assert.equal(first.task.id,1);assert.equal(first.task.key,'TIX-1');assert.equal(first.task.createdBy,'human');assert.equal(first.task.revision,1);
 assert.equal(first.task.uri,`agentworks://projects/${p.id}/tasks/1`);assert.deepEqual(await api(path,req),first);
 await api(path,{...req,title:'Changed'},'POST',409);
 for(const extra of [{title:''},{title:'x'.repeat(161)},{body:'x'.repeat(16385)},{status:'closed'},{projectId:q.id},{actor:'spoof'}])await api(path,{...req,requestId:randomUUID(),...extra},'POST',400);
 await api(path,{title:'No receipt'},'POST',400);
 const other=await api(`/api/projects/${q.id}/tasks`,{title:'Other',requestId:'other'});assert.equal(other.task.id,1);
 const update={revision:1,title:'Updated',status:'done',requestId:'human-update'};
 const updated=await api(path+'/1',update,'PUT');assert.equal(updated.task.revision,2);assert.equal(updated.task.status,'done');assert.deepEqual(await api(path+'/1',update,'PUT'),updated);
 await api(path+'/1',{...update,requestId:'stale'},'PUT',409);
 await api(path+'/1',{...update,taskId:1},'PUT',400);
 await api(`/api/projects/${p.id}`,{name:'Renamed'},'PATCH');assert.equal((await api(path)).project.slug,'TIX');
 await api(`/api/projects/${p.id}`,{name:'Renamed again',slug:'NEW'},'PATCH');assert.equal((await api(path+'/1')).task.key,'NEW-1');assert.equal((await api(path+'/1')).task.uri,first.task.uri);
 const create=(title,projectId,agentAccess)=>api('/api/sessions',{provider:'demo',cwd:dir,title,projectId,agentAccess});
 const participant=await create('Participant',p.id,{communicate:true}),coordinator=await create('Coordinator',p.id,{communicate:true,startSessions:true}),outsider=await create('Other coordinator',q.id,{startSessions:true}),off=await create('Off',p.id,{}),ungrouped=await create('Ungrouped',null,{communicate:true,startSessions:true});
 assert.equal(value(await call(participant.id,'get_task',{taskId:first.task.uri})).task.id,1);
 assert.equal(value(await call(participant.id,'list_tasks')).tasks.length,0);
 assert.equal(value(await call(participant.id,'list_tasks',{includeDone:true})).tasks.length,1);
 assert.equal((await api(path)).tasks.length,0);
 assert.equal((await api(path+'?includeDone=true')).tasks.length,1);
 denied(await call(participant.id,'create_task',{title:'Forbidden',requestId:'forbidden'}));denied(await call(off.id,'list_tasks'));denied(await call(ungrouped.id,'list_tasks'));
 denied(await call(outsider.id,'get_task',{taskId:first.task.uri}),400);
 for(const taskId of ['NEW-1','1',first.task.uri.replace('/1','/01'),0,-1])denied(await call(coordinator.id,'get_task',{taskId}),400);
 const tools=(await call(coordinator.id,'tools/list')).tools.map(t=>t.name);for(const name of ['list_tasks','get_task','create_task','update_task','delete_task'])assert(tools.includes(name),name);
 const nativeReq={title:'Native ticket',body:'# Native',status:'open',requestId:'native-create'};
 const native=value(await call(coordinator.id,'create_task',nativeReq));assert.equal(native.task.id,2);assert.equal(native.task.createdBy,coordinator.id);assert.deepEqual(value(await call(coordinator.id,'create_task',nativeReq)),native);
 denied(await call(coordinator.id,'create_task',{...nativeReq,title:'Changed'}),409);
 const nativeUpdate={taskId:native.task.uri,revision:1,body:'Edited',status:'done',requestId:'native-update'};
 const edited=value(await call(coordinator.id,'update_task',nativeUpdate));assert.equal(edited.task.revision,2);assert.deepEqual(value(await call(coordinator.id,'update_task',nativeUpdate)),edited);
 denied(await call(coordinator.id,'update_task',{...nativeUpdate,requestId:'native-stale'}),409);
 const ctl=await api(`/api/projects/${p.id}/controllers`,{name:'Read-only task controller',access:{communicate:true,startSessions:true,promptSessions:true}}),c=await controller(ctl.token);
 assert.equal(value(await c.callTool({name:'get_task',arguments:{taskId:native.task.uri}})).task.id,2);
 const controllerTools=(await c.listTools()).tools.map(t=>t.name);assert(controllerTools.includes('list_tasks'));assert(!controllerTools.includes('create_task'));
 for(const name of ['create_task','update_task','delete_task'])denied(await c.callTool({name,arguments:{title:'No',taskId:2,revision:2,requestId:'ctl-'+name}}));
 await api(`/api/sessions/${coordinator.id}/agent-access`,{communicate:true},'PUT');denied(await call(coordinator.id,'create_task',nativeReq));
 assert(!(await call(coordinator.id,'tools/list')).tools.some(t=>t.name==='create_task'));
 await api(`/api/sessions/${coordinator.id}/agent-access`,{communicate:true,startSessions:true},'PUT');
 await api(`/api/controllers/${ctl.controller.id}/revoke`,{});denied(await c.callTool({name:'list_tasks',arguments:{}}),401);
 const del={taskId:2,revision:2,requestId:'native-delete'};assert.deepEqual(value(await call(coordinator.id,'delete_task',del)),{ok:true});assert.deepEqual(value(await call(coordinator.id,'delete_task',del)),{ok:true});await api(path+'/2',undefined,'GET',404);
 await stopChild(server);await start();
 assert.deepEqual(await api(path,req),first,'Create receipt survives rename and restart');assert.deepEqual(await api(path+'/1',update,'PUT'),updated);
 assert.equal((await api(path+'?includeDone=true')).tasks.length,1);assert.equal((await api(path+'/1')).task.uri,first.task.uri);
 await api(`/api/sessions/${coordinator.id}/resume`,{});assert.deepEqual(value(await call(coordinator.id,'delete_task',del)),{ok:true});
 assert.equal((await api(path,{title:'Never reuse deleted numbers',requestId:'next'})).task.id,3);
 await api(path+'/1',{revision:1,requestId:'delete-stale'},'DELETE',409);const humanDel={revision:2,requestId:'human-delete'};assert.deepEqual(await api(path+'/1',humanDel,'DELETE'),{ok:true});assert.deepEqual(await api(path+'/1',humanDel,'DELETE'),{ok:true});
 await api(`/api/projects/${p.id}/archive`,{archived:true,sessions:'ungroup'});assert.equal((await api(path)).tasks.length,1);await api(path,req,'POST',409);await api(path+'/3',{revision:1,title:'No',requestId:'archived-update'},'PUT',409);await api(path+'/3',{revision:1,requestId:'archived-delete'},'DELETE',409);
 // Parallel HTTP mutations serialize number allocation without losing records.
 const parallel=await Promise.all(Array.from({length:8},(_,i)=>api(`/api/projects/${q.id}/tasks`,{title:'Parallel '+i,requestId:'parallel-'+i})));
 assert.equal(new Set(parallel.map(r=>r.task.id)).size,8);
 // A moved session cannot keep reading its former project's canonical references.
 await api(`/api/sessions/${participant.id}/resume`,{});
 await api(`/api/sessions/${participant.id}/project`,{projectId:q.id});
 denied(await call(participant.id,'get_task',{taskId:first.task.uri}),400);
 await api(`/api/projects/${p.id}?sessions=ungroup`,undefined,'DELETE');
 await api(path,undefined,'GET',404);
 // Successful receipt storage is bounded per project, not merely bounded in this fixture.
 for(let i=0;i<260;i++)await api(`/api/projects/${q.id}/tasks`,{title:'Bounded receipt '+i,requestId:'bounded-'+i});
 const catalog=JSON.parse(await readFile(join(dir,'catalog.json'),'utf8'));assert.equal(catalog.tasks[q.id].requests.length,256);assert.equal(catalog.tasks[p.id],undefined);
 console.log('PASS: tasks HTTP and native MCP CRUD, strict fields, scope, roles/controllers, live revocation, receipts, revisions, stable identity, numbering, archive and restart');
}finally{for(const c of clients)await c.close().catch(()=>{});await stopChild(server);await removeFixture(dir);}
