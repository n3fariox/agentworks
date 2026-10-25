// Exercise the linked harness against a loopback OpenAI fixture. No external inference.
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {createServer} from 'node:http';
import {mkdtemp,readFile,mkdir,stat,writeFile} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';
import {randomUUID} from 'node:crypto';
import {serverReady,stopChild,removeFixture} from '../tests/support/cleanup.mjs';

const dir=await mkdtemp(join(tmpdir(),'agentworks-goose-'));
const project=join(dir,'project');await mkdir(project);
const quote=value=>"'"+value.replaceAll("'","'\\''")+"'";
const remoteProviders=join(dir,'remote-providers'),ssh=join(dir,'fake-ssh');
await writeFile(ssh,`#!/bin/sh
exec env ${quote('AGENTWORKS_MODEL_PROVIDERS_DIR='+remoteProviders)} ${quote(resolve('scripts/fake-ssh.sh'))} "$@"
`,{mode:0o700});
const model='agentworks-fixture-model',profileModel='gpt-5';
const requests=[];let slowStarted=false,fixtureError;
const fixture=createServer(async(req,res)=>{
 try{
  const publicEndpoint=req.url.startsWith('/public/');
  if(!publicEndpoint&&req.headers.authorization!=='Bearer fixture-only'){res.writeHead(401);res.end('invalid key');return;}
  if(publicEndpoint)assert.equal(req.headers.authorization,undefined,'keyless profiles must not inherit a host API key');
  if(req.method==='GET'){
   res.writeHead(200,{'Content-Type':'application/json'});
   res.end(JSON.stringify({object:'list',data:[{id:model,object:'model',owned_by:'fixture'}]}));return;
  }
  assert(['/v1/chat/completions','/public/v1/chat/completions'].includes(req.url),req.url);
  let raw='';for await(const chunk of req)raw+=chunk;
  const body=JSON.parse(raw);requests.push(body);
  assert([model,profileModel].includes(body.model));
  if(!publicEndpoint)assert.equal(req.headers.authorization,'Bearer fixture-only');
  const lastUser=body.messages.findLast(m=>m.role==='user');
  const text=JSON.stringify(lastUser?.content);
  if(text.includes('slow-fixture')&&!text.includes('hello after cancellation')){slowStarted=true;return;}
  let delta,finish='stop';
  if(text.includes('tool-fixture')&&!text.includes('hello after cancellation')&&body.messages.at(-1).role!=='tool'){
   const shell=body.tools.find(t=>t.function.name==='shell');assert(shell,'built-in shell must be available');
   delta={role:'assistant',tool_calls:[{index:0,id:'fixture-shell',type:'function',function:{name:shell.function.name,arguments:JSON.stringify({command:'printf embedded-goose > goose-result.txt'})}}]};
   finish='tool_calls';
  }else delta={role:'assistant',content:'GOOSE_FIXTURE_OK'};
  res.writeHead(200,{'Content-Type':'text/event-stream'});
  const chunk={id:'fixture',object:'chat.completion.chunk',created:1,model};
  res.write('data: '+JSON.stringify({...chunk,choices:[{index:0,delta,finish_reason:null}]})+'\n\n');
  res.write('data: '+JSON.stringify({...chunk,choices:[{index:0,delta:{},finish_reason:finish}],usage:{prompt_tokens:20,completion_tokens:5,total_tokens:25}})+'\n\n');
  res.end('data: [DONE]\n\n');
 }catch(error){fixtureError=error;res.writeHead(500);res.end('fixture failed');}
});
await new Promise(r=>fixture.listen(0,'127.0.0.1',r));
const port=process.env.AGENTWORKS_GOOSE_TEST_PORT||'4497',base=`http://127.0.0.1:${port}`;
const env={...process.env,
 AGENTWORKS_SSH:ssh,AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:port,AGENTWORKS_ROOTS:project,
 AGENTWORKS_MODEL_PROVIDERS_DIR:join(dir,'providers'),
 AGENTWORKS_CATALOG_PATH:join(dir,'catalog.json'),AGENTWORKS_DB_PATH:join(dir,'sessions.sqlite3'),
 GOOSE_PATH_ROOT:join(dir,'goose'),GOOSE_PROVIDER:'openai',GOOSE_MODEL:model,
 GOOSE_MODE:'approve',GOOSE_DISABLE_SESSION_NAMING:'true',GOOSE_DISABLE_KEYRING:'1',
 OPENAI_API_KEY:'fixture-only',OPENAI_BASE_URL:`http://127.0.0.1:${fixture.address().port}/v1`,
 OPENAI_CUSTOM_HEADERS:'',
};
delete env.OPENAI_HOST;delete env.OPENAI_BASE_PATH;
let server,output='';
async function until(fn){
 const end=Date.now()+45000;
 while(Date.now()<end){if(fixtureError)throw fixtureError;const value=await fn();if(value)return value;await new Promise(r=>setTimeout(r,75));}
 throw Error('Timed out after '+requests.length+' fixture requests: '+output.slice(-4000));
}
async function api(path,body,method=body===undefined?'GET':'POST',expected=200){
 const res=await fetch(base+path,{method,headers:{Origin:base,'Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body)});
 assert.equal(res.status,expected,await res.clone().text());return res.headers.get('content-type')?.includes('application/json')?res.json():res.text();
}
async function start(){
 server=spawn(resolve(process.env.AGENTWORKS_TEST_BINARY||'target/debug/agentworks'),[],{env,stdio:['ignore','pipe','pipe']});
 let spawnError;server.on('error',e=>spawnError=e);server.stdout.on('data',b=>output+=b);server.stderr.on('data',b=>output+=b);
 await until(async()=>{if(spawnError)throw spawnError;if(server.exitCode!==null)throw Error(output);return serverReady(server,base);});
}
try{
 await start();
 const status=await api('/api/status');assert(status.providers.some(p=>p.id==='goose'&&p.installed));
 const models=await api('/api/models',{provider:'goose',cwd:project});assert(models.options.some(o=>o.value===model));
 assert.equal(requests.length,0,'model discovery must not send inference');
 const endpoint='/api/model-providers';
 const connection={name:'Fixture provider',baseUrl:env.OPENAI_BASE_URL,defaultModel:profileModel,apiKey:'fixture-only'};
 const test=await api(endpoint,{operation:'test',profile:connection});assert.equal(test.connected,true);
 const profile=await api(endpoint,{operation:'save',profile:connection});
 assert.equal(profile.keyConfigured,true);assert(!JSON.stringify(profile).includes('fixture-only'));
 const listing=await api(endpoint);assert(!JSON.stringify(listing).includes('fixture-only'));
 assert.equal((await stat(join(dir,'providers','credentials.json'))).mode&0o777,0o600);
 assert(!(await readFile(join(dir,'providers','profiles.json'),'utf8')).includes('fixture-only'));
 await api(endpoint,{operation:'test',profile:{...connection,apiKey:'wrong-fixture-key'}},'POST',400);
 await api(endpoint,{operation:'save',profile:{...connection,baseUrl:'https://user:password@host/v1'}},'POST',400);
 const selected=await api('/api/models',{provider:'goose',cwd:project,modelProvider:profile.id});assert(selected.options.some(o=>o.value===profileModel));
 const session=await api('/api/sessions',{provider:'goose',cwd:project,modelProvider:profile.id,title:'Goose fixture'});
 assert.equal(session.modelProvider,profile.id);
 await api(endpoint,{operation:'delete',id:profile.id},'POST',409);
 await api(endpoint,{operation:'save',profile:{...connection,id:profile.id,baseUrl:'http://localhost:1/v1'}},'POST',409);
 await api(endpoint,{operation:'save',profile:{...connection,id:profile.id,apiKey:null,name:'Renamed fixture'}});
 const publicProfile=await api(endpoint,{operation:'save',profile:{name:'No key',baseUrl:env.OPENAI_BASE_URL.replace('/v1','/public/v1'),defaultModel:model}});
 assert.equal(publicProfile.keyConfigured,false);
 await api(endpoint,{operation:'test',id:publicProfile.id});
 const publicSession=await api('/api/sessions',{provider:'goose',cwd:project,modelProvider:publicProfile.id});
 await until(async()=>(await api('/api/sessions/'+publicSession.id)).session.status==='idle');
 await api('/api/sessions/'+publicSession.id+'/prompt',{text:'keyless fixture',requestId:randomUUID()});
 await until(async()=>(await api('/api/sessions/'+publicSession.id)).events.some(e=>e.kind==='turn_end'));
 await api('/api/sessions/'+publicSession.id,undefined,'DELETE');
 await api(endpoint,{operation:'delete',id:publicProfile.id});

 const path='/api/sessions/'+session.id;
 async function snapshot(){const s=await api(path);const errors=s.events.filter(e=>e.kind==='error');assert.equal(errors.length,0,JSON.stringify(errors));assert.notEqual(s.session.status,'error');return s;}
 async function idle(turns){return until(async()=>{const s=await snapshot();return s.session.status==='idle'&&s.events.filter(e=>e.kind==='turn_end').length>=turns&&s;});}
 await idle(0);
 await api(path+'/prompt',{text:'tool-fixture',requestId:randomUUID()});
 const waiting=await until(async()=>{const s=await snapshot();return s.approvals.length&&s;});
 const approval=waiting.approvals[0];
 const allow=approval.request.options.find(o=>o.kind==='allow_once');assert(allow,JSON.stringify(approval));
 await api(path+'/approvals/'+approval.id,{optionId:allow.optionId});
 const done=await idle(1);
 assert.equal(await readFile(join(project,'goose-result.txt'),'utf8'),'embedded-goose');
 assert(done.events.some(e=>e.kind==='acp'&&e.data.status==='completed'));
 assert(done.events.some(e=>e.kind==='acp'&&e.data.content?.text?.includes('GOOSE_FIXTURE_OK')));
 const native=done.events.filter(e=>e.kind==='provider_session').at(-1).data.id;
 await stopChild(server,{signal:'SIGINT'});server=null;
 await start();await api(path+'/resume',{});const resumed=await idle(1);
 assert.equal(resumed.session.modelProvider,profile.id);
 assert.equal(resumed.events.filter(e=>e.kind==='provider_session').at(-1).data.id,native);
 await api(path+'/prompt',{text:'slow-fixture',requestId:randomUUID()});
 await until(()=>slowStarted);await api(path+'/cancel',{});await idle(2);
 await api(path+'/prompt',{text:'hello after cancellation',requestId:randomUUID()});await idle(3);
 for(const explicit of [false,true]){
  const child=await api(path+'/fork',{requestId:randomUUID(),workspace:{mode:'directory'},context:'transcript',...(explicit?{modelProvider:null}:{})});
  assert.equal(child.modelProvider,explicit?null:profile.id,'forks inherit the profile unless explicitly cleared');
  await until(async()=>{const state=await api('/api/sessions/'+child.id);assert.notEqual(state.session.status,'error',JSON.stringify(state.events.filter(e=>e.kind==='error')));return state.session.status==='idle';});
  await api('/api/sessions/'+child.id,undefined,'DELETE');
 }
 const remote=await api('/api/hosts',{name:'Goose fixture worker',destination:'dev@goose-fixture',command:resolve(process.env.AGENTWORKS_TEST_BINARY||'target/debug/agentworks')});
 await until(async()=>{const catalog=await api('/api/catalog');const host=catalog.hosts.find(h=>h.id===remote.id);return host?.status==='connected'&&host.worker.modelProviders;});
 await api('/api/hosts/'+remote.id+'/roots',{roots:[project]},'PUT');
 const remoteEndpoint=endpoint+'?hostId='+encodeURIComponent(remote.id);
 assert.deepEqual((await api(remoteEndpoint)).profiles,[],'providers must be explicitly enabled for a host');
 await api('/api/sessions',{provider:'goose',hostId:remote.id,cwd:project,modelProvider:profile.id},'POST',400);
 await api(endpoint,{operation:'save',profile:{...connection,id:profile.id,apiKey:null,hostIds:['local',remote.id]}});
 const shared=(await api(remoteEndpoint)).profiles;
 assert.equal(shared.length,1);assert.equal(shared[0].id,profile.id,'one provider is shared across hosts');
 assert.equal((await api(endpoint)).profiles.length,1,'assigning a host must not duplicate a provider');
 await api(remoteEndpoint,{operation:'test',id:profile.id});
 const remoteSession=await api('/api/sessions',{provider:'goose',hostId:remote.id,cwd:project,modelProvider:profile.id});
 const remotePath='/api/sessions/'+remoteSession.id;
 await until(async()=>{const state=await api(remotePath);assert.notEqual(state.session.status,'error',JSON.stringify(state.events.filter(e=>e.kind==='error')));return state.session.status==='idle';});
 await api(remotePath+'/prompt',{text:'remote fixture',requestId:randomUUID()});
 await until(async()=>{const state=await api(remotePath);assert(!state.events.some(e=>e.kind==='error'),JSON.stringify(state.events.filter(e=>e.kind==='error')));return state.session.status==='idle'&&state.events.some(e=>e.kind==='turn_end');});
 await assert.rejects(readFile(join(remoteProviders,'credentials.json')),error=>error.code==='ENOENT','remote credentials must not be persisted');
 await api(endpoint,{operation:'save',profile:{...connection,id:profile.id,apiKey:null,hostIds:['local']}});
 assert.equal((await api(remoteEndpoint)).profiles.length,0);
 await api(remotePath+'/prompt',{text:'revoked host must not run',requestId:randomUUID()},'POST',403);
 await api(remotePath+'/resume',{},'POST',403);
 await api(remotePath,undefined,'DELETE');await api('/api/hosts/'+remote.id,undefined,'DELETE');
 assert.equal(fixtureError,undefined);
 assert(!(await readFile(join(dir,'catalog.json'),'utf8')).includes('fixture-only'));
 assert(!(await readFile(join(dir,'sessions.sqlite3'))).includes(Buffer.from('fixture-only')));
 console.log('PASS shared provider, host assignments/revocation, ephemeral remote credentials, connection testing, keyless isolation; embedded Goose: ACP discovery, custom OpenAI endpoint, streaming, shell approval/execution, restart/resume and cancellation');
}finally{
 await stopChild(server,{signal:'SIGINT'});
 fixture.closeAllConnections();await new Promise(r=>fixture.close(r));
 await removeFixture(dir);
}
