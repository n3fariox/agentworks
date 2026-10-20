// R-06 isolated process/native demo/MCP/browser acceptance. No paid inference or live data.
import assert from 'node:assert/strict';
import {spawn,execFileSync} from 'node:child_process';
import {mkdtemp,mkdir,copyFile,chmod,writeFile,readFile,unlink,rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';
import {once} from 'node:events';
const dir=await mkdtemp(join(tmpdir(),'agentworks-restart-')),releases=join(dir,'releases'),control=join(dir,'control');await mkdir(releases);await mkdir(control);
const binary=resolve('target/debug/agentworks');await copyFile(binary,join(releases,'v1'));await chmod(join(releases,'v1'),0o700);await copyFile(binary,join(releases,'v2'));await chmod(join(releases,'v2'),0o700);
const base='http://127.0.0.1:4441',env={...process.env,AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:'4441',AGENTWORKS_ROOTS:dir,AGENTWORKS_CATALOG_PATH:join(dir,'catalog.json'),AGENTWORKS_DB_PATH:join(dir,'sessions.sqlite3'),AGENTWORKS_DEMO_STATE_DIR:join(dir,'demo')};
let server,browser;
const delay=ms=>new Promise(r=>setTimeout(r,ms));
async function api(path,body,method=body===undefined?'GET':'POST',code=200){const r=await fetch(base+path,{method,headers:{Origin:base,'Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body),signal:AbortSignal.timeout(3000)});const text=await r.text();assert.equal(r.status,code,text);return code===200?JSON.parse(text):text;}
async function until(fn,tries=160){for(let i=0;i<tries;i++){let r=await fn();if(r)return r;await delay(100);}throw Error('Restart fixture timed out');}
async function up(){await until(async()=>{try{return await api('/api/service/restart');}catch{return false;}});}
async function start(supervised=true){server=spawn(supervised?'/usr/bin/python3':join(releases,'v1'),supervised?[resolve('scripts/service/supervise.py'),'--release-dir',releases,'--release','v1','--control-dir',control,'--ready-timeout','8']:[],{env,stdio:['ignore','inherit','inherit']});await up();}
async function stop(){if(!server)return;if(server.exitCode===null){const done=once(server,'exit');server.kill('SIGTERM');const timeout=setTimeout(()=>server.kill('SIGKILL'),7000);await done;clearTimeout(timeout);}server=null;}
const path=id=>'/api/sessions/'+id,snap=id=>api(path(id)),idle=id=>until(async()=>{const s=await snap(id);return s.session.status==='idle'&&s;});
const prompt=(id,text)=>api(path(id)+'/prompt',{text,requestId:crypto.randomUUID()});
const restart=(requestId,more={})=>({requestId,mode:'wait',expiresInSeconds:30,...more});
const request=r=>api('/api/service/restart',r);
const phase=(id,p)=>until(async()=>{const s=await api('/api/service/restart');return s.requests.find(r=>r.request.requestId===id&&r.phase===p);});
async function call(id,name,args={}){await idle(id);const old=(await snap(id)).seq;await prompt(id,`mcp ${name} ${JSON.stringify(args)}`);const s=await idle(id);return s.events.filter(e=>e.seq>old&&e.kind==='acp'&&e.data.sessionUpdate==='tool_call_update').at(-1)?.data.rawOutput;}
try{
 await start(false);assert.equal((await api('/api/service/restart')).restartSupported,false);await api('/api/service/restart',restart('unsupported'),'POST',409);await stop();
 await start();assert((await api('/api/service/restart')).restartSupported);
 const project=await api('/api/projects',{name:'Restart fixtures'});
 const a=await api('/api/sessions',{provider:'demo',cwd:dir,title:'Restart coordinator',projectId:project.id});await idle(a.id);
 assert.equal((await api(path(a.id)+'/agent-access')).restartService,false);
 let denied=await call(a.id,'restart_service',restart('denied'));assert(denied?.isError);
 await api(path(a.id)+'/agent-access',{communicate:true,startSessions:true,restartService:true},'PUT');await idle(a.id);
 await writeFile(join(releases,'legacy'),'#!/bin/sh\necho incompatible\n',{mode:0o700});await api('/api/service/restart',restart('legacy',{release:'legacy'}),'POST',400);
 await api('/api/service/restart',restart('invalid',{release:'../sh'}),'POST',400);await api('/api/service/restart',restart('missing',{release:'absent'}),'POST',400);
 await prompt(a.id,'approval');await until(async()=>(await snap(a.id)).approvals.length);
 const pending=restart('cancelled');await request(pending);assert.equal((await request(pending)).request.requestId,'cancelled');await api('/api/service/restart',{...pending,mode:'interrupt'},'POST',409);await api('/api/service/restart',restart('concurrent'),'POST',409);
 for(const [p,b] of [[path(a.id)+'/prompt',{text:'blocked',requestId:'blocked'}],[path(a.id)+'/resume',{}],[path(a.id)+'/model',{model:'demo'}],['/api/sessions',{provider:'demo',cwd:dir}]])await api(p,b,'POST',409);
 assert((await api('/api/service/restart')).activity.approvals>0);await api('/api/service/restart/cancel',{actor:'human',requestId:'cancelled'});await phase('cancelled','cancelled');await api(path(a.id)+'/stop',{});await idle(a.id);
 // Expiry, live grant revocation, explicit Stop, read-only status, own cancel and child default grants.
 await prompt(a.id,'approval');await until(async()=>(await snap(a.id)).approvals.length);await request(restart('expires',{expiresInSeconds:5}));await phase('expires','cancelled');await api(path(a.id)+'/stop',{});await idle(a.id);
 const peer=await api('/api/sessions',{provider:'demo',cwd:dir,title:'Approval blocker',projectId:project.id});await idle(peer.id);await prompt(peer.id,'approval');await until(async()=>(await snap(peer.id)).approvals.length);
 let r=await call(a.id,'restart_service',restart('revoked'));assert(!r.isError);await api(path(a.id)+'/agent-access',{communicate:true,startSessions:true},'PUT');await phase('revoked','cancelled');assert((await call(a.id,'get_service_status')).isError);await api(path(a.id)+'/agent-access',{communicate:true,startSessions:true,restartService:true},'PUT');await idle(a.id);
 r=await call(a.id,'restart_service',restart('stop-wins',{handoff:true,handoffDeadline:Date.now()+30000}));assert(!r.isError);await api(path(a.id)+'/stop',{});await phase('stop-wins','cancelled');await idle(a.id);
 r=await call(a.id,'restart_service',restart('own-cancel'));assert(!r.isError);
 // Existing turns can cancel, but new turns are deliberately blocked. Use the human global cancel here.
 await api('/api/service/restart/cancel',{actor:a.id,requestId:'own-cancel'});await phase('own-cancel','cancelled');await api(path(peer.id)+'/stop',{});await idle(peer.id);
 const statusResult=await call(a.id,'get_service_status');assert(!statusResult.isError);
 const child=JSON.parse((await call(a.id,'start_session',{provider:'demo',cwd:dir,requestId:'unprivileged-child'})).content[0].text);await idle(child.id);assert.equal((await api(path(child.id)+'/agent-access')).restartService,false);
 await writeFile(join(control,'backups'),'backup failure fixture');await request(restart('backup-failure'));await phase('backup-failure','failed');assert.equal((await snap(a.id)).session.status,'idle');await unlink(join(control,'backups'));
 await api(path(peer.id)+'/continuation',{enabled:true},'PUT');await prompt(peer.id,'quota unknown');await idle(peer.id);const timer=await api(path(peer.id)+'/continuation',{enabled:true,manualDueAt:Date.now()+600000},'PUT');const savedTimer=(await snap(peer.id)).session.continuation.dueAt;assert(savedTimer);
 // Requesting MCP session gets durable identity; successful turn ends before manager dispatch.
 const before=await snap(a.id),native=before.events.filter(e=>e.kind==='provider_session').at(-1).data.id;
 const oldInstance=(await api('/api/service/restart')).instanceId;
 await prompt(a.id,'mcp restart_service '+JSON.stringify(restart('native-handoff',{release:'v2',handoff:true,handoffDeadline:Date.now()+60000})));
 await until(async()=>{try{const s=await api('/api/service/restart');return s.instanceId!==oldInstance&&s.requests.some(r=>r.request.requestId==='native-handoff'&&r.phase==='ready'&&r.handoffPhase==='dispatched');}catch{return false;}});
 let s=await idle(a.id);assert.equal(s.events.filter(e=>e.kind==='provider_session').at(-1).data.id,native);assert.equal(s.events.filter(e=>e.kind==='user'&&e.data.initiatedBy==='service-restart manager').length,1);assert((await api(path(a.id)+'/agent-access')).restartService);assert.equal(s.session.cwd,dir);assert.equal((await snap(peer.id)).session.continuation.dueAt,savedTimer);assert(s.events.length>=before.events.length);
 // Real interruption and requester disconnect; browser reconnect reads GET only.
 const {chromium}=await import('@playwright/test');browser=await chromium.launch({headless:true});const page=await browser.newPage({viewport:{width:1280,height:900},colorScheme:'dark'});const errors=[];page.on('pageerror',e=>errors.push(e.message));let posts=0;page.on('request',r=>{if(r.url().endsWith('/api/service/restart')&&r.method()==='POST')posts++;});await page.goto(base);await page.locator('#nav-settings').click();await page.locator('#settings-tab-service').click();await page.locator('#service-controls').waitFor();await page.setViewportSize({width:390,height:844});await until(async()=>(await page.locator('#service-status').innerText()).includes('Supervised'));
 assert(await page.locator('#service-controls').evaluate(n=>n.scrollWidth<=n.clientWidth));
 // Actual mobile web button uses one explicit POST; cancellation and polling never replay it.
 await prompt(a.id,'approval');await until(async()=>(await snap(a.id)).approvals.length);page.on('dialog',d=>d.accept());await api(path(a.id)+'/queue',{text:'Wait for explicit resume after drain',requestId:'queue-before-drain'});await page.locator('#service-restart').click();await until(async()=>(await api('/api/service/restart')).draining);assert.equal(posts,1);const queueBeforeDrain=(await api(path(a.id)+'/queue')).queue;await api(path(a.id)+'/queue/resume',{},'POST',409);assert.deepEqual((await api(path(a.id)+'/queue')).queue,queueBeforeDrain);await page.locator('#service-cancel').click();await until(async()=>!(await api('/api/service/restart')).draining);await api(path(a.id)+'/stop',{});await idle(a.id);
 await prompt(a.id,'approval');await until(async()=>(await snap(a.id)).approvals.length);const instance=(await api('/api/service/restart')).instanceId;await request(restart('interrupt',{mode:'interrupt'}));
 await until(async()=>{try{return (await api('/api/service/restart')).instanceId!==instance;}catch{return false;}});await phase('interrupt','ready');await until(async()=>(await page.locator('#service-status').innerText()).includes('interrupt: ready'));
 assert.equal((await snap(a.id)).approvals.length,0);assert.equal(posts,1);await page.reload();assert.equal(posts,1);await page.locator('#menu').click();await page.locator('#nav-settings').click();await page.locator('#settings-tab-service').click();await page.locator('#service-controls').waitFor();assert.deepEqual(errors,[]);await browser.close();browser=null;
 // Backup is readable with all persistent grants, history and timer policy fields.
 execFileSync('python3',['-c',`import sqlite3,glob,json,sys
p=sorted(glob.glob(sys.argv[1]+'/backups/*/sessions.sqlite3'))[-1];c=sqlite3.connect(p);assert c.execute('pragma integrity_check').fetchone()[0]=='ok';assert c.execute('select count(*) from events').fetchone()[0]>0;assert json.loads(c.execute('select grants from agent_access where session_id=?',(sys.argv[2],)).fetchone()[0])['restartService'];assert 'continuation' in json.loads(c.execute('select state from sessions where id=?',(sys.argv[2],)).fetchone()[0])`,control,a.id]);
 // Cold supervisor restart retains the last successfully selected immutable release.
 await stop();await start();assert.equal((await api('/api/service/restart')).executable,join(releases,'v2'));assert.equal((await snap(a.id)).events.filter(e=>e.kind==='user'&&e.data.initiatedBy==='service-restart manager').length,1);
 // Fake preparation failure is visible and bounded: executable passes validation but cannot start.
 await writeFile(join(releases,'bad'),`#!/bin/sh
if [ "$1" = "--service-protocol" ]; then echo '{"serviceProtocol":1}'; exit 0; fi
exit 1
`,{mode:0o700});await request(restart('failed-release',{release:'bad'}));await until(async()=>server.exitCode!==null);const outcome=JSON.parse(await readFile(join(control,'outcome.json')));assert.equal(outcome.phase,'failed');await stop();await start();assert.equal((await phase('failed-release','uncertain')).phase,'uncertain');
 console.log('PASS R-06: supervised process/release restart, durable identity/conflict, missing supervisor/release, drain/approval/start gates/cancel, MCP native handoff exactly once, history/native/grants/backup integrity, mobile dark/reconnect GET-only, bounded failed successor/uncertain recovery');
}finally{
 if(browser)await browser.close();await stop();
 // Each run copies two release binaries; tmpfs /tmp fills quickly if they are kept.
 if(process.env.AGENTWORKS_KEEP_FIXTURES)console.log('Fixture kept at '+dir);else await rm(dir,{recursive:true,force:true});
}
