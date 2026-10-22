import {test,expect} from '@playwright/test';
import {spawn} from 'node:child_process';
import {mkdtemp,mkdir,rm} from 'node:fs/promises';
import {resolve,join} from 'node:path';

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
  await expect(page.locator('#automation-trigger')).toHaveText('I click Run now');
  await expect(page.locator('#automation-recipe select, #automation-recipe input[type=time]')).toHaveCount(0);
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
