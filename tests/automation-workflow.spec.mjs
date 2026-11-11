import {test,expect} from '@playwright/test';
import {spawn} from 'node:child_process';
import {mkdtemp,mkdir,rm,readFile,writeFile} from 'node:fs/promises';
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
    PATH:join(fixture,'bin')+':'+process.env.PATH,AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:port,AGENTWORKS_ROOTS:fixture,
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
  await mkdir(join(fixture,'bin'));await writeFile(join(fixture,'bin/gh'),'#!/bin/sh\nexit 1\n',{mode:0o755});
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

test('downtime applies the missed-run policy to the latest occurrence only',async({page})=>{
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
  const alert=(await api('/api/notifications/inbox')).alerts.find(a=>a.automationId===skip.id);
  expect(alert).toMatchObject({event:'automationIssue',title:'Skip missed',dismissed:false});
  await page.goto('/dashboard');await page.locator('#nav-notifications').click();
  const card=page.locator('.notification-card').filter({hasText:'Skip missed'});
  await expect(card).toBeVisible();await card.getByRole('link').click();
  await expect(page.locator('.automation-item.selected')).toContainText('Skip missed');
  expect((await api('/api/notifications/inbox')).alerts.some(a=>a.id===alert.id)).toBe(true);

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
  // The run is listed once its session starts; its slot (and the completion) commit just after.
  await expect.poll(async()=>(await occurrences(keep.id))[0]?.scheduledFor,{timeout:15000}).toBe(slot);
  const runs=await occurrences(keep.id);expect(runs.length).toBe(1);const [late]=runs;
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


test('GitHub form generates the saved star file and preserves custom source until explicit regeneration',async({page})=>{
  const project=await api('/api/projects',{name:'Generated review tests'});
  await stopServer();const path=join(fixture,'catalog.json'),catalog=JSON.parse(await readFile(path,'utf8'));
  catalog.github={...(catalog.github||{}),connections:[{host:'ghe.example.com',login:'reviewer',enabled:true}]};
  await writeFile(path,JSON.stringify(catalog));await startServer();
  await page.setViewportSize({width:1280,height:1100});await page.goto('/?view=automations');await page.locator('#nav-automations').click();await page.locator('#new-automation').click();
  await page.locator('#automation-name').fill('Review incoming PRs');await page.locator('#automation-provider').selectOption('demo');await page.locator('#automation-directory').fill(fixture);await page.locator('#automation-project').selectOption(project.id);
  await page.getByLabel('Step 1 instructions').fill('Review correctness; leave draft comments.');await page.locator('#add-automation-step').click();await page.getByLabel('Step 2 instructions').fill('Report the findings.');
  await page.locator('#automation-trigger').selectOption('githubReview');
  await page.locator('#automation-review-host').fill('ghe.example.com');await page.locator('#automation-review-repo').fill('team/service');await page.locator('#automation-review-account').fill('reviewer');await page.locator('#automation-review-assigned').uncheck();await page.locator('#automation-enabled').uncheck();
  await page.locator('#automation-dialog').evaluate(n=>n.scrollTop=0);await page.screenshot({path:'.cache/automation-form-generates-star-20261105.png'});
  await page.locator('#automation-preview-script').click();await expect(page.locator('#automation-source-panel')).toBeVisible();
  const generated=await page.locator('#automation-source-code').inputValue();
  await writeFile(resolve('.cache/github-review-generated-20261105.star'),generated);
  expect(generated).toContain('agent.register_github_trigger(');expect(generated).toContain('settings = SETTINGS');expect(generated).toContain('"ghe.example.com"');expect(generated).toContain('events = ["pr.review_requested"]');expect(generated).not.toContain('parameter(');
  await page.locator('#save-automation').click();await expect(page.locator('#automation-dialog')).toBeHidden();
  let def=(await api('/api/automations')).find(d=>d.name==='Review incoming PRs');
  expect(def.workflow.trigger.script).toBe(`automation_${def.id}.star`);expect(def.workflow.trigger.parameters).toEqual({});expect(def.enabled).toBe(false);
  let file=await api(`/api/automations/${def.id}/script`);expect(file.source).toBe(generated);expect(file.generated).toBe(true);expect(file.form.steps).toHaveLength(2);
  await expect(page.locator('#automation-detail h1')).toHaveText('Review incoming PRs');
  await page.screenshot({path:'.cache/github-automation-added-desktop-20261106.png'});
  expect((await api('/api/scripts')).scripts.some(script=>script.name===file.name)).toBe(false);
  await page.locator('#nav-settings').click();await page.locator('#settings-tab-parsers').click();
  await expect(page.locator(`[data-script-entry="${file.name}"]`)).toHaveCount(0);await page.screenshot({path:'.cache/github-automation-separated-hooks-20261106.png'});await page.locator('#dismiss-settings').click();
  await expect(page.locator('#run-automation')).toBeHidden();await page.locator('#automation-detail').getByRole('button',{name:'View .star',exact:true}).click();await expect(page.locator('#automation-source-code')).toHaveValue(generated);
  const downloadEvent=page.waitForEvent('download');await page.locator('#automation-download-script').click();const download=await downloadEvent;expect(download.suggestedFilename()).toBe(`automation_${def.id}.star`);expect(await readFile(await download.path(),'utf8')).toBe(generated);
  const custom=generated+'\n# Custom routing policy: preserve this edit.\n';await page.locator('#automation-source-code').fill(custom);await expect(page.locator('#automation-source-note')).toContainText('Custom script');
  await page.locator('#automation-review-repo').fill('team/form-change-must-not-win');await page.locator('#save-automation').click();await expect(page.locator('#automation-dialog')).toBeHidden();
  file=await api(`/api/automations/${def.id}/script`);expect(file.source).toBe(custom);expect(file.generated).toBe(false);expect(file.form.repo).toBe('team/service');
  await page.locator('#automation-detail').getByRole('button',{name:'Edit',exact:true}).click();await expect(page.locator('#automation-source-code')).toHaveValue(custom);await expect(page.getByLabel('Step 2 instructions')).toHaveValue('Report the findings.');
  await page.locator('#automation-review-repo').fill('team/regenerated');await page.locator('#automation-review-existing').check();await page.locator('#automation-regenerate-script').click();await expect(page.locator('#automation-source-note')).toContainText('Generated from this form');
  expect(await page.locator('#automation-source-code').inputValue()).not.toContain('# Custom routing policy');
  await page.locator('#save-automation').click();await expect(page.locator('#automation-dialog')).toBeHidden();
  file=await api(`/api/automations/${def.id}/script`);expect(file.form.repo).toBe('team/regenerated');expect(file.form.includeExisting).toBe(true);expect(file.generated).toBe(true);
  await page.locator('#automation-detail').getByRole('button',{name:'View .star',exact:true}).click();await expect(page.locator('#automation-source-code')).toHaveValue(file.source);await page.locator('#automation-source-code').fill(file.source+'\ndef broken syntax\n');await page.locator('#save-automation').click();await expect(page.locator('#automation-error')).not.toBeEmpty();expect((await api(`/api/automations/${def.id}/script`)).source).toBe(file.source);
  await page.locator('#dismiss-automation').click();
  const extended=file.source.replace('"role": None','"role": "review-role"').replace('"title": "Review incoming PRs"','"title": None');
  await api('/api/automations/'+def.id,{name:def.name,enabled:false,source:extended},'PUT');
  expect((await api(`/api/automations/${def.id}/script`)).generated).toBe(true);
  await page.locator('#automation-detail').getByRole('button',{name:'Edit',exact:true}).click();await expect(page.getByLabel('Step 2 instructions')).toHaveValue('Report the findings.');await expect(page.locator('#save-automation')).toBeEnabled();await page.locator('#save-automation').click();await expect(page.locator('#automation-dialog')).toBeHidden();
  expect((await api(`/api/automations/${def.id}/script`)).source).toBe(extended);
  await api('/api/automations/'+def.id,undefined,'DELETE');
});


test('automation imports and source edits own entries while hook settings cannot manage automation files',async({page})=>{
  const project=await api('/api/projects',{name:'Imported automation tests'});
  const form={host:'ghe.example.com',repo:'team/service',account:'reviewer',settings:{provider:'demo',model:null,cwd:fixture,projectId:project.id,hostId:'local',title:'Handwritten review'},steps:[{kind:'agent',text:'Instructions from the source file.'}]};
  const source=(await api('/api/automations/preview-script',form)).source;
  await page.goto('/?view=automations');await page.locator('#nav-automations').click();await expect(page.locator('#import-automation')).toBeVisible();
  await page.locator('#import-automation-file').setInputFiles({name:'handwritten_review.star',mimeType:'text/plain',buffer:Buffer.from(source)});
  await expect(page.locator('#automation-dialog')).toBeVisible();await expect(page.locator('#automation-source-code')).toHaveValue(source);await expect(page.locator('#automation-enabled')).not.toBeChecked();
  await page.locator('#save-automation').click();await expect(page.locator('#automation-dialog')).toBeHidden();
  let def=(await api('/api/automations')).find(d=>d.workflow?.trigger?.script==='handwritten_review.star');expect(def.name).toBe('Handwritten review');expect(def.enabled).toBe(false);
  await expect(page.locator('#automation-detail h1')).toHaveText('Handwritten review');await expect(page.locator('#automation-detail .recipe-trigger')).toContainText('ghe.example.com/team/service');
  await page.screenshot({path:'.cache/automation-imported-and-owned-20261106.png'});
  expect((await api('/api/scripts')).scripts.some(s=>s.name==='handwritten_review.star')).toBe(false);
  const hook='load("@agentworks//github.star", "GithubContext")\ndef hook(ctx: GithubContext, event: dict[str, typing.Any]) -> None:\n    return None\nagent.register_github(hook)\n';
  for(const [suffix,body,method] of [['',{source:hook},'PUT'],['/enabled',{enabled:true},'PUT'],['/priority',{priority:1},'PUT'],['',undefined,'DELETE']])expect((await call('/api/scripts/handwritten_review.star'+suffix,body,method)).status).toBe(409);
  expect((await call('/api/scripts/sneaky.star',{source},'PUT')).status).toBe(400);
  expect((await call('/api/automations/inspect-script',{name:'mixed.star',source:source+'\n'+hook})).status).toBe(400);
  expect((await call('/api/automations',{source:hook,enabled:false,scriptName:'wrong_kind.star'})).status).toBe(400);
  const edited=source.replace('Handwritten review','Renamed from source').replace('Instructions from the source file.','Updated file instructions.');
  await api('/api/automations/'+def.id,{source:edited,enabled:false},'PUT');await page.reload();await page.locator('#nav-automations').click();
  await expect(page.locator('#automation-detail h1')).toHaveText('Renamed from source');await expect(page.locator('#automation-detail')).toContainText('Updated file instructions.');
  const multi=edited+'\nagent.register_github_trigger(review_requested, name="also", host="ghe.example.com", repo="team/service", account="reviewer", events=["pr.review_requested"], settings=SETTINGS, instructions=instruction_steps(STEPS), steps=STEPS)\n';
  await api('/api/automations/'+def.id,{source:multi,enabled:false},'PUT');await page.reload();await page.locator('#nav-automations').click();
  def=(await api('/api/automations')).find(d=>d.id===def.id);expect(def.registrations).toHaveLength(2);await expect(page.locator('.automation-item')).toHaveCount(1);await expect(page.locator('.automation-item small')).toContainText('2 trigger(s)');
  await page.locator('#automation-detail').getByRole('button',{name:'Edit',exact:true}).click();await expect(page.locator('#automation-source-code')).toHaveValue(multi);
  page.once('dialog',dialog=>dialog.accept());await page.locator('#delete-automation').click();await expect(page.locator('#automation-dialog')).toBeHidden();await expect(page.locator('.automation-item')).toHaveCount(0);
  expect((await api('/api/automations')).some(d=>d.id===def.id)).toBe(false);expect((await call(`/api/automations/${def.id}/script`)).status).toBe(404);expect(await api(`/api/automations/${def.id}/runs`)).toEqual([]);
});
