import {test,expect} from '@playwright/test';
import {spawn} from 'node:child_process';
import {mkdtemp,mkdir,rm} from 'node:fs/promises';
import {resolve,join} from 'node:path';
import {DatabaseSync} from 'node:sqlite';

// Own server, catalog, database and demo state on a dedicated port; never the live service.
const port=process.env.AGENTWORKS_WORKFLOW_PORT||'4651',base='http://127.0.0.1:'+port;
test.use({baseURL:base});
let fixture,server,log='',exited=false;

async function call(path,body,method){
  const response=await fetch(base+path,{method:method||(body?'POST':'GET'),headers:{Origin:base,'Content-Type':'application/json'},body:body?JSON.stringify(body):undefined});
  return {status:response.status,body:await response.json().catch(()=>null)};
}
async function api(path,body,method){const result=await call(path,body,method);expect(result.status,JSON.stringify(result.body)).toBe(200);return result.body;}
async function startServer(){
  exited=false;log='';
  server=spawn(process.env.AGENTWORKS_BIN||resolve('target/debug/agentworks'),[],{env:{...process.env,
    AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:port,AGENTWORKS_ROOTS:fixture,
    AGENTWORKS_CATALOG_PATH:join(fixture,'catalog.json'),AGENTWORKS_DB_PATH:join(fixture,'sessions.sqlite3'),AGENTWORKS_DEMO_STATE_DIR:join(fixture,'demo')},stdio:['ignore','pipe','pipe']});
  server.on('error',e=>{log+=e.message;exited=true;});server.on('exit',()=>exited=true);
  for(const stream of [server.stdout,server.stderr])stream.on('data',s=>{log+=s.toString();});
  for(let i=0;i<100;i++){
    if(exited)throw new Error(log);
    try{if((await fetch(base+'/api/status')).ok)break;}catch{}
    await new Promise(r=>setTimeout(r,100));
    if(i===99)throw new Error('Isolated server readiness timed out: '+log);
  }
}
async function stopServer(){
  if(server&&!exited){const stopped=new Promise(r=>server.once('exit',r));server.kill('SIGTERM');await stopped;}
}
const recipe=steps=>({version:1,trigger:{kind:'manual'},steps:steps.map(text=>({kind:'agent',text}))});
test.beforeAll(async()=>{
  await mkdir('.cache',{recursive:true});fixture=await mkdtemp(resolve('.cache/workflow-'));
  await startServer();
});
test.afterAll(async()=>{
  await stopServer();
  if(fixture)await rm(fixture,{recursive:true,force:true});
});

test('API stores typed manual recipes and rejects unsupported triggers and steps',async()=>{
  const def=await api('/api/automations',{name:'API recipe',provider:'demo',cwd:fixture,workflow:recipe([' Run tests ','Summarise failures'])});
  expect(def.workflow).toEqual(recipe(['Run tests','Summarise failures']));
  expect(def.prompt).toMatch(/^Complete these steps in order\./);
  expect(def.prompt.indexOf('Step 1:\nRun tests')).toBeLessThan(def.prompt.indexOf('Step 2:\nSummarise failures'));
  const single=await api('/api/automations/'+def.id,{name:'API recipe',provider:'demo',cwd:fixture,workflow:recipe(['/status'])},'PUT');
  expect(single.prompt).toBe('/status');
  for(const workflow of [
    {version:1,trigger:{kind:'schedule',cron:'0 * * * *'},steps:[{kind:'agent',text:'x'}]},
    {version:1,trigger:{kind:'schedule',timeZone:'Mars/Olympus',at:'09:00',days:[1]},steps:[{kind:'agent',text:'x'}]},
    {version:1,trigger:{kind:'schedule',timeZone:'UTC',at:'25:00',days:[1]},steps:[{kind:'agent',text:'x'}]},
    {version:1,trigger:{kind:'schedule',timeZone:'UTC',at:'09:00',days:[]},steps:[{kind:'agent',text:'x'}]},
    {version:1,trigger:{kind:'github',event:'push'},steps:[{kind:'agent',text:'x'}]},
    {version:1,trigger:{kind:'manual'},steps:[{kind:'shell',text:'echo hi'}]},
    {version:1,trigger:{kind:'manual'},steps:[{kind:'agent',text:'x',approval:true}]},
    recipe([]),recipe(['ok','  ']),recipe(Array.from({length:21},()=>'x')),
  ]){
    const result=await call('/api/automations',{name:'Bad',provider:'demo',cwd:fixture,workflow});
    expect([400,422],JSON.stringify(workflow)).toContain(result.status);
  }
  expect((await api('/api/automations')).filter(d=>d.name==='Bad')).toEqual([]);
  await api('/api/automations/'+def.id,undefined,'DELETE');
});

