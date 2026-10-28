import {serverReady} from './support/cleanup.mjs';
import {test,expect} from '@playwright/test';
import {spawn,execFileSync} from 'node:child_process';
import {mkdtemp,mkdir,rm} from 'node:fs/promises';
import {resolve,join} from 'node:path';

// Own server, catalog, database and demo state on a dedicated port; never the live service.
const base='http://127.0.0.1:4492';
test.use({baseURL:base,timezoneId:'America/Chicago'});
let fixture,server,log='',exited=false,session,project;

async function api(path,body,method){
  const response=await fetch(base+path,{method:method||(body?'POST':'GET'),headers:{Origin:base,'Content-Type':'application/json'},body:body?JSON.stringify(body):undefined});
  expect(response.ok,await response.clone().text()).toBe(true);return response.json();
}
async function openSession(page){
  await page.addInitScript(id=>{if(!sessionStorage.getItem('inbox-test-init')){localStorage.setItem('agentworks-session',id);sessionStorage.setItem('inbox-test-init','1');}},session.id);
  await page.goto('/');
  await expect(page.locator('#session-state')).toHaveText('Ready');
}
async function startServer(){
  exited=false;log='';
  server=spawn(resolve('target/debug/agentworks'),[],{env:{...process.env,
    AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:'4492',AGENTWORKS_ROOTS:fixture,
    AGENTWORKS_CATALOG_PATH:join(fixture,'catalog.json'),AGENTWORKS_DB_PATH:join(fixture,'sessions.sqlite3'),AGENTWORKS_DEMO_STATE_DIR:join(fixture,'demo')},stdio:['ignore','pipe','pipe']});
  server.on('error',e=>{log+=e.message;exited=true;});server.on('exit',()=>exited=true);
  for(const stream of [server.stdout,server.stderr])stream.on('data',s=>{log+=s.toString();});
  for(let i=0;i<100;i++){
    if(exited)throw new Error(log);
    if(await serverReady(server,base))break;
    await new Promise(r=>setTimeout(r,100));
    if(i===99)throw new Error('Isolated server readiness timed out: '+log);
  }
}
async function stopServer(){
  if(server&&!exited){const stopped=new Promise(r=>server.once('exit',r));server.kill('SIGTERM');await stopped;}
}
test.beforeAll(async()=>{
  await mkdir('.cache',{recursive:true});fixture=await mkdtemp(resolve('.cache/inbox-settings-'));
  await startServer();
  project=await api('/api/projects',{name:'Inbox settings fixture'});
});
test.beforeEach(async()=>{session=await api('/api/sessions',{provider:'demo',cwd:fixture,title:'Inbox settings test',projectId:project.id,agentAccess:{communicate:true}});});
test.afterEach(async()=>{if(session)await api(`/api/sessions/${session.id}`,undefined,'DELETE');});

const policy=()=>api(`/api/sessions/${session.id}/inbox-delivery`);
const authorize=page=>page.getByRole('button',{name:'Authorize standing inbox turns',exact:true});
async function openInbox(page){
  if(await page.locator('#session-more').isVisible())await page.locator('#session-more').click();
  await page.locator('#lifecycle-menu > summary').click();
  await page.locator('#session-agents').click();
  await expect(authorize(page)).toBeEnabled();
}
async function closeInbox(page){await page.locator('#dismiss-agents').click();await expect(page.locator('#agents-dialog')).toBeHidden();}
async function save(page){
  const response=page.waitForResponse(r=>r.url().endsWith('/inbox-delivery')&&r.request().method()==='PUT');
  await authorize(page).click();expect((await response).status()).toBe(200);
  await expect(page.locator('#inbox-delivery-status')).toContainText('Enabled');
  await expect(authorize(page)).toBeEnabled();
}
const localDate=timestamp=>{const d=new Date(timestamp);return new Date(d.getTime()-d.getTimezoneOffset()*60000).toISOString().slice(0,16);};

