import {removeFixture} from '../tests/support/cleanup.mjs';
// Agent-managed automations through the real stdio MCP bridge, as controllers use it.
// Owns its server, catalog, SQLite and demo state under .cache; never uses models or the live service.
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {once} from 'node:events';
import {mkdir,mkdtemp,writeFile} from 'node:fs/promises';
import {resolve,join} from 'node:path';
import {randomUUID} from 'node:crypto';
import {DatabaseSync} from 'node:sqlite';
import {Client} from '@modelcontextprotocol/sdk/client/index.js';
import {StdioClientTransport} from '@modelcontextprotocol/sdk/client/stdio.js';
const port=Number(process.env.AGENTWORKS_AUTOMATION_MANAGER_PORT||4561);
assert(port>=4561&&port<=4565,'Use an isolated test port from 4561 through 4565');
const base=`http://127.0.0.1:${port}`,headers={Origin:base,'Content-Type':'application/json'};
await mkdir('.cache',{recursive:true});
const dir=await mkdtemp(resolve('.cache/automation-manager-')),root=join(dir,'allowed');await mkdir(root,{recursive:true});
const db=join(dir,'sessions.sqlite3'),endpointFile=join(dir,'sessions.mcp-endpoint');
const env={...process.env,AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:String(port),AGENTWORKS_ROOTS:root,AGENTWORKS_CATALOG_PATH:join(dir,'catalog.json'),AGENTWORKS_DB_PATH:db,AGENTWORKS_DEMO_STATE_DIR:join(dir,'demo')};
let server;const clients=[];const delay=ms=>new Promise(r=>setTimeout(r,ms));
async function until(check,tries=300){for(let i=0;i<tries;i++){const v=await check();if(v)return v;await delay(50);}throw new Error('Test timed out');}
async function api(path,method='GET',body,status=200){const r=await fetch(base+path,{method,headers,body:body===undefined?undefined:JSON.stringify(body)}),text=await r.text();assert.equal(r.status,status,`${method} ${path}: ${text}`);return status===200?JSON.parse(text):text;}
async function start(){server=spawn(resolve('target/debug/agentworks'),[],{env,stdio:['ignore','ignore','inherit']});await until(async()=>{assert.equal(server.exitCode,null,'Server exited');try{return(await fetch(base+'/api/status')).ok;}catch{return false;}});}
async function stop(){const ended=once(server,'exit');server.kill('SIGINT');const timer=setTimeout(()=>server.kill('SIGKILL'),8000);timer.unref();await ended;clearTimeout(timer);server=null;}
async function controller(project,access){
 const made=await api(`/api/projects/${project}/controllers`,'POST',{name:`Manager ${randomUUID().slice(0,8)}`,access});
 const tokenFile=join(dir,`token-${randomUUID()}`);await writeFile(tokenFile,made.token+'\n',{mode:0o600});
 const transport=new StdioClientTransport({command:resolve('target/debug/agentworks'),args:['mcp'],env:{PATH:process.env.PATH,AGENTWORKS_MCP_ENDPOINT_FILE:endpointFile,AGENTWORKS_MCP_TOKEN_FILE:tokenFile},stderr:'pipe'});
 const client=new Client({name:'automation-manager-test',version:'1.0.0'});await client.connect(transport);clients.push(client);
 const call=async(name,args={})=>{const r=await client.callTool({name,arguments:args});const value=JSON.parse(r.content[0].text);return r.isError?{error:value}:value;};
 return {id:made.controller.id,actor:'controller:'+made.controller.id,call,tools:async()=>(await client.listTools()).tools.map(t=>t.name).sort()};
}
const status=r=>r.error?.status;
const hhmm=ms=>new Date(ms).toISOString().slice(11,16);
const weekly=(at,missed='runOnce')=>({version:1,trigger:{kind:'schedule',timeZone:'UTC',at,days:[1,2,3,4,5,6,7],missed},steps:[{kind:'agent',text:'Scheduled check'}]});
try{
 await start();
 const p=await api('/api/projects','POST',{name:'Managed'}),q=await api('/api/projects','POST',{name:'Elsewhere'});
 // The grant is explicit: absent from presets and the default controller.
 const runner=await controller(p.id,{runAutomations:true});
 assert.deepEqual(await runner.tools(),['get_automation','get_completion','list_automation_runs','list_automations','list_sessions','run_automation']);
 assert.equal(status(await runner.call('create_automation',{requestId:'r',name:'x',provider:'demo',cwd:root,prompt:'x'})),403);
 // manageAutomations alone cannot create work that starts sessions.
 const manageOnly=await controller(p.id,{manageAutomations:true});
 assert.deepEqual(await manageOnly.tools(),['create_automation','delete_automation','get_automation','list_automation_runs','list_automations','list_sessions','set_automation_enabled','update_automation']);
 assert.match((await manageOnly.call('create_automation',{requestId:'c0',name:'No start',provider:'demo',cwd:root,prompt:'x'})).error.message,/start_sessions/);
 const m=await controller(p.id,{manageAutomations:true,startSessions:true});
 // Create: project and host come from the credential; the author is recorded.
 const create={requestId:'create-1',name:'Agent nightly',provider:'demo',cwd:root,prompt:'Check the build'};
 const made=await m.call('create_automation',create);
 assert.equal(made.projectId,p.id);assert.equal(made.hostId,'local');assert.equal(made.revision,1);
 assert.deepEqual(made.createdBy,{id:m.actor,title:made.createdBy.title});
 assert.equal((await m.call('create_automation',create)).duplicate,true);
 assert.equal((await m.call('create_automation',create)).id,made.id);
 assert.equal(status(await m.call('create_automation',{...create,name:'Different'})),409);
 assert.equal(status(await m.call('create_automation',{...create,requestId:'create-2',projectId:q.id})),400);
 assert((await m.call('create_automation',{...create,requestId:'create-3',cwd:'/'})).error,'Directories outside the host roots are refused');
 assert.equal(status(await m.call('create_automation',{...create,requestId:'create-4',workflow:{version:1,trigger:{kind:'manual'},steps:[{kind:'shell',text:'rm -rf /'}]}})),400);
 // Update needs the current revision.
 const update={requestId:'update-1',automationId:made.id,revision:1,name:'Agent nightly',provider:'demo',cwd:root,workflow:weekly('09:30')};
 assert.equal(status(await m.call('update_automation',{...update,revision:0})),409);
 const updated=await m.call('update_automation',update);
 assert.equal(updated.revision,2);assert.equal(typeof updated.nextRun,'number');assert.deepEqual(updated.createdBy,made.createdBy);
 assert.equal((await m.call('update_automation',update)).duplicate,true);
 assert.equal(status(await m.call('update_automation',{...update,requestId:'update-2'})),409);
 const listed=(await m.call('list_automations')).automations;
 assert.deepEqual(listed.map(a=>[a.id,a.revision,a.trigger.kind]),[[made.id,2,'schedule']]);
 assert.deepEqual(Object.keys((await runner.call('list_automations')).automations[0]).sort(),['enabled','id','name']);
 assert.equal((await m.call('get_automation',{automationId:made.id})).workflow.trigger.at,'09:30');
 // One-time triggers: agents can schedule a follow-up, but never in the past.
 const soon=new Date(Date.now()+3600000).toISOString().slice(0,16);
 const followUp={requestId:'once-1',name:'Follow up',provider:'demo',cwd:root,workflow:{version:1,trigger:{kind:'once',timeZone:'UTC',at:soon,afterRun:'delete'},steps:[{kind:'agent',text:'Follow up'}]}};
 assert.equal(status(await m.call('create_automation',{...followUp,requestId:'once-0',workflow:{...followUp.workflow,trigger:{...followUp.workflow.trigger,at:'2020-01-01T00:00'}}})),400);
 const scheduled=await m.call('create_automation',followUp);
 assert.equal(scheduled.nextRun,Date.parse(soon+':00Z'));
 assert.deepEqual(await m.call('delete_automation',{requestId:'once-del',automationId:scheduled.id}),{ok:true,id:scheduled.id});
 // Human definitions: agents may pause but not resume, edit or delete them.
 const human=await api('/api/automations','POST',{name:'Human weekly',provider:'demo',cwd:root,projectId:p.id,prompt:'Human task'});
 assert.equal((await m.call('set_automation_enabled',{requestId:'pause-h',automationId:human.id,enabled:false})).enabled,false);
 assert.equal(status(await m.call('set_automation_enabled',{requestId:'resume-h',automationId:human.id,enabled:true})),403);
 assert.equal(status(await m.call('update_automation',{...update,requestId:'edit-h',automationId:human.id,revision:2})),403);
 assert.equal(status(await m.call('delete_automation',{requestId:'del-h',automationId:human.id})),403);
 assert.equal((await api('/api/automations')).find(a=>a.id===human.id).enabled,false);
 // Project boundary, even with a known ID.
 const other=await api('/api/automations','POST',{name:'Other project',provider:'demo',cwd:root,projectId:q.id,prompt:'x'});
 for(const [tool,args] of [['get_automation',{}],['list_automation_runs',{}],['set_automation_enabled',{requestId:'x1',enabled:false}],['delete_automation',{requestId:'x2'}]])
  assert.equal(status(await m.call(tool,{automationId:other.id,...args})),403,tool);
 // A human edit keeps the agent as author.
 const humanEdit=await api('/api/automations/'+made.id,'PUT',{name:'Agent nightly (reviewed)',provider:'demo',cwd:root,projectId:p.id,workflow:weekly('09:30')});
 assert.deepEqual(humanEdit.createdBy,made.createdBy);assert.equal(humanEdit.revision,3);
 // Unattended runs act on the author's live grants. Two schedules with a passed
 // occurrence; one author is then revoked. Downtime is simulated via the cursor.
 const slot=Math.floor((Date.now()-7200000)/60000)*60000;
 const ok=await controller(p.id,{manageAutomations:true,startSessions:true});
 const kept=await ok.call('create_automation',{requestId:'kept',name:'Kept author',provider:'demo',cwd:root,workflow:weekly(hhmm(slot))});
 const lost=await m.call('create_automation',{requestId:'lost',name:'Revoked author',provider:'demo',cwd:root,workflow:weekly(hhmm(slot))});
 await api(`/api/controllers/${m.id}/revoke`,'POST',{});
 await stop();
 {
  const sql=new DatabaseSync(db);
  const rewind=sql.prepare('UPDATE automation_schedules SET cursor=? WHERE automation_id=?');
  for(const def of [kept,lost])assert.equal(rewind.run(Date.now()-2*86400000,def.id).changes,1);
  // Mutations were audited with their outcome.
  const audit=sql.prepare("SELECT action,outcome FROM agent_audit WHERE actor=? AND action LIKE '%automation%' ORDER BY id").all(m.actor).map(r=>r.action+':'+r.outcome);
  assert(audit.includes('create_automation:accepted')&&audit.includes('delete_automation:rejected'),audit.join());
  assert(!audit.some(a=>a.startsWith('get_automation:accepted')),'Reads are not audited as mutations');
  sql.close();
 }
 await start();
 const runs=id=>api(`/api/automations/${id}/runs`);
 const [skipped]=await until(async()=>{const r=await runs(lost.id);return r.length&&r;});
 assert.equal(skipped.status,'skipped');assert.match(skipped.detail,/no longer exists or was revoked/);
 const [started]=await until(async()=>{const r=await runs(kept.id);return r.length&&r;});
 assert(started.sessionId,'The unrevoked author\'s occurrence starts a run');
 // The remaining author can read history and delete; history is kept.
 assert.equal((await ok.call('list_automation_runs',{automationId:kept.id})).runs[0].sessionId,started.sessionId);
 const del={requestId:'delete-kept',automationId:kept.id};
 assert.deepEqual(await ok.call('delete_automation',del),{ok:true,id:kept.id});
 assert.equal((await ok.call('delete_automation',del)).duplicate,true);
 assert.equal(status(await ok.call('get_automation',{automationId:kept.id})),404);
 assert.equal((await runs(kept.id)).length,1,'Deleting keeps run history');
 // One bad row no longer hides or stops the rest.
 await stop();
 {const sql=new DatabaseSync(db);sql.prepare('INSERT INTO automations VALUES (?,?)').run('broken','{"not":"a definition"}');sql.close();}
 await start();
 assert((await api('/api/automations')).some(a=>a.id===lost.id));
 console.log('Automation manager smoke passed');
}finally{
 for(const c of clients)await c.close().catch(()=>{});
 if(server)await stop().catch(()=>{});
 await removeFixture(dir);
}