test('recipe editor builds ordered steps that persist through restart and run once',async({page})=>{
  // A pre-recipe definition opens as a single step.
  const legacy=await api('/api/automations',{name:'Legacy prompt',provider:'demo',cwd:fixture,prompt:'/status'});
  expect(legacy.workflow).toBeUndefined();
  await page.goto('/?view=automations');await page.locator('#nav-automations').click();
  await page.locator('.automation-item',{hasText:'Legacy prompt'}).click();
  await expect(page.locator('.automation-item.selected small')).toContainText('1 step');
  await expect(page.locator('#automation-detail .automation-recipe-view li')).toHaveText(['/status']);
  await page.locator('#automation-detail').getByRole('button',{name:'Edit'}).click();
  await expect(page.locator('#automation-steps textarea')).toHaveCount(1);
  await expect(page.getByLabel('Step 1 instructions')).toHaveValue('/status');
  await expect(page.getByRole('button',{name:'Remove step 1'})).toBeDisabled();
  await page.locator('#dismiss-automation').click();

  await page.locator('#new-automation').click();
  await expect(page.locator('#automation-trigger')).toHaveValue('manual');
  await expect(page.locator('#automation-schedule')).toBeHidden();
  await page.locator('#automation-name').fill('Ordered recipe');await page.locator('#automation-provider').selectOption('demo');await page.locator('#automation-directory').fill(fixture);
  await page.getByLabel('Step 1 instructions').fill('Inspect the repository');
  await page.locator('#add-automation-step').click();await expect(page.getByLabel('Step 2 instructions')).toBeFocused();
  await page.getByLabel('Step 2 instructions').fill('Delete me');
  await page.locator('#add-automation-step').click();await page.getByLabel('Step 3 instructions').fill('Report findings');
  await page.getByRole('button',{name:'Move up step 3'}).click();
  expect(await page.locator('#automation-steps textarea').evaluateAll(n=>n.map(t=>t.value))).toEqual(['Inspect the repository','Report findings','Delete me']);
  await expect(page.getByRole('button',{name:'Move down step 3'})).toBeDisabled();
  await page.getByRole('button',{name:'Remove step 3'}).click();
  // Blank steps are blocked by the form before reaching the server.
  await page.locator('#add-automation-step').click();await page.locator('#save-automation').click();
  await expect(page.locator('#automation-dialog')).toBeVisible();
  await page.getByRole('button',{name:'Remove step 3'}).click();
  await page.locator('#save-automation').click();
  await expect(page.locator('#automation-dialog')).toBeHidden();
  await expect(page.locator('#automation-detail h1')).toHaveText('Ordered recipe');
  await expect(page.locator('#automation-detail .automation-recipe-view li')).toHaveText(['Inspect the repository','Report findings']);
  await expect(page.locator('#automation-detail .automation-recipe-view')).toContainText('2 steps in order, in one agent turn');
  const def=(await api('/api/automations')).find(d=>d.name==='Ordered recipe');
  expect(def.workflow).toEqual(recipe(['Inspect the repository','Report findings']));
  await page.screenshot({path:'.cache/workflow-recipe-detail.png'});

  await stopServer();await startServer();
  await page.reload();await page.locator('#nav-automations').click();
  await page.locator('.automation-item',{hasText:'Ordered recipe'}).click();
  await expect(page.locator('#automation-detail .automation-recipe-view li')).toHaveText(['Inspect the repository','Report findings']);
  await page.locator('#automation-detail').getByRole('button',{name:'Edit'}).click();
  expect(await page.locator('#automation-steps textarea').evaluateAll(n=>n.map(t=>t.value))).toEqual(['Inspect the repository','Report findings']);
  await page.screenshot({path:'.cache/workflow-recipe-editor.png'});
  await page.locator('#dismiss-automation').click();

  // A run is a single session whose queued prompt is the compiled ordered checklist.
  await page.locator('#run-automation').click();
  await expect(page.locator('#automation-source')).toContainText('Ordered recipe');
  await expect.poll(async()=>(await api('/api/automations/'+def.id+'/runs')).length).toBe(1);
  const [run]=await api('/api/automations/'+def.id+'/runs');
  await expect.poll(async()=>JSON.stringify((await api('/api/sessions/'+run.sessionId)).events),{timeout:15000}).toContain('Step 2:\\nReport findings');
  for(const id of [def.id,legacy.id])await api('/api/automations/'+id,undefined,'DELETE');
  await api('/api/sessions/'+run.sessionId,undefined,'DELETE');
});

