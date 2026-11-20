// Opening a session released by a server restart only loads its transcript;
// the first message resumes the harness. Own server on a dedicated port.
import {test,expect} from '@playwright/test';
import {spawn} from 'node:child_process';
import {mkdtemp,mkdir} from 'node:fs/promises';
import {resolve,join} from 'node:path';
import {serverReady,stopChild,removeFixture} from './support/cleanup.mjs';

const base='http://127.0.0.1:4547';
test.use({baseURL:base});
let fixture,server;

async function start(){
  let output='',error;
  const child=spawn(resolve(process.env.AGENTWORKS_TEST_BINARY||'target/debug/agentworks'),[],{env:{...process.env,
    AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:'4547',AGENTWORKS_ROOTS:fixture,
    AGENTWORKS_CATALOG_PATH:join(fixture,'catalog.json'),AGENTWORKS_DB_PATH:join(fixture,'sessions.sqlite3'),AGENTWORKS_DEMO_STATE_DIR:join(fixture,'demo')},stdio:['ignore','pipe','pipe']});
  child.stdout.on('data',b=>output+=b);child.stderr.on('data',b=>output+=b);child.on('error',e=>error=e);
  await expect.poll(async()=>{if(error)throw error;if(child.exitCode!==null)throw Error(output);return serverReady(child,base);},{timeout:10000}).toBe(true);
  return child;
}
async function api(path,body){
  const response=await fetch(base+path,{method:body===undefined?'GET':'POST',headers:{Origin:base,'Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body)});
  expect(response.ok,await response.clone().text()).toBeTruthy();return response.json();
}
const state=async id=>(await api('/api/sessions/'+id+'?view=meta')).status;

test.beforeAll(async()=>{await mkdir('.cache',{recursive:true});fixture=await mkdtemp(resolve('.cache/dormant-open-'));server=await start();});
test.afterAll(async()=>{await stopChild(server);await removeFixture(fixture);});

test('opening a restart-suspended session waits for a message before resuming',async({page})=>{
  const s=await api('/api/sessions',{provider:'demo',cwd:fixture,title:'Old session'});
  await expect.poll(()=>state(s.id)).toBe('idle');
  await api(`/api/sessions/${s.id}/prompt`,{text:'first message',requestId:'first'});
  await expect.poll(()=>state(s.id)).toBe('idle');
  await stopChild(server);server=await start();
  expect(await state(s.id)).toBe('suspended');

  await page.addInitScript(id=>localStorage.setItem('agentworks-session',id),s.id);
  await page.goto(base);
  await expect(page.locator('#messages')).toContainText('first message');
  await expect(page.locator('#connection')).toHaveText('Connected');
  // Browsing leaves the harness released.
  await page.waitForTimeout(1500);
  expect(await state(s.id)).toBe('suspended');
  await expect(page.locator('#session-state')).toHaveText('Suspended');
  await expect(page.locator('#send')).toBeEnabled();

  // Sending resumes it and runs the turn.
  await page.locator('#prompt-editor .cm-content').fill('second message');
  await page.locator('#send').click();
  await expect(page.locator('#messages')).toContainText('second message');
  await expect(page.locator('#session-state')).toHaveText('Ready',{timeout:20000});
  const events=(await api('/api/sessions/'+s.id)).events;
  expect(events.filter(e=>e.kind==='user').map(e=>e.data.text)).toEqual(['first message','second message']);
});
