import {removeFixture} from '../tests/support/cleanup.mjs';
// Owns its server, catalog and SQLite; uses the demo agent only. No models, UI or keys.
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {once} from 'node:events';
import {mkdir, mkdtemp} from 'node:fs/promises';
import {resolve, join} from 'node:path';
import {randomUUID} from 'node:crypto';
const port=Number(process.env.AGENTWORKS_ROLES_PORT||4357);
assert(port>=4351&&port<=4359,'Use an isolated test port from 4351 through 4359');
const base=`http://127.0.0.1:${port}`,headers={Origin:base,'Content-Type':'application/json'};
await mkdir('.cache',{recursive:true});
const dir=await mkdtemp(resolve('.cache/roles-')),root=join(dir,'allowed');
await mkdir(root,{recursive:true});
const env={...process.env,AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:String(port),AGENTWORKS_ROOTS:root,AGENTWORKS_CATALOG_PATH:join(dir,'catalog.json'),AGENTWORKS_DB_PATH:join(dir,'sessions.sqlite3'),AGENTWORKS_DEMO_STATE_DIR:join(dir,'demo')};
let server;const delay=ms=>new Promise(r=>setTimeout(r,ms));
async function until(check){for(let i=0;i<400;i++){const v=await check();if(v)return v;await delay(50);}throw new Error('Test timed out');}
async function api(path,method='GET',body,status=200){const r=await fetch(base+path,{method,headers,body:body===undefined?undefined:JSON.stringify(body)}),text=await r.text();assert.equal(r.status,status,`${method} ${path}: ${text}`);return status===200?JSON.parse(text):text;}
async function start(){server=spawn(resolve('target/debug/agentworks'),[],{env,stdio:['ignore','inherit','inherit']});await until(async()=>{assert.equal(server.exitCode,null,'Server exited');try{return(await fetch(base+'/api/status')).ok;}catch{return false;}});}
async function stop(){if(server.exitCode!==null){server=null;return;}const ended=once(server,'exit');server.kill('SIGINT');const timer=setTimeout(()=>server.kill('SIGKILL'),8000);timer.unref();const[code]=await ended;clearTimeout(timer);assert.equal(code,0);server=null;}
const idle=id=>until(async()=>{const s=await api('/api/sessions/'+id);if(s.session.status==='error')throw new Error('Session error: '+JSON.stringify(s.events.filter(e=>e.kind==='error').map(e=>e.data)));return s.session.status==='idle'&&s;});
const create=body=>api('/api/sessions','POST',{provider:'demo',cwd:root,...body});
// The demo agent answers `role?` with the system prompt its current connection received.
async function received(id){
  const before=(await idle(id)).seq;
  await api(`/api/sessions/${id}/prompt`,'POST',{text:'role?',requestId:randomUUID()});
  const s=await until(async()=>{const s=await idle(id);return s.seq>before&&s.events.some(e=>e.seq>before&&e.kind==='acp'&&e.data.sessionUpdate==='agent_message_chunk')&&s;});
  return s.events.filter(e=>e.seq>before&&e.kind==='acp'&&e.data.sessionUpdate==='agent_message_chunk').map(e=>e.data.content.text).join('');
}
try{
  await start();
  const library=await api('/api/roles');
  assert.deepEqual(library.filter(r=>r.builtin).map(r=>r.id),['coordinator','implementer','reviewer','researcher']);
  assert.match(await api('/api/roles/reviewer','PUT',{name:'Mine',instructions:'x'},409),/Built-in roles cannot be changed/);
  assert.match(await api('/api/roles/reviewer','DELETE',undefined,409),/Built-in/);
  await api('/api/roles','POST',{name:' ',instructions:'x'},400);
  await api('/api/roles','POST',{name:'x',instructions:'x',builtin:true},422);
  const custom=await api('/api/roles','POST',{name:'Release captain',description:'Ships it',instructions:'Always tag the release.'});
  assert.equal(custom.builtin,false);

  // Creation snapshots the role and the harness receives it.
  const plain=await create({title:'No role'});assert.equal(plain.role,null);
  const shared=await received(plain.id);
  assert.match(shared,new RegExp('agentworks://sessions/'+plain.id));
  assert.match(shared,/Use the Agentworks MCP tools/);
  assert.doesNotMatch(shared,/Agentworks role:/);
  const s=await create({title:'Captain',role:custom.id});
  assert.equal(s.role.name,'Release captain');
  let text=await received(s.id);
  assert.match(text,/Use the Agentworks MCP tools/);
  assert.match(text,/Agentworks role: Release captain/);assert.match(text,/Always tag the release\./);

  // Role validation happens before anything is created.
  assert.match(await api('/api/sessions','POST',{provider:'demo',cwd:root,role:'missing'},400),/Role not found/);
  assert.match(await api('/api/sessions','POST',{provider:'codex',cwd:root,role:'reviewer'},400),/does not accept role instructions/);

  // Editing the library leaves the running session's snapshot alone, across resume and restart.
  await api(`/api/roles/${custom.id}`,'PUT',{name:'Release captain',instructions:'Never tag anything.'});
  await api(`/api/sessions/${s.id}/suspend`,'POST',{});await until(async()=>(await api('/api/sessions/'+s.id)).session.status==='suspended');
  await api(`/api/sessions/${s.id}/resume`,'POST',{});
  text=await received(s.id);assert.match(text,/Always tag the release\./);assert.doesNotMatch(text,/Never tag/);
  await stop();await start();
  await api(`/api/sessions/${s.id}/resume`,'POST',{}).catch(()=>{});
  text=await received(s.id);assert.match(text,/Always tag the release\./,'A restarted server re-sends the stored role');

  // A fork carries its parent's snapshot, not the edited library text.
  const fork=await api(`/api/sessions/${s.id}/fork`,'POST',{requestId:randomUUID(),title:'Captain fork'});
  assert.equal(fork.role.hash,s.role.hash);
  // The first prompt includes carried transcript context, which the demo echoes.
  // Query again to inspect the actual system prompt rather than echoed parent text.
  await received(fork.id);
  const forkText=await received(fork.id);
  assert.match(forkText,/Always tag the release\./);
  assert.match(forkText,new RegExp('agentworks://sessions/'+fork.id));
  assert.doesNotMatch(forkText,new RegExp('agentworks://sessions/'+s.id));

  // Forks cannot silently drop the parent's role on unsupported providers.
  const count=(await api('/api/sessions')).length;
  assert.match(await api(`/api/sessions/${s.id}/fork`,'POST',{requestId:randomUUID(),provider:'codex'},400),/Forks must keep their parent's role/);
  assert.equal((await api('/api/sessions')).length,count);

  // A role never grants Agentworks access.
  const settings=await api('/api/settings');await api('/api/settings','PUT',{...settings,defaultCommunicationRole:'participant'});
  const coord=await create({title:'Coordinator role',role:'coordinator'});
  const access=await api(`/api/sessions/${coord.id}/agent-access`);
  assert.equal(access.communicate,true,JSON.stringify(access));assert.equal(access.startSessions,false,JSON.stringify(access));

  // Deleting a custom role leaves sessions created from it intact.
  await api(`/api/roles/${custom.id}`,'DELETE');
  assert(!(await api('/api/roles')).some(r=>r.id===custom.id));
  assert.equal((await api('/api/sessions/'+s.id)).session.role.name,'Release captain');
  console.log('PASS: role library, validation, creation snapshot, resume/restart re-send, fork inheritance, no implied grants');
}finally{
  if(server)await stop().catch(()=>server?.kill('SIGKILL'));
  await removeFixture(dir);
}