const schedule=(at,extra={})=>({version:1,trigger:{kind:'schedule',timeZone:'UTC',at,days:[1,2,3,4,5,6,7],...extra},steps:[{kind:'agent',text:'/status'}]});
const hhmm=ms=>new Date(ms).toISOString().slice(11,16);
const minute=ms=>Math.floor(ms/60000)*60000;
const scheduledRuns=async id=>(await api('/api/automations/'+id+'/runs')).filter(r=>r.scheduledFor);

test('schedule editor saves a weekly trigger and pausing clears the next run',async({page})=>{
  await page.goto('/?view=automations');await page.locator('#nav-automations').click();
  await page.locator('#new-automation').click();
  await page.locator('#automation-name').fill('Weekday review');await page.locator('#automation-provider').selectOption('demo');await page.locator('#automation-directory').fill(fixture);
  await page.getByLabel('Step 1 instructions').fill('Review');
  await page.locator('#automation-trigger').selectOption('schedule');
  await expect(page.locator('#automation-schedule')).toBeVisible();
  await expect(page.locator('#automation-days input:checked')).toHaveCount(5);
  await page.locator('#automation-at').fill('07:45');await page.locator('#automation-time-zone').fill('Europe/Berlin');
  await page.locator('#automation-days').getByLabel('Fri').uncheck();await page.locator('#automation-missed').selectOption('runOnce');
  await page.screenshot({path:'.cache/workflow-schedule-editor.png'});
  await page.locator('#save-automation').click();await expect(page.locator('#automation-dialog')).toBeHidden();
  await expect(page.locator('#automation-detail .recipe-trigger')).toHaveText('Mon, Tue, Wed, Thu at 07:45 (Europe/Berlin)');
  await expect(page.locator('#automation-detail .automation-badge')).toContainText('Next run');
  await expect(page.locator('.automation-item.selected small')).toContainText('Scheduled');
  const def=(await api('/api/automations')).find(d=>d.name==='Weekday review');
  expect(def.workflow.trigger).toEqual({kind:'schedule',timeZone:'Europe/Berlin',at:'07:45',days:[1,2,3,4],missed:'runOnce'});
  expect(def.nextRun).toBeGreaterThan(Date.now());
  const berlin=new Intl.DateTimeFormat('en-GB',{timeZone:'Europe/Berlin',weekday:'short',hour:'2-digit',minute:'2-digit',hour12:false}).format(def.nextRun);
  expect(berlin).toMatch(/^(Mon|Tue|Wed|Thu) 07:45$/);
  // Reopening restores the schedule fields.
  await page.locator('#automation-detail').getByRole('button',{name:'Edit'}).click();
  await expect(page.locator('#automation-at')).toHaveValue('07:45');await expect(page.locator('#automation-time-zone')).toHaveValue('Europe/Berlin');
  await expect(page.locator('#automation-missed')).toHaveValue('runOnce');await expect(page.locator('#automation-days input:checked')).toHaveCount(4);
  await page.locator('#automation-enabled').uncheck();await page.locator('#save-automation').click();await expect(page.locator('#automation-dialog')).toBeHidden();
  await expect(page.locator('#automation-detail .automation-badge')).toHaveText('Paused');
  expect((await api('/api/automations')).find(d=>d.id===def.id).nextRun).toBeNull();
  await api('/api/automations/'+def.id,undefined,'DELETE');
});

test('scheduled occurrences dispatch once and stay deduplicated across restart',async()=>{
  test.setTimeout(180000);
  // The next whole minute, leaving time to save before it.
  const slot=minute(Date.now()+(new Date().getUTCSeconds()>45?120000:60000));
  const def=await api('/api/automations',{name:'Every minute soon',provider:'demo',cwd:fixture,workflow:schedule(hhmm(slot))});
  expect(def.nextRun).toBe(slot);
  await expect.poll(async()=>(await scheduledRuns(def.id)).length,{timeout:slot-Date.now()+30000,intervals:[1000]}).toBe(1);
  const [run]=await scheduledRuns(def.id);
  expect(run.scheduledFor).toBe(slot);expect(run.sessionId).toBeTruthy();expect(run.created-slot).toBeLessThan(60000);
  expect((await api('/api/sessions/'+run.sessionId)).session.title).toBe('Every minute soon · Scheduled run');
  // Its request ID is durable: a restart within the grace window does not start it again.
  await stopServer();await startServer();
  await new Promise(r=>setTimeout(r,7000));
  expect((await scheduledRuns(def.id)).map(r=>r.id)).toEqual([run.id]);
  // Manual runs remain separate from the schedule.
  await api('/api/automations/'+def.id,{...def,workflow:schedule(hhmm(slot)),enabled:false},'PUT');
  expect((await call('/api/automations/'+def.id+'/run',{requestId:'paused'})).status).toBe(409);
  await api('/api/automations/'+def.id,undefined,'DELETE');
  await api('/api/sessions/'+run.sessionId,undefined,'DELETE');
});

