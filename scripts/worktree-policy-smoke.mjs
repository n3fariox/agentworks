import {removeFixture} from '../tests/support/cleanup.mjs';
// Owns its server, catalog, SQLite, demo state and real Git fixtures; no models or UI.
import assert from 'node:assert/strict';
import {spawn, execFileSync} from 'node:child_process';
import {once} from 'node:events';
import {mkdir, mkdtemp, writeFile, readFile, access, symlink, chmod, unlink, rename} from 'node:fs/promises';
import {resolve, join, dirname} from 'node:path';
import {randomUUID} from 'node:crypto';
const port=Number(process.env.AGENTWORKS_POLICY_PORT||4351);
assert(port>=4351&&port<=4355,'Use an isolated test port from 4351 through 4355');
const base=`http://127.0.0.1:${port}`,headers={Origin:base,'Content-Type':'application/json'};
await mkdir('.cache',{recursive:true});
const dir=await mkdtemp(resolve('.cache/worktree-policy-')),root=join(dir,'allowed'),repo=join(root,'repo'),plain=join(root,'plain'),outside=join(dir,'outside');
await Promise.all([mkdir(repo,{recursive:true}),mkdir(plain,{recursive:true}),mkdir(outside,{recursive:true})]);
const gitEnv=Object.fromEntries(Object.entries(process.env).filter(([k])=>!k.startsWith('GIT_')));
const git=(cwd,...args)=>execFileSync('git',['-c','core.hooksPath=/dev/null','-c','core.fsmonitor=false',...args],{cwd,env:gitEnv,encoding:'utf8',stdio:['ignore','pipe','pipe']}).trim();
git(repo,'init','-b','main');await mkdir(join(repo,'nested'));await writeFile(join(repo,'tracked.txt'),'committed\n');await writeFile(join(repo,'nested/keep.txt'),'nested\n');git(repo,'add','.');git(repo,'-c','user.name=Agentworks Test','-c','user.email=test@localhost','commit','-m','fixture');
const committed=git(repo,'rev-parse','HEAD');await writeFile(join(repo,'tracked.txt'),'dirty\n');await writeFile(join(repo,'credentials-untracked'),'not copied');
// Fault injection belongs to this isolated server's PATH, never the real checkout.
const bin=join(dir,'bin'),gate=join(dir,'git-gate');await mkdir(bin);
const realGit=execFileSync('which',['git'],{encoding:'utf8'}).trim();
await writeFile(join(bin,'git'),`#!${process.execPath}
const fs=require('node:fs'),cp=require('node:child_process'),a=process.argv.slice(2),gate=process.env.AGENTWORKS_POLICY_GIT_GATE;
if(fs.existsSync(gate+'.overflow')&&a.includes('list')){process.stdout.write('x'.repeat(300000));process.exitCode=0;}
else if(fs.existsSync(gate+'.inspection')&&a.includes('--show-toplevel')){process.stdout.write('x'.repeat(20000));process.exitCode=0;}
else {
 if(a.includes('worktree')&&a.includes('add')&&fs.existsSync(gate)){
  fs.writeFileSync(gate+'.entered','yes');const deadline=Date.now()+30000;
  while(fs.existsSync(gate)&&Date.now()<deadline)Atomics.wait(new Int32Array(new SharedArrayBuffer(4)),0,0,30);
 }
 const r=cp.spawnSync(${JSON.stringify(realGit)},a,{stdio:'inherit',env:process.env});process.exitCode=r.status??1;
}
`);await chmod(join(bin,'git'),0o755);
const env={...process.env,PATH:bin+':'+process.env.PATH,AGENTWORKS_POLICY_GIT_GATE:gate,AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:String(port),AGENTWORKS_ROOTS:root,AGENTWORKS_CATALOG_PATH:join(dir,'catalog.json'),AGENTWORKS_DB_PATH:join(dir,'sessions.sqlite3'),AGENTWORKS_DEMO_STATE_DIR:join(dir,'demo'),GIT_DIR:'/nonexistent/inherited-override',GIT_WORK_TREE:'/nonexistent/inherited-worktree',GIT_CONFIG_COUNT:'1',GIT_CONFIG_KEY_0:'core.hooksPath',GIT_CONFIG_VALUE_0:'/nonexistent/hooks'};
let server;const delay=ms=>new Promise(r=>setTimeout(r,ms));
async function until(check){for(let i=0;i<300;i++){const v=await check();if(v)return v;await delay(50);}throw new Error('Test timed out');}
async function api(path,method='GET',body,status=200){const r=await fetch(base+path,{method,headers,body:body===undefined?undefined:JSON.stringify(body)}),text=await r.text();assert.equal(r.status,status,`${method} ${path}: ${text}`);return status===200?JSON.parse(text):text;}
async function start(){server=spawn(resolve('target/debug/agentworks'),[],{env,stdio:['ignore','inherit','inherit']});await until(async()=>{assert.equal(server.exitCode,null,'Server exited');try{return(await fetch(base+'/api/status')).ok;}catch{return false;}});}
async function stop(signal='SIGINT'){if(server.exitCode!==null){server=null;return;}const ended=once(server,'exit');server.kill(signal);const timer=setTimeout(()=>server.kill('SIGKILL'),8000);timer.unref();const [code]=await ended;clearTimeout(timer);if(signal==='SIGINT')assert.equal(code,0);server=null;}
async function idle(id){return until(async()=>{const s=await api('/api/sessions/'+id);assert.notEqual(s.session.status,'error',JSON.stringify(s.events.filter(e=>e.kind==='error')));return s.session.status==='idle'&&s;});}
const create=body=>api('/api/sessions','POST',{provider:'demo',cwd:repo,...body});
async function remove(s){await api('/api/sessions/'+s.id,'DELETE');}
async function call(id,name,args){await idle(id);const old=await api('/api/sessions/'+id);await api('/api/sessions/'+id+'/prompt','POST',{text:`mcp ${name} ${JSON.stringify(args)}`,requestId:randomUUID()});const s=await idle(id);const result=s.events.filter(e=>e.seq>old.seq&&e.kind==='acp'&&e.data.sessionUpdate==='tool_call_update').at(-1)?.data.rawOutput;assert(result,`Missing MCP output ${name}`);return {result,value:JSON.parse(result.content[0].text)};}
const treeCount=()=>git(repo,'worktree','list','--porcelain').split('\n').filter(s=>s.startsWith('worktree ')).length;
async function external(cwd){const id=randomUUID();await mkdir(env.AGENTWORKS_DEMO_STATE_DIR,{recursive:true});await writeFile(join(env.AGENTWORKS_DEMO_STATE_DIR,id+'.json'),JSON.stringify({cwd,turn:1,model:'demo/steady',history:[]}));return {provider:'demo',providerSessionId:id,cwd};}
try{
 await start();
 const p=await api('/api/projects','POST',{name:'Policy fixture'}),policy='/api/projects/'+p.id+'/workspace-policy';
 assert.deepEqual(await api(policy),{defaultMode:'directory',requireWorktree:false});
 // Paused Git proves activity observes preparation before a session exists.
 await writeFile(gate,'hold');const inFlight=create({requestId:'activity-preparation',workspace:{mode:'newWorktree'}});
 await until(async()=>{try{await access(gate+'.entered');return true;}catch{return false;}});
 const busy=await api('/api/activity');assert.equal(busy.workspacePreparing,true);assert.equal(busy.safeToClose,false);
 await unlink(gate);const prepared=await inFlight;await idle(prepared.id);await remove(prepared);await unlink(gate+'.entered');assert.equal((await api('/api/activity')).workspacePreparing,false);
 // Timeout after mutation is armed retains the reserved directory/key and releases activity.
 await writeFile(gate,'timeout');const timeoutRequest={provider:'demo',cwd:repo,requestId:'timeout-preparation',workspace:{mode:'newWorktree'}};
 const timeoutMessage=await api('/api/sessions','POST',timeoutRequest,409);assert.match(timeoutMessage,/timed out/);assert.equal((await api('/api/activity')).workspacePreparing,false);await unlink(gate);await unlink(gate+'.entered');await api('/api/sessions','POST',timeoutRequest,409);
 await writeFile(gate+'.overflow','large');await api('/api/worktrees/list','POST',{cwd:repo},409);await unlink(gate+'.overflow');
 const ordinary=await create({cwd:plain});await idle(ordinary.id);assert.equal(ordinary.cwd,plain);await remove(ordinary);
 // Capacity rejection is mutation-free: retain payload binding, permit unchanged retry.
 const fillers=[];for(let i=0;i<12;i++)fillers.push(await create({cwd:plain}));
 await Promise.all(fillers.map(s=>idle(s.id)));
 const capacityRequest={provider:'demo',cwd:plain,requestId:'capacity-retry'};
 await api('/api/sessions','POST',capacityRequest,409);
 await api('/api/sessions','POST',{...capacityRequest,title:'different payload'},409);
 await remove(fillers.shift());const recovered=await api('/api/sessions','POST',capacityRequest);await idle(recovered.id);await remove(recovered);for(const s of fillers)await remove(s);
 const actor=await create({projectId:p.id,title:'Isolated test coordinator',agentAccess:{startSessions:true,runAutomations:true}});await idle(actor.id);
 const oldImportRequest={...await external(repo),projectId:p.id};const oldImport=await api('/api/sessions/import','POST',oldImportRequest);await idle(oldImport.id);
 const input={provider:'demo',cwd:repo,projectId:p.id,requestId:'tree-once',workspace:{mode:'newWorktree'}};
 const [first,duplicate]=await Promise.all([api('/api/sessions','POST',input),api('/api/sessions','POST',input)]);assert.equal(first.id,duplicate.id);assert.equal(first.cwd.startsWith(join(repo,'.worktrees')+'/'),true);await idle(first.id);
 assert.equal(await readFile(join(first.cwd,'tracked.txt'),'utf8'),'committed\n');await assert.rejects(access(join(first.cwd,'credentials-untracked')));assert.equal(git(first.cwd,'rev-parse','HEAD'),committed);assert(git(first.cwd,'branch','--show-current').startsWith('agentworks/'));
 // A worktree started from a linked worktree goes beside it under the main checkout, not nested inside it.
 const sibling=await create({cwd:first.cwd,projectId:p.id,workspace:{mode:'newWorktree'}});await idle(sibling.id);assert.equal(dirname(sibling.cwd),join(repo,'.worktrees'));await remove(sibling);
 await api('/api/sessions','POST',{...input,title:'changed'},409);
 const unique=await create({workspace:{mode:'newWorktree'}});assert.notEqual(unique.cwd,first.cwd);await remove(unique);await access(unique.cwd);
 const customPath=join(root,'custom');const custom=await create({workspace:{mode:'newWorktree',branch:'custom-policy-test',baseRef:'main',path:customPath}});await idle(custom.id);assert.equal(custom.cwd,customPath);assert.equal(git(customPath,'branch','--show-current'),'custom-policy-test');await remove(custom);
 const failCreate=(workspace,status=400,cwd=repo)=>api('/api/sessions','POST',{provider:'demo',cwd,workspace},status);
 for(const branch of ['--force','bad..branch','bad\nbranch','@{-1}'])await failCreate({mode:'newWorktree',branch});
 for(const baseRef of ['--upload-pack=oops','missing-ref','HEAD\n'])await failCreate({mode:'newWorktree',baseRef});
 await failCreate({mode:'newWorktree',branch:'custom-policy-test'});
 const empty=join(root,'empty-collision');await mkdir(empty);await failCreate({mode:'newWorktree',path:empty},409);
 const safeRetry={provider:'demo',cwd:repo,requestId:'destination-preflight-retry',workspace:{mode:'newWorktree',path:empty}};
 await api('/api/sessions','POST',safeRetry,409);await rename(empty,empty+'-retained');const recoveredTree=await api('/api/sessions','POST',safeRetry);await idle(recoveredTree.id);await remove(recoveredTree);await access(empty+'-retained');
 const collision=join(root,'collision');await mkdir(collision);await writeFile(join(collision,'dirty.txt'),'keep me');await failCreate({mode:'newWorktree',path:collision},409);assert.equal(await readFile(join(collision,'dirty.txt'),'utf8'),'keep me');
 await failCreate({mode:'newWorktree',path:join(outside,'escape')},403);
 const link=join(root,'link');await symlink(outside,link);await failCreate({mode:'newWorktree',path:join(link,'escape')},403);
 await failCreate({mode:'newWorktree',path:repo+'/../escape'});
 await failCreate({mode:'newWorktree'},400,plain);await failCreate({mode:'existingWorktree'});await failCreate({mode:'existingWorktree'},400,plain);
 const linked=join(root,'linked');git(repo,'worktree','add','-b','linked-policy-test',linked,'HEAD');git(repo,'worktree','lock',linked);
 const detached=join(root,'detached');git(repo,'worktree','add','--detach',detached,'HEAD');
 const hidden=join(outside,'hidden');git(repo,'worktree','add','-b','hidden-policy-test',hidden,'HEAD');
 const listed=await api('/api/worktrees/list','POST',{cwd:repo});assert.equal(listed.worktrees.find(w=>w.path===repo).isMain,true);assert.equal(listed.worktrees.find(w=>w.path===linked).locked,true);assert.equal(listed.worktrees.find(w=>w.path===detached).branch,null);assert(!listed.worktrees.some(w=>w.path===hidden));assert.equal(listed.worktrees.find(w=>w.path===first.cwd).commit,committed);
 const nested=await create({cwd:join(linked,'nested'),workspace:{mode:'existingWorktree',path:linked}});await idle(nested.id);assert.equal(nested.cwd,join(linked,'nested'));await remove(nested);
 await failCreate({mode:'existingWorktree',path:first.cwd},400,linked);
 await api(policy,'PUT',{defaultMode:'directory',requireWorktree:true},400);
 await api(policy,'PUT',{defaultMode:'newWorktree',requireWorktree:true});
 await api('/api/sessions','POST',{provider:'demo',cwd:repo,projectId:p.id,workspace:{mode:'directory'}},400);
 await api('/api/sessions','POST',{provider:'demo',cwd:plain,projectId:p.id},400);
 const linkedDirectory=await create({cwd:linked,projectId:p.id,workspace:{mode:'directory'}});await idle(linkedDirectory.id);await remove(linkedDirectory);
 const automatic=await create({projectId:p.id});await idle(automatic.id);assert.notEqual(automatic.cwd,repo);await remove(automatic);
 await api('/api/sessions/import','POST',{...await external(repo),projectId:p.id},400);
 const imported=await api('/api/sessions/import','POST',{...await external(linked),projectId:p.id});await idle(imported.id);assert.equal(imported.cwd,linked);await remove(imported);
 await api('/api/sessions/import','POST',{...await external(repo),cwd:linked,projectId:p.id},400);
 assert.equal((await api('/api/sessions/import','POST',oldImportRequest)).id,oldImport.id);
 const denied=await call(actor.id,'start_session',{provider:'demo',cwd:repo,workspace:{mode:'directory'},requestId:'denied-directory'});assert(denied.result.isError);assert.equal(denied.value.status,400);
 const mcpInput={provider:'demo',cwd:repo,workspace:{mode:'newWorktree'},requestId:'mcp-tree'};const child=(await call(actor.id,'start_session',mcpInput));assert(!child.result.isError);assert.notEqual(child.value.cwd,repo);await idle(child.value.id);assert.equal((await call(actor.id,'start_session',mcpInput)).value.id,child.value.id);await remove(child.value);
 const def=await api('/api/automations','POST',{name:'Policy automation',provider:'demo',cwd:repo,projectId:p.id,prompt:'local test',enabled:true});
 const runRequest={requestId:'run-once'};const run=await api('/api/automations/'+def.id+'/run','POST',runRequest);await idle(run.session.id);assert.notEqual(run.session.cwd,repo);assert.equal((await api('/api/automations/'+def.id+'/run','POST',runRequest)).session.id,run.session.id);
 await api(policy,'PUT',{defaultMode:'existingWorktree',requireWorktree:true});await api('/api/automations/'+def.id+'/run','POST',{requestId:'requires-linked'},400);
 await api('/api/sessions','POST',{provider:'demo',cwd:repo,projectId:p.id},400);
 const defaultExisting=await create({cwd:join(linked,'nested'),projectId:p.id});await idle(defaultExisting.id);await remove(defaultExisting);
 await api(policy,'PUT',{defaultMode:'newWorktree',requireWorktree:true});
 const failedRequest={provider:'demo',cwd:repo,requestId:'failed-reservation',workspace:{mode:'newWorktree',path:collision}};await api('/api/sessions','POST',failedRequest,409);const before=treeCount();
 // Crash/restart: successful and failed request reservations and policies remain durable.
 await stop('SIGKILL');await start();assert.equal((await api(policy)).requireWorktree,true);
 assert.equal((await api('/api/sessions','POST',input)).id,first.id);assert.equal(treeCount(),before);
 await api('/api/sessions','POST',failedRequest,409);assert.equal(treeCount(),before);
 assert.equal((await api('/api/automations/'+def.id+'/run','POST',runRequest)).session.id,run.session.id);
 // Existing registered directory sessions resume without relocation under a requiring policy.
 await api('/api/sessions/'+actor.id+'/resume','POST',{});await idle(actor.id);assert.equal((await api('/api/sessions/'+actor.id)).session.cwd,repo);
 await api('/api/sessions/'+oldImport.id+'/resume','POST',{});await idle(oldImport.id);assert.equal((await api('/api/sessions/import','POST',oldImportRequest)).id,oldImport.id);
 await writeFile(gate+'.inspection','large');const capped=await api('/api/sessions/'+first.id+'/workspace');assert.equal(capped.status,'unavailable');assert.match(capped.message,/limit/);await unlink(gate+'.inspection');
 const info=await api('/api/sessions/'+first.id+'/workspace');assert.equal(info.linkedWorktree,true);assert.equal(info.cwd,first.cwd);
 // Session deletion never owns branch/file cleanup, including dirty work.
 await writeFile(join(first.cwd,'keep-dirty'),'retained');await remove(first);assert.equal(await readFile(join(first.cwd,'keep-dirty'),'utf8'),'retained');assert(git(repo,'branch','--list',git(first.cwd,'branch','--show-current')));
 await api('/api/sessions','POST',input,410);assert.equal(treeCount(),before);
 await api('/api/settings','PUT',{roots:[plain],defaultModels:{}});await api('/api/worktrees/list','POST',{cwd:repo},403);await api('/api/sessions','POST',{provider:'demo',cwd:repo,workspace:{mode:'newWorktree'}},403);
 console.log('PASS: activity during preparation, Git timeout and output caps; directory compatibility; real committed/unique/custom/existing/nested worktrees; policy API/MCP/automation/import/resume; refs/branches/collisions/roots/symlinks; retained dirty files; concurrent, mutation-free retry, failed and restart-persistent request IDs; session deletion retains branch/tree');
 console.log('Isolated fixture root:',dir);
}finally{if(server)await stop();await removeFixture(dir);}
