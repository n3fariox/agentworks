import {serverReady} from './support/cleanup.mjs';
import {test,expect} from '@playwright/test';
import {spawn} from 'node:child_process';
import {mkdtemp,mkdir,rm} from 'node:fs/promises';
import {resolve,join} from 'node:path';

// Own server and data: never touch the user's sessions or settings.
const base='http://127.0.0.1:4491';
test.use({baseURL:base});
let fixture,server,exited,log='';
async function api(path,body,method,status=200){
  const r=await fetch(base+path,{method:method||(body?'POST':'GET'),headers:{Origin:base,'Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body)});
  expect(r.status,await r.clone().text()).toBe(status);return r.json().catch(()=>null);
}
async function start(){
  exited=false;log='';
  server=spawn(resolve('target/debug/agentworks'),[],{env:{...process.env,AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:'4491',AGENTWORKS_ROOTS:fixture,AGENTWORKS_CATALOG_PATH:join(fixture,'catalog.json'),AGENTWORKS_DB_PATH:join(fixture,'sessions.sqlite3'),AGENTWORKS_DEMO_STATE_DIR:join(fixture,'demo')},stdio:['ignore','pipe','pipe']});
  server.on('error',e=>{log+=e.message;exited=true;});server.on('exit',()=>exited=true);
  for(const stream of [server.stdout,server.stderr])stream.on('data',s=>log+=s);
  for(let i=0;i<100;i++){
    if(exited)throw Error(log);
    if(await serverReady(server,base))return;
    await new Promise(r=>setTimeout(r,100));
  }
  throw Error('Server readiness timed out: '+log);
}
async function stop(){if(server&&!exited){const stopped=new Promise(r=>server.once('exit',r));server.kill('SIGTERM');await stopped;}}
async function setGlobal(role){const current=await api('/api/settings');return api('/api/settings',{...current,defaultCommunicationRole:role},'PUT');}
async function create(project,agentAccess){return api('/api/sessions',{provider:'demo',cwd:fixture,projectId:project?.id,...(agentAccess===undefined?{}:{agentAccess})});}
async function access(s){return api(`/api/sessions/${s.id}/agent-access`);}
const off={communicate:false,startSessions:false,promptSessions:false,cancelSessions:false,runAutomations:false,restartService:false,manageReviewQueue:false,archiveSessions:false,manageAutomations:false};
const participant={...off,communicate:true};
const coordinator={...participant,startSessions:true,promptSessions:true,cancelSessions:true,runAutomations:true,archiveSessions:true};
test.beforeAll(async()=>{await mkdir('.cache',{recursive:true});fixture=await mkdtemp(resolve('.cache/communication-defaults-'));await start();});
test.beforeEach(async({},info)=>{if(!info.title.startsWith('unconfigured'))await setGlobal('off');});
test.afterEach(async()=>{
  for(const s of await api('/api/sessions'))await api('/api/sessions/'+s.id,undefined,'DELETE');
  for(const p of (await api('/api/catalog')).projects)await api('/api/projects/'+p.id,undefined,'DELETE');
});
test.afterAll(async()=>{await stop();if(fixture)await rm(fixture,{recursive:true,force:true});});

test('unconfigured global and project defaults allow communication without coordinator permissions',async()=>{
  expect((await api('/api/settings')).defaultCommunicationRole).toBe('participant');
  expect((await api('/api/status')).defaultCommunicationRole).toBe('participant');
  const project=await api('/api/projects',{name:'Default participant'});
  expect(await access(await create())).toEqual(participant);
  expect(await access(await create(project))).toEqual(participant);
  expect(await access(await create(project,{}))).toEqual(off);
  await setGlobal('off');
  expect(await access(await create(project))).toEqual(off);
});

test('global/project roles inherit, preserve explicit choices and existing grants, and survive restart',async()=>{
  const inherited=await api('/api/projects',{name:'Inherited'});
  const overridden=await api('/api/projects',{name:'Override',defaultCommunicationRole:'coordinator'});
  expect(await access(await create(inherited))).toEqual(off);
  await setGlobal('participant');
  const existing=await create(inherited);
  expect(await access(existing)).toEqual(participant);
  expect(await access(await create())).toEqual(participant);
  expect(await access(await create(overridden))).toEqual(coordinator);
  expect(await access(await create(overridden,{}))).toEqual(off);
  await setGlobal('coordinator');
  expect(await access(existing)).toEqual(participant);
  expect(await access(await create(inherited))).toEqual(coordinator);
  const automation=await api('/api/automations',{name:'Unprivileged run',provider:'demo',cwd:fixture,projectId:inherited.id,prompt:'Check role defaults',enabled:true});
  const run=await api(`/api/automations/${automation.id}/run`,{requestId:'role-default-check'});
  expect(await access(run.session)).toEqual(off);
  await api('/api/automations/'+automation.id,undefined,'DELETE');
  await setGlobal('participant');
  await api('/api/projects/'+overridden.id,{name:'Override',defaultCommunicationRole:'off'},'PATCH');
  await api('/api/projects/'+overridden.id,{name:'Renamed'},'PATCH');
  expect((await api('/api/catalog')).projects.find(p=>p.id===overridden.id).defaultCommunicationRole).toBe('off');
  expect(await access(await create(overridden))).toEqual(off);
  expect(await access(existing)).toEqual(participant);
  await api('/api/projects/'+overridden.id,{name:'Renamed',defaultCommunicationRole:null},'PATCH');
  expect(await access(await create(overridden))).toEqual(participant);
  // Older clients that omit the new field must preserve the saved global role.
  const settings=await api('/api/settings');delete settings.defaultCommunicationRole;
  await api('/api/settings',settings,'PUT');
  expect((await api('/api/settings')).defaultCommunicationRole).toBe('participant');
  await api('/api/settings',{...settings,defaultCommunicationRole:'admin'},'PUT',422);
  await api('/api/projects/'+overridden.id,{name:'Renamed',defaultCommunicationRole:'admin'},'PATCH',422);
  await stop();await start();
  expect((await api('/api/settings')).defaultCommunicationRole).toBe('participant');
  expect(await access(existing)).toEqual(participant);
  expect(await access(await create(overridden))).toEqual(participant);
});

for(const mobile of [false,true])test(`role defaults and scoped settings ${mobile?'mobile':'desktop'}`,async({page})=>{
  const errors=[];page.on('pageerror',e=>errors.push(e.message));
  await page.setViewportSize(mobile?{width:390,height:844}:{width:1440,height:1000});
  await page.goto('/');
  await expect(page.locator('#open-settings')).toHaveCount(0);
  await page.locator('#nav-settings').click();await page.locator('#settings-tab-communication').click();
  await page.locator('#default-communication-role').selectOption('participant');
  await page.locator('#save-settings').click();await expect(page.locator('#settings-dialog')).toBeHidden();
  expect((await api('/api/settings')).defaultCommunicationRole).toBe('participant');
  await page.locator('#welcome-new').click();
  await expect(page.locator('#create-agent-access option[value=""]')).toHaveText('Use global default · Participant');
  await page.locator('#create-project-new').click();
  await expect(page.locator('#project-communication-role option[value=""]')).toHaveText('Use global default · Participant');
  await page.locator('#project-name').fill('Role UI project');
  await page.locator('#project-communication-role').selectOption('coordinator');
  await page.locator('#save-project').click();await expect(page.locator('#project-dialog')).toBeHidden();
  await expect(page.locator('#create-agent-access option[value=""]')).toHaveText('Use project default · Coordinator');
  await page.locator('#provider').selectOption('demo');await page.locator('#session-title').fill('Role UI session');
  await page.locator('#create-submit').click();await expect(page.locator('#session-state')).toHaveText('Ready');
  const created=(await api('/api/sessions'))[0];expect(await access(created)).toEqual(coordinator);
  if(await page.locator('#session-more').isVisible())await page.locator('#session-more').click();
  await page.locator('#lifecycle-menu > summary').click();
  await page.locator('#session-settings').click();
  await expect(page.locator('#session-settings-dialog')).toBeVisible();
  await expect(page.locator('#session-settings-scope')).toContainText('Role UI session');
  await expect(page.locator('#session-timeout-controls [data-note]')).toContainText('Using global default');
  await page.screenshot({path:`.cache/session-settings-${mobile?'mobile':'desktop'}.png`});
  await page.locator('#dismiss-session-settings').click();
  if(mobile)await page.locator('#session-home').click();
  await page.getByRole('button',{name:'Edit project Role UI project',exact:true}).click();
  await expect(page.locator('#project-communication-role')).toHaveValue('coordinator');
  await page.locator('#project-communication-role').selectOption('');await page.locator('#save-project').click();
  await expect(page.locator('#project-dialog')).toBeHidden();
  expect((await api('/api/catalog')).projects[0].defaultCommunicationRole).toBeUndefined();
  expect(await access(created)).toEqual(coordinator);
  await page.locator('#nav-settings').click();await page.locator('#settings-tab-communication').click();
  await expect(page.locator('#default-communication-role')).toHaveValue('participant');
  await page.screenshot({path:`.cache/communication-settings-${mobile?'mobile':'desktop'}.png`});
  expect(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBe(true);
  expect(errors).toEqual([]);
});
