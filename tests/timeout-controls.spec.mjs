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
test.beforeAll(async()=>{
  await mkdir('.cache',{recursive:true});fixture=await mkdtemp(resolve('.cache/timeouts-'));
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
});
test.beforeEach(async()=>{session=await api('/api/sessions',{provider:'demo',cwd:fixture,title:'Timeout controls test'});});
test.afterEach(async()=>{if(session)await api(`/api/sessions/${session.id}`,undefined,'DELETE');});

test('timeout defaults validate, session override applies to the next turn and is shown before it runs',async({page})=>{
  await openSession(page);
  await page.locator('#open-settings').click();
  const def=page.locator('[data-timeout-scope="default"]'),ses=page.locator('[data-timeout-scope="session"]');
  await expect(def.locator('[data-note]')).toContainText('900s total (includes approval waits)');
  await expect(def.locator('[data-note]')).toContainText('no inactivity warning');
  await def.getByLabel('Total turn limit (seconds)').fill('30');
  await def.locator('[data-save]').click();
  await expect(def.locator('[data-note]')).toContainText('Total limit must be 1 minute to 7 days');
  expect((await api('/api/timeouts')).default.totalSeconds).toBe(900);
  await expect(ses.locator('[data-note]')).toContainText('Next turn uses: 900s total');
  await ses.getByLabel('Override the default for this session').check();
  await ses.getByLabel('Total turn limit (seconds)').fill('3600');
  await ses.getByLabel('Inactivity warning (seconds)').fill('120');
  await ses.getByLabel('Total limit counts time waiting for approvals').uncheck();
  await ses.locator('[data-save]').click();
  await expect(ses.locator('[data-note]')).toContainText('Saved. Next turn uses: 3600s total (excludes approval waits) · warn after 120s quiet');
  await page.locator('#dismiss-settings').click();
  await page.locator('#prompt').fill('hello timeouts');await page.locator('#send').click();
  await expect(page.locator('#session-state')).toHaveText('Ready');
  await expect(page.getByText('Turn limits: 3600s total (excludes approval waits) · warn after 120s quiet')).toBeVisible();
  await page.reload();await expect(page.locator('#session-state')).toHaveText('Ready');
  await expect(page.getByText('Turn limits: 3600s total')).toBeVisible();
});
test.afterAll(async()=>{
  if(server&&!exited){const stopped=new Promise(r=>server.once('exit',r));server.kill('SIGTERM');await stopped;}
  if(fixture)await rm(fixture,{recursive:true,force:true});
});
