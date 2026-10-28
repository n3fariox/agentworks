import {removeFixture} from '../tests/support/cleanup.mjs';
// C-01: named external controllers through the real stdio MCP bridge, as an external client uses it.
// Owns its server, catalog, SQLite and demo state under .cache; never uses models or the live service.
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {once} from 'node:events';
import {mkdir,mkdtemp,writeFile,stat,readFile} from 'node:fs/promises';
import {resolve,join} from 'node:path';
import {randomUUID} from 'node:crypto';
import {Client} from '@modelcontextprotocol/sdk/client/index.js';
import {StdioClientTransport} from '@modelcontextprotocol/sdk/client/stdio.js';
import {chromium,expect} from '@playwright/test';
const port=Number(process.env.AGENTWORKS_CONTROLLER_PORT||4531);
assert(port>=4531&&port<=4535,'Use an isolated test port from 4531 through 4535');
const base=`http://127.0.0.1:${port}`,headers={Origin:base,'Content-Type':'application/json'};
await mkdir('.cache',{recursive:true});
const dir=await mkdtemp(resolve('.cache/controllers-')),root=join(dir,'allowed');await mkdir(root,{recursive:true});
const db=join(dir,'sessions.sqlite3'),endpointFile=join(dir,'sessions.mcp-endpoint');
const env={...process.env,AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:String(port),AGENTWORKS_ROOTS:root,AGENTWORKS_CATALOG_PATH:join(dir,'catalog.json'),AGENTWORKS_DB_PATH:db,AGENTWORKS_DEMO_STATE_DIR:join(dir,'demo')};
let server,browser;const clients=[];const delay=ms=>new Promise(r=>setTimeout(r,ms));
async function until(check){for(let i=0;i<300;i++){const v=await check();if(v)return v;await delay(50);}throw new Error('Test timed out');}
async function api(path,method='GET',body,status=200){const r=await fetch(base+path,{method,headers,body:body===undefined?undefined:JSON.stringify(body)}),text=await r.text();assert.equal(r.status,status,`${method} ${path}: ${text}`);return status===200?JSON.parse(text):text;}
async function start(){server=spawn(resolve('target/debug/agentworks'),[],{env,stdio:['ignore','ignore','inherit']});await until(async()=>{assert.equal(server.exitCode,null,'Server exited');try{return(await fetch(base+'/api/status')).ok;}catch{return false;}});}
async function stop(){const ended=once(server,'exit');server.kill('SIGINT');const timer=setTimeout(()=>server.kill('SIGKILL'),8000);timer.unref();await ended;clearTimeout(timer);server=null;}
const snap=id=>api('/api/sessions/'+id);
const idle=id=>until(async()=>{const s=await snap(id);assert.notEqual(s.session.status,'error',JSON.stringify(s.events.filter(e=>e.kind==='error')));return s.session.status==='idle'&&s;});
// A session's own MCP call, through the deterministic demo harness.
async function sessionCall(id,name,args){await idle(id);const old=await snap(id);await api(`/api/sessions/${id}/prompt`,'POST',{text:`mcp ${name} ${JSON.stringify(args)}`,requestId:randomUUID()});const s=await idle(id);const out=s.events.filter(e=>e.seq>old.seq&&e.kind==='acp'&&e.data.sessionUpdate==='tool_call_update').at(-1)?.data.rawOutput;assert(out,'Missing MCP output');return JSON.parse(out.content[0].text);}
// External client: real `agentworks mcp` stdio process; token from a private file.
async function controller(token){
 const tokenFile=join(dir,`token-${randomUUID()}`);await writeFile(tokenFile,token+'\n',{mode:0o600});
 const transport=new StdioClientTransport({command:resolve('target/debug/agentworks'),args:['mcp'],env:{PATH:process.env.PATH,AGENTWORKS_MCP_ENDPOINT_FILE:endpointFile,AGENTWORKS_MCP_TOKEN_FILE:tokenFile},stderr:'pipe'});
 const client=new Client({name:'external-controller-test',version:'1.0.0'});await client.connect(transport);clients.push(client);
 const call=async(name,args={})=>{const r=await client.callTool({name,arguments:args});const value=JSON.parse(r.content[0].text);return r.isError?{error:value}:value;};
 return {client,call,tools:async()=>(await client.listTools()).tools.map(t=>t.name).sort()};
}
try{
 await start();
 assert.equal((await stat(endpointFile)).mode&0o777,0o600);
 const p=await api('/api/projects','POST',{name:'Controlled'}),q=await api('/api/projects','POST',{name:'Elsewhere'});
 const a=await api('/api/sessions','POST',{provider:'demo',cwd:root,projectId:p.id,title:'Worker A',agentAccess:{communicate:true}});
 const b=await api('/api/sessions','POST',{provider:'demo',cwd:root,projectId:q.id,title:'Other project',agentAccess:{communicate:true}});
 await idle(a.id);await idle(b.id);
 // Creation: least-privilege default, no restart grant, token shown once and never listed.
 await api(`/api/projects/${p.id}/controllers`,'POST',{name:'Bad',access:{communicate:true,restartService:true}},400);
 await api(`/api/projects/${p.id}/controllers`,'POST',{name:'Empty',access:{}},400);
 await api(`/api/projects/${p.id}/controllers`,'POST',{name:' '},400);
 await api('/api/projects/missing/controllers','POST',{name:'Nowhere'},404);
 const messenger=await api(`/api/projects/${p.id}/controllers`,'POST',{name:'Coordinating conversation'});
 assert.match(messenger.token,/^awc_[0-9a-f]{64}$/);assert.deepEqual(messenger.controller.access,{communicate:true,startSessions:false,promptSessions:false,cancelSessions:false,runAutomations:false,restartService:false,manageReviewQueue:false,archiveSessions:false,manageAutomations:false});
 assert.equal(messenger.mcpServer.env.AGENTWORKS_MCP_ENDPOINT_FILE,endpointFile);
 const listed=JSON.stringify(await api(`/api/projects/${p.id}/controllers`));assert(!listed.includes(messenger.token)&&!listed.includes('token'),'Listing must not expose tokens');
 assert(!(await readFile(db)).includes(Buffer.from(messenger.token)),'Database must hold only token digests');
 const m=await controller(messenger.token);
 assert.deepEqual(await m.tools(),['acknowledge_message','get_completion','list_sessions','read_inbox','send_message']);
 const peers=await m.call('list_sessions');
 assert.equal(peers.self,'controller:'+messenger.controller.id);assert.equal(peers.selfUri,'agentworks://controllers/'+messenger.controller.id);
 assert.deepEqual(peers.sessions.map(s=>s.id),[a.id]);
 // Attributed messaging both ways; controllers never appear as sessions.
 const sent=await m.call('send_message',{recipient:a.id,text:'Status please',requestId:'ask-1'});assert.equal(sent.duplicate,false);
 assert.equal((await m.call('send_message',{recipient:a.id,text:'Status please',requestId:'ask-1'})).duplicate,true);
 const inboxA=await sessionCall(a.id,'read_inbox',{});const received=inboxA.messages.at(-1);
 assert.equal(received.sender,'controller:'+messenger.controller.id);assert.equal(received.senderTitle,'Coordinating conversation');assert.equal(received.senderUri,peers.selfUri);
 const seenByA=await sessionCall(a.id,'list_sessions',{});assert.deepEqual(seenByA.controllers.map(c=>c.id),[peers.self]);
 await sessionCall(a.id,'send_message',{recipient:peers.selfUri,text:'All good',requestId:'reply-1',replyTo:received.uri});
 const inboxC=await m.call('read_inbox');const reply=inboxC.messages.find(x=>x.text==='All good');assert.equal(reply.sender,a.id);assert.equal(reply.recipient,peers.self);
 assert.deepEqual(await m.call('acknowledge_message',{messageId:reply.uri}),{acknowledged:true});
 // Project boundary and least privilege, even with known IDs.
 assert.equal((await m.call('send_message',{recipient:b.id,text:'x',requestId:'cross'})).error.status,403);
 assert.equal((await m.call('get_completion',{sessionId:b.id})).error.status,403);
 assert.equal((await m.call('prompt_session',{sessionId:a.id,text:'work',requestId:'p0'})).error.status,403);
 assert.equal((await m.call('start_session',{provider:'demo',cwd:root,requestId:'s0'})).error.status,403);
 assert.match((await m.call('report_completion',{status:'completed',summary:'x',requestId:'r0'})).error.message,/Only Agentworks sessions/);
 assert.match((await m.call('restart_service',{requestId:'x',mode:'wait',expiresInSeconds:5})).error.message,/Only Agentworks sessions/);
 assert.equal((await sessionCall(b.id,'send_message',{recipient:peers.selfUri,text:'x',requestId:'from-b'})).status,403);
 // Coordinator controller: explicit grants start attributed work within its project only.
 const coordinator=await api(`/api/projects/${p.id}/controllers`,'POST',{name:'Release driver',access:{promptSessions:true,startSessions:true}});
 const c=await controller(coordinator.token);
 assert.deepEqual(await c.tools(),['get_completion','list_sessions','prompt_session','start_session']);
 const started=await c.call('start_session',{provider:'demo',cwd:root,title:'Started externally',requestId:'start-1'});
 assert.equal(started.projectId,p.id);assert.equal(started.initiatedBy.actor,'controller:'+coordinator.controller.id);assert.equal(started.initiatedBy.actorTitle,'Release driver');
 assert.equal((await c.call('start_session',{provider:'demo',cwd:root,title:'Started externally',requestId:'start-1'})).id,started.id);
 assert.deepEqual(await api(`/api/sessions/${started.id}/agent-access`),{communicate:true,startSessions:false,promptSessions:false,cancelSessions:false,runAutomations:false,restartService:false,manageReviewQueue:false,archiveSessions:false,manageAutomations:false},'Started sessions use project defaults without copying controller grants');
 await idle(started.id);
 assert.equal((await c.call('prompt_session',{sessionId:a.id,text:'Please continue',requestId:'turn-1'})).accepted,true);
 const turned=await idle(a.id);const user=turned.events.filter(e=>e.kind==='user').at(-1);
 assert.equal(user.data.initiatedBy,'controller:'+coordinator.controller.id);assert.equal(user.data.initiatorTitle,'Release driver');
 assert.equal((await c.call('prompt_session',{sessionId:b.id,text:'x',requestId:'turn-2'})).error.status,403);
 const auditA=await api(`/api/sessions/${a.id}/agent-audit`);assert(auditA.events.some(e=>e.action==='prompt_session'&&e.outcome==='accepted'&&e.actorTitle==='Release driver'));
 const auditC=await api(`/api/controllers/${coordinator.controller.id}/audit`);assert(auditC.events.some(e=>e.action==='prompt_session'&&e.outcome==='rejected'&&e.target===b.id));
 // Revocation applies to the already-connected client on its next request.
 await api(`/api/controllers/${coordinator.controller.id}/revoke`,'POST',{});
 assert.equal((await c.call('list_sessions')).error.status,401);
 assert.equal((await api(`/api/controllers/${coordinator.controller.id}/revoke`,'POST',{})).alreadyRevoked,true);
 assert.equal((await m.call('read_inbox')).messages.length>0,true);
 const forged=await controller('awc_'+'0'.repeat(64));assert.equal((await forged.call('list_sessions')).error.status,401);
 // Restart: grants persist, revocation persists, and a running bridge follows the new endpoint.
 const before=await readFile(endpointFile,'utf8');await stop();await start();
 assert.notEqual(await readFile(endpointFile,'utf8'),before);
 assert.equal((await m.call('list_sessions')).self,peers.self);
 assert.equal((await c.call('list_sessions')).error.status,401);
 // Browser: create shows the configuration once; revoke updates the list.
 browser=await chromium.launch({headless:true});const page=await browser.newPage({colorScheme:'dark'});page.setDefaultTimeout(10000);const errors=[];page.on('pageerror',e=>errors.push(e.message));
 page.on('dialog',d=>d.accept());
 await page.goto(base);await page.locator(`.tree-heading[data-project-id="${p.id}"]`).first().click({button:'right'});
 await page.getByRole('menuitem',{name:'Edit project…'}).click();await page.locator('#open-controllers').click();
 await expect(page.locator('#controllers-list')).toContainText('Coordinating conversation');await expect(page.locator('#controllers-list')).toContainText('Revoked');
 await page.locator('#controller-name').fill('Browser client');await page.locator('#controller-form button[type=submit]').click();
 await expect(page.locator('#controller-secret')).toBeVisible();assert.match(await page.locator('#controller-config').inputValue(),/"AGENTWORKS_MCP_TOKEN": "awc_[0-9a-f]{64}"/);
 const item=page.locator('.controller-item',{hasText:'Browser client'});await item.getByRole('button',{name:'Revoke'}).click();await expect(item).toContainText('Revoked');
 await page.locator('#dismiss-controllers').click();await expect(page.locator('#controller-config')).toHaveValue('');
 await page.setViewportSize({width:390,height:844});await page.locator('#open-controllers').click();
 assert(await page.locator('#controllers-dialog').evaluate(n=>{const r=n.getBoundingClientRect();return r.left>=0&&r.right<=innerWidth&&r.bottom<=innerHeight;}),'Controller dialog must fit a phone viewport');
 assert.deepEqual(errors,[]);await browser.close();browser=null;
 // Deleting the project revokes its controllers.
 await api(`/api/projects/${p.id}?sessions=ungroup`,'DELETE');
 assert.equal((await m.call('list_sessions')).error.status,401);
 console.log(`Controller smoke passed; fixture root: ${dir}`);
}finally{await browser?.close();for(const client of clients)await client.close().catch(()=>{});if(server)await stop();await removeFixture(dir);}
