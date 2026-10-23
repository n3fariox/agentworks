import {test,expect} from '@playwright/test';
import {spawn} from 'node:child_process';
import {mkdtemp} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';
import {serverReady,stopChild,removeFixture} from './support/cleanup.mjs';
// The Rust Claude bridge against the fake stream-json CLI: no Node adapter, no inference.
const base='http://127.0.0.1:4497';
let dir,server,output='';
async function api(path,body){
  const response=await fetch(base+path,{method:body===undefined?'GET':'POST',headers:{Origin:base,'Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body)});
  expect(response.ok,await response.clone().text()).toBeTruthy();return response.json();
}
async function state(id,status){await expect.poll(async()=>(await api('/api/sessions/'+id)).session.status).toBe(status);}
test.beforeAll(async()=>{
  dir=await mkdtemp(join(tmpdir(),'agentworks-claude-rust-'));
  server=spawn(resolve(process.env.AGENTWORKS_TEST_BINARY||'target/debug/agentworks'),[],{env:{...process.env,CLAUDE_CODE_EXECUTABLE:resolve('tests/claude-acp.test.py'),CLAUDE_CONFIG_DIR:join(dir,'claude'),AGENTWORKS_RUNTIME_DIR:dir,AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:'4497',AGENTWORKS_ROOTS:dir,AGENTWORKS_CATALOG_PATH:join(dir,'catalog.json'),AGENTWORKS_DB_PATH:join(dir,'sessions.sqlite3')},stdio:['ignore','pipe','pipe']});
  server.stdout.on('data',b=>output+=b);server.stderr.on('data',b=>output+=b);
  let error;server.on('error',e=>error=e);
  await expect.poll(async()=>{if(error)throw error;if(server.exitCode!==null)throw Error(output);return serverReady(server,base);},{timeout:10000}).toBe(true);
});
test.afterAll(async()=>{await stopChild(server);await removeFixture(dir);});

test('claude-rust background subagents get their own tab and approvals',async({page})=>{
  const errors=[];page.on('pageerror',e=>errors.push(e.message));
  const s=await api('/api/sessions',{provider:'claude-rust',cwd:dir});await state(s.id,'idle');
  await page.goto(base);await page.evaluate(id=>localStorage.setItem('agentworks-session',id),s.id);await page.reload();
  await api('/api/sessions/'+s.id+'/prompt',{text:'agent',requestId:'claude-rust-agent'});
  // The subagent asks after Claude's first result; the turn is still open, so it waits.
  await state(s.id,'waiting');
  const tab=page.getByRole('tab',{name:'Sub: Look around',exact:true});
  await expect(tab).toBeVisible();
  await expect(page.locator('#approval-area')).toContainText('Look around');
  await expect(page.locator('#messages')).toContainText('Waiting for the agent');
  await expect(page.locator('#messages')).not.toContainText('Found 3 files');
  await tab.click();
  await expect(page.locator('#subagent-view')).toContainText('Found 3 files');
  await page.locator('#approval-area').getByRole('button',{name:'Yes',exact:true}).click();
  await state(s.id,'idle');
  await page.getByRole('tab',{name:'Chat',exact:true}).click();
  await expect(page.locator('#messages')).toContainText('done');
  await page.reload();
  await expect(page.getByRole('tab',{name:'Sub: Look around',exact:true})).toBeVisible();
  expect(errors).toEqual([]);
});
