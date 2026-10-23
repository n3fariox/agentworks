import {serverReady} from './support/cleanup.mjs';
import {test,expect} from '@playwright/test';
import {spawn} from 'node:child_process';
import {mkdtemp,mkdir,rm} from 'node:fs/promises';
import {resolve,join} from 'node:path';

// Own server and data: never touch the user's sessions or settings.
const base='http://127.0.0.1:4493';
test.use({baseURL:base});
let fixture,server,exited,log='';
async function api(path,body,method,status=200){
  const r=await fetch(base+path,{method:method||(body?'POST':'GET'),headers:{Origin:base,'Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body)});
  expect(r.status,await r.clone().text()).toBe(status);return r.json().catch(()=>null);
}
test.beforeAll(async()=>{
  await mkdir('.cache',{recursive:true});fixture=await mkdtemp(resolve('.cache/roles-ui-'));
  server=spawn(resolve('target/debug/agentworks'),[],{env:{...process.env,AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:'4493',AGENTWORKS_ROOTS:fixture,AGENTWORKS_CATALOG_PATH:join(fixture,'catalog.json'),AGENTWORKS_DB_PATH:join(fixture,'sessions.sqlite3'),AGENTWORKS_DEMO_STATE_DIR:join(fixture,'demo')},stdio:['ignore','pipe','pipe']});
  server.on('error',e=>{log+=e.message;exited=true;});server.on('exit',()=>exited=true);
  for(const stream of [server.stdout,server.stderr])stream.on('data',s=>log+=s);
  for(let i=0;i<100;i++){if(exited)throw Error(log);if(await serverReady(server,base))return;await new Promise(r=>setTimeout(r,100));}
  throw Error('Server readiness timed out: '+log);
});
test.afterAll(async()=>{
  if(server&&!exited){const stopped=new Promise(r=>server.once('exit',r));server.kill('SIGTERM');await stopped;}
  if(fixture)await rm(fixture,{recursive:true,force:true});
});

test('new session applies a chosen role; unsupported agents disable the picker',async({page})=>{
  const errors=[];page.on('pageerror',e=>errors.push(e.message));
  await api('/api/roles',{name:'Release captain',description:'Ships it',instructions:'Always tag the release.'});
  await page.goto('/');
  await page.locator('#welcome-new').click();
  await expect(page.locator('#create-role optgroup[label="Built-in"] option')).toHaveText(['Coordinator','Implementer','Reviewer','Researcher']);
  await expect(page.locator('#create-role optgroup[label="Custom"] option')).toHaveText(['Release captain']);
  await page.locator('#provider').selectOption('demo');
  await page.locator('#create-role').selectOption({label:'Release captain'});
  await expect(page.locator('#create-role-note')).toContainText('Ships it');
  await page.locator('#create-submit').click();
  await expect(page.locator('#create-dialog')).toBeHidden();
  const [created]=await api('/api/sessions');
  expect(created.role.name).toBe('Release captain');
  await page.locator('#prompt').fill('role?');await page.locator('#prompt').press('Enter');
  await expect(page.locator('#messages')).toContainText('Always tag the release.');

  // A fork keeps its parent's snapshot and cannot select an unsupported agent.
  await page.locator('#lifecycle-menu > summary').click();
  await page.locator('#fork-session').click();
  await expect(page.locator('#create-role')).toBeDisabled();
  await expect(page.locator('#create-role-note')).toContainText("Forks keep the parent's role · Release captain");
  await page.locator('#provider').selectOption('codex');
  await expect(page.locator('#create-submit')).toBeDisabled();
  await expect(page.locator('#create-role-note')).toContainText('must keep the “Release captain” role');
  await page.locator('#provider').selectOption('demo');
  await expect(page.locator('#create-role-note')).toContainText("Forks keep the parent's role · Release captain");
  await page.locator('#dismiss-create').click();

  // Unsupported agents disable the picker instead of sending a role they would drop.
  await page.locator('#new-session').click();
  await page.locator('#create-role').selectOption({label:'Reviewer'});
  await page.locator('#provider').selectOption('codex');
  await expect(page.locator('#create-role')).toBeDisabled();
  await expect(page.locator('#create-role')).toHaveValue('');
  await expect(page.locator('#create-role-note')).toContainText('Codex does not accept role instructions');
  await page.locator('#provider').selectOption('demo');
  await expect(page.locator('#create-role')).toBeEnabled();
  await page.locator('#dismiss-create').click();
  expect(errors).toEqual([]);
});
