import {removeFixture} from '../tests/support/cleanup.mjs';
// Owns its server, catalog, SQLite and a real Git repository; no models, no UI, no keys.
import assert from 'node:assert/strict';
import {spawn, execFileSync} from 'node:child_process';
import {once} from 'node:events';
import {mkdir, mkdtemp, writeFile, readFile} from 'node:fs/promises';
import {resolve, join} from 'node:path';
import {randomUUID} from 'node:crypto';
const port=Number(process.env.AGENTWORKS_FORK_PORT||4356);
assert(port>=4351&&port<=4359,'Use an isolated test port from 4351 through 4359');
const base=`http://127.0.0.1:${port}`,headers={Origin:base,'Content-Type':'application/json'};
await mkdir('.cache',{recursive:true});
const dir=await mkdtemp(resolve('.cache/fork-')),root=join(dir,'allowed'),repo=join(root,'repo');
await mkdir(repo,{recursive:true});
const gitEnv=Object.fromEntries(Object.entries(process.env).filter(([k])=>!k.startsWith('GIT_')));
const git=(cwd,...args)=>execFileSync('git',['-c','core.hooksPath=/dev/null','-c','core.fsmonitor=false',...args],{cwd,env:gitEnv,encoding:'utf8',stdio:['ignore','pipe','pipe']}).trim();
git(repo,'init','-b','main');await writeFile(join(repo,'tracked.txt'),'committed\n');git(repo,'add','.');git(repo,'-c','user.name=Agentworks Test','-c','user.email=test@localhost','commit','-m','fixture');
const firstCommit=git(repo,'rev-parse','HEAD');
await writeFile(join(repo,'tracked.txt'),'second revision\n');git(repo,'add','.');git(repo,'-c','user.name=Agentworks Test','-c','user.email=test@localhost','commit','-m','second');
const secondCommit=git(repo,'rev-parse','HEAD');
const env={...process.env,AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:String(port),AGENTWORKS_ROOTS:root,AGENTWORKS_CATALOG_PATH:join(dir,'catalog.json'),AGENTWORKS_DB_PATH:join(dir,'sessions.sqlite3'),AGENTWORKS_DEMO_STATE_DIR:join(dir,'demo')};
let server;const delay=ms=>new Promise(r=>setTimeout(r,ms));
async function until(check){for(let i=0;i<400;i++){const v=await check();if(v)return v;await delay(50);}throw new Error('Test timed out');}
async function api(path,method='GET',body,status=200){const r=await fetch(base+path,{method,headers,body:body===undefined?undefined:JSON.stringify(path.endsWith('/fork')?{requestId:randomUUID(),...body}:body)}),text=await r.text();assert.equal(r.status,status,`${method} ${path}: ${text}`);return status===200?JSON.parse(text):text;}
async function start(){server=spawn(resolve('target/debug/agentworks'),[],{env,stdio:['ignore','inherit','inherit']});await until(async()=>{assert.equal(server.exitCode,null,'Server exited');try{return(await fetch(base+'/api/status')).ok;}catch{return false;}});}
async function stop(signal='SIGINT'){if(server.exitCode!==null){server=null;return;}const ended=once(server,'exit');server.kill(signal);const timer=setTimeout(()=>server.kill('SIGKILL'),8000);timer.unref();const[code]=await ended;clearTimeout(timer);assert.equal(code,0);server=null;}
async function idle(id,context='') {
 const last=await until(async()=>{const s=await api('/api/sessions/'+id);if(s.session.status==='error')throw new Error('Session error: '+JSON.stringify(s.events.filter(e=>e.kind==='error').map(e=>e.data)));return s.session.status==='idle'&&s;}).catch(e=>{throw new Error(`${e.message} (was: ${context})`);});
 return last;
}
const create=(body)=>api('/api/sessions','POST',{provider:'demo',cwd:repo,...body});
const prompt=(id,text)=>api(`/api/sessions/${id}/prompt`,'POST',{text,requestId:randomUUID()});
try{
 await start();
 const settings=await api('/api/settings');await api('/api/settings','PUT',{...settings,defaultCommunicationRole:'off'});

 // A session with no recorded conversation has nothing to carry over.
 const empty=await create({title:'Empty session'});
 assert.match(await api(`/api/sessions/${empty.id}/fork`,'POST',{},409),/No recorded completed turn/);
 await api(`/api/sessions/${empty.id}`,'DELETE');

 // The parent completes one turn, then a second one that is still streaming.
 // `/status` is a fixed local reply, so the carried text has no tool-like wording.
 const parent=await create({title:'Fork me'});
 await idle(parent.id);await prompt(parent.id,'/status');
 const afterFirst=await idle(parent.id);
 await prompt(parent.id,'take much longer, slow please');
 await until(async()=>((await api('/api/sessions/'+parent.id)).session.status==='running'));

 // Forking while the parent is mid-turn must not interrupt or block it.
 const forked=await api(`/api/sessions/${parent.id}/fork`,'POST',{title:'Fork of Fork me',workspace:{mode:'newWorktree'}});
 await idle(forked.id);
 const parentMid=await api('/api/sessions/'+parent.id);
 assert.equal(parentMid.session.status,'running','Forking must not interrupt a running parent');
 assert.equal(parentMid.session.fork.parentId,null,'A forked session must not become a fork of itself');
 const [parentDone,child]=await Promise.all([idle(parent.id),api('/api/sessions/'+forked.id)]);
 assert.equal(parentDone.session.title,'Fork me','The original session is preserved');
 assert.ok(parentDone.events.length>child.events.length);

 // Lineage and boundary: the running turn is excluded, the finished one is carried.
 assert.equal(child.session.fork.parentId,parent.id);
 assert.equal(child.session.fork.parentTitle,'Fork me');
 assert.equal(child.session.fork.parentCwd,repo);
 assert.equal(child.session.fork.boundaryTurn,1);
 assert.equal(child.session.fork.boundarySeq,afterFirst.events.filter(e=>e.kind==='turn_end').at(-1).seq);
 assert.equal(child.session.fork.parentCwd!==child.session.cwd,true,'The fork runs in its own worktree');

 // The child's transcript begins with the carried conversation, nothing newer.
 const marker=child.events[0];
 assert.equal(marker.kind,'fork');
 assert.equal(marker.data.parentTitle,'Fork me');
 assert.ok(child.events.slice(1).every(e=>e.seq<=child.events.at(-1).seq));
 const carriedText=child.events.filter(e=>e.kind==='user').map(e=>e.data.text);
 assert.deepEqual(carriedText,['/status'],'Only the finished turn is carried over');
 assert.ok(child.events.some(e=>e.kind==='acp'&&e.data.sessionUpdate==='agent_message_chunk'),'The reply is carried too');
 // Everything the child shows before its own first status must be conversation only.
 let prefixEnd=0;
 while(prefixEnd<child.events.length&&['fork','user','acp'].includes(child.events[prefixEnd].kind))prefixEnd++;
 assert.deepEqual([...new Set(child.events.slice(0,prefixEnd).map(e=>e.kind))].sort(),['acp','fork','user'],
   'Only conversation is carried; parent lifecycle events stay behind');
 assert.equal(child.events[prefixEnd].data.status,'starting','The child then opens its own harness');
 assert.ok(!child.events.some(e=>e.data?.requestId===parentDone.session.id));
 // The carried user event must not enter the child's own turn identity set.
 assert.equal(child.session.inputQueue.items.length,0,'Queued inputs are not copied');

 // The fork branched from the parent's position, on its own branch.
 assert.equal(child.session.cwd.startsWith(join(repo,'.worktrees')+'/'),true);
 assert.equal(git(child.session.cwd,'rev-parse','HEAD'),secondCommit);
 assert.notEqual(child.session.cwd,parentDone.session.cwd);

 // Nothing is granted by forking: no coordinator access, no continuation timer.
 const access=await api(`/api/sessions/${forked.id}/agent-access`);
 assert.deepEqual(access,{communicate:false,startSessions:false,promptSessions:false,cancelSessions:false,runAutomations:false,restartService:false,manageReviewQueue:false},'Forking grants no Agentworks access');
 assert.equal(child.session.continuation.enabled,false);
 assert.equal(child.session.continuation.parent,null);

 // The carried transcript reaches the harness with the first prompt, once.
 const beforeSend=child.events.length;
 await prompt(forked.id,'Continue from where we left off');
 const sent=await idle(forked.id,JSON.stringify(child.events.map(e=>e.kind)));
 const handoff=sent.events.slice(beforeSend).filter(e=>e.kind==='carried_context');
 assert.equal(handoff.length,1,'Carry-over is handed over exactly once');
 await prompt(forked.id,'And now?');
 const second=await idle(forked.id);
 assert.equal(second.events.filter(e=>e.kind==='carried_context').length,1,'Carry-over is not repeated on later turns');
 // The user's own message is stored verbatim; the context rides alongside it.
 assert.equal(second.events.filter(e=>e.kind==='user').map(e=>e.data.text).at(-1),'And now?');

 // A retry after a lost response returns the same session, never a second fork.
 const retryBody={title:'Retry me',workspace:{mode:'directory'},requestId:'fork-retry'};
 const [a,b]=await Promise.all([api(`/api/sessions/${parent.id}/fork`,'POST',retryBody),api(`/api/sessions/${parent.id}/fork`,'POST',retryBody)]);
 assert.equal(a.id,b.id);
 assert.equal(a.title,'Retry me');
 assert.match(await api(`/api/sessions/${parent.id}/fork`,'POST',{...retryBody,title:'Changed payload'},409),/different session payload/);

 // Retry identity is bound to the raw request, even after parent history and defaults change.
 await prompt(parent.id,'/status');await idle(parent.id);
 assert.equal((await api(`/api/sessions/${parent.id}/fork`,'POST',retryBody)).id,a.id);
 // Explicit earlier boundaries are exact completed-turn sequences, never arbitrary events.
 const earlierBody={requestId:'earlier-summary',boundarySeq:afterFirst.events.filter(e=>e.kind==='turn_end').at(-1).seq,
   context:'summary',summary:'Summary selected by the human: inspect tracked.txt relative to the ORIGINAL directory.',workspace:{mode:'directory'}};
 const earlier=await api(`/api/sessions/${parent.id}/fork`,'POST',earlierBody);
 const earlierState=await idle(earlier.id);
 assert.deepEqual(earlierState.events.filter(e=>e.kind==='user').map(e=>e.data.text),['/status']);
 assert.equal(earlierState.session.fork.context,'summary');
 assert(earlierState.events.slice(0,earlierState.events.findIndex(e=>e.kind==='status')).filter(e=>e.kind==='user'||e.kind==='acp').every(e=>e.data.forkSourceCwd===repo));
 await api(`/api/sessions/${parent.id}/fork`,'POST',{boundarySeq:afterFirst.events.find(e=>e.kind==='user').seq},409);
 await api(`/api/sessions/${parent.id}/fork`,'POST',{context:'summary',summary:''},400);
 await api(`/api/sessions/${parent.id}/fork`,'POST',{context:'hidden-clone'},400);
 // Snapshot during an approval/queued turn copies neither pending action, nor live status.
 await prompt(parent.id,'approval');
 await until(async()=>(await api('/api/sessions/'+parent.id)).approvals.length);
 await api(`/api/sessions/${parent.id}/queue`,'POST',{text:'do not copy queued input',requestId:randomUUID()});
 const waitingFork=await api(`/api/sessions/${parent.id}/fork`,'POST',{boundarySeq:earlierBody.boundarySeq});
 const waitingChild=await idle(waitingFork.id);
 assert.equal(waitingChild.approvals.length,0);assert.equal(waitingChild.session.inputQueue.items.length,0);
 assert.equal((await api('/api/sessions/'+parent.id)).approvals.length,1);
 await api(`/api/sessions/${parent.id}/stop`,'POST',{});await idle(parent.id);
 await api(`/api/sessions/${waitingFork.id}`,'DELETE');

 // A fork obeys the same provider and model rules as any new session: choosing
 // another harness does not bypass installation or authentication checks.
 await api(`/api/sessions/${parent.id}/fork`,'POST',{provider:'nope'},400);
 const uninstalled=await api('/api/status');
 const absent=uninstalled.providers.find(p=>!p.installed&&p.id!=='demo');
 if(absent)assert.match(await api(`/api/sessions/${parent.id}/fork`,'POST',{provider:absent.id},409),/missing|not installed|API key|sign in/i);
 // The fork chooses its own model rather than inheriting the parent's.
 const remodelled=await api(`/api/sessions/${parent.id}/fork`,'POST',{model:'demo/quick'});
 const ready=await idle(remodelled.id);
 assert.equal(ready.models.currentValue,'demo/quick','A fork can choose a different model');
 assert.equal(ready.session.fork.parentId,parent.id);
 await api(`/api/sessions/${remodelled.id}`,'DELETE');
 // A same-harness fork inherits the parent's model when none is chosen.
 const inherited=await create({provider:'demo',model:'demo/quick'});
 await idle(inherited.id);await prompt(inherited.id,'/status');await idle(inherited.id);
 const kept=await api(`/api/sessions/${inherited.id}/fork`,'POST',{});
 const keptReady=await idle(kept.id);
 assert.equal(keptReady.models.currentValue,'demo/quick','A same-harness fork keeps the model');
 await api(`/api/sessions/${kept.id}`,'DELETE');
 await api(`/api/sessions/${inherited.id}`,'DELETE');

 // Unknown fields are rejected rather than silently ignored.
 assert.match(await api(`/api/sessions/${parent.id}/fork`,'POST',{carriedFrom:'nope'},422),/unknown field/);

 // Deleting the parent leaves the fork's own record intact and usable.
 const survivor=await api(`/api/sessions/${parent.id}/fork`,'POST',{});
 await idle(survivor.id);await api(`/api/sessions/${parent.id}`,'DELETE');
 await idle(survivor.id);
 assert.equal((await api('/api/sessions/'+survivor.id)).session.fork.parentId,parent.id);

 // Lineage, carried transcript and pending hand-over survive a restart.
 await idle(forked.id);
 const beforeRestart=await api('/api/sessions/'+forked.id);
 await stop();await start();
 const afterRestart=await api('/api/sessions/'+forked.id);
 assert.deepEqual(afterRestart.session.fork,beforeRestart.session.fork,'Fork lineage survives a restart');
 assert.deepEqual(afterRestart.events.map(e=>e.kind).slice(0,prefixEnd),beforeRestart.events.map(e=>e.kind).slice(0,prefixEnd),
   'The carried transcript survives a restart');
 assert.equal(afterRestart.session.fork.parentCwd,repo);
 // A pending summary handoff survives restart and reaches the provider, alongside the exact user text.
 await api(`/api/sessions/${earlier.id}/resume`,'POST',{});await idle(earlier.id);
 await prompt(earlier.id,'/status');const handed=await idle(earlier.id);
 const nativeId=handed.events.filter(e=>e.kind==='provider_session').at(-1).data.id;
 const nativeState=JSON.parse(await readFile(join(env.AGENTWORKS_DEMO_STATE_DIR,nativeId+'.json'),'utf8'));
 const providerText=nativeState.history.find(e=>e.sessionUpdate==='user_message_chunk').content.text;
 assert(providerText.includes(earlierBody.summary));assert(providerText.includes(repo));assert(providerText.endsWith('/status'));
 assert.equal((await api(`/api/sessions/${parent.id}/fork`,'POST',retryBody)).id,a.id,'Deleted parent still permits an exact creation retry');

 console.log('fork smoke: ok');
} finally{
 if(server&&server.exitCode===null)await stop();
  await removeFixture(dir);
}