test('downtime applies the missed-run policy to the latest occurrence only',async()=>{
  test.setTimeout(60000);
  // An occurrence two hours ago, while the cursor says the server last ran two days ago.
  const slot=minute(Date.now()-7200000);
  const skip=await api('/api/automations',{name:'Skip missed',provider:'demo',cwd:fixture,workflow:schedule(hhmm(slot))});
  const once=await api('/api/automations',{name:'Run missed once',provider:'demo',cwd:fixture,workflow:schedule(hhmm(slot),{missed:'runOnce'})});
  const paused=await api('/api/automations',{name:'Paused missed',provider:'demo',cwd:fixture,enabled:false,workflow:schedule(hhmm(slot),{missed:'runOnce'})});
  await stopServer();
  const db=new DatabaseSync(join(fixture,'sessions.sqlite3'));
  const update=db.prepare('UPDATE automation_schedules SET cursor=? WHERE automation_id=?');
  for(const def of [skip,once])expect(update.run(Date.now()-2*86400000,def.id).changes).toBe(1);
  // Paused definitions are not armed, so no downtime is attributed to them.
  expect(db.prepare('SELECT count(*) AS n FROM automation_schedules WHERE automation_id=?').get(paused.id).n).toBe(0);
  db.close();
  await startServer();
  await expect.poll(async()=>(await scheduledRuns(skip.id)).length,{timeout:15000}).toBe(1);
  const [missed]=await scheduledRuns(skip.id);
  expect(missed).toMatchObject({status:'missed',scheduledFor:slot,sessionId:null});
  expect(missed.detail).toContain('Earlier missed occurrences were not run.');
  await expect.poll(async()=>(await scheduledRuns(once.id)).length,{timeout:15000}).toBe(1);
  const [late]=await scheduledRuns(once.id);
  expect(late.scheduledFor).toBe(slot);expect(late.sessionId).toBeTruthy();
  await expect.poll(async()=>(await scheduledRuns(once.id))[0].detail).toContain('Started late under the run-once policy.');
  // Resuming the paused definition never catches up on time spent paused.
  await api('/api/automations/'+paused.id,{name:'Paused missed',provider:'demo',cwd:fixture,enabled:true,workflow:schedule(hhmm(slot),{missed:'runOnce'})},'PUT');
  await new Promise(r=>setTimeout(r,7000));
  expect(await scheduledRuns(paused.id)).toEqual([]);
  expect((await scheduledRuns(skip.id)).length).toBe(1);expect((await scheduledRuns(once.id)).length).toBe(1);
  for(const def of [skip,once,paused])await api('/api/automations/'+def.id,undefined,'DELETE');
  await api('/api/sessions/'+late.sessionId,undefined,'DELETE');
});

const utcLocal=ms=>new Date(ms).toISOString().slice(0,16);
const oneTime=(at,extra={})=>({version:1,trigger:{kind:'once',timeZone:'UTC',at,...extra},steps:[{kind:'agent',text:'Once'}]});
const occurrences=async id=>(await api('/api/automations/'+id+'/runs'));

