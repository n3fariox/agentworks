import {serverReady,stopChild,removeFixture} from '../tests/support/cleanup.mjs';
// Native stdio MCP on remote workers over SSH relay. No paid inference.
import assert from 'node:assert/strict';
import {spawn,execFileSync} from 'node:child_process';
import {mkdtemp,mkdir,writeFile} from 'node:fs/promises';
import {join,resolve} from 'node:path';
await mkdir('.cache',{recursive:true});
const dir=await mkdtemp(resolve('.cache/remote-mcp-'));
const local=join(dir,'local'),root=join(dir,'remote'),repo=join(root,'repo'),bin=join(dir,'bin');
for(const path of [local,repo,bin])await mkdir(path,{recursive:true});
await writeFile(join(bin,'mise'),'#!/bin/sh\nprintf "{}\\n"\n',{mode:0o700});
const git=(...args)=>execFileSync('git',['-C',repo,'-c','user.name=Fixture','-c','user.email=fixture@example.test',...args],{encoding:'utf8'}).trim();
git('init','-q');await writeFile(join(repo,'tracked.txt'),'remote fixture\n');git('add','tracked.txt');git('commit','-qm','fixture');
const binary=resolve('target/debug/agentworks'),port=process.env.AGENTWORKS_REMOTE_MCP_PORT||'4534',base=`http://127.0.0.1:${port}`;
const env={...process.env,AGENTWORKS_SSH:resolve('scripts/fake-ssh.sh'),HOME:join(dir,'home'),PATH:bin+':'+process.env.PATH,AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:port,AGENTWORKS_ROOTS:local,AGENTWORKS_CATALOG_PATH:join(dir,'catalog.json'),AGENTWORKS_DB_PATH:join(dir,'state.sqlite3'),AGENTWORKS_DEMO_STATE_DIR:join(dir,'demo')};
let server,box,other,project;
const delay=ms=>new Promise(r=>setTimeout(r,ms));
async function api(path,body,method=body===undefined?'GET':'POST',code=200){const r=await fetch(base+path,{method,headers:{Origin:base,'Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body),signal:AbortSignal.timeout(30000)}),text=await r.text();assert.equal(r.status,code,`${method} ${path}: ${text}`);return code===200?JSON.parse(text):text;}
async function until(check,label='condition'){for(let i=0;i<350;i++){const value=await check();if(value)return value;await delay(100);}throw Error('Timed out: '+label);}
const snapshot=id=>api('/api/sessions/'+id);
const idle=id=>until(async()=>{const s=await snapshot(id);assert.notEqual(s.session.status,'error',JSON.stringify(s.events.filter(e=>e.kind==='error')));return s.session.status==='idle'&&s;},'idle '+id);
const create=async(extra={})=>{const s=await api('/api/sessions',{provider:'demo',hostId:box.id,cwd:repo,projectId:project.id,...extra});await idle(s.id);return s;};
const prompt=async(id,text)=>{await idle(id);await api(`/api/sessions/${id}/prompt`,{text,requestId:crypto.randomUUID()});return idle(id);};
const connected=id=>until(async()=>(await api('/api/catalog')).hosts.find(h=>h.id===id)?.status==='connected','host connected');
async function start(){server=spawn(binary,[],{env,stdio:['ignore','inherit','inherit']});await until(()=>serverReady(server,base),'server ready');}
async function stop(){await stopChild(server,{signal:'SIGINT'});server=null;}
async function call(id,name,args={}) {
 await idle(id);const old=await snapshot(id);
 const s=await prompt(id,`mcp ${name} ${JSON.stringify(args)}`);
 const result=s.events.filter(e=>e.seq>old.seq&&e.kind==='acp'&&e.data.sessionUpdate==='tool_call_update').at(-1)?.data.rawOutput;
 assert(result,'Missing MCP result '+name);return result;
}
function value(result){assert(!result.isError,JSON.stringify(result));return JSON.parse(result.content[0].text);}
function denied(result,status=403){assert(result.isError,JSON.stringify(result));assert.equal(JSON.parse(result.content[0].text).status,status);}
try {
 await start();
 project=await api('/api/projects',{name:'Remote MCP',defaultCommunicationRole:'participant'});
 box=await api('/api/hosts',{name:'Remote A',destination:'dev@box',command:binary});
 other=await api('/api/hosts',{name:'Remote B',destination:'dev@other',command:binary});
 for(const host of [box,other]){await connected(host.id);await api(`/api/hosts/${host.id}/roots`,{roots:[root]},'PUT');}
 const a=await create({title:'remote A'}),b=await create({title:'remote B',hostId:other.id});
 const c=await create({title:'local',hostId:'local',cwd:local});
 const outsideProject=await api('/api/projects',{name:'Other',defaultCommunicationRole:'participant'});
 const outsider=await create({projectId:outsideProject.id});
 const off=await create({agentAccess:{}});
 assert.deepEqual((await call(off.id,'tools/list')).tools,[]);denied(await call(off.id,'read_inbox'));
 const names=(await call(a.id,'tools/list')).tools.map(t=>t.name).sort();
 assert.deepEqual(names,['acknowledge_message','list_sessions','read_inbox','send_message']);
 const peers=value(await call(a.id,'list_sessions'));
 assert(peers.sessions.some(p=>p.id===b.id));assert(peers.sessions.some(p=>p.id===c.id));
 assert(!peers.sessions.some(p=>p.id===outsider.id||p.id===off.id));
 const args={recipient:'agentworks://sessions/'+b.id,text:'Remote to remote',requestId:'remote-first'};
 const sent=value(await call(a.id,'send_message',args));
 assert.equal(value(await call(a.id,'send_message',args)).id,sent.id);
 denied(await call(a.id,'send_message',{...args,text:'changed'}),409);
 denied(await call(a.id,'send_message',{...args,sender:b.id,requestId:'spoof'}),400);
 denied(await call(a.id,'send_message',{...args,recipient:outsider.id,requestId:'cross-project'}));
 const inbox=value(await call(b.id,'read_inbox'));assert.equal(inbox.messages.length,1);assert.equal(inbox.messages[0].sender,a.id);
 value(await call(b.id,'acknowledge_message',{messageId:sent.uri}));
 value(await call(b.id,'send_message',{recipient:c.id,text:'Remote to local',requestId:'local-first'}));
 assert.equal(value(await call(c.id,'read_inbox')).messages[0].sender,b.id);
 value(await call(c.id,'send_message',{recipient:a.id,text:'Local to remote',requestId:'return'}));
 assert(value(await call(a.id,'read_inbox')).messages.some(m=>m.text==='Local to remote'));
 // Cached configuration cannot retain a revoked grant; denied methods cannot be called directly.
 await api(`/api/sessions/${a.id}/agent-access`,{},'PUT');
 denied(await call(a.id,'read_inbox'));denied(await call(a.id,'send_message',{...args,requestId:'revoked'}));
 await api(`/api/sessions/${a.id}/agent-access`,{communicate:true,startSessions:true},'PUT');
 denied(await call(a.id,'start_session',{provider:'demo',cwd:repo,requestId:'unrelayed'}));
 assert(!(await call(a.id,'tools/list')).tools.some(t=>t.name==='start_session'));
 // A fresh link and native resume install new relay configuration; durable retries stay deduplicated.
 await api(`/api/hosts/${box.id}/reconnect`,{});await connected(box.id);
 await until(async()=>(await snapshot(a.id)).session.status==='suspended');
 await api(`/api/sessions/${a.id}/resume`,{});await idle(a.id);
 assert.equal(value(await call(a.id,'send_message',args)).id,sent.id);
 console.log('PASS remote MCP: native discovery, two workers/local peers, grants, scope, spoof denial, retries, reconnect/resume');
} finally {if(server)await stop();await removeFixture(dir);}
