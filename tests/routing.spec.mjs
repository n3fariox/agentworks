import {serverReady} from './support/cleanup.mjs';
import {test,expect} from '@playwright/test';
import {spawn} from 'node:child_process';
import {mkdtemp,mkdir,rm} from 'node:fs/promises';
import {resolve,join} from 'node:path';

const base='http://127.0.0.1:4496';
test.use({baseURL:base});
let fixture,server,exited,log='';

async function api(path,body,method,status=200){
  const r=await fetch(base+path,{method:method||(body?'POST':'GET'),headers:{Origin:base,'Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body)});
  expect(r.status,await r.clone().text()).toBe(status);
  return r.json().catch(()=>null);
}

test.beforeAll(async()=>{
  await mkdir('.cache',{recursive:true});
  fixture=await mkdtemp(resolve('.cache/routing-ui-'));
  server=spawn(resolve('target/debug/agentworks'),[],{
    env:{...process.env,AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:'4496',AGENTWORKS_ROOTS:fixture,AGENTWORKS_CATALOG_PATH:join(fixture,'catalog.json'),AGENTWORKS_DB_PATH:join(fixture,'sessions.sqlite3'),AGENTWORKS_DEMO_STATE_DIR:join(fixture,'demo')},
    stdio:['ignore','pipe','pipe'],
  });
  server.on('error',e=>{log+=e.message;exited=true;});
  server.on('exit',()=>exited=true);
  for(const stream of [server.stdout,server.stderr])stream.on('data',s=>log+=s);
  for(let i=0;i<100;i++){if(exited)throw Error(log);if(await serverReady(server,base))return;await new Promise(r=>setTimeout(r,100));}
  throw Error('Server readiness timed out: '+log);
});

test.afterAll(async()=>{
  if(server&&!exited){const stopped=new Promise(r=>server.once('exit',r));server.kill('SIGTERM');await stopped;}
  if(fixture)await rm(fixture,{recursive:true,force:true});
});

test('global and project session router settings drive new session dialog',async({page})=>{
  const errors=[];page.on('pageerror',e=>errors.push(e.message));
  await page.goto('/');

  // Configure global session router via Settings -> Session router.
  await page.locator('#nav-settings').click();
  await page.locator('#settings-tab-routing').click();
  await page.locator('#routing-mode').selectOption('default');
  await page.locator('#routing-approach').selectOption('quota_round_robin');
  await page.locator('#add-routing-candidate').click();
  const row=page.locator('.routing-candidate-row').first();
  await row.locator('select').first().selectOption('demo');
  await row.locator('input[placeholder="Default model"]').fill('demo/quick');
  await page.locator('#save-settings').click();
  await expect(page.locator('#settings-dialog')).toBeHidden();

  // New session defaults to Auto · routed and submits initial prompt.
  await page.locator('#welcome-new').click();
  await expect(page.locator('#provider')).toHaveValue('auto');
  await expect(page.locator('#provider')).toBeEnabled();
  await expect(page.locator('#create-model')).toBeDisabled();
  await page.locator('#session-title').fill('UI Routed Session');
  await page.locator('#create-initial-prompt').fill('Hello routed UI');
  await page.locator('#create-submit').click();
  await expect(page.locator('#create-dialog')).toBeHidden();

  await expect(page.locator('#messages')).toContainText('Hello routed UI');
  const [created]=await api('/api/sessions');
  expect(created.routing.mode).toBe('default');
  expect(created.routing.provider).toBe('demo');
  expect(created.routing.model).toBe('demo/quick');

  // Create a project with Always routed and verify provider picker locks to Auto.
  await page.locator('#new-project').click();
  await page.locator('#project-name').fill('Always Project');
  await page.locator('#project-routing-mode').selectOption('always');
  await page.locator('#save-project').click();
  await expect(page.locator('#project-dialog')).toBeHidden();

  await page.locator('#new-session').click();
  await page.locator('#create-place').click();
  await page.locator('#create-place').fill('Always Project');
  await page.locator('#create-place').press('Enter');
  await expect(page.locator('#provider')).toHaveValue('auto');
  await expect(page.locator('#provider')).toBeDisabled();
  await page.locator('#dismiss-create').click();

  expect(errors).toEqual([]);
});
