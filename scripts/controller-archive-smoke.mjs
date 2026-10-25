// Disposable server and Git fixtures only. No live service access or restart.
import assert from 'node:assert/strict';
import {spawn,execFileSync} from 'node:child_process';
import {mkdir,mkdtemp,writeFile,readFile,access} from 'node:fs/promises';
import {resolve,join} from 'node:path';
import WebSocket from 'ws';
import {Client} from '@modelcontextprotocol/sdk/client/index.js';
import {StdioClientTransport} from '@modelcontextprotocol/sdk/client/stdio.js';
import {removeFixture,stopChild,serverReady} from '../tests/support/cleanup.mjs';
const port=Number(process.env.AGENTWORKS_CONTROLLER_ARCHIVE_PORT||4536);
assert(port>=4536&&port<=4540,'Use an isolated test port from 4536 through 4540');
const base=`http://127.0.0.1:${port}`,headers={Origin:base,'Content-Type':'application/json'};
await mkdir('.cache',{recursive:true});
const dir=await mkdtemp(resolve('.cache/controller-archive-')),root=join(dir,'allowed'),repo=join(root,'repo');
await mkdir(repo,{recursive:true});
const gitEnv=Object.fromEntries(Object.entries(process.env).filter(([k])=>!k.startsWith('GIT_')));
const git=(cwd,...args)=>execFileSync('git',['-c','core.hooksPath=/dev/null','-c','user.name=Test','-c','user.email=test@localhost',...args],{cwd,env:gitEnv,encoding:'utf8',stdio:['ignore','pipe','pipe']}).trim();
git(repo,'init','-b','main');await writeFile(join(repo,'tracked.txt'),'committed\n');git(repo,'add','.');git(repo,'commit','-m','fixture');
const env={...process.env,AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:String(port),AGENTWORKS_ROOTS:root,AGENTWORKS_CATALOG_PATH:join(dir,'catalog.json'),AGENTWORKS_DB_PATH:join(dir,'sessions.sqlite3'),AGENTWORKS_DEMO_STATE_DIR:join(dir,'demo')};
let server,terminal;const clients=[];
const delay=ms=>new Promise(r=>setTimeout(r,ms));
async function until(check){for(let i=0;i<300;i++){const result=await check();if(result)return result;await delay(50);}throw Error('Timed out');}
async function api(path,method='GET',body){const r=await fetch(base+path,{method,headers,signal:AbortSignal.timeout(20000),body:body===undefined?undefined:JSON.stringify(body)});assert.equal(r.status,200,await r.clone().text());return r.json();}
const snap=id=>api(`/api/sessions/${id}`);
const idle=id=>until(async()=>{const s=await snap(id);assert.notEqual(s.session.status,'error',JSON.stringify(s.events));return s.session.status==='idle'&&s;});
const exists=path=>access(path).then(()=>true,()=>false);
async function controller(project,grants){
 const created=await api(`/api/projects/${project}/controllers`,'POST',{name:'Archive test',...(grants?{access:grants}:{})});
 const client=new Client({name:'archive-test',version:'1'});
 await client.connect(new StdioClientTransport({command:resolve('target/debug/agentworks'),args:['mcp'],env:{PATH:process.env.PATH,AGENTWORKS_MCP_ENDPOINT_FILE:join(dir,'sessions.mcp-endpoint'),AGENTWORKS_MCP_TOKEN:created.token},stderr:'pipe'}));clients.push(client);
 return {id:created.controller.id,tools:async()=>(await client.listTools()).tools,call:async(name,args={})=>{const r=await client.callTool({name,arguments:args});const value=JSON.parse(r.content[0].text);return r.isError?{error:value}:value;}};
}
try{
 server=spawn(resolve('target/debug/agentworks'),[],{env,stdio:['ignore','ignore','inherit']});
 await until(async()=>{assert.equal(server.exitCode,null,'Fixture server exited');return serverReady(server,base);});
 const p=await api('/api/projects','POST',{name:'Archive fixtures'}),q=await api('/api/projects','POST',{name:'Other fixtures'});
 const create=async(project,workspace)=>{const s=await api('/api/sessions','POST',{provider:'demo',cwd:repo,projectId:project,agentAccess:{},workspace});await idle(s.id);return s;};
 const clean=await create(p.id,{mode:'newWorktree'}),other=await create(q.id),ungrouped=await create(undefined);
 const m=await controller(p.id),c=await controller(p.id,{startSessions:true,promptSessions:true,cancelSessions:true}),archiver=await controller(p.id,{archiveSessions:true});
 assert(!(await m.tools()).some(t=>t.name==='archive_session'));
 assert(!(await c.tools()).some(t=>t.name==='archive_session'));
 assert.deepEqual((await archiver.tools()).map(t=>t.name).sort(),['archive_session','list_sessions']);
 for(const name of ['get_completion','get_review_queue','start_session','prompt_session','cancel_session','run_automation','restart_service'])assert.equal((await archiver.call(name,{sessionId:clean.id})).error.status,403,`${name} must remain denied`);
 assert.equal((await archiver.call('report_completion',{status:'completed',summary:'Denied',requestId:'no-report'})).error.status,403);
 assert((await archiver.call('list_sessions')).sessions.some(s=>s.id===clean.id),'Archive-only controller discovers non-messaging sessions');
 for(const caller of [m,c])assert.equal((await caller.call('archive_session',{sessionId:clean.id})).error.status,403);
 for(const id of [other.id,ungrouped.id])assert.equal((await archiver.call('archive_session',{sessionId:id})).error.status,403);
 assert.equal((await archiver.call('archive_session',{sessionId:'missing'})).error.status,404);
 for(const args of [{},{sessionId:1},{sessionId:''},{sessionId:clean.id,archived:false},{sessionId:clean.id,title:'rename'},{sessionId:clean.id,worktree:'remove'},{sessionId:clean.id,confirmDirty:'x'},{sessionId:clean.id,actor:'human'}])assert.equal((await archiver.call('archive_session',args)).error.status,400);
 assert.equal((await snap(clean.id)).session.archived,false);
 const branch=git(clean.cwd,'branch','--show-current');
 const result=await archiver.call('archive_session',{sessionId:`agentworks://sessions/${clean.id}`});
 assert.equal(result.archived,true);assert.equal(result.worktreeCleanup.outcome,'removed');assert.equal(await exists(clean.cwd),false);assert(git(repo,'branch','--list',branch));
 assert.equal((await snap(clean.id)).session.status,'suspended');
 assert.equal((await archiver.call('archive_session',{sessionId:clean.id})).archived,true);
 const dirty=await create(p.id,{mode:'newWorktree'});await writeFile(join(dirty.cwd,'tracked.txt'),'keep edits\n');
 const retained=await archiver.call('archive_session',{sessionId:dirty.id});assert.equal(retained.archived,true);assert.equal(retained.worktreeCleanup.outcome,'retained');assert.equal(await readFile(join(dirty.cwd,'tracked.txt'),'utf8'),'keep edits\n');
 const locked=await create(p.id,{mode:'newWorktree'});git(repo,'worktree','lock',locked.cwd);
 assert.equal((await archiver.call('archive_session',{sessionId:locked.id})).worktreeCleanup.outcome,'retained');assert(await exists(locked.cwd));git(repo,'worktree','unlock',locked.cwd);
 const shared=await create(p.id,{mode:'newWorktree'});
 const guest=await api('/api/sessions','POST',{provider:'demo',cwd:shared.cwd,projectId:p.id});await idle(guest.id);
 assert.match((await archiver.call('archive_session',{sessionId:shared.id})).worktreeCleanup.reason,/Another session/);assert(await exists(shared.cwd));
 const busy=await create(p.id,{mode:'newWorktree'});
 await api(`/api/sessions/${busy.id}/prompt`,'POST',{text:'approval please',requestId:'busy-fixture'});
 await until(async()=>(await snap(busy.id)).approvals.length>0);
 assert.match((await archiver.call('archive_session',{sessionId:busy.id})).worktreeCleanup.reason,/running/);assert(await exists(busy.cwd));
 // Unowned directories remain intact; archiving stops the PTY and preserves its saved frame.
 const plain=await create(p.id);await api(`/api/sessions/${plain.id}/terminal`,'POST',{});
 const frames=[];terminal=new WebSocket(base.replace('http','ws')+`/api/sessions/${plain.id}/terminal/events`,{origin:base});
 terminal.on('message',bytes=>frames.push(JSON.parse(bytes)));await until(()=>frames.some(f=>!f.exited));
 assert.equal((await archiver.call('archive_session',{sessionId:plain.id})).worktreeCleanup.outcome,'none');
 await until(()=>frames.some(f=>f.exited));assert(await exists(repo));
 assert.equal((await api(`/api/sessions/${plain.id}/terminal`,'POST',{})).available,true);
 terminal.close();terminal=null;
 // Moving a target out of the project denies the existing connected controller.
 const moved=await create(p.id);await api(`/api/sessions/${moved.id}/project`,'POST',{projectId:q.id});
 assert.equal((await archiver.call('archive_session',{sessionId:moved.id})).error.status,403);
 assert.equal((await snap(moved.id)).session.archived,false);
 const audit=await api(`/api/controllers/${archiver.id}/audit`);assert(audit.events.some(e=>e.action==='archive_session'&&e.outcome==='accepted'&&e.target===clean.id));assert(audit.events.some(e=>e.action==='archive_session'&&e.outcome==='rejected'&&e.target===other.id));
 // Only coordinator session identities may archive; a standalone explicit flag
 // on a participant does not elevate its role.
 const session=await create(p.id);await api(`/api/sessions/${session.id}/agent-access`,'PUT',{archiveSessions:true});await idle(session.id);
 await api(`/api/sessions/${session.id}/prompt`,'POST',{text:`mcp archive_session ${JSON.stringify({sessionId:guest.id})}`,requestId:'session-denial'});
 const denied=await idle(session.id);assert(JSON.stringify(denied.events).includes('Agentworks capability not granted: archive_sessions'));
 // A coordinator session uses the same project-scoped MCP archive path as an external controller.
 const coordinator=await api('/api/sessions','POST',{provider:'demo',cwd:repo,projectId:p.id,agentAccess:{communicate:true,startSessions:true,promptSessions:true,cancelSessions:true,runAutomations:true}});await idle(coordinator.id);
 const coordinatorTarget=await create(p.id);
 await api(`/api/sessions/${coordinator.id}/prompt`,'POST',{text:`mcp archive_session ${JSON.stringify({sessionId:coordinatorTarget.id})}`,requestId:'coordinator-archive'});
 const archived=await idle(coordinator.id);assert(!JSON.stringify(archived.events).includes('capability not granted'));
 assert.equal((await snap(coordinatorTarget.id)).session.archived,true,'A coordinator session can archive a same-project session');
 await api(`/api/controllers/${archiver.id}/revoke`,'POST',{});
 assert.equal((await archiver.call('archive_session',{sessionId:other.id})).error.status,401);
 assert.equal((await snap(other.id)).session.archived,false);
 console.log('Controller archive smoke passed');
}finally{terminal?.close();for(const client of clients)await client.close().catch(()=>{});await stopChild(server);await removeFixture(dir);}