test('inbox deliveries are attributed to Agentworks live and after reload',async({page})=>{
  await openSession(page);
  await api(`/api/sessions/${session.id}/prompt`,{text:'Human request',requestId:crypto.randomUUID()});
  await expect(page.locator('.message.user').filter({hasText:'Human request'}).locator('.message-label')).toHaveText('You');
  await expect(page.locator('#session-state')).toHaveText('Ready');
  const sender=await api('/api/sessions',{provider:'demo',cwd:fixture,title:'Peer sender',projectId:project.id,agentAccess:{communicate:true}});
  try{
    await expect.poll(async()=>(await api(`/api/sessions/${sender.id}`)).session.status).toBe('idle');
    await api(`/api/sessions/${session.id}/inbox-delivery`,{enabled:true,requestId:crypto.randomUUID(),deadline:Date.now()+60000,maxTurns:1,cooldownSeconds:5,maxBatch:1},'PUT');
    await api(`/api/sessions/${sender.id}/mailbox`,{recipient:session.id,text:`Peer **review** request\n\n[This session](agentworks://sessions/${session.id})`,requestId:crypto.randomUUID()});
    const delivery=page.locator('.message.agentworks');
    await expect(delivery.locator('.message-label')).toHaveText('Agentworks',{timeout:15000});
    await expect(delivery.locator('.avatar')).toHaveText('A');
    await expect(delivery.getByRole('link',{name:'@peer-sender'})).toHaveAttribute('href',`agentworks://sessions/${sender.id}`);
    await expect(delivery.locator('blockquote strong')).toHaveText('review');
    await expect(delivery.locator('blockquote').getByRole('link',{name:'@inbox-settings-test'})).toHaveAttribute('href',`agentworks://sessions/${session.id}`);
    await expect(page.locator('.message.user').filter({hasText:'Agentworks inbox delivery'})).toHaveCount(0);
    await page.reload();
    await expect(delivery.locator('.message-label')).toHaveText('Agentworks');
    await expect(delivery.locator('.avatar')).toHaveText('A');
    await expect(page.locator('.message.user').filter({hasText:'Human request'}).locator('.message-label')).toHaveText('You');
    await delivery.getByRole('link',{name:'@peer-sender'}).click();
    await expect(page.locator('#session-heading-title')).toHaveText('Peer sender');
    await expect(page.locator('#workspace-preview')).not.toBeVisible();
  }finally{await api(`/api/sessions/${sender.id}`,undefined,'DELETE');}
});

for(const mobile of [false,true])test(`inbox settings persist and reload capped, uncapped and disabled policies ${mobile?'mobile':'desktop'}`,async({page})=>{
  const errors=[];page.on('pageerror',e=>errors.push(e.message));
  await page.setViewportSize(mobile?{width:390,height:844}:{width:1440,height:1000});
  await openSession(page);await openInbox(page);
  await expect(page.locator('#inbox-unlimitedTurns')).not.toBeChecked();
  const stopTimestamp=Math.floor((Date.now()+3600000)/60000)*60000;
  const stop=await page.evaluate(localDate,stopTimestamp);
  await page.locator('#inbox-maxTurns').fill('7');
  await page.locator('#inbox-deadline').fill(stop);
  await page.locator('#inbox-cooldownSeconds').fill('47');
  await page.locator('#inbox-maxBatch').fill('3');
  await save(page);
  expect((await policy()).policy).toMatchObject({enabled:true,maxTurns:7,deadline:stopTimestamp,cooldownSeconds:47,maxBatch:3});
  // Read from a separate SQLite connection to prove the write reached persistent storage.
  const persisted=JSON.parse(execFileSync('python3',['-c',
    'import sqlite3,sys; print(sqlite3.connect(sys.argv[1]).execute("SELECT policy FROM inbox_policies WHERE session_id=?", (sys.argv[2],)).fetchone()[0])',
    join(fixture,'sessions.sqlite3'),session.id],{encoding:'utf8'}));
  expect(persisted).toMatchObject({enabled:true,maxTurns:7,deadline:stopTimestamp,cooldownSeconds:47,maxBatch:3});
  await closeInbox(page);await page.reload();await openInbox(page);
  await expect(page.locator('#inbox-maxTurns')).toHaveValue('7');
  await expect(page.locator('#inbox-deadline')).toHaveValue(stop);
  await expect(page.locator('#inbox-cooldownSeconds')).toHaveValue('47');
  await expect(page.locator('#inbox-maxBatch')).toHaveValue('3');
  // Polling must update status without replacing an unsaved draft.
  await page.locator('#inbox-maxBatch').fill('4');
  await page.waitForResponse(r=>r.url().endsWith('/inbox-delivery')&&r.request().method()==='GET');
  await expect(page.locator('#inbox-maxBatch')).toHaveValue('4');
  await page.locator('#inbox-unlimitedTurns').check();
  await expect(page.locator('#inbox-maxTurns')).toBeDisabled();
  await page.locator('#inbox-deadline').fill('');await save(page);
  expect((await policy()).policy).toMatchObject({enabled:true,maxTurns:null,deadline:null,cooldownSeconds:47,maxBatch:4});
  await closeInbox(page);await page.reload();await openInbox(page);
  await expect(page.locator('#inbox-unlimitedTurns')).toBeChecked();
  await expect(page.locator('#inbox-maxTurns')).toBeDisabled();
  await expect(page.locator('#inbox-deadline')).toHaveValue('');
  await expect(page.locator('#inbox-maxBatch')).toHaveValue('4');
  await page.locator('#inbox-cooldownSeconds').focus();
  expect(await page.locator('#inbox-cooldownSeconds').evaluate(el=>getComputedStyle(el).outlineColor)).toBe('rgb(185, 173, 255)');
  expect(await page.locator('#agents-dialog').evaluate(el=>el.scrollWidth<=el.clientWidth)).toBe(true);
  await page.locator('#inbox-delivery-controls').scrollIntoViewIfNeeded();
  await page.screenshot({path:`.cache/inbox-settings-${mobile?'mobile':'desktop'}.png`});
  await page.getByRole('button',{name:'Disable and cancel pending wakes',exact:true}).click();
  await expect(page.locator('#inbox-delivery-status')).toContainText('Off');
  expect((await policy()).policy.enabled).toBe(false);
  await closeInbox(page);await page.reload();await openInbox(page);
  await expect(page.locator('#inbox-delivery-status')).toContainText('Off');
  await closeInbox(page);
  if(await page.locator('#session-more').isVisible())await page.locator('#session-more').click();
  await page.locator('#lifecycle-menu > summary').click();
  await page.locator('#session-settings').click();
  await page.locator('[data-field="override"]').check();
  await expect(page.locator('#session-settings-dialog')).toBeVisible();
  expect(await page.locator('#session-settings-dialog').evaluate(el=>el.scrollWidth<=el.clientWidth)).toBe(true);
  await page.screenshot({path:`.cache/scoped-session-settings-${mobile?'mobile':'desktop'}.png`});
  expect(errors).toEqual([]);
});

