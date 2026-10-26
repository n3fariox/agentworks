import {test,expect} from '@playwright/test';
import {spawn} from 'node:child_process';
import {createServer} from 'node:http';
import {randomUUID} from 'node:crypto';
import {mkdtemp,mkdir} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';
import {serverReady,stopChild,removeFixture} from './support/cleanup.mjs';

// Own server, provider store and loopback OpenAI fixture. The host environment and the
// profile point at different paths and keys, so each request shows which one a run used.
const port=process.env.AGENTWORKS_AUTOMATION_PROVIDER_PORT||'4671',base='http://127.0.0.1:'+port;
let dir,project,server,fixture,output='',profileUrl;const chats=[];
async function call(path,body,method){
  const res=await fetch(base+path,{method:method||(body===undefined?'GET':'POST'),headers:{Origin:base,'Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body)});
  const text=await res.text();let json=null;try{json=JSON.parse(text);}catch{}
  return {status:res.status,body:json,text};
}
async function api(path,body,method){const r=await call(path,body,method);expect(r.status,JSON.stringify(r.body)).toBe(200);return r.body;}
async function start(){
  const env={...process.env,
    AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:port,AGENTWORKS_ROOTS:project,
    AGENTWORKS_CATALOG_PATH:join(dir,'catalog.json'),AGENTWORKS_DB_PATH:join(dir,'sessions.sqlite3'),
    AGENTWORKS_MODEL_PROVIDERS_DIR:join(dir,'providers'),GOOSE_PATH_ROOT:join(dir,'goose'),
    GOOSE_PROVIDER:'openai',GOOSE_MODEL:'host-model',GOOSE_MODE:'approve',GOOSE_DISABLE_SESSION_NAMING:'true',GOOSE_DISABLE_KEYRING:'1',
    OPENAI_API_KEY:'host-key',OPENAI_BASE_URL:`http://127.0.0.1:${fixture.address().port}/host/v1`,OPENAI_CUSTOM_HEADERS:'',
  };delete env.OPENAI_HOST;delete env.OPENAI_BASE_PATH;
  server=spawn(resolve(process.env.AGENTWORKS_TEST_BINARY||'target/debug/agentworks'),[],{env,stdio:['ignore','pipe','pipe']});
  server.stdout.on('data',b=>output+=b);server.stderr.on('data',b=>output+=b);
  let error;server.on('error',e=>error=e);
  await expect.poll(async()=>{if(error)throw error;if(server.exitCode!==null)throw Error(output);return serverReady(server,base);},{timeout:10000}).toBe(true);
}
test.beforeAll(async()=>{
  dir=await mkdtemp(join(tmpdir(),'agentworks-automation-provider-'));project=join(dir,'project');await mkdir(project);
  fixture=createServer(async(req,res)=>{
    if(req.method==='GET'){
      if(req.headers.authorization!=='Bearer profile-key'){res.writeHead(401);res.end();return;}
      res.writeHead(200,{'Content-Type':'application/json'});res.end(JSON.stringify({data:[{id:'profile-model'}]}));return;
    }
    let raw='';for await(const chunk of req)raw+=chunk;
    const body=JSON.parse(raw);chats.push({url:req.url,authorization:req.headers.authorization,model:body.model,text:JSON.stringify(body.messages)});
    const chunk={id:'fixture-'+randomUUID(),object:'chat.completion.chunk',created:1,model:body.model};
    res.writeHead(200,{'Content-Type':'text/event-stream'});
    res.write('data: '+JSON.stringify({...chunk,choices:[{index:0,delta:{role:'assistant',content:'FIXTURE_OK'},finish_reason:null}]})+'\n\n');
    res.write('data: '+JSON.stringify({...chunk,choices:[{index:0,delta:{},finish_reason:'stop'}],usage:{prompt_tokens:1,completion_tokens:1,total_tokens:2}})+'\n\n');
    res.end('data: [DONE]\n\n');
  });
  await new Promise(r=>fixture.listen(0,'127.0.0.1',r));profileUrl=`http://127.0.0.1:${fixture.address().port}/profile/v1`;
  await start();
});
test.afterAll(async()=>{await stopChild(server);fixture?.closeAllConnections();if(fixture)await new Promise(r=>fixture.close(r));await removeFixture(dir);});

const recipe=text=>({version:1,trigger:{kind:'manual'},steps:[{kind:'agent',text}]});
async function runChat(def,marker){
  const result=await api(`/api/automations/${def.id}/run`,{requestId:randomUUID()});
  await expect.poll(async()=>chats.find(c=>c.text.includes(marker))||JSON.stringify((await api('/api/sessions/'+result.session.id)).events).slice(-1500),{timeout:20000}).toMatchObject({url:expect.any(String)});
  return {session:result.session,chat:chats.find(c=>c.text.includes(marker))};
}

test('automations bind an explicit Goose provider profile; omission keeps the host environment',async()=>{
  const profile=await api('/api/model-providers',{operation:'save',profile:{name:'Gateway',baseUrl:profileUrl,defaultModel:'profile-model',hostIds:['local'],apiKey:'profile-key'}});
  const bound=await api('/api/automations',{name:'Bound',provider:'goose',modelProvider:profile.id,cwd:project,workflow:recipe('bound-marker')});
  expect(bound.modelProvider).toBe(profile.id);
  expect(bound.modelProviderStatus).toEqual({available:true,name:'Gateway',keyConfigured:true});
  const plain=await api('/api/automations',{name:'Plain',provider:'goose',cwd:project,workflow:recipe('plain-marker')});
  expect(plain.modelProvider).toBeUndefined();expect(plain.modelProviderStatus).toBeUndefined();

  // The run session is bound to the profile and requests go to its endpoint, key and default model.
  const first=await runChat(bound,'bound-marker');
  expect(first.session.modelProvider).toBe(profile.id);
  expect(first.chat).toMatchObject({url:'/profile/v1/chat/completions',authorization:'Bearer profile-key',model:'profile-model'});
  const second=await runChat(plain,'plain-marker');
  expect(second.session.modelProvider).toBeNull();
  expect(second.chat).toMatchObject({url:'/host/v1/chat/completions',authorization:'Bearer host-key',model:'host-model'});

  // Editing can clear the binding; omission on update means host environment.
  const cleared=await api('/api/automations/'+bound.id,{name:'Bound',provider:'goose',cwd:project,workflow:recipe('bound-marker')},'PUT');
  expect(cleared.modelProvider).toBeUndefined();
  const rebound=await api('/api/automations/'+bound.id,{name:'Bound',provider:'goose',modelProvider:profile.id,cwd:project,workflow:recipe('bound-marker')},'PUT');
  expect(rebound.modelProvider).toBe(profile.id);

  // Persisted across restart.
  await stopChild(server);await start();
  const listed=(await api('/api/automations')).find(d=>d.id===bound.id);
  expect(listed.modelProvider).toBe(profile.id);expect(listed.modelProviderStatus.available).toBe(true);
  expect((await api('/api/automations')).find(d=>d.id===plain.id).modelProvider).toBeUndefined();
});

test('invalid profiles are rejected and unavailable profiles block runs without falling back',async()=>{
  const profile=await api('/api/model-providers',{operation:'save',profile:{name:'Keyless',baseUrl:profileUrl,defaultModel:'profile-model',hostIds:['local']}});
  for(const [input,message] of [
    [{provider:'demo',modelProvider:profile.id},'require Goose'],
    [{provider:'goose',modelProvider:randomUUID()},'no longer exists'],
  ]){
    const result=await call('/api/automations',{name:'Bad',cwd:project,workflow:recipe('x'),...input});
    expect(result.status).toBe(400);expect(result.text).toContain(message);
  }
  const hidden=await api('/api/model-providers',{operation:'save',profile:{name:'Nowhere',baseUrl:profileUrl,defaultModel:'profile-model',hostIds:[]}});
  const unavailable=await call('/api/automations',{name:'Bad',provider:'goose',modelProvider:hidden.id,cwd:project,workflow:recipe('x')});
  expect(unavailable.status).toBe(400);expect(unavailable.text).toContain('not available on this host');
  expect((await api('/api/automations')).filter(d=>d.name==='Bad')).toEqual([]);

  // No stored key is reported as such, never as authenticated.
  const def=await api('/api/automations',{name:'Keyless run',provider:'goose',modelProvider:profile.id,cwd:project,workflow:recipe('keyless-marker')});
  expect(def.modelProviderStatus).toEqual({available:true,name:'Keyless',keyConfigured:false});

  // A profile used by an automation cannot be deleted.
  const blocked=await call('/api/model-providers',{operation:'delete',id:profile.id});
  expect(blocked.status).toBe(409);expect(blocked.text).toContain('used by automations');

  // Disabling the profile on this host blocks runs instead of using the host environment.
  await api('/api/model-providers',{operation:'save',profile:{id:profile.id,name:'Keyless',baseUrl:profileUrl,defaultModel:'profile-model',hostIds:[]}});
  const listed=(await api('/api/automations')).find(d=>d.id===def.id);
  expect(listed.modelProviderStatus).toMatchObject({available:false,name:'Keyless'});
  expect(listed.modelProviderStatus.keyConfigured).toBeUndefined();
  const before=chats.length;
  const run=await call(`/api/automations/${def.id}/run`,{requestId:randomUUID()});
  expect(run.status).toBe(409);expect(run.text).toContain('Model provider profile is unavailable');
  expect(await api(`/api/automations/${def.id}/runs`)).toEqual([]);
  expect(chats.length).toBe(before);
});

test('editor selects, keeps and flags provider profiles',async({page})=>{
  const errors=[];page.on('pageerror',e=>errors.push(e.message));
  const profile=(await api('/api/model-providers')).profiles.find(p=>p.name==='Gateway');
  await page.goto(base+'/?view=automations');await page.locator('#nav-automations').click();
  await page.locator('#new-automation').click();
  await page.locator('#automation-provider').selectOption('demo');
  await expect(page.locator('#automation-model-provider-field')).toBeHidden();
  await page.locator('#automation-provider').selectOption('goose');
  await expect(page.locator('#automation-model-provider-field')).toBeVisible();
  // New automations default to the host environment; profiles are opt-in.
  await expect(page.locator('#automation-model-provider')).toHaveValue('');
  await expect(page.locator('#automation-model-provider option',{hasText:'Gateway · API key stored'})).toHaveCount(1);
  await expect(page.locator('#automation-model-provider option',{hasText:'Nowhere'})).toHaveCount(0);
  await page.locator('#automation-model-provider').selectOption(profile.id);
  await expect(page.locator('#automation-model')).toHaveAttribute('placeholder','Profile default · profile-model');
  await page.locator('#automation-load-models').click();
  await expect(page.locator('#automation-models option')).toHaveCount(1);
  await page.locator('#automation-name').fill('Editor bound');await page.getByLabel('Step 1 instructions').fill('editor-marker');
  await page.locator('#automation-directory').fill(project);
  await page.locator('#save-automation').click();await expect(page.locator('#automation-dialog')).toBeHidden();
  const def=(await api('/api/automations')).find(d=>d.name==='Editor bound');expect(def.modelProvider).toBe(profile.id);
  await expect(page.locator('#automation-detail')).toContainText('Model provider: Gateway · API key stored');
  await page.locator('#automation-detail').getByRole('button',{name:'Edit'}).click();
  await expect(page.locator('#automation-model-provider')).toHaveValue(profile.id);
  await page.locator('#dismiss-automation').click();

  // A bound profile that becomes unavailable stays selected and flagged rather than silently cleared.
  await page.locator('.automation-item',{hasText:'Keyless run'}).click();
  await expect(page.locator('#automation-detail .error-text')).toContainText('Model provider unavailable');
  await page.locator('#automation-detail').getByRole('button',{name:'Edit'}).click();
  const keyless=(await api('/api/automations')).find(d=>d.name==='Keyless run').modelProvider;
  await expect(page.locator('#automation-model-provider')).toHaveValue(keyless);
  await expect(page.locator('#automation-model-provider-note')).toContainText('Choose another before saving');
  await page.locator('#save-automation').click();
  await expect(page.locator('#automation-error')).toContainText('not available on this host');
  expect(errors).toEqual([]);
});
