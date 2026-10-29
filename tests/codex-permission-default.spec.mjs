// Real server + browser against an offline ACP fixture; never starts Codex or a model.
import {test,expect} from '@playwright/test';
import {spawn} from 'node:child_process';
import {mkdtemp,writeFile,readFile} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';
import {acpAgent} from './support/acp-agent.mjs';
import {serverReady,stopChild,removeFixture} from './support/cleanup.mjs';

const base='http://127.0.0.1:4486';
let dir,server,agent,output='';
async function api(path,body,method=body===undefined?'GET':'POST'){
  const response=await fetch(base+path,{method,headers:{Origin:base,'Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body)});
  expect(response.ok,await response.clone().text()).toBeTruthy();return response.json();
}
async function ready(id,status='idle'){
  await expect.poll(async()=>{const data=await api('/api/sessions/'+id);if(data.session.status==='error'&&status!=='error')throw Error(JSON.stringify(data.events));return data.session.status;}).toBe(status);
}
async function native(id){return (await api('/api/sessions/'+id)).events.findLast(e=>e.kind==='provider_session').data.id;}
async function state(id){return JSON.parse(await readFile(join(dir,await native(id)+'.json'),'utf8'));}
async function controls(value){await writeFile(join(dir,'controls.json'),JSON.stringify(value));}
async function open(page,id){
  await page.goto(base);await page.evaluate(id=>localStorage.setItem('agentworks-session',id),id);await page.reload();
  await expect(page.getByLabel('Session permission mode')).toBeVisible();
}
test.beforeAll(async()=>{
  dir=await mkdtemp(join(tmpdir(),'agentworks-codex-default-'));

  agent=await acpAgent(dir,'tests/fixtures/permission-mode-agent.mjs');
  await controls({});
  server=spawn(resolve(process.env.AGENTWORKS_TEST_BINARY||'target/debug/agentworks'),[],{env:{...process.env,AGENTWORKS_ACP_COMMAND:agent,AGENTWORKS_RUNTIME_DIR:dir,AGENTWORKS_PERMISSION_FIXTURE:dir,AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:'4486',AGENTWORKS_ROOTS:dir,AGENTWORKS_CATALOG_PATH:join(dir,'catalog.json'),AGENTWORKS_DB_PATH:join(dir,'sessions.sqlite3')},stdio:['ignore','pipe','pipe']});
  server.stdout.on('data',b=>output+=b);server.stderr.on('data',b=>output+=b);
  let error;server.on('error',e=>error=e);
  await expect.poll(async()=>{if(error)throw error;if(server.exitCode!==null)throw Error(output);return serverReady(server,base);},{timeout:10000}).toBe(true);
});
test.afterAll(async()=>{await stopChild(server);await removeFixture(dir);});

test('Codex defaults, confirmed picker, manual override, reconnect, and unsupported fallback',async({page})=>{
  const errors=[];page.on('pageerror',e=>errors.push(e.message));
  expect((await api('/api/settings')).codexDefaultPermissionMode).toBe('agent-full-access');
  const automation=await api('/api/automations',{name:'Permission fixture',provider:'codex-rust',cwd:dir,prompt:'inspect'});
  const first=(await api('/api/automations/'+automation.id+'/run',{requestId:'confirmed-default'})).session;
  await ready(first.id);
  expect((await state(first.id)).selections).toEqual(['agent-full-access']);
  expect((await state(first.id)).prompts).toEqual(['agent-full-access']);
  await open(page,first.id);
  const picker=page.getByLabel('Session permission mode');
  await expect(picker).toHaveValue('agent-full-access');
  await picker.selectOption('workspace-write');
  await expect.poll(async()=>(await api('/api/sessions/'+first.id+'/permission-mode')).currentValue).toBe('workspace-write');
  await expect(picker).toBeEnabled();
  await expect(picker).toHaveValue('workspace-write');

  await api('/api/settings',{roots:[dir],defaultModels:{},codexDefaultPermissionMode:'read-only'},'PUT');
  // Older clients omit the control: its saved value must survive.
  await api('/api/settings',{roots:[dir],defaultModels:{}},'PUT');
  expect((await api('/api/settings')).codexDefaultPermissionMode).toBe('read-only');
  expect((await api('/api/sessions/'+first.id+'/permission-mode')).currentValue).toBe('workspace-write');
  await api('/api/sessions/'+first.id+'/suspend',{});await ready(first.id,'suspended');
  // The harness has a different mode at reconnect; the saved manual choice wins.
  const sid=await native(first.id),saved=await state(first.id);saved.mode='agent';
  await writeFile(join(dir,sid+'.json'),JSON.stringify(saved));
  await api('/api/sessions/'+first.id+'/resume',{});await ready(first.id);
  expect((await state(first.id)).mode).toBe('workspace-write');
  expect(await native(first.id)).toBe(sid);
  const second=await api('/api/sessions',{provider:'codex-rust',cwd:dir});await ready(second.id);
  expect((await state(second.id)).selections).toEqual(['read-only']);

  await api('/api/settings',{roots:[dir],defaultModels:{},codexDefaultPermissionMode:'agent-full-access'},'PUT');
  await controls({unsupported:true});
  const fallback=await api('/api/sessions',{provider:'codex-rust',cwd:dir});await ready(fallback.id);
  expect((await state(fallback.id)).selections).toEqual([]);
  const mode=await api('/api/sessions/'+fallback.id+'/permission-mode');
  expect(mode.currentValue).toBe('agent');expect(mode.defaultFallback).toContain("Codex default 'agent-full-access' is not advertised");
  await open(page,fallback.id);
  await expect(picker).toHaveValue('agent');
  await expect(picker.locator('option[value="agent-full-access"]')).toHaveCount(0);
  await expect(page.locator('#permission-mode-note')).toContainText('not advertised');

  await controls({unconfirmed:true});
  const rejected=(await api('/api/automations/'+automation.id+'/run',{requestId:'unconfirmed-default'})).session;await ready(rejected.id,'error');
  expect((await state(rejected.id)).prompts).toEqual([]);
  expect((await api('/api/sessions/'+rejected.id)).events.some(e=>e.kind==='error'&&e.data.message.includes('did not confirm'))).toBe(true);
  expect(errors).toEqual([]);
});
