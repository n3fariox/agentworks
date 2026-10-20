import {removeFixture} from '../tests/support/cleanup.mjs';
// L-01: isolated deterministic ACP, SQLite restart and dark desktop/mobile checks.
// No credentials, real providers, live service, or shared fixtures are used.
import assert from 'node:assert/strict';
import {spawn,execFileSync} from 'node:child_process';
import {once} from 'node:events';
import {mkdtemp,mkdir,writeFile,readFile} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';
import {chromium,expect} from '@playwright/test';
const dir=await mkdtemp(join(tmpdir(),'agentworks-lifecycle-')),repository=join(dir,'repository'),cwd=join(dir,'worktree');await mkdir(repository);
execFileSync('git',['init','-q',repository]);
await writeFile(join(repository,'retained.txt'),'Original file\n');
execFileSync('git',['-C',repository,'add','retained.txt']);
execFileSync('git',['-C',repository,'-c','user.name=Agentworks automation','-c','user.email=agentworks@localhost','commit','-qm','Fixture base']);
execFileSync('git',['-C',repository,'worktree','add','-qb','lifecycle-fixture',cwd]);
await writeFile(join(cwd,'retained.txt'),'Keep this workspace\n');
await writeFile(join(cwd,'untracked.txt'),'Retain untracked files\n');
const git=()=>execFileSync('git',['-C',cwd,'status','--porcelain'],{encoding:'utf8'});
const originalGit=git(),worktrees=()=>execFileSync('git',['-C',repository,'worktree','list','--porcelain'],{encoding:'utf8'}),originalWorktrees=worktrees();
const port=Number(process.env.AGENTWORKS_LIFECYCLE_PORT||4401);assert(port>=4401&&port<=4405);
const base=`http://127.0.0.1:${port}`,env={...process.env,AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:String(port),AGENTWORKS_ROOTS:dir,AGENTWORKS_CATALOG_PATH:join(dir,'catalog.json'),AGENTWORKS_DB_PATH:join(dir,'sessions.sqlite3'),AGENTWORKS_DEMO_STATE_DIR:join(dir,'demo')};
let server,browser;
const delay=ms=>new Promise(r=>setTimeout(r,ms));
async function api(path,body,method=body===undefined?'GET':'POST',status=200){const r=await fetch(base+path,{method,headers:{Origin:base,'Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body),signal:AbortSignal.timeout(10000)});const text=await r.text();assert.equal(r.status,status,text);return status===200?JSON.parse(text):text;}
async function until(fn){for(let i=0;i<180;i++){const v=await fn();if(v)return v;await delay(80);}throw Error('Lifecycle wait timed out');}
async function start(){server=spawn(resolve('target/debug/agentworks'),[],{env,stdio:['ignore','inherit','inherit']});await until(async()=>{if(server.exitCode!==null)throw Error('Server exited');try{return(await fetch(base+'/api/status')).ok;}catch{return false;}});}
async function stop(signal='SIGINT'){const done=once(server,'exit');server.kill(signal);const timer=setTimeout(()=>server.kill('SIGKILL'),6000);timer.unref();await done;clearTimeout(timer);server=null;}
const path=id=>`/api/sessions/${id}`,snap=id=>api(path(id));
const idle=id=>until(async()=>{const s=await snap(id);assert.notEqual(s.session.status,'error',JSON.stringify(s.events.slice(-3)));return s.session.status==='idle'&&s;});
const prompt=(id,text)=>api(path(id)+'/prompt',{text,requestId:crypto.randomUUID()});
const users=s=>s.events.filter(e=>e.kind==='user').length;
const native=s=>s.events.filter(e=>e.kind==='provider_session').at(-1)?.data.id;
try{
 await start();
 const project=await api('/api/projects',{name:'Lifecycle fixture'});
 const session=await api('/api/sessions',{provider:'demo',cwd,title:'Original title',projectId:project.id,agentAccess:{communicate:true},resumePolicy:{enabled:true}}),id=session.id;
 await idle(id);await prompt(id,'mcp report_completion {"requestId":"retained-claim","status":"completed","summary":"Fixture claim"}');
 let s=await idle(id);const provider=native(s),claim=await api(path(id)+'/completion');assert(claim.report,'Completion fixture failed');
 await api(path(id),{title:'  Renamed <safe>  ',archived:true},'PATCH');
 s=await snap(id);assert.equal(s.session.title,'Renamed <safe>');assert.equal(s.session.archived,true);assert.equal(s.session.status,'idle');
 assert(!(await api('/api/sessions')).some(s=>s.id===id));assert((await api('/api/sessions?archived=true')).some(s=>s.id===id));assert((await api('/api/sessions?archived=all')).some(s=>s.id===id));
 await api(path(id),{title:'   '},'PATCH',400);await api(path(id),{title:'a'.repeat(81)},'PATCH',400);await api(path(id),{title:'bad\nname'},'PATCH',400);
 assert.equal((await snap(id)).session.title,'Renamed <safe>');
 const discovery=await api('/api/external-sessions',{provider:'demo',cwd});assert(discovery.sessions.some(s=>s.agentworksId===id&&s.archived&&s.title==='Renamed <safe>'));
 // Archive does not stop work, and unarchive does not create a new context.
 await prompt(id,'slow');await until(async()=>users(await snap(id))===2);
 await api(path(id),{archived:false},'PATCH');assert(['running','waiting'].includes((await snap(id)).session.status));
 await api(path(id)+'/stop',{});s=await idle(id);assert.equal(s.approvals.length,0);assert.equal(s.session.continuation.enabled,false);assert.equal(s.session.continuation.dueAt,null);
 // Explicit suspension clears approval and continuation, retains native context/claims.
 await api(path(id)+'/continuation',{enabled:true},'PUT');await prompt(id,'approval');
 const approval=await until(async()=>{const s=await snap(id);return s.approvals[0];});
 await api(path(id)+'/suspend',{});s=await snap(id);assert.equal(s.session.status,'suspended');assert.equal(s.session.explicitlySuspended,true);assert.equal(s.approvals.length,0);assert.equal(s.session.continuation.enabled,false);
 await api(path(id)+`/approvals/${approval.id}`,{optionId:'allow'},'POST',409);
 const beforeUsers=users(s);assert.deepEqual(await api(path(id)+'/completion'),claim);
 // Persist cancellation through crash/restart past quota due time.
 await api(path(id)+'/resume',{});await idle(id);await api(path(id)+'/continuation',{enabled:true},'PUT');await prompt(id,'quota known');s=await idle(id);assert(s.session.continuation.dueAt);
 await api(path(id)+'/suspend',{});await api(path(id),{archived:true},'PATCH');await stop('SIGKILL');await delay(4500);await start();
 s=await snap(id);assert.equal(s.session.explicitlySuspended,true);assert.equal(s.session.archived,true);assert.equal(s.session.title,'Renamed <safe>');assert.equal(s.session.continuation.dueAt,null);assert.equal(users(s),beforeUsers+1);assert.deepEqual(await api(path(id)+'/completion'),claim);
 // Browser opening/reloading an explicitly suspended transcript must not resume.
 browser=await chromium.launch({headless:true});const page=await browser.newPage({colorScheme:'dark'});page.setDefaultTimeout(10000);const errors=[];page.on('pageerror',e=>errors.push(e.message));
 await page.goto(base);await page.evaluate(id=>localStorage.setItem('agentworks-session',id),id);await page.reload();
 await expect(page.locator('#session-state')).toHaveText('Suspended');await expect(page.locator('#messages')).toContainText('Fixture claim');
 await page.waitForTimeout(800);assert.equal((await snap(id)).session.status,'suspended');
 await page.locator('#session-filter').selectOption('archived');await expect(page.locator(`[data-session-id="${id}"]`)).toBeVisible();
 await page.locator('#lifecycle-menu > summary').click();await page.locator('#rename-session').click();await page.locator('#rename-title').fill('Browser renamed');await page.locator('#rename-form button[type=submit]').click();await expect(page.locator('#header-title')).toHaveText('Browser renamed');
 await page.locator('#archive-session').click();await expect(page.locator(`[data-session-id="${id}"]`)).toHaveCount(0);
 await page.locator('#session-filter').selectOption('active');await expect(page.locator(`[data-session-id="${id}"]`)).toBeVisible();
 await page.locator('#resume-session').click();await expect(page.locator('#session-state')).toHaveText('Ready');s=await idle(id);assert.equal(native(s),provider);assert.equal(users(s),beforeUsers+1);
 await page.setViewportSize({width:390,height:844});await page.waitForTimeout(250);
 await page.locator('#suspend-session').click();await expect(page.locator('#session-state')).toHaveText('Suspended');
 assert(await page.locator('.lifecycle-actions').evaluate(n=>{const r=n.getBoundingClientRect();return r.left>=0&&r.right<=innerWidth&&r.bottom<=innerHeight;}),'Mobile lifecycle menu must fit the viewport');
 assert(await page.locator('#suspend-session').evaluate(n=>n.getBoundingClientRect().height>=44));
 assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),'No mobile page overflow');
 await page.screenshot({path:join(dir,'mobile-lifecycle.png')});await page.reload();await expect(page.locator('#session-state')).toHaveText('Suspended');
 await stop();await start();await page.waitForTimeout(1000);assert.equal((await snap(id)).session.status,'suspended');
 assert.deepEqual(errors,[]);await browser.close();browser=null;
 // Delayed native opening makes Stop/Suspend races deterministic.
 const demoFile=join(env.AGENTWORKS_DEMO_STATE_DIR,provider+'.json');
 const saved=JSON.parse(await readFile(demoFile,'utf8'));saved.resumeDelayMs=600;await writeFile(demoFile,JSON.stringify(saved));
 for(const action of ['suspend','stop','suspend']){
  await api(path(id)+'/resume',{});assert.equal((await snap(id)).session.status,'resuming');
  await api(path(id)+'/'+action,{});await delay(750);assert.equal((await snap(id)).session.status,'suspended');
  assert.equal((await snap(id)).session.explicitlySuspended,true);
 }
 saved.resumeDelayMs=0;await writeFile(demoFile,JSON.stringify(saved));
 await api(path(id)+'/resume',{});await idle(id);
 for(let i=0;i<4;i++){
  await prompt(id,'quota generic');await api(path(id)+'/stop',{});await idle(id);
 }
 await prompt(id,'approval');await until(async()=>(await snap(id)).approvals.length);
 await api(path(id)+'/stop',{});s=await idle(id);assert.equal(s.approvals.length,0);
 // Permission outcomes that end a prompt immediately must not resurrect running.
 for(let i=0;i<3;i++){
  await prompt(id,'quota active-approval');const a=await until(async()=>(await snap(id)).approvals[0]);
  await api(path(id)+`/approvals/${a.id}`,{optionId:'allow'});await idle(id);
 }
 await prompt(id,'approval');await until(async()=>(await snap(id)).approvals.length);
 // DELETE remains compatible for active workers; separate Suspend never deletes history.
 await api(path(id),undefined,'DELETE');await stop();await start();await api(path(id),undefined,'GET',404);
 assert.equal(git(),originalGit);assert.equal(worktrees(),originalWorktrees);assert.equal(await readFile(join(cwd,'retained.txt'),'utf8'),'Keep this workspace\n');
 console.log(`PASS: L-01 rename/archive discovery, active-stop/approval suspension, quota cancellation, crash/restart/native identity/claims/transcripts, resume race, deletion compatibility, dark desktop/mobile controls. Fixture root: ${dir}`);
}finally{await browser?.close();if(server)await stop();await removeFixture(dir);}
