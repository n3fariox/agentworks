import {serverReady,stopChild,removeFixture} from '../tests/support/cleanup.mjs';
// Exercises remote host links: ssh launch, WebSocket over ssh stdio,
// handshake, failure detail, remote chat sessions, drop and reconnect,
// restart, and removal. Uses
// scripts/fake-ssh.sh in place of ssh, so no SSH server or keys are needed.
import assert from 'node:assert/strict';
import {spawn,execFileSync} from 'node:child_process';
import {mkdtemp,mkdir,symlink,readdir,readlink,writeFile,readFile} from 'node:fs/promises';
import {resolve,join} from 'node:path';
await mkdir('.cache',{recursive:true});
const dir=await mkdtemp(resolve('.cache/remote-host-'));
const port=process.env.AGENTWORKS_REMOTE_HOST_PORT||'4323',base=`http://127.0.0.1:${port}`;
const binary=resolve('target/debug/agentworks');
// A unique path makes this run's workers identifiable even after they are orphaned.
const worker=join(dir,'agentworks');await symlink(binary,worker);
const bin=join(dir,'bin'),miseMarker=join(dir,'mise.pid');await mkdir(bin);
// A slow project environment must not block other channels or cancellation.
await writeFile(join(bin,'mise'),`#!${process.execPath}
const fs=require('node:fs');
if(process.argv.at(-1).endsWith('/slowwork')){fs.writeFileSync(${JSON.stringify(miseMarker)},String(process.pid));setTimeout(()=>console.log('{}'),10000);}
else console.log('{}');
`,{mode:0o700});
let server;
const delay=ms=>new Promise(r=>setTimeout(r,ms));
async function api(path,body,method=body===undefined?'GET':'POST',code=200){const r=await fetch(base+path,{method,headers:{Origin:base,'Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body)});const text=await r.text();assert.equal(r.status,code,text);return code===200?JSON.parse(text):text;}
async function start(){
  const env={...process.env};for(const key of Object.keys(env))if(key.startsWith('AGENTWORKS_'))delete env[key];
  server=spawn(binary,[],{env:{...env,AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:port,AGENTWORKS_ROOTS:resolve('.'),AGENTWORKS_CATALOG_PATH:dir+'/catalog.json',AGENTWORKS_DB_PATH:dir+'/sessions.sqlite3',AGENTWORKS_DEMO_STATE_DIR:dir+'/demo',HOME:join(dir,'worker-home'),PATH:bin+':'+env.PATH,AGENTWORKS_SSH:resolve('scripts/fake-ssh.sh')},stdio:['ignore','inherit','inherit']});
  for(let i=0;i<80;i++){if(await serverReady(server,base))return;await delay(100);}throw new Error('Startup failed');
}
async function stop(){await stopChild(server,{signal:'SIGINT'});server=null;}
const host=async id=>(await api('/api/catalog')).hosts.find(h=>h.id===id);
async function until(id,check,label){let last;for(let i=0;i<150;i++){last=await host(id);if(last&&check(last))return last;await delay(100);}throw new Error(`${label}: ${JSON.stringify(last)}`);}
async function idle(id){for(let i=0;i<200;i++){const s=await api(`/api/sessions/${id}`);if(s.session.status==='idle')return s;assert.notEqual(s.session.status,'error',JSON.stringify(s.events.at(-1)));await delay(100);}throw new Error('Idle timed out '+id);}
async function prompt(id,text){
  const old=await idle(id);await api(`/api/sessions/${id}/prompt`,{text,requestId:crypto.randomUUID()});
  await delay(300);const s=await idle(id);
  return s.events.filter(e=>e.seq>old.seq&&e.kind==='acp'&&e.data.sessionUpdate==='agent_message_chunk').map(e=>e.data.content.text).join('');
}
// Demo agents started for this run: identified by their working directory.
async function agents(cwd){
  const found=[];
  for(const pid of await readdir('/proc').catch(()=>[])){
    if(!/^\d+$/.test(pid))continue;
    try{if((await readlink(`/proc/${pid}/cwd`))===cwd&&execFileSync('cat',[`/proc/${pid}/cmdline`]).toString().includes('demo-agent'))found.push(pid);}catch{}
  }
  return found;
}
async function settled(check,label){for(let i=0;i<100;i++){if(await check())return;await delay(100);}throw new Error(label);}
const workers=()=>{try{return execFileSync('pgrep',['-f',`${worker} agent --stdio`]).toString().trim().split('\n').filter(Boolean);}catch{return [];}};
try{
  await start();
  await api('/api/hosts',{name:'Bad',destination:'-oProxyCommand=sh'},'POST',400);
  await api('/api/hosts',{name:'Local',destination:'box'},'POST',400);

  const box=await api('/api/hosts',{name:'Box',destination:'dev@box',command:worker});
  assert.equal(box.kind,'remote');
  const connected=await until(box.id,h=>h.status==='connected','Box never connected');
  assert.ok(connected.worker.providers.includes('demo'));
  assert.equal(connected.worker.version,'0.1.0');
  assert.equal(connected.worker.workspace,true);
  assert.equal(typeof connected.worker.tools,'object');
  assert.equal(workers().length,1);

  const down=await api('/api/hosts',{name:'Down',destination:'unreachable.example'});
  const offline=await until(down.id,h=>h.status==='offline','Unreachable host never reported offline');
  assert.match(offline.detail,/Connection refused/);
  // Shell startup chatter before the worker's ready line is skipped.
  const noisy=await api('/api/hosts',{name:'Noisy',destination:'noisy-box',command:worker});
  await until(noisy.id,h=>h.status==='connected','Noisy shell broke the handshake');
  await api(`/api/hosts/${noisy.id}`,undefined,'DELETE');
  const missing=await api('/api/hosts',{name:'Missing',destination:'dev@box',command:'/nonexistent/agentworks'});
  const absent=await until(missing.id,h=>h.status==='offline','Missing binary never reported offline');
  assert.match(absent.detail,/No such file or directory/);
  await api(`/api/hosts/${missing.id}`,undefined,'DELETE');assert.doesNotMatch(offline.detail,/ssh: ssh:/);assert.match(offline.detail,/Retrying in \d+s/);

  // Chat sessions run on the host: the worker starts the adapter there.
  const work=join(dir,'work');await mkdir(work);
  await api('/api/sessions',{provider:'demo',cwd:work,hostId:box.id},'POST',409);
  await api(`/api/hosts/${box.id}/roots`,{roots:['']},'PUT',400);
  await api(`/api/hosts/${box.id}/roots`,{roots:[work]},'PUT');
  const sibling=work+'-sibling';await mkdir(sibling);
  await symlink(sibling,join(work,'escape'));
  for(const cwd of [sibling,join(work,'escape'),join(work,'..')])await api('/api/sessions',{provider:'demo',cwd,hostId:box.id},'POST',403);
  await api('/api/sessions',{provider:'demo',cwd:'/etc',hostId:box.id},'POST',403);
  // Models use the remote ACP channel and the remote host's roots.
  const models=await api('/api/models',{provider:'demo',cwd:work,hostId:box.id});
  assert(models.options.some(m=>m.value==='demo/steady'));
  await api('/api/models',{provider:'demo',cwd:sibling,hostId:box.id},'POST',403);
  // All Git validation/mutation happens inside the worker.
  execFileSync('git',['init','-q',work]);
  await writeFile(join(work,'tracked.txt'),'remote worktree fixture\n');
  execFileSync('git',['-C',work,'add','tracked.txt']);
  execFileSync('git',['-C',work,'-c','user.name=Fixture','-c','user.email=fixture@example.test','commit','-qm','initial']);
  await api('/api/worktrees/list',{cwd:sibling,hostId:box.id},'POST',400);
  const tree=await api('/api/sessions',{provider:'demo',cwd:work,hostId:box.id,workspace:{mode:'newWorktree',branch:'remote-fixture'},requestId:'remote-worktree'});
  assert(tree.cwd.startsWith(join(work,'.worktrees')+'/'));
  assert.equal(tree.worktree.branch,'remote-fixture');
  assert.match(await prompt(tree.id,'remote worktree'),/remote worktree/);
  const trees=await api('/api/worktrees/list',{cwd:work,hostId:box.id});
  assert(trees.worktrees.some(t=>t.path===tree.cwd&&!t.isMain));
  await api('/api/sessions',{provider:'demo',cwd:work,hostId:box.id,workspace:{mode:'existingWorktree'}},'POST',409);
  await api('/api/sessions',{provider:'demo',cwd:work,hostId:box.id,workspace:{mode:'newWorktree',path:join(work,'escape','new')}},'POST',409);
  const selected=await api('/api/sessions',{provider:'demo',cwd:tree.cwd,hostId:box.id,workspace:{mode:'existingWorktree'}});
  assert.equal(selected.cwd,tree.cwd);assert.equal(selected.worktree,null);
  await idle(selected.id);await api(`/api/sessions/${selected.id}`,undefined,'DELETE');
  const ownership=await api(`/api/sessions/${tree.id}/worktree`);
  assert.equal(ownership.cleanup.allowed,true);
  await api(`/api/sessions/${tree.id}`,undefined,'DELETE');
  assert.equal(await readFile(join(tree.cwd,'tracked.txt'),'utf8'),'remote worktree fixture\n');
  // A project requiring isolation can launch remotely with its default workspace.
  const project=await api('/api/projects',{name:'Remote isolation'});
  await api(`/api/projects/${project.id}/workspace-policy`,{defaultMode:'newWorktree',requireWorktree:true},'PUT');
  const policyTree=await api('/api/sessions',{provider:'demo',cwd:work,hostId:box.id,projectId:project.id});
  assert.notEqual(policyTree.cwd,work);await idle(policyTree.id);
  await api(`/api/sessions/${policyTree.id}`,undefined,'DELETE');
  await api('/api/sessions',{provider:'demo',cwd:work,hostId:box.id,projectId:project.id,workspace:{mode:'directory'}},'POST',409);
  const remote=await api('/api/sessions',{provider:'demo',cwd:work,hostId:box.id,title:'Remote'});
  assert.equal(remote.hostId,box.id);
  assert.match(await prompt(remote.id,'hello'),/hello/);
  assert.equal((await agents(work)).length,1);
  await api(`/api/sessions/${remote.id}/diff`,undefined,'GET',409);
  // Main's permission and question handlers must both work over the relay.
  for(const [text,kind] of [['approval','permission'],['question','question']]){
    await api(`/api/sessions/${remote.id}/prompt`,{text,requestId:crypto.randomUUID()});
    let pending;
    await settled(async()=>{pending=(await api(`/api/sessions/${remote.id}`)).approvals.find(a=>a.kind===kind);return pending;},'Remote '+kind+' missing');
    if(kind==='permission')await api(`/api/sessions/${remote.id}/approvals/${pending.id}`,{optionId:'allow'});
    else await api(`/api/sessions/${remote.id}/questions/${pending.id}`,{action:'decline',submissionId:crypto.randomUUID()});
    await idle(remote.id);
  }
  // Host-scoped locations stay distinct even when their path strings match.
  const local=await api('/api/sessions',{provider:'demo',cwd:work,title:'Local boundary control'});
  assert.notEqual(local.locationId,remote.locationId);await idle(local.id);
  await api(`/api/sessions/${local.id}`,undefined,'DELETE');
  await api(`/api/sessions/${remote.id}/suspend`,{});
  await settled(async()=>(await agents(work)).length===0,'Suspend left remote agent alive');
  await api(`/api/sessions/${remote.id}/resume`,{});await idle(remote.id);
  const slowwork=join(work,'slowwork');await mkdir(slowwork);
  const slow=await api('/api/sessions',{provider:'demo',cwd:slowwork,hostId:box.id,title:'Slow environment'});
  await settled(async()=>readFile(miseMarker,'utf8').catch(()=>false),'mise did not start');
  const beforeFastPrompt=Date.now();
  assert.match(await prompt(remote.id,'independent channel'),/independent channel/);
  assert(Date.now()-beforeFastPrompt<8000,'Slow environment blocked another channel');
  await api(`/api/sessions/${slow.id}/suspend`,{});
  const misePid=Number(await readFile(miseMarker,'utf8'));
  await settled(async()=>{try{process.kill(misePid,0);return false;}catch{return true;}},'Cancelled mise process survived');
  await api(`/api/sessions/${slow.id}`,undefined,'DELETE');
  const other=await api('/api/hosts',{name:'Other',destination:'dev@other',command:worker});
  await until(other.id,h=>h.status==='connected','Second host did not connect');
  await api(`/api/hosts/${other.id}/roots`,{roots:[sibling]},'PUT');
  await api('/api/sessions',{provider:'demo',cwd:work,hostId:other.id},'POST',403);
  const otherSession=await api('/api/sessions',{provider:'demo',cwd:sibling,hostId:other.id});
  assert.equal(otherSession.cwd,sibling);assert.equal(otherSession.hostId,other.id);
  assert.match(await prompt(otherSession.id,'second host'),/second host/);
  await api(`/api/sessions/${otherSession.id}`,undefined,'DELETE');
  await api(`/api/hosts/${other.id}`,undefined,'DELETE');
  await settled(async()=>(await agents(sibling)).length===0&&workers().length===1,'Second host leaked processes');
  // Paths on the host are never read from this server's disk.
  await api(`/api/sessions/${remote.id}/file?path=x`,undefined,'GET',409);
  await api(`/api/sessions/${remote.id}/workspace`,undefined,'GET',409);
  await api(`/api/hosts/${box.id}`,undefined,'DELETE',409);

  // A dropped link is treated like a restart: the worker exits, nothing it
  // started survives, a fresh worker connects, and the session resumes.
  const interruptedRequest=crypto.randomUUID();
  await api(`/api/sessions/${remote.id}/prompt`,{text:'approval during link loss',requestId:interruptedRequest});
  await settled(async()=>(await api(`/api/sessions/${remote.id}`)).approvals.length===1,'Turn did not reach approval');
  const [first]=workers();process.kill(Number(first),'SIGKILL');
  await until(box.id,h=>h.status!=='connected','Drop not noticed');
  await settled(async()=>(await agents(work)).length===0,'Agent survived the dropped link');
  await until(box.id,h=>h.status==='connected','No reconnect after drop');
  assert.equal(workers().length,1);assert.notEqual(workers()[0],first);
  const dropped=await api(`/api/sessions/${remote.id}`);
  assert.equal(dropped.session.status,'suspended');
  assert.equal(dropped.approvals.length,0);
  assert.equal(dropped.events.filter(e=>e.kind==='user'&&e.data.requestId===interruptedRequest).length,1);
  assert.equal(dropped.session.continuation.enabled,false);
  assert.match(dropped.events.filter(e=>e.kind==='interrupted').at(-1).data.message,/host dropped/);
  await api(`/api/sessions/${remote.id}/resume`,{});
  assert.match(await prompt(remote.id,'after the drop'),/after the drop/);

  const before=workers()[0];await api(`/api/hosts/${box.id}/reconnect`,{});
  await until(box.id,h=>h.status==='connected'&&workers()[0]!==before,'Reconnect did not replace the worker');
  assert.equal(workers().length,1);

  await stop();
  await delay(300);assert.deepEqual(workers(),[],'Workers survived server shutdown');
  await start();
  await until(box.id,h=>h.status==='connected','Box did not reconnect after server restart');
  await api(`/api/sessions/${remote.id}/resume`,{});
  assert.match(await prompt(remote.id,'after the restart'),/after the restart/);
  await api(`/api/sessions/${remote.id}`,undefined,'DELETE');
  await settled(async()=>(await agents(work)).length===0,'Agent survived session deletion');

  await api(`/api/hosts/${down.id}`,undefined,'DELETE');
  await api(`/api/hosts/${box.id}`,undefined,'DELETE');
  assert.equal((await api('/api/catalog')).hosts.length,1);
  await delay(300);assert.deepEqual(workers(),[],'Worker survived host removal');
  console.log('Remote host smoke passed');
}finally{
  if(server)await stop();
  await removeFixture(dir);
}
