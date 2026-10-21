import {removeFixture} from '../tests/support/cleanup.mjs';
// C-04b: owned worktree archive cleanup/restore against real Git fixtures and the demo harness.
// Owns its server, catalog, SQLite and repositories under .cache; never uses models or the live service.
import assert from 'node:assert/strict';
import {spawn,execFileSync} from 'node:child_process';
import {once} from 'node:events';
import {mkdir,mkdtemp,writeFile,readFile,access,rm} from 'node:fs/promises';
import {resolve,join} from 'node:path';
import {randomUUID} from 'node:crypto';
import {chromium,expect} from '@playwright/test';
const remote=process.env.AGENTWORKS_CLEANUP_REMOTE==='1';
const port=Number(process.env.AGENTWORKS_CLEANUP_PORT||(remote?4522:4521));
let hostId;
assert(port>=4521&&port<=4525,'Use an isolated test port from 4521 through 4525');
const base=`http://127.0.0.1:${port}`,headers={Origin:base,'Content-Type':'application/json'};
await mkdir('.cache',{recursive:true});
const dir=await mkdtemp(resolve('.cache/worktree-cleanup-')),root=join(dir,'allowed'),repo=join(root,'repo');
await mkdir(repo,{recursive:true});
const gitEnv=Object.fromEntries(Object.entries(process.env).filter(([k])=>!k.startsWith('GIT_')));
const git=(cwd,...args)=>execFileSync('git',['-c','core.hooksPath=/dev/null','-c','user.name=Agentworks Test','-c','user.email=test@localhost',...args],{cwd,env:gitEnv,encoding:'utf8',stdio:['ignore','pipe','pipe']}).trim();
git(repo,'init','-b','main');await writeFile(join(repo,'tracked.txt'),'committed\n');await writeFile(join(repo,'.gitignore'),'ignored/\n');git(repo,'add','.');git(repo,'commit','-m','fixture');
const env={...process.env,AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:String(port),AGENTWORKS_ROOTS:root,AGENTWORKS_CATALOG_PATH:join(dir,'catalog.json'),AGENTWORKS_DB_PATH:join(dir,'sessions.sqlite3'),AGENTWORKS_DEMO_STATE_DIR:join(dir,'demo')};
if(remote)Object.assign(env,{AGENTWORKS_SSH:resolve('scripts/fake-ssh.sh'),HOME:join(dir,'worker-home')});
let server,browser;const delay=ms=>new Promise(r=>setTimeout(r,ms));
async function until(check){for(let i=0;i<300;i++){const v=await check();if(v)return v;await delay(50);}throw new Error('Test timed out');}
async function api(path,method='GET',body,status=200){if(remote&&path==='/api/sessions'&&method==='POST')body={hostId,...body};const r=await fetch(base+path,{method,headers,signal:AbortSignal.timeout(30000),body:body===undefined?undefined:JSON.stringify(body)}),text=await r.text();assert.equal(r.status,status,`${method} ${path}: ${text}`);return status===200?JSON.parse(text):text;}
async function start(){server=spawn(resolve('target/debug/agentworks'),[],{env,stdio:['ignore','inherit','inherit']});await until(async()=>{assert.equal(server.exitCode,null,'Server exited');try{return(await fetch(base+'/api/status')).ok;}catch{return false;}});}
async function stop(check=true){const ended=once(server,'exit');server.kill('SIGINT');const timer=setTimeout(()=>server.kill('SIGKILL'),8000);timer.unref();const [code]=await ended;clearTimeout(timer);server=null;if(check)assert.equal(code,0,'Server must shut down cleanly');}
const snap=id=>api('/api/sessions/'+id);
const idle=id=>until(async()=>{const s=await snap(id);assert.notEqual(s.session.status,'error',JSON.stringify(s.events.filter(e=>e.kind==='error')));return s.session.status==='idle'&&s;});
const exists=path=>access(path).then(()=>true,()=>false);
const owned=async(title)=>{const s=await api('/api/sessions','POST',{provider:'demo',cwd:repo,projectId:project.id,title,workspace:{mode:'newWorktree'}});await idle(s.id);return s;};
const patch=(id,body,status)=>api('/api/sessions/'+id,'PATCH',body,status);
const tree=id=>api(`/api/sessions/${id}/worktree`);
const branchExists=branch=>git(repo,'branch','--list',branch)!=='';
let project;
try{
 await start();
 project=await api('/api/projects','POST',{name:'Cleanup fixture'});
 if(remote){
   const host=await api('/api/hosts','POST',{name:'Cleanup remote',destination:'dev@box',command:resolve('target/debug/agentworks')});hostId=host.id;
   await until(async()=>(await api('/api/catalog')).hosts.find(h=>h.id===hostId)?.status==='connected');
   await api(`/api/hosts/${hostId}/roots`,'PUT',{roots:[root]});
 }


 // Clean owned worktree: removed on archive, branch kept, recreated on restore.
 const clean=await owned('Clean owner');
 let status=await tree(clean.id);
 assert.equal(status.owned,true);assert.equal(status.state,'present');assert.equal(status.path,clean.cwd);
 assert.deepEqual([status.dirty.staged,status.dirty.unstaged,status.dirty.untracked],[0,0,0]);
 assert.equal(status.cleanup.allowed,true);assert.equal(status.cleanup.requiresConfirmation,false);assert.equal(status.integration.merge,false);
 const branch=status.branch;assert(branch.startsWith('agentworks/'));
 if(remote){
   const local=await api('/api/sessions','POST',{provider:'demo',cwd:clean.cwd,hostId:'local'});await idle(local.id);
   await api(`/api/sessions/${local.id}/suspend`,'POST',{});
   assert.equal((await tree(clean.id)).cleanup.allowed,true,'Identical paths on different hosts must not count as sharing');
   await api(`/api/sessions/${local.id}`,'DELETE');
   const serverRoot=join(dir,'server-only');await mkdir(serverRoot);
   await api('/api/settings','PUT',{...(await api('/api/settings')),roots:[serverRoot]});
 }

 // Ignored build output does not make a worktree dirty; it is deleted with the checkout.
 await mkdir(join(clean.cwd,'ignored'));await writeFile(join(clean.cwd,'ignored/build.log'),'x');
 let updated=await patch(clean.id,{archived:true});
 assert.equal(updated.archived,true);assert.equal(updated.worktreeCleanup.outcome,'removed');assert.equal(updated.worktreeCleanup.forced,false);
 assert.equal(await exists(clean.cwd),false);assert(branchExists(branch),'Branch must be retained');
 assert.equal((await snap(clean.id)).session.status,'suspended');
 assert.equal((await tree(clean.id)).state,'removed');assert.equal(updated.worktree.removed,true);
 assert.match(await api(`/api/sessions/${clean.id}/resume`,'POST',undefined,409),/unarchive/);
 // Removal metadata survives restart.
 await stop();await start();
 if(remote)await until(async()=>(await api('/api/catalog')).hosts.find(h=>h.id===hostId)?.status==='connected');
 assert.equal((await tree(clean.id)).state,'removed');
 updated=await patch(clean.id,{archived:false});
 assert.equal(updated.archived,false);assert.equal(updated.worktreeRestore.outcome,'restored');
 assert.equal(git(clean.cwd,'branch','--show-current'),branch);assert.equal(await readFile(join(clean.cwd,'tracked.txt'),'utf8'),'committed\n');
 await api(`/api/sessions/${clean.id}/resume`,'POST');await idle(clean.id);
 assert.equal((await tree(clean.id)).state,'present');

 // Dirty worktree: automatic archive keeps it; removal needs the exact confirmed state.
 const dirty=await owned('Dirty owner');
 await writeFile(join(dirty.cwd,'tracked.txt'),'edited\n');await writeFile(join(dirty.cwd,'staged.txt'),'staged\n');git(dirty.cwd,'add','staged.txt');await writeFile(join(dirty.cwd,'untracked.txt'),'untracked\n');
 status=await tree(dirty.id);
 assert.deepEqual([status.dirty.staged,status.dirty.unstaged,status.dirty.untracked],[1,1,1]);assert.equal(status.cleanup.requiresConfirmation,true);
 updated=await patch(dirty.id,{archived:true});
 assert.equal(updated.archived,true);assert.equal(updated.worktreeCleanup.outcome,'retained');assert.match(updated.worktreeCleanup.reason,/uncommitted/);
 assert.equal(await readFile(join(dirty.cwd,'untracked.txt'),'utf8'),'untracked\n');
 await patch(dirty.id,{archived:false});
 // Declined confirmation: archive and keep.
 updated=await patch(dirty.id,{archived:true,worktree:'keep'});assert.equal(updated.worktreeCleanup.outcome,'retained');assert(await exists(join(dirty.cwd,'untracked.txt')));
 await patch(dirty.id,{archived:false});
 await api(`/api/sessions/${dirty.id}`,'PATCH',{archived:false,worktree:'remove'},400);
 // Stale or missing confirmations are refused without archiving.
 const fingerprint=(await tree(dirty.id)).fingerprint;
 await patch(dirty.id,{archived:true,worktree:'remove'},409);
 await writeFile(join(dirty.cwd,'untracked.txt'),'changed after confirmation\n');
 assert.match(await patch(dirty.id,{archived:true,worktree:'remove',confirmDirty:fingerprint},409),/differ/);
 assert.equal((await snap(dirty.id)).session.archived,false);assert(await exists(join(dirty.cwd,'untracked.txt')));
 const dirtyBranch=status.branch;
 updated=await patch(dirty.id,{archived:true,worktree:'remove',confirmDirty:(await tree(dirty.id)).fingerprint});
 assert.equal(updated.worktreeCleanup.outcome,'removed');assert.equal(updated.worktreeCleanup.forced,true);assert.equal(await exists(dirty.cwd),false);assert(branchExists(dirtyBranch));

 // Occupied restore path is refused safely; the session stays archived and the occupant is untouched.
 const occupied=await owned('Occupied restore');
 await patch(occupied.id,{archived:true});assert.equal(await exists(occupied.cwd),false);
 await mkdir(occupied.cwd);await writeFile(join(occupied.cwd,'someone-else.txt'),'keep me\n');
 assert.match(await patch(occupied.id,{archived:false},409),/occupied/);
 assert.equal((await snap(occupied.id)).session.archived,true);assert.equal(await readFile(join(occupied.cwd,'someone-else.txt'),'utf8'),'keep me\n');
 await rm(occupied.cwd,{recursive:true});
 assert.equal((await patch(occupied.id,{archived:false})).worktreeRestore.outcome,'restored');
 // A manually recreated checkout of the same branch is adopted rather than replaced.
 await patch(occupied.id,{archived:true});const occupiedBranch=(await tree(occupied.id)).branch;
 git(repo,'worktree','add',occupied.cwd,occupiedBranch);
 assert.equal((await patch(occupied.id,{archived:false})).worktreeRestore.outcome,'adopted');
 // A deleted branch cannot be restored.
 await patch(occupied.id,{archived:true});git(repo,'branch','-D',occupiedBranch);
 assert.match(await patch(occupied.id,{archived:false},409),/no longer exists/);assert.equal((await snap(occupied.id)).session.archived,true);

 // Unsafe cleanup is refused: unowned, shared, locked, detached and running worktrees are retained.
 const linked=join(root,'manual-linked');git(repo,'worktree','add','-b','manual-linked',linked);
 const unowned=await api('/api/sessions','POST',{provider:'demo',cwd:linked,projectId:project.id});await idle(unowned.id);
 assert.equal((await tree(unowned.id)).owned,false);
 updated=await patch(unowned.id,{archived:true});assert.equal(updated.worktreeCleanup.outcome,'none');assert(await exists(linked));
 const host=await owned('Shared owner');
 const guest=await api('/api/sessions','POST',{provider:'demo',cwd:host.cwd,projectId:project.id});await idle(guest.id);
 assert.equal((await tree(host.id)).cleanup.allowed,false);
 updated=await patch(host.id,{archived:true});assert.equal(updated.worktreeCleanup.outcome,'retained');assert.match(updated.worktreeCleanup.reason,/Another session/);assert(await exists(host.cwd));
 await api('/api/sessions/'+guest.id,'DELETE');
 assert.match(await api(`/api/sessions/${host.id}/worktree/remove`,'POST',{},200).then(v=>v.outcome),/removed/);assert.equal(await exists(host.cwd),false);
 const locked=await owned('Locked owner');git(repo,'worktree','lock',locked.cwd);
 updated=await patch(locked.id,{archived:true});assert.match(updated.worktreeCleanup.reason,/locked/);assert(await exists(locked.cwd));
 const detached=await owned('Detached owner');git(detached.cwd,'checkout','--detach');
 updated=await patch(detached.id,{archived:true});assert.match(updated.worktreeCleanup.reason,/detached/);assert(await exists(detached.cwd));
 const running=await owned('Running owner');
 await api(`/api/sessions/${running.id}/prompt`,'POST',{text:'approval please',requestId:randomUUID()});
 await until(async()=>(await snap(running.id)).approvals.length>0);
 updated=await patch(running.id,{archived:true});assert.match(updated.worktreeCleanup.reason,/running/);assert(await exists(running.cwd));
 await api(`/api/sessions/${running.id}/worktree/remove`,'POST',{},409);
 await api(`/api/sessions/${running.id}/stop`,'POST');
 const active=await owned('Not archived');
 assert.match(await api(`/api/sessions/${active.id}/worktree/remove`,'POST',{},409),/Archive the session/);

 // Browser: dirty archive asks first; Cancel leaves it active, Keep archives without removing.
 await writeFile(join(active.cwd,'notes.txt'),'draft\n');
 browser=await chromium.launch({headless:true});const page=await browser.newPage({colorScheme:'dark'});page.setDefaultTimeout(10000);const errors=[];page.on('pageerror',e=>errors.push(e.message));
 const archiveButton=async()=>{if(!await page.locator('#lifecycle-menu').evaluate(n=>n.open))await page.locator('#lifecycle-menu > summary').click();await page.locator('#archive-session').click();};
 await page.goto(base);await page.evaluate(id=>localStorage.setItem('agentworks-session',id),active.id);await page.reload();
 await archiveButton();
 await expect(page.locator('#worktree-archive-dialog')).toBeVisible();await expect(page.locator('#worktree-archive-paths')).toContainText('notes.txt');
 await page.locator('#worktree-archive-cancel').click();await expect(page.locator('#worktree-archive-dialog')).toBeHidden();
 await delay(300);assert.equal((await snap(active.id)).session.archived,false);
 await archiveButton();await page.locator('#worktree-archive-keep').click();
 await until(async()=>(await snap(active.id)).session.archived);assert(await exists(join(active.cwd,'notes.txt')));
 await archiveButton();await until(async()=>!(await snap(active.id)).session.archived);
 await page.setViewportSize({width:390,height:844});
 await archiveButton();
 assert(await page.locator('#worktree-archive-dialog').evaluate(n=>{const r=n.getBoundingClientRect();return r.left>=0&&r.right<=innerWidth;}),'Dialog must fit a phone viewport');
 await page.locator('#worktree-archive-remove').click();
 await until(async()=>(await snap(active.id)).session.archived);assert.equal(await exists(active.cwd),false);
 assert.deepEqual(errors,[]);
 if(remote){
   const restricted=await owned('Changed host roots');
   await api(`/api/hosts/${hostId}/roots`,'PUT',{roots:[join(dir,'server-only')]});
   assert.equal((await tree(restricted.id)).cleanup.allowed,false);
   await patch(restricted.id,{archived:true,worktree:'remove'},409);
   assert(await exists(join(restricted.cwd,'tracked.txt')),'Revoked host roots must prevent cleanup');
   await api(`/api/hosts/${hostId}/roots`,'PUT',{roots:[root]});
   assert.equal((await patch(restricted.id,{archived:true})).worktreeCleanup.outcome,'removed');
 }

 console.log(`${remote?'Remote':'Local'} worktree cleanup smoke passed; fixture root: ${dir}`);
}finally{await browser?.close();if(server)await stop(false);await removeFixture(dir);}
