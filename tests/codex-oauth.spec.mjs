import {test,expect} from '@playwright/test';
import {spawn} from 'node:child_process';
import {createServer as httpServer} from 'node:http';
import {createServer as tcpServer,connect} from 'node:net';
import {createHash,randomUUID} from 'node:crypto';
import {mkdtemp,mkdir,writeFile,chmod,readFile,stat,readdir} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';
import {serverReady,stopChild,removeFixture} from './support/cleanup.mjs';

let dir,server,issuer,tunnel,base,remote,profile,remoteProviders;
let exchanges=0,refreshes=0,inferences=0,currentAccess='',currentRefresh='',log='',holdExchange=null,exchangeStarted=null;
const codes=new Map(),connections=new Set(),devices=new Map();
let deviceDisabled=false,devicePollFailure=false,devicePolls=0;
const direct='http://127.0.0.1:4499';
const listen=server=>new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
async function api(path,body,method){
  const response=await fetch(base+path,{method:method||(body===undefined?'GET':'POST'),headers:{Origin:base,'Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body)});
  expect(response.ok,await response.clone().text()).toBeTruthy();return response.json();
}
async function snapshot(id){const result=await api('/api/sessions/'+id);expect(result.session.status,JSON.stringify(result.events.filter(e=>e.kind==='error'))).not.toBe('error');expect(result.events.filter(e=>e.kind==='error')).toEqual([]);return result;}
async function files(path){let out=[];for(const entry of await readdir(path,{withFileTypes:true})){const name=join(path,entry.name);out.push(...entry.isDirectory()?await files(name):[name]);}return out;}

test.beforeAll(async()=>{
  dir=await mkdtemp(join(tmpdir(),'agentworks-codex-oauth-'));remoteProviders=join(dir,'remote-providers');await mkdir(remoteProviders);await mkdir(join(dir,'project'));
  issuer=httpServer(async(req,res)=>{
    const url=new URL(req.url,'http://fixture');
    if(url.pathname==='/oauth/authorize'){
      const code=randomUUID();codes.set(code,{redirect:url.searchParams.get('redirect_uri'),challenge:url.searchParams.get('code_challenge')});
      const callback=new URL(url.searchParams.get('redirect_uri'));callback.searchParams.set('state',url.searchParams.get('state'));callback.searchParams.set('code',code);
      res.writeHead(200,{'Content-Type':'text/html'});res.end(`<h1>OAuth fixture</h1><a href="${callback.href.replaceAll('&','&amp;')}">Approve sign-in</a>`);return;
    }
    if(url.pathname==='/codex/device'){
      res.writeHead(200,{'Content-Type':'text/html'});res.end('<h1>Device verification fixture</h1>');return;
    }
    let text='';for await(const chunk of req)text+=chunk;
    if(url.pathname==='/api/accounts/deviceauth/usercode'){
      if(deviceDisabled){res.writeHead(403);res.end('{"error_description":"fixture-refresh-private"}');return;}
      const body=JSON.parse(text);if(body.client_id!=='app_EMoamEEZ73f0CkXaXp7hrann'){res.writeHead(400);res.end('{}');return;}
      const id=randomUUID(),code='TEST-'+devices.size;devices.set(id,{code,approved:false});
      res.writeHead(200,{'Content-Type':'application/json'});res.end(JSON.stringify({device_auth_id:id,user_code:code,interval:'1'}));return;
    }
    if(url.pathname==='/api/accounts/deviceauth/token'){
      devicePolls++;
      if(devicePollFailure){res.writeHead(410);res.end('{"error_description":"fixture-refresh-private"}');return;}
      const body=JSON.parse(text),device=devices.get(body.device_auth_id);
      if(!device||device.code!==body.user_code||!device.approved){res.writeHead(403);res.end('{}');return;}
      const code=randomUUID(),verifier=randomUUID();
      codes.set(code,{device:true,redirect:`http://127.0.0.1:${issuer.address().port}/deviceauth/callback`,challenge:createHash('sha256').update(verifier).digest('base64url')});
      res.writeHead(200,{'Content-Type':'application/json'});res.end(JSON.stringify({authorization_code:code,code_verifier:verifier}));return;
    }
    if(url.pathname==='/oauth/token'){
      const form=new URLSearchParams(text);
      if(form.get('grant_type')==='authorization_code'){
        const code=codes.get(form.get('code'));codes.delete(form.get('code'));
        if(!code||![code.redirect,...(code.device?['https://auth.openai.com/deviceauth/callback']:[])].includes(form.get('redirect_uri'))||code.challenge!==createHash('sha256').update(form.get('code_verifier')||'').digest('base64url')){res.writeHead(400);res.end('{}');return;}
        if(holdExchange){const pending=holdExchange;holdExchange=null;exchangeStarted?.();await pending;}
        exchanges++;currentAccess='fixture-access-initial';currentRefresh='fixture-refresh-initial';
      }else{
        if(form.get('refresh_token')!==currentRefresh){res.writeHead(400);res.end('{}');return;}
        refreshes++;await new Promise(r=>setTimeout(r,80));currentAccess='fixture-access-rotated-'+refreshes;currentRefresh='fixture-refresh-rotated-'+refreshes;
      }
      const claims=Buffer.from(JSON.stringify({exp:Math.floor(Date.now()/1000)+(refreshes?3600:1),'https://api.openai.com/auth':{chatgpt_account_id:'fixture-workspace'}})).toString('base64url');
      currentAccess=currentAccess+'.'+claims+'.signature';
      res.writeHead(200,{'Content-Type':'application/json'});res.end(JSON.stringify({access_token:currentAccess,refresh_token:currentRefresh,id_token:'fixture.'+claims+'.signature',expires_in:refreshes?3600:1}));return;
    }
    if(url.pathname==='/codex/responses'){
      if(req.headers.authorization!=='Bearer '+currentAccess||req.headers['chatgpt-account-id']!=='fixture-workspace'){res.writeHead(401);res.end('{}');return;}
      const request=JSON.parse(text);if(!request.stream||request.store!==false||!Array.isArray(request.input)){res.writeHead(400);res.end('{}');return;}
      inferences++;
      const events=[{type:'response.created',sequence_number:0,response:{id:'response',object:'response',created_at:1,status:'in_progress',model:request.model,output:[]}},{type:'response.output_text.delta',sequence_number:1,item_id:'message',output_index:0,content_index:0,delta:'CODEX_OAUTH_OK'},
        {type:'response.completed',sequence_number:2,response:{id:'response',object:'response',created_at:1,status:'completed',model:request.model,output:[],usage:{input_tokens:4,output_tokens:3,total_tokens:7}}}];
      res.writeHead(200,{'Content-Type':'text/event-stream'});res.end(events.map(event=>'data: '+JSON.stringify(event)+'\n\n').join('')+'data: [DONE]\n\n');return;
    }
    res.writeHead(404);res.end();
  });await listen(issuer);
  const ssh=join(dir,'ssh-fixture');await writeFile(ssh,`#!/bin/sh\nexport AGENTWORKS_MODEL_PROVIDERS_DIR='${remoteProviders}'\nexec '${resolve('scripts/fake-ssh.sh')}' "$@"\n`);await chmod(ssh,0o700);
  server=spawn(resolve('target/debug/agentworks'),[],{env:{...process.env,AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:'4499',AGENTWORKS_ROOTS:dir,
    AGENTWORKS_CATALOG_PATH:join(dir,'catalog.json'),AGENTWORKS_DB_PATH:join(dir,'sessions.sqlite3'),AGENTWORKS_MODEL_PROVIDERS_DIR:join(dir,'providers'),
    AGENTWORKS_SSH:ssh,GOOSE_DISABLE_SESSION_NAMING:'true',AGENTWORKS_CODEX_OAUTH_ISSUER:`http://127.0.0.1:${issuer.address().port}`,AGENTWORKS_CODEX_API_URL:`http://127.0.0.1:${issuer.address().port}/codex/responses`},stdio:['ignore','pipe','pipe']});
  server.stdout.on('data',b=>log+=b);server.stderr.on('data',b=>log+=b);
  await expect.poll(async()=>{if(server.exitCode!==null)throw Error(log);return serverReady(server,direct);},{timeout:15000}).toBe(true);
  // A real TCP forward: browser origin/port differs from the server's listener.
  tunnel=tcpServer(socket=>{const upstream=connect(4499,'127.0.0.1');connections.add(socket);connections.add(upstream);socket.pipe(upstream).pipe(socket);socket.on('error',()=>upstream.destroy());upstream.on('error',()=>socket.destroy());socket.on('close',()=>connections.delete(socket));upstream.on('close',()=>connections.delete(upstream));});await listen(tunnel);base=`http://127.0.0.1:${tunnel.address().port}`;
  remote=await api('/api/hosts',{name:'Codex worker',destination:'codex-worker-fixture',command:resolve('target/debug/agentworks')});
  await expect.poll(async()=>(await api('/api/catalog')).hosts.find(h=>h.id===remote.id)?.status).toBe('connected');
  await api(`/api/hosts/${remote.id}/roots`,{roots:[join(dir,'project')]},'PUT');
  profile=await api('/api/model-providers',{operation:'save',profile:{name:'Shared Codex account',kind:'chatgpt_codex',defaultModel:'gpt-6.1-sol',hostIds:['local',remote.id]}});
});
test.afterAll(async()=>{await stopChild(server);for(const socket of connections)socket.destroy();if(tunnel)await new Promise(r=>tunnel.close(r));issuer?.closeAllConnections();if(issuer)await new Promise(r=>issuer.close(r));await removeFixture(dir);});

test('OAuth through the web tunnel feeds local and SSH Goose sessions without worker token files',async({page})=>{
  const errors=[];page.on('pageerror',error=>errors.push(error.message));
  await page.goto(base+'/?settings');await page.getByRole('tab',{name:'Model providers',exact:true}).click();
  const popupPromise=page.waitForEvent('popup');await page.getByRole('button',{name:'Sign in with ChatGPT',exact:true}).click();const popup=await popupPromise;
  await popup.getByRole('link',{name:'Approve sign-in'}).waitFor();
  const authorization=new URL(popup.url());expect(authorization.searchParams.get('redirect_uri')).toBe(base+'/auth/callback');expect(base).not.toBe(direct);
  const before=exchanges;await popup.getByRole('link',{name:'Approve sign-in'}).click();await expect(popup.getByRole('heading',{name:'Sign-in complete'})).toBeVisible();
  await expect(page.locator('#codex-oauth-status')).toContainText('Signed in');expect(exchanges).toBe(before+1);await popup.close();
  const replay=await fetch(base+'/auth/callback?state='+authorization.searchParams.get('state')+'&code=replayed');expect(replay.status).toBe(400);
  const wrong=await fetch(base+'/auth/callback?state=wrong&code=unused');expect(wrong.status).toBe(400);expect(exchanges).toBe(before+1);
  const publicProfiles=await api('/api/model-providers');expect(publicProfiles.profiles.find(p=>p.id===profile.id).signedIn).toBe(true);expect(JSON.stringify(publicProfiles)).not.toContain('fixture-access');
  const sessions=await Promise.all(['local',remote.id].map(hostId=>api('/api/sessions',{provider:'goose',hostId,cwd:join(dir,'project'),modelProvider:profile.id,title:'Managed Codex'})));
  for(const session of sessions)await expect.poll(async()=>(await snapshot(session.id)).session.status,{timeout:20000}).toBe('idle');
  await Promise.all(sessions.map(session=>api(`/api/sessions/${session.id}/prompt`,{text:'Reply CODEX_OAUTH_OK without tools.',requestId:randomUUID()})));
  for(const session of sessions){try{await expect.poll(async()=>{const state=await snapshot(session.id);return state.events.some(e=>e.kind==='turn_end')&&JSON.stringify(state.events).includes('CODEX_OAUTH_OK');},{timeout:10000}).toBe(true);}catch(error){const state=await api('/api/sessions/'+session.id);console.log('Codex fixture diagnostic',JSON.stringify({exchanges,refreshes,inferences,session:state.session,events:state.events.slice(-12)}));throw error;}}
  expect(refreshes).toBe(1);expect(inferences).toBe(2);
  const stored=await readFile(join(dir,'providers','codex-oauth.json'),'utf8');expect(stored).toContain('fixture-refresh-rotated-1');expect((await stat(join(dir,'providers','codex-oauth.json'))).mode&0o777).toBe(0o600);
  const workerFiles=await files(remoteProviders);expect(workerFiles.some(path=>/codex-oauth\.json|tokens\.json/.test(path))).toBe(false);
  for(const session of sessions){const publicState=JSON.stringify(await api('/api/sessions/'+session.id));expect(publicState).not.toContain('fixture-access');expect(publicState).not.toContain('fixture-refresh');}
  expect(log).not.toContain('fixture-access');expect(log).not.toContain('fixture-refresh');
  // Changes to enabled hosts are checked at the broker, not trusted from workers.
  await api('/api/model-providers',{operation:'save',profile:{...profile,kind:'chatgpt_codex',hostIds:['local'],apiKey:null,keyConfigured:undefined,type:undefined,signedIn:undefined}});
  const denied=await fetch(base+`/api/sessions/${sessions[1].id}/prompt`,{method:'POST',headers:{Origin:base,'Content-Type':'application/json'},body:JSON.stringify({text:'must not run',requestId:randomUUID()})});expect(denied.status).toBe(403);expect(await denied.text()).toContain('not available on this host');expect(inferences).toBe(2);
  await api(`/api/model-providers/${profile.id}/codex-auth`,undefined,'DELETE');expect((await api(`/api/model-providers/${profile.id}/codex-auth`)).signedIn).toBe(false);
  await api(`/api/sessions/${sessions[0].id}/prompt`,{text:'must not call inference after disconnect',requestId:randomUUID()});
  await expect.poll(async()=>(await api('/api/sessions/'+sessions[0].id)).events.some(e=>e.kind==='error'),{timeout:10000}).toBe(true);expect(inferences).toBe(2);
  expect(errors).toEqual([]);
});

test('cancellation, denied consent, and superseded attempts never exchange or save credentials',async()=>{
  const before=exchanges;const path=`/api/model-providers/${profile.id}/codex-auth`;
  const first=await api(path,{},'POST');const old=new URL(first.authorizationUrl);
  const second=await api(path,{},'POST');const active=new URL(second.authorizationUrl);
  const stale=await fetch(base+'/auth/callback?state='+old.searchParams.get('state')+'&code=stale');expect(stale.status).toBe(400);
  await api(path+'/cancel',{},'POST');const cancelled=await fetch(base+'/auth/callback?state='+active.searchParams.get('state')+'&code=cancelled');expect(cancelled.status).toBe(400);
  const third=new URL((await api(path,{},'POST')).authorizationUrl);
  const denied=await fetch(base+'/auth/callback?state='+third.searchParams.get('state')+'&error=access_denied');expect(denied.status).toBe(400);
  const status=await api(path);expect(status.status).toBe('error');expect(status.error).toContain('declined');expect(status.signedIn).toBe(false);expect(exchanges).toBe(before);
});


test('cancelling an in-flight code exchange cannot restore credentials',async()=>{
  const path=`/api/model-providers/${profile.id}/codex-auth`;
  const authorization=(await api(path,{},'POST')).authorizationUrl;
  const html=await (await fetch(authorization)).text();const callback=html.match(/href="([^"]+)"/)[1].replaceAll('&amp;','&');
  let release;holdExchange=new Promise(resolve=>release=resolve);
  const started=new Promise(resolve=>exchangeStarted=resolve);
  const completion=fetch(callback);
  try{
    await started;await api(path+'/cancel',{},'POST');release();
    expect((await completion).status).toBe(400);
    const status=await api(path);expect(status.signedIn).toBe(false);expect(status.status).toBe('disconnected');
  }finally{release();exchangeStarted=null;holdExchange=null;}
});


test('device button shows a code and saves approval without a browser callback',async({page})=>{
  const path=`/api/model-providers/${profile.id}/codex-auth`;
  await page.goto(base+'/?settings');await page.getByRole('tab',{name:'Model providers',exact:true}).click();
  await page.getByRole('button',{name:'Device code',exact:true}).click();
  await expect(page.locator('#codex-device-code')).toContainText('TEST-');
  const code=await page.locator('#codex-device-code').textContent();
  expect(page.context().pages()).toHaveLength(1);
  await expect(page.locator('#codex-oauth-link')).toHaveAttribute('href',`http://127.0.0.1:${issuer.address().port}/codex/device`);
  const pending=await api(path);expect(pending.status).toBe('pending');expect(pending.signedIn).toBe(false);
  expect(JSON.stringify(pending)).not.toContain(code);expect(JSON.stringify(pending)).not.toContain('device_auth_id');
  const before=exchanges;[...devices.values()].find(device=>device.code===code).approved=true;
  await expect(page.locator('#codex-oauth-status')).toContainText('Signed in');expect(exchanges).toBe(before+1);
  await expect(page.locator('#codex-device-instructions')).toBeHidden();
  const stored=await readFile(join(dir,'providers','codex-oauth.json'),'utf8');expect(stored).toContain('fixture-refresh-initial');
  expect(log).not.toContain('fixture-refresh');expect(log).not.toContain(code);
  const session=await api('/api/sessions',{provider:'goose',hostId:'local',cwd:join(dir,'project'),modelProvider:profile.id,title:'Device login'});
  await expect.poll(async()=>(await snapshot(session.id)).session.status,{timeout:20000}).toBe('idle');
  await api(`/api/sessions/${session.id}/prompt`,{text:'Reply CODEX_OAUTH_OK without tools.',requestId:randomUUID()});
  await expect.poll(async()=>JSON.stringify((await snapshot(session.id)).events).includes('CODEX_OAUTH_OK'),{timeout:10000}).toBe(true);
  // Cancelling another attempt preserves the current login.
  await page.getByRole('button',{name:'Device code',exact:true}).click();await expect(page.locator('#codex-device-instructions')).toBeVisible();
  await page.getByRole('button',{name:'Cancel sign-in',exact:true}).click();await expect(page.locator('#codex-oauth-status')).toHaveText('Sign-in cancelled.');
  expect((await api(path)).signedIn).toBe(true);
  await api(path,undefined,'DELETE');
});

test('late device approval cannot survive cancellation, retry, deletion, or disconnect',async()=>{
  const path=`/api/model-providers/${profile.id}/codex-auth`,before=exchanges;
  const cancelled=await api(path+'/device',{},'POST');await api(path+'/cancel',{},'POST');
  [...devices.values()].find(d=>d.code===cancelled.userCode).approved=true;
  const stale=await api(path+'/device',{},'POST');
  const next=await api(path+'/device',{},'POST');[...devices.values()].find(d=>d.code===stale.userCode).approved=true;
  const polls=devicePolls;await expect.poll(()=>devicePolls).toBeGreaterThan(polls);
  expect((await api(path)).status).toBe('pending');expect(exchanges).toBe(before);
  await api(path,undefined,'DELETE');[...devices.values()].find(d=>d.code===next.userCode).approved=true;
  const deleted=await api('/api/model-providers',{operation:'save',profile:{name:'Deleted device profile',kind:'chatgpt_codex',defaultModel:'gpt-6.1-sol',hostIds:['local']}});
  const pending=await api(`/api/model-providers/${deleted.id}/codex-auth/device`,{},'POST');
  await api('/api/model-providers',{operation:'delete',id:deleted.id});[...devices.values()].find(d=>d.code===pending.userCode).approved=true;
  await new Promise(r=>setTimeout(r,1200));expect(exchanges).toBe(before);expect((await api(path)).signedIn).toBe(false);
});

test('device approval cancelled during token exchange does not replace credentials',async()=>{
  const path=`/api/model-providers/${profile.id}/codex-auth`;
  const pending=await api(path+'/device',{},'POST');
  let release;holdExchange=new Promise(resolve=>release=resolve);const started=new Promise(resolve=>exchangeStarted=resolve);
  try{
    [...devices.values()].find(d=>d.code===pending.userCode).approved=true;
    await started;await api(path+'/cancel',{},'POST');release();
    await expect.poll(async()=>(await api(path)).signedIn).toBe(false);
    await new Promise(r=>setTimeout(r,100));expect((await api(path)).status).toBe('disconnected');
  }finally{release();exchangeStarted=null;holdExchange=null;}
});

test('disabled device login and polling failures show actionable errors',async()=>{
  const path=`/api/model-providers/${profile.id}/codex-auth`;
  deviceDisabled=true;
  try{
    const response=await fetch(base+path+'/device',{method:'POST',headers:{Origin:base}});expect(response.status).toBe(400);
    expect(await response.text()).toContain('workspace admin');expect((await api(path)).status).toBe('error');
  }finally{deviceDisabled=false;}
  devicePollFailure=true;
  try{
    await api(path+'/device',{},'POST');await expect.poll(async()=>(await api(path)).status).toBe('error');
    expect((await api(path)).error).toContain('HTTP 410');expect((await api(path)).signedIn).toBe(false);
  }finally{devicePollFailure=false;}
});


for(const backend of ['device','device-rig'])test(`${backend}: cancel and retry across backends preserve the existing account`,async()=>{
  const path=`/api/model-providers/${profile.id}/codex-auth`,other=backend==='device'?'device-rig':'device';
  const pending=await api(path+'/'+backend,{},'POST');const before=exchanges;
  const next=await api(path+'/'+other,{},'POST');
  [...devices.values()].find(d=>d.code===pending.userCode).approved=true;
  await new Promise(r=>setTimeout(r,1200));expect(exchanges).toBe(before);
  expect((await api(path)).status).toBe('pending');
  await api(path+'/cancel',{},'POST');[...devices.values()].find(d=>d.code===next.userCode).approved=true;
  await new Promise(r=>setTimeout(r,1200));expect(exchanges).toBe(before);expect((await api(path)).signedIn).toBe(false);
});

test('Rig button stages approval in memory and shares credentials with Goose and existing login',async({page})=>{
  const path=`/api/model-providers/${profile.id}/codex-auth`;
  await page.goto(base+'/?settings');await page.getByRole('tab',{name:'Model providers',exact:true}).click();
  await expect(page.getByRole('button',{name:'Device code',exact:true})).toBeVisible();
  await page.getByRole('button',{name:'Device code · Rig',exact:true}).click();
  await expect(page.locator('#codex-device-code')).toContainText('TEST-');
  await expect(page.locator('#codex-oauth-title')).toContainText('Rig');
  const code=await page.locator('#codex-device-code').textContent();
  expect(page.context().pages()).toHaveLength(1);
  const before=exchanges;[...devices.values()].find(d=>d.code===code).approved=true;
  await expect(page.locator('#codex-oauth-status')).toContainText('Signed in');expect(exchanges).toBe(before+1);
  const stored=await readFile(join(dir,'providers','codex-oauth.json'),'utf8');expect(stored).toContain('fixture-refresh-initial');
  expect((await stat(join(dir,'providers','codex-oauth.json'))).mode&0o777).toBe(0o600);
  const session=await api('/api/sessions',{provider:'goose',hostId:'local',cwd:join(dir,'project'),modelProvider:profile.id,title:'Rig device login'});
  await expect.poll(async()=>(await snapshot(session.id)).session.status,{timeout:20000}).toBe('idle');
  await api(`/api/sessions/${session.id}/prompt`,{text:'Reply CODEX_OAUTH_OK without tools.',requestId:randomUUID()});
  await expect.poll(async()=>JSON.stringify((await snapshot(session.id)).events).includes('CODEX_OAUTH_OK'),{timeout:10000}).toBe(true);
  for(const backend of ['device','device-rig']){
    const pending=await api(path+'/'+backend,{},'POST');await api(path+'/cancel',{},'POST');
    expect((await api(path)).signedIn).toBe(true);
    expect([...devices.values()].find(d=>d.code===pending.userCode).approved).toBe(false);
  }
  expect(log).not.toContain('fixture-access');expect(log).not.toContain('fixture-refresh');
  const snapshotText=JSON.stringify(await api('/api/sessions/'+session.id));expect(snapshotText).not.toContain('fixture-access');
  await api(path,undefined,'DELETE');
});

test('Rig late exchange, disconnect, and profile deletion cannot resurrect credentials',async()=>{
  const path=`/api/model-providers/${profile.id}/codex-auth`;
  const pending=await api(path+'/device-rig',{},'POST');
  let release;holdExchange=new Promise(resolve=>release=resolve);const started=new Promise(resolve=>exchangeStarted=resolve);
  try{
    [...devices.values()].find(d=>d.code===pending.userCode).approved=true;await started;
    await api(path,undefined,'DELETE');release();await new Promise(r=>setTimeout(r,100));expect((await api(path)).signedIn).toBe(false);
  }finally{release();exchangeStarted=null;holdExchange=null;}
  const deleted=await api('/api/model-providers',{operation:'save',profile:{name:'Deleted Rig profile',kind:'chatgpt_codex',defaultModel:'gpt-6.1-sol',hostIds:['local']}});
  const next=await api(`/api/model-providers/${deleted.id}/codex-auth/device-rig`,{},'POST');const before=exchanges;
  await api('/api/model-providers',{operation:'delete',id:deleted.id});[...devices.values()].find(d=>d.code===next.userCode).approved=true;
  await new Promise(r=>setTimeout(r,1200));expect(exchanges).toBe(before);
});

test('Rig failures are sanitized and can be retried using the existing implementation',async()=>{
  const path=`/api/model-providers/${profile.id}/codex-auth`;
  deviceDisabled=true;
  try{
    const response=await fetch(base+path+'/device-rig',{method:'POST',headers:{Origin:base}});expect(response.status).toBe(400);
    expect(await response.text()).toContain('workspace admin');expect((await api(path)).status).toBe('error');
  }finally{deviceDisabled=false;}
  devicePollFailure=true;
  try{
    const response=await fetch(base+path+'/device-rig',{method:'POST',headers:{Origin:base}});expect([200,400]).toContain(response.status);
    expect(await response.text()).not.toContain('fixture-refresh-private');await expect.poll(async()=>(await api(path)).status).toBe('error');
    expect((await api(path)).error).not.toContain('fixture-refresh-private');expect((await api(path)).error).toContain('Rig');expect((await api(path)).signedIn).toBe(false);
  }finally{devicePollFailure=false;}
  await api(path+'/device',{},'POST');expect((await api(path)).status).toBe('pending');await api(path+'/cancel',{},'POST');
});

test('existing and Rig device login can run independently on separate connections',async()=>{
  const other=await api('/api/model-providers',{operation:'save',profile:{name:'Rig comparison account',kind:'chatgpt_codex',defaultModel:'gpt-6.1-sol',hostIds:['local']}});
  const original=`/api/model-providers/${profile.id}/codex-auth`,rig=`/api/model-providers/${other.id}/codex-auth`;
  const [nativePending,rigPending]=await Promise.all([api(original+'/device',{},'POST'),api(rig+'/device-rig',{},'POST')]);
  [...devices.values()].find(d=>d.code===rigPending.userCode).approved=true;
  await expect.poll(async()=>(await api(rig)).signedIn).toBe(true);
  expect((await api(original)).status).toBe('pending');expect((await api(original)).signedIn).toBe(false);
  [...devices.values()].find(d=>d.code===nativePending.userCode).approved=true;
  await expect.poll(async()=>(await api(original)).signedIn).toBe(true);
  await api(original,undefined,'DELETE');expect((await api(rig)).signedIn).toBe(true);
  await api('/api/model-providers',{operation:'delete',id:other.id});
});
