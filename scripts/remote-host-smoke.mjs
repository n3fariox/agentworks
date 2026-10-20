// Exercises remote host links: ssh launch, WebSocket over ssh stdio,
// handshake, failure detail, remote chat sessions, drop and reconnect,
// restart, and removal. Uses
// scripts/fake-ssh.sh in place of ssh, so no SSH server or keys are needed.
import assert from 'node:assert/strict';
import {spawn,execFileSync} from 'node:child_process';
import {mkdtemp,mkdir,symlink,readdir,readlink} from 'node:fs/promises';
import {resolve,join} from 'node:path';
import {once} from 'node:events';
const dir=await mkdtemp(resolve('.cache/remote-host-'));
const port=process.env.AGENTWORKS_REMOTE_HOST_PORT||'4323',base=`http://127.0.0.1:${port}`;
const binary=resolve('target/debug/agentworks');
// A unique path makes this run's workers identifiable even after they are orphaned.
const worker=join(dir,'agentworks');await symlink(binary,worker);
let server;
const delay=ms=>new Promise(r=>setTimeout(r,ms));
async function api(path,body,method=body===undefined?'GET':'POST',code=200){const r=await fetch(base+path,{method,headers:{Origin:base,'Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body)});const text=await r.text();assert.equal(r.status,code,text);return code===200?JSON.parse(text):text;}
async function start(){
  const env={...process.env};for(const key of Object.keys(env))if(key.startsWith('AGENTWORKS_'))delete env[key];
  server=spawn(binary,[],{env:{...env,AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:port,AGENTWORKS_ROOTS:resolve('.'),AGENTWORKS_CATALOG_PATH:dir+'/catalog.json',AGENTWORKS_DB_PATH:dir+'/sessions.sqlite3',AGENTWORKS_DEMO_STATE_DIR:dir+'/demo',AGENTWORKS_SSH:resolve('scripts/fake-ssh.sh')},stdio:['ignore','inherit','inherit']});
  for(let i=0;i<80;i++){try{if((await fetch(base+'/api/status')).ok)return;}catch{}await delay(100);}throw new Error('Startup failed');
}
async function stop(){const exit=once(server,'exit');server.kill('SIGINT');await exit;server=null;}
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
  await api(`/api/hosts/${box.id}/roots`,{roots:[work]},'PUT');
  await api('/api/sessions',{provider:'demo',cwd:'/etc',hostId:box.id},'POST',403);
  await api('/api/sessions',{provider:'demo',cwd:work,hostId:box.id,workspace:{mode:'newWorktree'}},'POST',400);
  const remote=await api('/api/sessions',{provider:'demo',cwd:work,hostId:box.id,title:'Remote'});
  assert.equal(remote.hostId,box.id);
  assert.match(await prompt(remote.id,'hello'),/hello/);
  assert.equal((await agents(work)).length,1);
  // Paths on the host are never read from this server's disk.
  await api(`/api/sessions/${remote.id}/file?path=x`,undefined,'GET',409);
  await api(`/api/sessions/${remote.id}/workspace`,undefined,'GET',409);
  await api(`/api/hosts/${box.id}`,undefined,'DELETE',409);

  // A dropped link is treated like a restart: the worker exits, nothing it
  // started survives, a fresh worker connects, and the session resumes.
  const [first]=workers();process.kill(Number(first),'SIGKILL');
  await until(box.id,h=>h.status!=='connected','Drop not noticed');
  await settled(async()=>(await agents(work)).length===0,'Agent survived the dropped link');
  await until(box.id,h=>h.status==='connected','No reconnect after drop');
  assert.equal(workers().length,1);assert.notEqual(workers()[0],first);
  const dropped=await api(`/api/sessions/${remote.id}`);
  assert.equal(dropped.session.status,'suspended');
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
}
