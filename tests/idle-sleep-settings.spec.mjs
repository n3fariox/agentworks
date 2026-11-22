import {serverReady} from './support/cleanup.mjs';
import {test,expect} from '@playwright/test';
import {spawn} from 'node:child_process';
import {mkdtemp,mkdir,rm} from 'node:fs/promises';
import {resolve,join} from 'node:path';

// Own server, catalog and database on a dedicated port; never the live service.
const base='http://127.0.0.1:4543';
test.use({baseURL:base});
let fixture,server,log='',exited=false;

async function settings(body){
  const response=await fetch(base+'/api/settings',{method:body?'PUT':'GET',headers:{Origin:base,'Content-Type':'application/json'},body:body?JSON.stringify(body):undefined});
  return {ok:response.ok,body:response.ok?await response.json():await response.text()};
}
test.beforeAll(async()=>{
  await mkdir('.cache',{recursive:true});fixture=await mkdtemp(resolve('.cache/idle-sleep-'));
  server=spawn(resolve('target/debug/agentworks'),[],{env:{...process.env,
    AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:'4543',AGENTWORKS_ROOTS:fixture,
    AGENTWORKS_CATALOG_PATH:join(fixture,'catalog.json'),AGENTWORKS_DB_PATH:join(fixture,'sessions.sqlite3'),AGENTWORKS_DEMO_STATE_DIR:join(fixture,'demo')},stdio:['ignore','pipe','pipe']});
  server.on('error',e=>{log+=e.message;exited=true;});server.on('exit',()=>exited=true);
  for(const stream of [server.stdout,server.stderr])stream.on('data',s=>{log+=s.toString();});
  for(let i=0;i<100;i++){
    if(exited)throw new Error(log);
    if(await serverReady(server,base))break;
    await new Promise(r=>setTimeout(r,100));
    if(i===99)throw new Error('Isolated server readiness timed out: '+log);
  }
});
test.afterAll(async()=>{
  if(server&&!exited){const stopped=new Promise(r=>server.once('exit',r));server.kill('SIGTERM');await stopped;}
  if(fixture)await rm(fixture,{recursive:true,force:true});
});

test('idle sleep defaults to two hours and saves globally',async({page})=>{
  const initial=await settings();
  expect(initial.body.idleSleepMinutes).toBe(120);
  const roots=initial.body.roots;
  // Shorter than the prompt cache, or longer than a week, is refused; 0 means never.
  expect((await settings({roots,idleSleepMinutes:30})).ok).toBe(false);
  expect((await settings({roots,idleSleepMinutes:7*24*60+1})).ok).toBe(false);
  expect((await settings({roots,idleSleepMinutes:0})).body.idleSleepMinutes).toBe(0);
  // Older forms that omit the field keep the saved value.
  expect((await settings({roots})).body.idleSleepMinutes).toBe(0);

  await page.goto('/');await page.locator('#nav-settings').click();await page.locator('#settings-tab-models').click();
  await expect(page.locator('#idle-sleep-hours')).toHaveValue('0');
  await page.locator('#idle-sleep-hours').fill('3.5');
  await page.locator('#save-settings').click();await expect(page.locator('#settings-dialog')).toBeHidden();
  expect((await settings()).body.idleSleepMinutes).toBe(210);
  await page.locator('#nav-settings').click();await page.locator('#settings-tab-models').click();
  await expect(page.locator('#idle-sleep-hours')).toHaveValue('3.5');
});
