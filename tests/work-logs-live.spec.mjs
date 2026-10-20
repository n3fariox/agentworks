import {serverReady,stopChild} from './support/cleanup.mjs';
import {test,expect} from '@playwright/test';
import {spawn} from 'node:child_process';
import {mkdtemp,mkdir,rm} from 'node:fs/promises';
import {resolve,join} from 'node:path';

// Isolated server, store, catalog and demo state on assigned port 4451; demo agent only, no model inference.
const base='http://127.0.0.1:4451';
let fixture,server,log='',exited=false;
async function api(path,body){
  const r=await fetch(base+path,{method:body?'POST':'GET',headers:{Origin:base,'Content-Type':'application/json'},body:body?JSON.stringify(body):undefined});
  expect(r.ok,await r.clone().text()).toBe(true);return r.status===204?null:r.json();
}
test.beforeAll(async()=>{
  await mkdir('.cache',{recursive:true});fixture=await mkdtemp(resolve('.cache/work-logs-'));await mkdir(join(fixture,'project'));
  server=spawn(process.env.AGENTWORKS_TEST_BINARY||resolve('target/debug/agentworks'),[],{env:{...process.env,AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:'4451',AGENTWORKS_ROOTS:fixture,
    AGENTWORKS_CATALOG_PATH:join(fixture,'catalog.json'),AGENTWORKS_DB_PATH:join(fixture,'sessions.sqlite3'),AGENTWORKS_DEMO_STATE_DIR:join(fixture,'demo')},stdio:['ignore','pipe','pipe']});
  server.on('exit',()=>exited=true);for(const s of [server.stdout,server.stderr])s.on('data',c=>{log+=c;});
  for(let i=0;i<100;i++){if(exited)throw new Error(log);if(await serverReady(server,base))return;await new Promise(r=>setTimeout(r,100));}
  throw new Error('readiness timed out: '+log);
});
test.afterAll(async()=>{await stopChild(server);if(fixture)await rm(fixture,{recursive:true,force:true});});

test('demo approval turn: approval stays actionable, log folds after completion and survives reload',async({page})=>{
  const session=await api('/api/sessions',{provider:'demo',cwd:join(fixture,'project'),title:'Work log test'});
  await page.goto(base+'/');await page.evaluate(id=>localStorage.setItem('agentworks-session',id),session.id);
  await page.goto(base+'/');
  await expect(page.locator('#prompt')).toBeEnabled({timeout:15000});
  await page.fill('#prompt','approval');await page.click('#send');
  const approval=page.locator('#approval-area .approval');
  await expect(approval).toBeVisible();
  await expect(page.locator('.work-log')).toHaveAttribute('open','');
  await expect(page.locator('.work-log-counts')).toHaveText('1 tool call · 1 in progress');
  await approval.getByRole('button',{name:'Allow once'}).click();
  await expect(page.locator('#prompt')).toBeEnabled();
  await expect(page.locator('.work-log')).not.toHaveAttribute('open','');
  await expect(page.locator('.work-log-counts')).toHaveText(/^1 tool call( · \d+s)?$/);
  await page.reload();
  await expect(page.locator('.work-log-counts')).toHaveText('1 tool call');
  await expect(page.locator('.work-log')).not.toHaveAttribute('open','');
  await expect(page.locator('.message.assistant').last()).toContainText('demo agent');
  // Explicit expansion survives a full reload (fresh random item ids) and session switching.
  const other=await api('/api/sessions',{provider:'demo',cwd:join(fixture,'project'),title:'Other work log session'});
  await page.locator('.work-log-summary').click();
  await expect(page.locator('.work-log')).toHaveAttribute('open','');
  await page.reload();
  await expect(page.locator('.work-log-counts')).toHaveText('1 tool call');
  await expect(page.locator('.work-log')).toHaveAttribute('open','');
  await page.getByText('Other work log session').first().click();
  await expect(page.locator('#header-title')).toHaveText('Other work log session');
  await expect(page.locator('.work-log')).toHaveCount(0);
  await page.getByText('Work log test').first().click();
  await expect(page.locator('#header-title')).toHaveText('Work log test');
  await expect(page.locator('.work-log')).toHaveAttribute('open','');
  expect(other.id).not.toBe(session.id);
});
