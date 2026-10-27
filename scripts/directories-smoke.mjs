import {serverReady,stopChild,removeFixture} from '../tests/support/cleanup.mjs';
// Project directory picker API: browse and create folders beneath allowed
// roots on the local server and on a remote host (scripts/fake-ssh.sh runs the
// worker locally), and project default directories used by new sessions.
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {mkdtemp,mkdir,symlink,stat,realpath} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {resolve,join} from 'node:path';
await mkdir('.cache',{recursive:true});
const dir=await realpath(await mkdtemp(resolve('.cache/directories-')));
const outside=await realpath(await mkdtemp(join(tmpdir(),'agentworks-outside-')));
const port=process.env.AGENTWORKS_DIRECTORIES_PORT||'4353',base=`http://127.0.0.1:${port}`;
const binary=resolve('target/debug/agentworks');
const root=join(dir,'root'),remoteRoot=join(dir,'remote-root');
await mkdir(join(root,'existing'),{recursive:true});await mkdir(remoteRoot);
await symlink(outside,join(root,'escape'));
const delay=ms=>new Promise(r=>setTimeout(r,ms));
async function call(path,body,method=body===undefined?'GET':'POST'){const r=await fetch(base+path,{method,headers:{Origin:base,'Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body)});const text=await r.text();return {status:r.status,body:text.startsWith('{')?JSON.parse(text):text};}
async function api(path,body,method){const r=await call(path,body,method);assert.equal(r.status,200,JSON.stringify(r.body));return r.body;}
const env={...process.env};for(const key of Object.keys(env))if(key.startsWith('AGENTWORKS_'))delete env[key];
const server=spawn(binary,[],{env:{...env,AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:port,AGENTWORKS_ROOTS:root,AGENTWORKS_CATALOG_PATH:dir+'/catalog.json',AGENTWORKS_DB_PATH:dir+'/sessions.sqlite3',AGENTWORKS_DEMO_STATE_DIR:dir+'/demo',HOME:join(dir,'worker-home'),AGENTWORKS_SSH:resolve('scripts/fake-ssh.sh')},stdio:['ignore','inherit','inherit']});
const sessions=[];
try{
  let ready=false;for(let i=0;i<80&&!ready;i++){ready=await serverReady(server,base);if(!ready)await delay(100);}
  assert(ready,'Server startup failed');
  // Local: roots, then only contained subdirectories.
  const top=await api('/api/directories');
  assert.deepEqual(top.entries.map(e=>e.path),[root]);
  const listing=await api('/api/directories?path='+encodeURIComponent(root));
  assert.deepEqual(listing.entries.map(e=>e.name),['existing'],'escaping symlinks are not listed');
  assert.equal(listing.parent,null);
  for(const path of [outside,join(root,'escape'),root+'/../','relative','/nonexistent-agentworks'])
    assert.notEqual((await call('/api/directories?path='+encodeURIComponent(path))).status,200,path);
  // One error for missing and outside paths, so existence beyond the roots is not revealed.
  assert.deepEqual((await call('/api/directories?path='+encodeURIComponent(outside))).body,(await call('/api/directories?path=/nonexistent-agentworks')).body);
  // Create: one plain component, inside the root, never over an existing entry.
  const created=await api('/api/directories',{parent:root,name:'New thing'});
  assert.equal(created.path,join(root,'New thing'));assert((await stat(created.path)).isDirectory());
  assert.equal((await call('/api/directories',{parent:root,name:'New thing'})).status,409);
  for(const name of ['..','a/b','.hidden',' padded',''])assert.equal((await call('/api/directories',{parent:root,name})).status,400,name);
  for(const parent of [outside,join(root,'escape'),'relative'])assert.notEqual((await call('/api/directories',{parent,name:'x'})).status,200,parent);
  await assert.rejects(stat(join(outside,'x')));
  // Projects: the directory is optional, explicit, and validated before anything is saved.
  const plain=await api('/api/projects',{name:'Plain'});assert.equal(plain.defaultLocationId,undefined);
  assert.notEqual((await call('/api/projects',{name:'Escapes',defaultLocation:{hostId:'local',path:outside}})).status,200);
  assert(!(await api('/api/catalog')).projects.some(p=>p.name==='Escapes'),'a rejected directory creates no project');
  const thing=await api('/api/projects',{name:'Thing',defaultLocation:{hostId:'local',path:created.path}});
  const catalog=await api('/api/catalog');
  assert.equal(catalog.locations.find(l=>l.id===thing.defaultLocationId).path,created.path);
  assert.deepEqual(thing.locationIds,[thing.defaultLocationId]);
  assert.equal(catalog.projects.find(p=>p.id===plain.id).defaultLocationId,undefined,'other projects are unchanged');
  const renamed=await api('/api/projects/'+thing.id,{name:'Thing renamed'},'PATCH');
  assert.equal(renamed.defaultLocationId,thing.defaultLocationId,'omitting the directory keeps it');
  // Sessions without a directory start in the project directory; explicit ones still win.
  const s=await api('/api/sessions',{provider:'demo',cwd:'',projectId:thing.id,hostId:'local'});sessions.push(s.id);
  assert.equal(s.cwd,created.path);assert.equal(s.locationId,thing.defaultLocationId);
  const explicit=await api('/api/sessions',{provider:'demo',cwd:join(root,'existing'),projectId:thing.id,hostId:'local'});sessions.push(explicit.id);
  assert.equal(explicit.cwd,join(root,'existing'));
  assert.equal((await call('/api/sessions',{provider:'demo',cwd:'',projectId:plain.id,hostId:'local'})).status,400);
  const cleared=await api('/api/projects/'+thing.id,{name:'Thing renamed',defaultLocation:null},'PATCH');
  assert.equal(cleared.defaultLocationId,undefined);

  // Remote: the same operations run on the host's worker against its working directories.
  const box=await api('/api/hosts',{name:'Box',destination:'dev@box',command:binary});
  for(let i=0;i<150;i++){const h=(await api('/api/catalog')).hosts.find(h=>h.id===box.id);if(h.status==='connected'){assert.equal(h.worker.directories,true);break;}assert(i<149,'Box never connected');await delay(100);}
  assert.equal((await call('/api/directories?hostId='+box.id)).status,200);
  await api(`/api/hosts/${box.id}/roots`,{roots:[remoteRoot]},'PUT');
  const remoteTop=await api('/api/directories?hostId='+box.id);
  assert.deepEqual(remoteTop.entries.map(e=>e.path),[remoteRoot]);
  const remoteCreated=await api('/api/directories',{hostId:box.id,parent:remoteRoot,name:'remote thing'});
  assert.equal(remoteCreated.path,join(remoteRoot,'remote thing'));
  assert.deepEqual((await api(`/api/directories?hostId=${box.id}&path=${encodeURIComponent(remoteRoot)}`)).entries.map(e=>e.name),['remote thing']);
  assert.notEqual((await call('/api/directories',{hostId:box.id,parent:root,name:'x'})).status,200,'local roots do not apply on the host');
  assert.notEqual((await call(`/api/directories?hostId=${box.id}&path=${encodeURIComponent(outside)}`)).status,200);
  const remoteProject=await api('/api/projects',{name:'Remote thing',defaultLocation:{hostId:box.id,path:remoteCreated.path}});
  const remoteLocation=(await api('/api/catalog')).locations.find(l=>l.id===remoteProject.defaultLocationId);
  assert.equal(remoteLocation.hostId,box.id);assert.equal(remoteLocation.path,remoteCreated.path);
  // The project directory belongs to its host; a local session there must name its directory.
  assert.equal((await call('/api/sessions',{provider:'demo',cwd:'',projectId:remoteProject.id,hostId:'local'})).status,400);
  console.log('PASS: local and remote folder browsing/creation stay inside roots; project directories are explicit and seed new sessions');
}finally{
  for(const id of sessions)await call('/api/sessions/'+id,undefined,'DELETE');
  await stopChild(server,{signal:'SIGINT'});await removeFixture(dir);await removeFixture(outside);
}
