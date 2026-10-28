// Isolated local/fake-SSH verification; uses only the demo harness.
import assert from 'node:assert/strict';
import {spawn,execFileSync} from 'node:child_process';
import {once} from 'node:events';
import {mkdir,mkdtemp,writeFile,readFile,access,rm} from 'node:fs/promises';
import {resolve,join} from 'node:path';
const remote=process.env.AGENTWORKS_GIT_REMOTE==='1',port=remote?4532:4531;
const base=`http://127.0.0.1:${port}`,headers={Origin:base,'Content-Type':'application/json'};
await mkdir('.cache',{recursive:true});
const dir=await mkdtemp(resolve('.cache/git-settings-')),repo=join(dir,'repo');await mkdir(repo);
const gitEnv=Object.fromEntries(Object.entries(process.env).filter(([key])=>!key.startsWith('GIT_')));
const git=(...args)=>execFileSync('git',['-c','core.hooksPath=/dev/null','-c','user.name=Test','-c','user.email=test@localhost',...args],{cwd:repo,env:gitEnv,encoding:'utf8'}).trim();
git('init','-b','main');await writeFile(join(repo,'tracked.txt'),'committed\n');await writeFile(join(repo,'.gitignore'),'*.log\nbuild/\ncompile_commands.json\n');git('add','.');git('commit','-m','fixture');
const env={...process.env,AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:String(port),AGENTWORKS_ROOTS:dir,AGENTWORKS_CATALOG_PATH:join(dir,'catalog.json'),AGENTWORKS_DB_PATH:join(dir,'sessions.sqlite3'),AGENTWORKS_DEMO_STATE_DIR:join(dir,'demo')};
if(remote)Object.assign(env,{AGENTWORKS_SSH:resolve('scripts/fake-ssh.sh'),HOME:join(dir,'worker-home')});
let server,hostId='local';
const delay=ms=>new Promise(ok=>setTimeout(ok,ms));
async function until(check){for(let i=0;i<400;i++){const result=await check();if(result)return result;await delay(50);}throw new Error('Timed out');}
async function api(path,method='GET',body,status=200){const response=await fetch(base+path,{method,headers,signal:AbortSignal.timeout(30000),body:body===undefined?undefined:JSON.stringify(body)});const text=await response.text();assert.equal(response.status,status,`${method} ${path}: ${text}`);return status===200?JSON.parse(text):text;}
const snapshot=id=>api(`/api/sessions/${id}`);
const idle=id=>until(async()=>{const value=await snapshot(id);assert.notEqual(value.session.status,'error',JSON.stringify(value.events));return value.session.status==='idle'&&value;});
const exists=path=>access(path).then(()=>true,()=>false);
const config=(extra={})=>api('/api/git/config','POST',{cwd:repo,hostId,...extra});
const log=()=>readFile(join(repo,'hooks.log'),'utf8').then(text=>text.trim().split('\n'),()=>[]);
const create=async()=>{const s=await api('/api/sessions','POST',{provider:'demo',cwd:repo,hostId,workspace:{mode:'newWorktree'}});await idle(s.id);return s;};
async function start(){server=spawn(resolve('target/debug/agentworks'),[],{env,stdio:['ignore','inherit','inherit']});await until(async()=>{assert.equal(server.exitCode,null);try{return(await fetch(base+'/api/status')).ok;}catch{return false;}});}
async function stop(){if(!server)return;const exited=once(server,'exit');server.kill('SIGINT');const timer=setTimeout(()=>server.kill('SIGKILL'),8000);timer.unref();const [code]=await exited;clearTimeout(timer);server=null;assert.equal(code,0);}
const command='printf "%s:%s\\n" "$AGENTWORKS_HOOK_EVENT" "$AGENTWORKS_HOOK_REASON" >> "$AGENTWORKS_REPOSITORY/hooks.log"';
const text=`version = 1\n[future]\nlabel = "preserve me"\n[git.worktrees]\ncopy = "prefer-cow"\n[[git.hooks.enter]]\nname = "Compile commands"\ncommand = ["sh", "-c", ${JSON.stringify(command+'; printf generated > compile_commands.json')}]\ntimeout_seconds = 10\n[[git.hooks.exit]]\nname = "Shutdown"\ncommand = ["sh", "-c", ${JSON.stringify(command)}]\ntimeout_seconds = 10\n`;
try{
 await start();
 if(remote){const host=await api('/api/hosts','POST',{name:'Git remote',destination:'dev@box',command:resolve('target/debug/agentworks')});hostId=host.id;await until(async()=>(await api('/api/catalog')).hosts.find(h=>h.id===hostId)?.status==='connected');await api(`/api/hosts/${hostId}/roots`,'PUT',{roots:[dir]});}
 let settings=await api('/api/settings');assert.equal(settings.git.copy,'plain');assert.equal(settings.git.runHooks,false);
 settings=await api('/api/settings','PUT',{...settings,git:{copy:'plain',share:[],runHooks:true}});assert.equal(settings.git.runHooks,true);
 const first=await config(),saved=await config({text,fingerprint:first.fingerprint});assert.equal(saved.text,text);assert.equal(saved.effective.copy,'prefer-cow');
 await api('/api/git/config','POST',{cwd:repo,hostId,text,fingerprint:first.fingerprint},409);
 await api('/api/git/config','POST',{cwd:repo,hostId,text:'[git.worktrees]\nshare=["../escape"]',fingerprint:saved.fingerprint},400+(remote?9:0));
 const owner=await create();assert.deepEqual(await log(),['enter:create']);assert.equal(await readFile(join(owner.cwd,'compile_commands.json'),'utf8'),'generated');
 let status=await api(`/api/sessions/${owner.id}/worktree`);assert.equal(status.hooks.enter,'succeeded');assert.match(status.population,/^(cow|plain)/);
 await api(`/api/sessions/${owner.id}/suspend`,'POST');await api(`/api/sessions/${owner.id}/resume`,'POST');await idle(owner.id);assert.equal((await log()).length,1,'Resume must not rerun enter');
 const existing=await api('/api/sessions','POST',{provider:'demo',cwd:owner.cwd,hostId,workspace:{mode:'existingWorktree',path:owner.cwd}});await idle(existing.id);assert.equal((await log()).length,1);
 assert.match(await api(`/api/sessions/${owner.id}`,'PATCH',{archived:true,worktree:'keep'},409),/Another session/);
 await api(`/api/sessions/${existing.id}`,'DELETE');assert.equal((await log()).length,1,'Selected worktrees have no hooks');
 // Definitions used by an existing owner stay fixed even if the repository file changes.
 const edited=await config({text:text.replace('name = "Shutdown"','name = "Changed shutdown"').replace(JSON.stringify(command),JSON.stringify("echo unexpected-new-definition; exit 1")),fingerprint:saved.fingerprint});assert(edited.text.includes('Changed shutdown'));
 await api(`/api/sessions/${owner.id}`,'PATCH',{archived:true,worktree:'keep'});assert.deepEqual(await log(),['enter:create','exit:archive']);assert(await exists(owner.cwd));
 await api(`/api/sessions/${owner.id}`,'DELETE');assert.equal((await log()).length,2,'Delete after archive must not repeat exit');
 await config({text,fingerprint:edited.fingerprint});
 const restored=await create();await api(`/api/sessions/${restored.id}`,'PATCH',{archived:true});assert.equal(await exists(restored.cwd),false);
 await api(`/api/sessions/${restored.id}`,'PATCH',{archived:false});assert.equal((await log()).at(-1),'enter:unarchive');
 await api(`/api/sessions/${restored.id}`,'DELETE');assert.equal((await log()).at(-1),'exit:delete');assert(await exists(restored.cwd),'Delete retains checkout files');
 // Exit failure blocks archive and preserves state for deliberate retry.
 const latest=await config();const failing=text.replace(JSON.stringify(command),JSON.stringify('test -f allow-exit || { echo exit-blocked; exit 1; }; '+command));
 await config({text:failing,fingerprint:latest.fingerprint});const failed=await create();
 assert.match(await api(`/api/sessions/${failed.id}`,'PATCH',{archived:true,worktree:'keep'},409),/exit-blocked/);
 assert.equal((await snapshot(failed.id)).session.archived,false);assert(await exists(failed.cwd));assert.equal((await api(`/api/sessions/${failed.id}/worktree`)).hooks.exit,'failed');
 await writeFile(join(failed.cwd,'allow-exit'),'yes');await api(`/api/sessions/${failed.id}`,'PATCH',{archived:true,worktree:'keep'});assert.equal((await api(`/api/sessions/${failed.id}/worktree`)).hooks.exit,'succeeded');
 await stop();await start();if(remote)await until(async()=>(await api('/api/catalog')).hosts.find(h=>h.id===hostId)?.status==='connected');
 assert.equal((await api('/api/settings')).git.runHooks,true);assert.equal((await config()).text,failing);
 const count=(await log()).length;await api(`/api/sessions/${failed.id}`,'DELETE');assert.equal((await log()).length,count,'Successful exit survives restart');
 // Failed enter retains a durable session and can be retried before starting its harness.
 const beforeEnter=await config();
 const blockedEnter=text.replace(JSON.stringify(command+'; printf generated > compile_commands.json'),JSON.stringify('test -f allow-enter || { echo enter-blocked; exit 1; }; '+command));
 await config({text:blockedEnter,fingerprint:beforeEnter.fingerprint});
 const blocked=await api('/api/sessions','POST',{provider:'demo',cwd:repo,hostId,workspace:{mode:'newWorktree'}});
 await until(async()=>(await snapshot(blocked.id)).session.status==='error');
 assert.equal((await api(`/api/sessions/${blocked.id}/worktree`)).hooks.enter,'failed');
 await writeFile(join(blocked.cwd,'allow-enter'),'yes');
 await api(`/api/sessions/${blocked.id}/worktree/hooks/retry`,'POST',{event:'enter'});
 const afterRetry=(await log()).length;await api(`/api/sessions/${blocked.id}/resume`,'POST');await idle(blocked.id);assert.equal((await log()).length,afterRetry);
 await api(`/api/sessions/${blocked.id}`,'DELETE');
 // Cascading project lifecycle must use the same hooks without nesting workspace locks.
 const beforeProject=await config();await config({text,fingerprint:beforeProject.fingerprint});
 const project=await api('/api/projects','POST',{name:'Git lifecycle'});
 const child=await api('/api/sessions','POST',{provider:'demo',cwd:repo,hostId,projectId:project.id,workspace:{mode:'newWorktree'}});await idle(child.id);
 await api(`/api/projects/${project.id}/archive`,'POST',{archived:true});assert.equal((await log()).at(-1),'exit:archive');assert(await exists(child.cwd));
 await api(`/api/projects/${project.id}/archive`,'POST',{archived:false});assert.equal((await log()).at(-1),'enter:unarchive');
 await api(`/api/projects/${project.id}`,'DELETE');assert.equal((await log()).at(-1),'exit:delete');assert(await exists(child.cwd));
 console.log(`Git settings, configuration, CoW and hooks (${remote?'remote':'local'}): PASS`);
}finally{await stop();if(!process.env.AGENTWORKS_KEEP_TEST_FIXTURES)await rm(dir,{recursive:true,force:true});}
