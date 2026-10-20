// Exercises remote host links: ssh launch, WebSocket over ssh stdio,
// handshake, failure detail, drop and reconnect, restart, and removal. Uses
// scripts/fake-ssh.sh in place of ssh, so no SSH server or keys are needed.
import assert from 'node:assert/strict';
import {spawn,execFileSync} from 'node:child_process';
import {mkdtemp,symlink} from 'node:fs/promises';
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

  // A dropped link is treated like a restart: the worker exits and a fresh one connects.
  const [first]=workers();process.kill(Number(first),'SIGKILL');
  await until(box.id,h=>h.status!=='connected','Drop not noticed');
  await until(box.id,h=>h.status==='connected','No reconnect after drop');
  assert.equal(workers().length,1);assert.notEqual(workers()[0],first);

  const before=workers()[0];await api(`/api/hosts/${box.id}/reconnect`,{});
  await until(box.id,h=>h.status==='connected'&&workers()[0]!==before,'Reconnect did not replace the worker');
  assert.equal(workers().length,1);

  // Sessions still only run locally.
  await api('/api/sessions',{provider:'demo',cwd:resolve('.'),title:'Remote',hostId:box.id},'POST',400);

  await stop();
  await delay(300);assert.deepEqual(workers(),[],'Workers survived server shutdown');
  await start();
  await until(box.id,h=>h.status==='connected','Box did not reconnect after server restart');

  await api(`/api/hosts/${down.id}`,undefined,'DELETE');
  await api(`/api/hosts/${box.id}`,undefined,'DELETE');
  assert.equal((await api('/api/catalog')).hosts.length,1);
  await delay(300);assert.deepEqual(workers(),[],'Worker survived host removal');
  console.log('Remote host smoke passed');
}finally{
  if(server)await stop();
}