test('one-time triggers are validated and edited like schedules',async({page})=>{
  for(const at of [utcLocal(Date.now()-60000),utcLocal(Date.now()+400*86400000)])
    expect((await call('/api/automations',{name:'Bad time',provider:'demo',cwd:fixture,workflow:oneTime(at)})).status).toBe(400);
  // A paused one-time definition may keep a past time; resuming needs a new one.
  const paused=await api('/api/automations',{name:'Past but paused',provider:'demo',cwd:fixture,enabled:false,workflow:oneTime(utcLocal(Date.now()-3600000))});
  expect((await call('/api/automations/'+paused.id,{name:'Past but paused',provider:'demo',cwd:fixture,enabled:true,workflow:oneTime(paused.workflow.trigger.at)},'PUT')).status).toBe(400);
  await api('/api/automations/'+paused.id,undefined,'DELETE');
  await page.goto('/?view=automations');await page.locator('#nav-automations').click();
  await page.locator('#new-automation').click();
  await page.locator('#automation-name').fill('Release check');await page.locator('#automation-provider').selectOption('demo');await page.locator('#automation-directory').fill(fixture);
  await page.getByLabel('Step 1 instructions').fill('Check the release');
  await page.locator('#automation-trigger').selectOption('once');
  await expect(page.locator('#automation-date')).toBeVisible();await expect(page.locator('#automation-days')).toBeHidden();
  const day=new Date(Date.now()+3*86400000).toISOString().slice(0,10);
  await page.locator('#automation-date').fill(day);await page.locator('#automation-at').fill('06:15');await page.locator('#automation-time-zone').fill('UTC');
  await page.locator('#automation-after-run').selectOption('delete');
  await page.screenshot({path:'.cache/workflow-once-editor.png'});
  await page.locator('#save-automation').click();await expect(page.locator('#automation-dialog')).toBeHidden();
  await expect(page.locator('#automation-detail .recipe-trigger')).toHaveText(`Once on ${day} at 06:15 (UTC), then delete it`);
  await expect(page.locator('#automation-detail .workspace-eyebrow')).toHaveText('ONE-TIME AUTOMATION');
  await expect(page.locator('.automation-item.selected small')).toContainText('Once');
  const def=(await api('/api/automations')).find(d=>d.name==='Release check');
  expect(def.workflow.trigger).toEqual({kind:'once',timeZone:'UTC',at:`${day}T06:15`,missed:'skip',afterRun:'delete'});
  expect(def.nextRun).toBe(Date.parse(`${day}T06:15:00Z`));
  await page.locator('#automation-detail').getByRole('button',{name:'Edit'}).click();
  await expect(page.locator('#automation-date')).toHaveValue(day);await expect(page.locator('#automation-at')).toHaveValue('06:15');
  await expect(page.locator('#automation-after-run')).toHaveValue('delete');
  await page.locator('#dismiss-automation').click();
  await api('/api/automations/'+def.id,undefined,'DELETE');
});

test('a one-time occurrence is handled once, then completes or deletes the definition',async({page})=>{
  test.setTimeout(60000);
  const future=utcLocal(Date.now()+3600000);
  const keep=await api('/api/automations',{name:'Once then complete',provider:'demo',cwd:fixture,workflow:oneTime(future,{missed:'runOnce'})});
  const drop=await api('/api/automations',{name:'Once then delete',provider:'demo',cwd:fixture,workflow:oneTime(future,{afterRun:'delete'})});
  // Simulate downtime across the occurrence: it is now two hours past.
  await stopServer();
  const past=utcLocal(Date.now()-7200000),slot=Date.parse(past+':00Z');
  const db=new DatabaseSync(join(fixture,'sessions.sqlite3'));
  for(const def of [keep,drop]){
    db.prepare("UPDATE automations SET definition=json_set(definition,'$.workflow.trigger.at',?) WHERE id=?").run(past,def.id);
    db.prepare('UPDATE automation_schedules SET cursor=? WHERE automation_id=?').run(slot-3600000,def.id);
  }
  db.close();
  await startServer();
  await expect.poll(async()=>(await occurrences(keep.id)).length,{timeout:15000}).toBe(1);
  const [late]=await occurrences(keep.id);
  expect(late.scheduledFor).toBe(slot);expect(late.sessionId).toBeTruthy();
  const done=(await api('/api/automations')).find(d=>d.id===keep.id);
  expect(done).toMatchObject({enabled:false,nextRun:null,revision:2});expect(done.completed).toBeGreaterThan(slot);
  await expect.poll(async()=>(await occurrences(drop.id)).length,{timeout:15000}).toBe(1);
  expect((await occurrences(drop.id))[0]).toMatchObject({status:'missed',scheduledFor:slot,sessionId:null});
  expect((await api('/api/automations')).some(d=>d.id===drop.id)).toBe(false);
  // Nothing fires again after a restart.
  await stopServer();await startServer();await new Promise(r=>setTimeout(r,7000));
  expect((await occurrences(keep.id)).length).toBe(1);
  await page.goto('/?view=automations');await page.locator('#nav-automations').click();
  await page.locator('.automation-item',{hasText:'Once then complete'}).click();
  await expect(page.locator('.automation-item.selected small')).toContainText('Completed');
  await expect(page.locator('#automation-detail .automation-badge')).toContainText('Completed');
  // Rescheduling clears completion.
  const again=await api('/api/automations/'+keep.id,{name:'Once then complete',provider:'demo',cwd:fixture,workflow:oneTime(utcLocal(Date.now()+7200000))},'PUT');
  expect(again.enabled).toBe(true);expect(again.completed).toBeUndefined();expect(again.nextRun).toBeGreaterThan(Date.now());
  await api('/api/automations/'+keep.id,undefined,'DELETE');
  await api('/api/sessions/'+late.sessionId,undefined,'DELETE');
});
