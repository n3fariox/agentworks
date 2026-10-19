import {test,expect} from '@playwright/test';
import {spawn} from 'node:child_process';
import {mkdtemp,mkdir,writeFile,rm} from 'node:fs/promises';
import {resolve,join} from 'node:path';

// Own server, catalog, database and demo state on a dedicated port; never the live service.
const base='http://127.0.0.1:4472';
test.use({baseURL:base});
let fixture,server,log='',exited=false,session;

async function api(path,body,method){
  const response=await fetch(base+path,{method:method||(body?'POST':'GET'),headers:{Origin:base,'Content-Type':'application/json'},body:body?JSON.stringify(body):undefined});
  expect(response.ok,await response.clone().text()).toBe(true);return response.json();
}
async function openSession(page){
  await page.addInitScript(id=>{if(!sessionStorage.getItem('timeout-test-init')){localStorage.setItem('agentworks-session',id);sessionStorage.setItem('timeout-test-init','1');}},session.id);
  await page.goto('/');
  await expect(page.locator('#session-state')).toHaveText('Ready');
}
async function startServer(){
  exited=false;log='';
  server=spawn(resolve('target/debug/agentworks'),[],{env:{...process.env,
    AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:'4472',AGENTWORKS_ROOTS:fixture,
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
test.beforeAll(async()=>{
  await mkdir('.cache',{recursive:true});fixture=await mkdtemp(resolve('.cache/timeouts-'));
  await startServer();
});
test.beforeEach(async()=>{session=await api('/api/sessions',{provider:'demo',cwd:fixture,title:'Timeout controls test'});});
test.afterEach(async()=>{if(session)await api(`/api/sessions/${session.id}`,undefined,'DELETE');});

async function openGlobalTimeouts(page){
  await page.locator('#nav-settings').click();await page.locator('#settings-tab-timeouts').click();
}
async function openSessionTimeouts(page){
  if(await page.locator('#session-more').isVisible())await page.locator('#session-more').click();
  await page.locator('#session-settings').click();
  await expect(page.locator('#session-settings-dialog')).toBeVisible();
}
test('global defaults and session overrides are separate and apply to the next turn',async({page})=>{
  await openSession(page);await openGlobalTimeouts(page);
  const def=page.locator('[data-timeout-scope="default"]'),ses=page.locator('[data-timeout-scope="session"]');
  await expect(page.locator('#open-settings')).toHaveCount(0);
  await expect(page.locator('#settings-dialog [data-timeout-scope="session"]')).toHaveCount(0);
  await expect(def.locator('[data-note]')).toContainText('900s total (includes approval waits)');
  await def.getByLabel('Total turn limit (seconds)').fill('30');await def.locator('[data-save]').click();
  await expect(def.locator('[data-note]')).toContainText('Total limit must be 1 minute to 7 days');
  expect((await api('/api/timeouts')).default.totalSeconds).toBe(900);
  await page.locator('#dismiss-settings').click();await openSessionTimeouts(page);
  await expect(ses.locator('[data-note]')).toContainText('Using global default. Next turn uses: 900s total');
  await expect(ses.getByLabel('Total turn limit (seconds)')).toBeDisabled();
  await ses.getByLabel('Override the global default for this session').check();
  await ses.getByLabel('Total turn limit (seconds)').fill('3600');
  await ses.getByLabel('Inactivity warning (seconds)').fill('120');
  await ses.getByLabel('Total limit counts time waiting for approvals').uncheck();
  await page.locator('#save-session-settings').click();
  await expect(page.locator('#session-settings-dialog')).toBeHidden();
  expect((await api('/api/sessions/'+session.id)).session.timeouts.totalSeconds).toBe(3600);
  await page.locator('#prompt').fill('hello timeouts');await page.locator('#send').click();
  await expect(page.locator('#session-state')).toHaveText('Ready');
  await expect(page.getByText('Turn limits: 3600s total (excludes approval waits) · warn after 120s quiet')).toBeVisible();
  await openGlobalTimeouts(page);
  await def.getByLabel('Total turn limit (seconds)').fill('1200');await page.locator('#save-settings').click();
  await expect(page.locator('#settings-dialog')).toBeHidden();
  await openSessionTimeouts(page);
  await expect(ses.getByLabel('Total turn limit (seconds)')).toHaveValue('3600');
  await ses.getByLabel('Override the global default for this session').uncheck();await page.locator('#save-session-settings').click();
  await expect(page.locator('#session-settings-dialog')).toBeHidden();
  await openSessionTimeouts(page);
  await expect(ses.locator('[data-note]')).toContainText('Using global default. Next turn uses: 1200s total');
  await page.locator('#dismiss-session-settings').click();await page.reload();
  await expect(page.locator('#session-state')).toHaveText('Ready');
  await expect(page.getByText('Turn limits: 3600s total')).toBeVisible();
});
test('main Save settings persists turn limits through reopening, restart, and new turns',async({page})=>{
  await openSession(page);
  await page.locator('#nav-settings').click();await page.locator('#settings-tab-timeouts').click();
  const def=page.locator('[data-timeout-scope="default"]');
  await def.getByLabel('Total turn limit (seconds)').fill('7200');
  await page.locator('#save-settings').click();
  await expect(page.locator('#settings-dialog')).toBeHidden();
  expect((await api('/api/timeouts')).default.totalSeconds).toBe(7200);
  await page.locator('#nav-settings').click();
  await expect(def.getByLabel('Total turn limit (seconds)')).toHaveValue('7200');
  await page.locator('#dismiss-settings').click();
  await stopServer();await startServer();
  expect((await api('/api/timeouts')).default.totalSeconds).toBe(7200);
  const fresh=await api('/api/sessions',{provider:'demo',cwd:fixture,title:'Saved turn limit'});
  try{
    await expect.poll(async()=> (await api('/api/sessions/'+fresh.id)).session.status).toBe('idle');
    expect((await api('/api/sessions/'+fresh.id)).session.effectiveTimeouts.totalSeconds).toBe(7200);
    await api(`/api/sessions/${fresh.id}/prompt`,{text:'Verify saved turn limit',requestId:'saved-timeout-check'});
    await expect.poll(async()=> (await api('/api/sessions/'+fresh.id)).events.find(e=>e.kind==='timeout_policy')?.data.policy.totalSeconds).toBe(7200);
  }finally{await api('/api/sessions/'+fresh.id,undefined,'DELETE');}
});

test('session override save failures preserve the draft for retry',async({page})=>{
  await openSession(page);await openSessionTimeouts(page);
  const ses=page.locator('[data-timeout-scope="session"]');
  await ses.getByLabel('Override the global default for this session').check();
  await ses.getByLabel('Total turn limit (seconds)').fill('3600');
  await page.route('**/api/sessions/*/timeouts',route=>route.fulfill({status:503,body:'Session save failed'}));
  await page.locator('#save-session-settings').click();
  await expect(page.locator('#session-settings-error')).toContainText('Session settings were not saved: Session save failed');
  await expect(ses.getByLabel('Total turn limit (seconds)')).toHaveValue('3600');
  expect((await api('/api/sessions/'+session.id)).session.timeouts).toBeNull();
  await page.unroute('**/api/sessions/*/timeouts');await page.locator('#save-session-settings').click();
  await expect(page.locator('#session-settings-dialog')).toBeHidden();
  const saved=(await api('/api/sessions/'+session.id)).session;
  expect(saved.timeouts.totalSeconds).toBe(3600);expect(saved.effectiveTimeouts.totalSeconds).toBe(3600);
});

test('failed main saves preserve timeout edits for retry, including disabling the limit',async({page})=>{
  await openSession(page);
  await page.locator('#nav-settings').click();await page.locator('#settings-tab-timeouts').click();
  const def=page.locator('[data-timeout-scope="default"]'),total=def.getByLabel('Total turn limit (seconds)');
  const original=(await api('/api/timeouts')).default.totalSeconds;
  await total.fill('30');await page.locator('#save-settings').click();
  await expect(page.locator('#settings-error')).toContainText('Total limit must be 1 minute to 7 days');
  expect((await api('/api/timeouts')).default.totalSeconds).toBe(original);
  await total.fill('');
  await page.route('**/api/settings',route=>route.request().method()==='PUT'?route.fulfill({status:503,body:'Temporary save failure'}):route.continue());
  await page.locator('#save-settings').click();
  await expect(page.locator('#settings-error')).toContainText('Temporary save failure');
  await expect(page.locator('#settings-dialog')).toBeVisible();await expect(total).toHaveValue('');
  expect((await api('/api/timeouts')).default.totalSeconds).toBe(original);
  await page.unroute('**/api/settings');
  await page.locator('#save-settings').click();await expect(page.locator('#settings-dialog')).toBeHidden();
  expect((await api('/api/timeouts')).default.totalSeconds).toBeNull();
  await page.locator('#nav-settings').click();await expect(total).toHaveValue('');
  await expect(def.locator('[data-note]')).toContainText('no total limit');
});

test.afterAll(async()=>{
  await stopServer();
  if(fixture)await rm(fixture,{recursive:true,force:true});
});
