import {serverReady,stopChild,removeFixture} from '../tests/support/cleanup.mjs';
// Server-owned workflows over the existing remote ACP relay. No paid inference.
import assert from 'node:assert/strict';
import {spawn,execFileSync} from 'node:child_process';
import {mkdtemp,mkdir,writeFile,readFile} from 'node:fs/promises';
import {join,resolve} from 'node:path';
await mkdir('.cache',{recursive:true});
const dir=await mkdtemp(resolve('.cache/remote-plumbing-'));
const local=join(dir,'local'),root=join(dir,'remote'),repo=join(root,'repo'),outside=join(dir,'outside'),bin=join(dir,'bin');
for(const path of [local,repo,outside,bin])await mkdir(path,{recursive:true});
await writeFile(join(bin,'mise'),'#!/bin/sh\nprintf "{}\\n"\n',{mode:0o700});
const git=(...args)=>execFileSync('git',['-C',repo,'-c','user.name=Fixture','-c','user.email=fixture@example.test',...args],{encoding:'utf8'}).trim();
git('init','-q');await writeFile(join(repo,'tracked.txt'),'remote fixture\n');git('add','tracked.txt');git('commit','-qm','fixture');
const binary=resolve('target/debug/agentworks'),port=process.env.AGENTWORKS_REMOTE_PLUMBING_PORT||'4530',base=`http://127.0.0.1:${port}`;
const env={...process.env,AGENTWORKS_SSH:resolve('scripts/fake-ssh.sh'),HOME:join(dir,'home'),PATH:bin+':'+process.env.PATH,AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:port,AGENTWORKS_ROOTS:local,AGENTWORKS_CATALOG_PATH:join(dir,'catalog.json'),AGENTWORKS_DB_PATH:join(dir,'state.sqlite3'),AGENTWORKS_DEMO_STATE_DIR:join(dir,'demo')};
let server,box,other,project;
const delay=ms=>new Promise(r=>setTimeout(r,ms));
async function api(path,body,method=body===undefined?'GET':'POST',code=200){const r=await fetch(base+path,{method,headers:{Origin:base,'Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body),signal:AbortSignal.timeout(30000)}),text=await r.text();assert.equal(r.status,code,`${method} ${path}: ${text}`);return code===200?JSON.parse(text):text;}
async function until(check,label='condition'){for(let i=0;i<350;i++){const value=await check();if(value)return value;await delay(100);}throw Error('Timed out: '+label);}
const snapshot=id=>api('/api/sessions/'+id);
const idle=id=>until(async()=>{const s=await snapshot(id);assert.notEqual(s.session.status,'error',JSON.stringify(s.events.filter(e=>e.kind==='error')));return s.session.status==='idle'&&s;},'idle '+id);
const create=async(extra={})=>{const s=await api('/api/sessions',{provider:'demo',hostId:box.id,cwd:repo,projectId:project.id,...extra});await idle(s.id);return s;};
const remove=id=>api('/api/sessions/'+id,undefined,'DELETE');
const prompt=async(id,text)=>{await idle(id);await api(`/api/sessions/${id}/prompt`,{text,requestId:crypto.randomUUID()});return idle(id);};
const connected=id=>until(async()=>(await api('/api/catalog')).hosts.find(h=>h.id===id)?.status==='connected','host connected');
async function start(){server=spawn(binary,[],{env,stdio:['ignore','inherit','inherit']});await until(()=>serverReady(server,base),'server ready');}
async function stop(){await stopChild(server,{signal:'SIGINT'});server=null;}
const queue=id=>api(`/api/sessions/${id}/queue`);
const continuations=s=>s.events.filter(e=>e.kind==='user'&&e.data.initiatedBy==='usage-limit scheduler');
try{
 await start();project=await api('/api/projects',{name:'Remote plumbing'});
 box=await api('/api/hosts',{name:'Remote A',destination:'dev@box',command:binary});other=await api('/api/hosts',{name:'Remote B',destination:'dev@other',command:binary});
 for(const host of [box,other]){await connected(host.id);await api(`/api/hosts/${host.id}/roots`,{roots:[root]},'PUT');}

 // Queue dispatch, durable retry identity and revoked remote roots.
 const a=await create();await api(`/api/sessions/${a.id}/prompt`,{text:'approval for queue',requestId:crypto.randomUUID()});
 const waiting=await until(async()=>{const s=await snapshot(a.id);return s.approvals.length&&s;});
 const requestId=crypto.randomUUID();
 await api(`/api/sessions/${a.id}/queue`,{text:'queued followup',requestId});
 await api(`/api/sessions/${a.id}/queue`,{text:'queued followup',requestId});
 await api(`/api/sessions/${a.id}/approvals/${waiting.approvals[0].id}`,{optionId:'allow'});
 await until(async()=>{const s=await snapshot(a.id);return s.session.status==='idle'&&s.events.some(e=>e.kind==='user'&&e.data.text==='queued followup');},'queued turn');
 assert.equal((await snapshot(a.id)).events.filter(e=>e.kind==='user'&&e.data.text==='queued followup').length,1);
 await api(`/api/sessions/${a.id}/queue`,{text:'paused until permitted',requestId:crypto.randomUUID()});
 await api(`/api/hosts/${box.id}/roots`,{roots:[outside]},'PUT');
 await api(`/api/sessions/${a.id}/queue/resume`,{},'POST',403);
 assert(!(await snapshot(a.id)).events.some(e=>e.kind==='user'&&e.data.text==='paused until permitted'));
 await api(`/api/hosts/${box.id}/roots`,{roots:[root]},'PUT');
 await api(`/api/sessions/${a.id}/queue/resume`,{});await until(async()=>(await queue(a.id)).queue.items.every(i=>i.state==='sent'));await idle(a.id);
 console.log('PASS remote queue: relay dispatch, retry identity, current host roots');

 // Same-host fork defaults to the parent's host and can create an isolated worktree.
 const forkBody={requestId:crypto.randomUUID(),workspace:{mode:'newWorktree'},context:'transcript'};
 const fork=await api(`/api/sessions/${a.id}/fork`,forkBody);await idle(fork.id);
 assert.equal(fork.hostId,box.id);assert.notEqual(fork.cwd,repo);assert.equal((await api(`/api/sessions/${a.id}/fork`,forkBody)).id,fork.id);
 const forkState=await snapshot(fork.id);assert.equal(forkState.session.fork.parentHostId,box.id);assert(forkState.events.some(e=>e.kind==='user'&&e.data.text==='queued followup'));
 await remove(fork.id);
 const cross=await api(`/api/sessions/${a.id}/fork`,{requestId:crypto.randomUUID(),hostId:other.id,cwd:repo,workspace:{mode:'directory'}});await idle(cross.id);
 assert.equal(cross.hostId,other.id);assert.equal((await snapshot(cross.id)).session.fork.sourceSessionId,null);await remove(cross.id);
 await api(`/api/sessions/${a.id}/fork`,{requestId:crypto.randomUUID(),hostId:'local'},'POST',400);
 console.log('PASS remote fork: default host, worktree, carried conversation, idempotency, cross-host native history isolation');

 // Import a provider-native conversation after deleting only its server registration.
 const originalNative=(await snapshot(a.id)).session.providerSessionId;
 // The native ID is exposed through provider_session events, rather than session metadata.
 const native=originalNative||(await snapshot(a.id)).events.find(e=>e.kind==='provider_session').data.id;
 await remove(a.id);
 const page=await api('/api/external-sessions',{provider:'demo',hostId:box.id,cwd:repo});
 assert(page.sessions.some(s=>s.sessionId===native));
 await api('/api/external-sessions',{provider:'demo',hostId:box.id,cwd:outside},'POST',403);
 await api('/api/sessions/import',{provider:'demo',hostId:box.id,cwd:root,providerSessionId:native},'POST',400);
 await api(`/api/projects/${project.id}/workspace-policy`,{defaultMode:'newWorktree',requireWorktree:true},'PUT');
 const beforeTrees=git('worktree','list','--porcelain');
 await api('/api/sessions/import',{provider:'demo',hostId:box.id,cwd:repo,providerSessionId:native,projectId:project.id},'POST',409);
 assert.equal(git('worktree','list','--porcelain'),beforeTrees,'Import must never move history into a new worktree to satisfy policy');
 await api(`/api/projects/${project.id}/workspace-policy`,{defaultMode:'directory',requireWorktree:false},'PUT');

 const imported=await api('/api/sessions/import',{provider:'demo',hostId:box.id,cwd:repo,providerSessionId:native});await idle(imported.id);
 assert((await snapshot(imported.id)).events.some(e=>e.kind==='acp'&&e.data.sessionUpdate==='user_message_chunk'));
 assert.equal((await api('/api/sessions/import',{provider:'demo',hostId:box.id,cwd:repo,providerSessionId:native})).id,imported.id);
 const separate=await api('/api/sessions/import',{provider:'demo',hostId:other.id,cwd:repo,providerSessionId:native});await idle(separate.id);assert.notEqual(separate.id,imported.id);
 const pageA=await api('/api/external-sessions',{provider:'demo',hostId:box.id,cwd:repo});assert.equal(pageA.sessions.find(s=>s.sessionId===native).agentworksId,imported.id);
 await remove(separate.id);await remove(imported.id);
 console.log('PASS remote history: listing, original cwd verification, transcript replay, host-scoped import identity');

 // Remote automation definitions and execution use remote roots/provider availability.
 const def=await api('/api/automations',{name:'Remote automation',provider:'demo',hostId:box.id,cwd:repo,projectId:project.id,prompt:'/status'});
 assert.equal(def.hostId,box.id);
 const runBody={requestId:crypto.randomUUID()},run=await api(`/api/automations/${def.id}/run`,runBody);await idle(run.session.id);
 assert.equal(run.session.hostId,box.id);assert.equal((await api(`/api/automations/${def.id}/run`,runBody)).session.id,run.session.id);
 await remove(run.session.id);
 await api(`/api/hosts/${box.id}/roots`,{roots:[outside]},'PUT');await api(`/api/automations/${def.id}/run`,{requestId:crypto.randomUUID()},'POST',403);
 await api(`/api/hosts/${box.id}/roots`,{roots:[root]},'PUT');await api(`/api/automations/${def.id}`,undefined,'DELETE');
 console.log('PASS remote automations: definition, execution, idempotency and root revalidation');

 // Continuation relies on structured ACP quota evidence; no local account probe.
 const quota=await create({resumePolicy:{enabled:true,maxAttempts:2}});
 await prompt(quota.id,'quota known');
 await until(async()=>{const s=await snapshot(quota.id);return continuations(s).length===1&&s.session.status==='idle';},'remote quota continuation');
 assert.equal((await snapshot(quota.id)).session.continuation.attempts,1);
 await api(`/api/sessions/${quota.id}/continuation`,{enabled:true},'PUT');await prompt(quota.id,'quota unknown');
 assert.equal((await snapshot(quota.id)).session.continuation.dueAt,null);
 await api(`/api/sessions/${quota.id}/continuation`,{enabled:true,manualDueAt:Date.now()+1000},'PUT');
 await until(async()=>{const s=await snapshot(quota.id);return continuations(s).length===2&&s.session.status==='idle';},'manual reset');
 await remove(quota.id);
 console.log('PASS remote continuation: ACP reset evidence, bounded resubmission, manually supplied reset');

 // Server-injected inbox turns remain independent of the agent-initiated MCP relay.
 const sender=await create({agentAccess:{communicate:true}}),recipient=await create({agentAccess:{communicate:true}});
 await api(`/api/sessions/${recipient.id}/inbox-delivery`,{enabled:true,requestId:crypto.randomUUID(),maxTurns:1,cooldownSeconds:5,maxBatch:1},'PUT');
 await api(`/api/hosts/${box.id}/roots`,{roots:[outside]},'PUT');
 await until(async()=>!(await api(`/api/sessions/${recipient.id}/inbox-delivery`)).policy.enabled,'inbox root revocation');
 assert.equal((await api(`/api/sessions/${recipient.id}/inbox-delivery`)).policy.attempts,0);
 await api(`/api/hosts/${box.id}/roots`,{roots:[root]},'PUT');
 await api(`/api/sessions/${recipient.id}/inbox-delivery`,{enabled:true,requestId:crypto.randomUUID(),maxTurns:1,cooldownSeconds:5,maxBatch:1},'PUT');
 await api(`/api/sessions/${sender.id}/mailbox`,{recipient:recipient.id,text:'Remote peer context',requestId:crypto.randomUUID()});
 await until(async()=>{const s=await snapshot(recipient.id);return s.session.status==='idle'&&s.events.some(e=>e.kind==='user'&&e.data.inboxDelivery);},'remote inbox turn');
 const inbox=await api(`/api/sessions/${recipient.id}/inbox-delivery`);assert.equal(inbox.policy.attempts,1);assert.equal(inbox.deliveries[0].state,'sent');
 await remove(sender.id);await remove(recipient.id);
 console.log('PASS remote inbox dispatch: human authorization, server mail delivery and bounded native turn');

 // A lost channel cannot replay an accepted turn or silently drain queued work.
 const interrupted=await create({agentAccess:{communicate:true}});await api(`/api/sessions/${interrupted.id}/prompt`,{text:'approval at disconnect',requestId:crypto.randomUUID()});
 await until(async()=>(await snapshot(interrupted.id)).approvals.length);
 const pending=crypto.randomUUID();await api(`/api/sessions/${interrupted.id}/queue`,{text:'inspect before replay',requestId:pending});
 await api(`/api/sessions/${interrupted.id}/inbox-delivery`,{enabled:true,requestId:crypto.randomUUID(),maxTurns:1,cooldownSeconds:5,maxBatch:1},'PUT');
 await api(`/api/hosts/${box.id}/reconnect`,{});await connected(box.id);
 await until(async()=>(await snapshot(interrupted.id)).session.status==='suspended');
 await api(`/api/sessions/${interrupted.id}/resume`,{});await idle(interrupted.id);
 assert(!(await snapshot(interrupted.id)).events.some(e=>e.kind==='user'&&e.data.text==='inspect before replay'));
 assert((await queue(interrupted.id)).queue.paused);
 assert.equal((await api(`/api/sessions/${interrupted.id}/inbox-delivery`)).policy.enabled,false);
 await api(`/api/sessions/${interrupted.id}/queue/resume`,{});await until(async()=>{const s=await snapshot(interrupted.id);return s.session.status==='idle'&&s.events.some(e=>e.kind==='user'&&e.data.text==='inspect before replay');});
 assert.equal((await snapshot(interrupted.id)).events.filter(e=>e.kind==='user'&&e.data.text==='inspect before replay').length,1);await remove(interrupted.id);
 console.log('PASS remote disconnect: uncertain turn interrupted, queue paused, deliberate resume sends once');

 // Authorized, unclaimed quota timers survive a server restart, including native resume.
 const restart=await create({resumePolicy:{enabled:true}});await prompt(restart.id,'quota known');
 const due=(await snapshot(restart.id)).session.continuation.dueAt;assert(due);
 await stop();await delay(Math.max(0,due-Date.now()+50));await start();await connected(box.id);
 await until(async()=>{const s=await snapshot(restart.id);return continuations(s).length===1&&s.session.status==='idle';},'continuation after restart');
 await remove(restart.id);
 console.log('Remote plumbing smoke passed');
}finally{if(server)await stop();await removeFixture(dir);}