test('invalid inbox policy and failed saves preserve persisted settings and the draft for retry',async({page})=>{
  await openSession(page);await openInbox(page);
  const before=(await policy()).policy;
  await page.locator('#inbox-maxTurns').fill('6');
  await page.locator('#inbox-deadline').fill(localDate(Date.now()+48*3600000));
  await authorize(page).click();
  await expect(page.locator('#inbox-delivery-error')).toContainText('optional stop time within 24 hours');
  expect((await policy()).policy).toEqual(before);
  await expect(page.locator('#inbox-maxTurns')).toHaveValue('6');
  await page.locator('#inbox-deadline').fill('');
  const attempts=[];
  await page.route('**/inbox-delivery',route=>{
    if(route.request().method()!=='PUT')return route.continue();
    attempts.push(route.request().postDataJSON());
    return attempts.length===1?route.fulfill({status:503,body:'Temporary storage failure'}):route.continue();
  });
  await authorize(page).click();
  await expect(page.locator('#inbox-delivery-error')).toContainText('Temporary storage failure');
  await expect(page.locator('#inbox-maxTurns')).toHaveValue('6');
  expect((await policy()).policy).toEqual(before);
  await save(page);expect(attempts).toHaveLength(2);expect(attempts[0]).toEqual(attempts[1]);
  await expect(page.locator('#inbox-delivery-error')).toBeEmpty();
  await closeInbox(page);await page.reload();await openInbox(page);
  await expect(page.locator('#inbox-maxTurns')).toHaveValue('6');
});

test('failed initial load blocks saving stale fields and recovers on polling',async({page})=>{
  await openSession(page);
  await page.route('**/inbox-delivery',route=>route.fulfill({status:503,body:'Load unavailable'}));
  if(await page.locator('#session-more').isVisible())await page.locator('#session-more').click();
  await page.locator('#lifecycle-menu > summary').click();
  await page.locator('#session-agents').click();
  await expect(page.locator('#inbox-delivery-status')).toContainText('Cannot load inbox settings');
  await expect(authorize(page)).toBeDisabled();
  await expect(page.locator('#inbox-maxTurns')).toBeDisabled();
  await page.unroute('**/inbox-delivery');
  await expect(authorize(page)).toBeEnabled();
});

test('opening another session loads its own policy and discards the previous draft',async({page})=>{
  await openSession(page);await openInbox(page);
  await page.locator('#inbox-maxTurns').fill('8');await save(page);
  await page.locator('#inbox-maxTurns').fill('9');await closeInbox(page);await openInbox(page);
  await expect(page.locator('#inbox-maxTurns')).toHaveValue('8');
  // Access grants are a separate form and must also save and reload.
  await page.locator('#agent-preset').selectOption('coordinator');
  await page.locator('#save-agent-access').click();
  await expect(page.locator('#save-agent-access')).toBeEnabled();
  expect((await api(`/api/sessions/${session.id}/agent-access`)).startSessions).toBe(true);
  await closeInbox(page);await openInbox(page);
  await expect(page.locator('#agent-preset')).toHaveValue('coordinator');
  await closeInbox(page);
  const other=await api('/api/sessions',{provider:'demo',cwd:fixture,title:'Other inbox policy',projectId:project.id,agentAccess:{communicate:true}});
  try{
    await page.evaluate(id=>localStorage.setItem('agentworks-session',id),other.id);await page.reload();
    await expect(page.locator('#session-heading-title')).toHaveText('Other inbox policy');
    await openInbox(page);await expect(page.locator('#inbox-maxTurns')).toHaveValue('1');
    await expect(page.locator('#inbox-delivery-status')).toContainText('Off');
    await page.locator('#inbox-maxTurns').fill('2');await save(page);
    expect((await api(`/api/sessions/${other.id}/inbox-delivery`)).policy.maxTurns).toBe(2);
    expect((await policy()).policy.maxTurns).toBe(8);
  }finally{await api(`/api/sessions/${other.id}`,undefined,'DELETE');}
});

test.afterAll(async()=>{await stopServer();if(fixture)await rm(fixture,{recursive:true,force:true});});
