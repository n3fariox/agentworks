import {removeFixture} from '../tests/support/cleanup.mjs';
// Owns an isolated server/database. Tests process restarts and crashes, not just browser reloads.
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {once} from 'node:events';
import {mkdtemp,mkdir,rename,unlink,writeFile} from 'node:fs/promises';
import {resolve,join} from 'node:path';
import {randomUUID} from 'node:crypto';
import {chromium} from '@playwright/test';
const port=process.env.AGENTWORKS_PERSISTENCE_PORT||'4321',base=`http://127.0.0.1:${port}`;
await mkdir('.cache',{recursive:true});
const dir=await mkdtemp(resolve('.cache/persistence-')),cwd=join(dir,'project');await mkdir(cwd);
const env={...process.env,AGENTWORKS_PORT:port,AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_ROOTS:resolve('.'),AGENTWORKS_CATALOG_PATH:join(dir,'catalog.json'),AGENTWORKS_DB_PATH:join(dir,'sessions.sqlite3'),AGENTWORKS_DEMO_STATE_DIR:join(dir,'demo')};
const headers={Origin:base,'Content-Type':'application/json'};
const delay=ms=>new Promise(r=>setTimeout(r,ms));
let server,browser,page;
async function api(path,method='GET',body){const r=await fetch(base+path,{method,headers,body:body===undefined?undefined:JSON.stringify(body)});assert.equal(r.status,200,await r.clone().text());return r.json();}
async function until(check,timeout=90000){const deadline=Date.now()+timeout;while(Date.now()<deadline){const v=await check();if(v)return v;await delay(100);}throw new Error('Timed out waiting for persistence test');}
async function start(){server=spawn(resolve('target/debug/agentworks'),[],{env,stdio:['ignore','inherit','inherit']});await until(async()=>{if(server.exitCode!==null)throw new Error('Test server exited');try{return(await fetch(base+'/api/status')).ok;}catch{return false;}},10000);}
async function stop(signal='SIGINT'){const exit=once(server,'exit');server.kill(signal);const timer=setTimeout(()=>server.kill('SIGKILL'),8000);timer.unref();const[code]=await exit;clearTimeout(timer);if(signal==='SIGINT')assert.equal(code,0);server=null;}
async function ready(id){return until(async()=>{const snap=await api('/api/sessions/'+id);if(snap.session.status==='error')throw new Error(snap.events.filter(e=>e.kind==='error').map(e=>e.data.message).join('\n'));return snap.session.status==='idle'&&snap;});}
function providerID(snap){return snap.events.filter(e=>e.kind==='ready').at(-1)?.data.providerSessionId;}
try{
  await start();
  const project=await api('/api/projects','POST',{name:'Persistence test'});
  await api('/api/settings','PUT',{roots:[resolve('.')],defaultModels:{demo:'demo/quick'}});
  const s=await api('/api/sessions','POST',{provider:'demo',cwd,projectId:project.id,title:'Persistent conversation'}),path='/api/sessions/'+s.id;
  await ready(s.id);
  const request={text:'hello before update',requestId:randomUUID()};await api(path+'/prompt','POST',request);
  const before=await ready(s.id),providerId=providerID(before);assert(providerId);
  const automationInput={name:'Persistent manual automation',provider:'demo',model:'demo/quick',cwd,projectId:project.id,prompt:'hello from automation',enabled:true};
  const automation=await api('/api/automations','POST',automationInput),automationRequest={requestId:randomUUID()};
  const automated=await api('/api/automations/'+automation.id+'/run','POST',automationRequest);await ready(automated.session.id);
  const externalProviderId=randomUUID();await mkdir(env.AGENTWORKS_DEMO_STATE_DIR,{recursive:true});
  await writeFile(join(env.AGENTWORKS_DEMO_STATE_DIR,externalProviderId+'.json'),JSON.stringify({cwd,turn:1,model:'demo/steady',history:[{sessionUpdate:'user_message_chunk',content:{type:'text',text:'External history question'}},{sessionUpdate:'agent_message_chunk',content:{type:'text',text:'External history answer'}}]}));
  const imported=await api('/api/sessions/import','POST',{provider:'demo',providerSessionId:externalProviderId,cwd,title:'Persistent external conversation'});
  await ready(imported.id);
  const unused=await api('/api/sessions','POST',{provider:'demo',cwd,title:'Never-used conversation'});
  const unusedBefore=await ready(unused.id),unusedProviderId=providerID(unusedBefore);
  await unlink(join(env.AGENTWORKS_DEMO_STATE_DIR,unusedProviderId+'.json'));
  // Opt-in: two short inference prompts per native harness prove actual conversation context survives.
  const harnesses=[];
  if(process.env.AGENTWORKS_TEST_HARNESS_RESUME==='1'){
    for(const provider of ['claude','codex','opencode']){
      const native=await api('/api/sessions','POST',{provider,cwd,model:provider==='opencode'?'opencode/big-pickle':null,title:'Native resume '+provider});
      await ready(native.id);
      const marker='MEMORY_'+randomUUID().replaceAll('-','').slice(0,12);
      await api('/api/sessions/'+native.id+'/prompt','POST',{text:`Remember this marker for my next message: ${marker}. Reply only SAVED. Do not use tools, read files or run commands.`,requestId:randomUUID()});
      const snap=await ready(native.id);assert(!snap.events.some(e=>e.kind==='error'));
      harnesses.push({id:native.id,provider,marker,providerId:providerID(snap)});
    }
  }
  browser=await chromium.launch();page=await browser.newPage();
  await page.goto(base);await page.evaluate(id=>localStorage.setItem('agentworks-session',id),s.id);await page.reload();
  await page.waitForFunction(()=>document.querySelector('#session-state').textContent==='Ready');
  await page.locator('#prompt').fill('draft survives the update');
  assert.equal((await api('/api/activity')).safeToClose,true);
  await stop();await delay(1200);await start();
  await page.waitForFunction(()=>document.querySelector('#session-state').textContent==='Ready'&&document.querySelector('#connection').textContent==='Connected');
  assert.equal(await page.locator('#prompt').inputValue(),'draft survives the update');
  await api('/api/sessions/'+unused.id+'/resume','POST',{});
  const unusedAfter=await ready(unused.id);assert.notEqual(providerID(unusedAfter),unusedProviderId);assert.equal(unusedAfter.session.id,unused.id);
  await api('/api/sessions/'+unused.id,'DELETE');
  await api('/api/sessions/'+imported.id+'/resume','POST',{});const importedAfter=await ready(imported.id);
  assert.equal(providerID(importedAfter),externalProviderId);
  assert.equal(importedAfter.events.filter(e=>e.kind==='history_imported').length,1);
  assert.equal(importedAfter.events.filter(e=>e.kind==='acp'&&e.data.content?.text==='External history answer').length,1);
  assert((await api('/api/automations')).some(a=>a.id===automation.id&&a.prompt===automationInput.prompt));
  assert.equal((await api('/api/automations/'+automation.id+'/runs'))[0].status,'completed');
  const duplicateAutomation=await api('/api/automations/'+automation.id+'/run','POST',automationRequest);assert.equal(duplicateAutomation.session.id,automated.session.id);
  await api('/api/sessions/'+automated.session.id+'/resume','POST',{});const resumedAutomation=await ready(automated.session.id);assert.equal(resumedAutomation.events.filter(e=>e.kind==='user').length,1);
  const after=await ready(s.id);assert.equal(providerID(after),providerId);assert.equal(after.models.currentValue,'demo/quick');assert.equal(after.session.projectId,project.id);
  assert.equal(after.events.filter(e=>e.kind==='user').length,1);assert.equal((await api('/api/settings')).defaultModels.demo,'demo/quick');
  await api(path+'/prompt','POST',request);assert.equal((await api(path)).events.filter(e=>e.kind==='user').length,1);
  await page.locator('#prompt').fill('hello after update');await page.locator('#send').click();
  await until(async()=>{const snap=await api(path);return snap.session.status==='idle'&&snap.events.filter(e=>e.kind==='turn_end').length===2;});
  await page.waitForFunction(()=>[...document.querySelectorAll('.markdown h3')].some(n=>n.textContent.includes('turn 2')));
  assert.equal(await page.locator('.markdown h3').filter({hasText:'turn 1'}).count(),1,'history must not duplicate during resume');
  for(const native of harnesses){
    await api('/api/sessions/'+native.id+'/resume','POST',{});const resumed=await ready(native.id);assert.equal(providerID(resumed),native.providerId);
    const count=resumed.seq;
    await api('/api/sessions/'+native.id+'/prompt','POST',{text:'What was the marker I asked you to remember? Reply with only the marker. Do not use tools, read files or run commands.',requestId:randomUUID()});
    const remembered=await ready(native.id);
    const text=remembered.events.filter(e=>e.seq>count && e.kind==='acp' && e.data.sessionUpdate==='agent_message_chunk' && e.data.content?.type==='text').map(e=>e.data.content.text).join('');
    assert(text.includes(native.marker),native.provider+' lost the prior conversation context');
    console.log(`PASS: ${native.provider} resumed its original harness session and recalled its marker after server restart`);
  }
  const interrupted={text:'approval',requestId:randomUUID()};await api(path+'/prompt','POST',interrupted);
  await until(async()=>{const snap=await api(path);return snap.approvals.length>0;});
  await api('/api/automations/'+automation.id,'PUT',{...automationInput,prompt:'approval'});
  const interruptedRequest={requestId:randomUUID()},interruptedRun=await api('/api/automations/'+automation.id+'/run','POST',interruptedRequest);
  await until(async()=>{const snap=await api('/api/sessions/'+interruptedRun.session.id);return snap.approvals.length;});
  await stop('SIGKILL');await delay(1200);await start();
  assert.equal((await api('/api/automations/'+automation.id+'/runs'))[0].status,'interrupted');
  const sameInterrupted=await api('/api/automations/'+automation.id+'/run','POST',interruptedRequest);assert.equal(sameInterrupted.session.id,interruptedRun.session.id);
  await api('/api/sessions/'+interruptedRun.session.id+'/resume','POST',{});const recoveredRun=await ready(interruptedRun.session.id);assert.equal(recoveredRun.events.filter(e=>e.kind==='user').length,1);assert.equal(recoveredRun.approvals.length,0);
  await ready(s.id);const recovered=await api(path);
  assert.equal(recovered.approvals.length,0);assert(recovered.events.some(e=>e.kind==='interrupted'));
  assert.equal(providerID(recovered),providerId);
  await api(path+'/prompt','POST',interrupted);assert.equal((await api(path)).session.status,'idle','interrupted request must not execute again');
  await api(path+'/prompt','POST',{text:'hello after crash',requestId:randomUUID()});await ready(s.id);
  await until(()=>page.locator('#messages').innerText().then(text=>text.includes('turn 4')));
  await page.screenshot({path:'.cache/prototype-persistent-session.png'});
  // An unavailable harness session must preserve the transcript and fail, never create a fresh context.
  const demoFile=join(env.AGENTWORKS_DEMO_STATE_DIR,providerId+'.json');
  await stop();await rename(demoFile,demoFile+'.saved');await start();
  await until(async()=>{const snap=await api(path);return snap.session.status==='error';});
  const failed=await api(path);assert.equal(providerID(failed),providerId);assert.equal(failed.events.filter(e=>e.kind==='user').length,4);
  await rename(demoFile+'.saved',demoFile);await api(path+'/resume','POST',{});await ready(s.id);
  // Deletion must remain deleted on disk; other saved sessions must survive it.
  await api(path,'DELETE');await stop();await start();
  assert(!(await api('/api/sessions')).some(row=>row.id===s.id));
  console.log('PASS: durable transcript/model/project/settings, imported history and automation runs, browser reconnect and draft, same harness context, crash/approval recovery, idempotency, missing-context failure and permanent deletion');
}finally{
  await browser?.close();if(server)await stop();
  await removeFixture(dir);
}
