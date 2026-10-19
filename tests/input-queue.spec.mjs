import {test,expect} from '@playwright/test';
import {spawn} from 'node:child_process';
import {mkdtemp,mkdir} from 'node:fs/promises';
import {resolve,join} from 'node:path';

// L-05: isolated servers/databases on assigned ports 4511-4512; demo agent only, no paid inference.
const base='http://127.0.0.1:4511';
test.use({baseURL:base});
let fixture,server,session;

function launch(port,dir){
  const proc={log:'',exited:false};
  proc.child=spawn(resolve('target/debug/agentworks'),[],{env:{...process.env,
    AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:String(port),AGENTWORKS_ROOTS:dir,
    AGENTWORKS_CATALOG_PATH:join(dir,'catalog.json'),AGENTWORKS_DB_PATH:join(dir,'sessions.sqlite3'),AGENTWORKS_DEMO_STATE_DIR:join(dir,'demo')},stdio:['ignore','pipe','pipe']});
  proc.child.on('error',e=>{proc.log+=e.message;proc.exited=true;});proc.child.on('exit',()=>proc.exited=true);
  for(const stream of [proc.child.stdout,proc.child.stderr])stream.on('data',s=>{proc.log+=s.toString();});
  return proc;
}
async function ready(proc,url){
  for(let i=0;i<100;i++){
    if(proc.exited)throw new Error(proc.log);
    try{if((await fetch(url+'/api/status')).ok)return;}catch{}
    await new Promise(r=>setTimeout(r,100));
  }
  throw new Error('Isolated server readiness timed out: '+proc.log);
}
async function call(path,body,method,url=base){
  const response=await fetch(url+path,{method:method||(body?'POST':'GET'),headers:{Origin:url,'Content-Type':'application/json'},body:body?JSON.stringify(body):undefined});
  return {status:response.status,body:await response.text().then(t=>{try{return JSON.parse(t);}catch{return t;}})};
}
async function api(path,body,method,url=base){const r=await call(path,body,method,url);expect(r.status,JSON.stringify(r.body)).toBeLessThan(300);return r.body;}
async function until(fn,ms=20000){const end=Date.now()+ms;while(Date.now()<end){if(await fn())return;await new Promise(r=>setTimeout(r,100));}throw new Error('Condition timed out');}
const state=async(url=base,id=session.id)=>(await api(`/api/sessions/${id}`,undefined,undefined,url)).session.status;
async function openSession(page,id=session.id){
  await page.addInitScript(id=>{if(!sessionStorage.getItem('queue-init')){localStorage.setItem('agentworks-session',id);sessionStorage.setItem('queue-init','1');}},id);
  await page.goto('/');
  await expect(page.locator('#session-state')).toHaveText('Ready');
}
async function type(page,text){await page.locator('#prompt').fill(text);await page.locator('#prompt').press('Enter');}

test.beforeAll(async()=>{
  await mkdir('.cache',{recursive:true});fixture=await mkdtemp(resolve('.cache/input-queue-'));
  server=launch(4511,fixture);await ready(server,base);
});
test.afterAll(()=>server?.child.kill());
test.beforeEach(async()=>{session=await api('/api/sessions',{provider:'demo',cwd:fixture,title:'Queue test'});await until(async()=>await state()==='idle');});
test.afterEach(async()=>{if(session)await call(`/api/sessions/${session.id}/stop`,{});});

test('queue during a turn: FIFO order, edit/remove, activity label, dispatch only after normal completion',async({page})=>{
  await openSession(page);
  await type(page,'slow first turn');
  const indicator=page.locator('#activity-indicator');
  await expect(indicator).toBeVisible();
  await expect(indicator.locator('[data-label]')).toHaveText('Model working');
  await expect(indicator.locator('[data-elapsed]')).toHaveText(/^\d+s$/);
  // Composer stays usable while busy; Enter queues and clears the draft only after acceptance.
  await type(page,'queued alpha');
  await type(page,'queued beta');
  await type(page,'queued gamma');
  await expect(page.locator('#prompt')).toHaveValue('');
  const items=page.locator('#input-queue .queue-item');
  await expect(items).toHaveCount(3);
  await expect(items.locator('.queue-text')).toHaveText(['queued alpha','queued beta','queued gamma']);
  await expect(items.first().locator('.queue-state')).toHaveText('Queued');
  // Keyboard edit: Escape cancels, Ctrl+Enter saves.
  await items.nth(1).getByRole('button',{name:/^Edit queued input/}).focus();
  await page.keyboard.press('Enter');
  await page.keyboard.press('Escape');
  await expect(items.nth(1).locator('.queue-text')).toHaveText('queued beta');
  await items.nth(0).getByRole('button',{name:/^Edit queued input/}).click();
  await items.nth(0).locator('textarea').fill('queued alpha edited');
  await items.nth(0).locator('textarea').press('Control+Enter');
  await expect(items.nth(0).locator('.queue-text')).toHaveText('queued alpha edited');
  await items.nth(2).getByRole('button',{name:/^Remove queued input/}).click();
  await expect(items).toHaveCount(2);
  // After the first turn ends normally, the edited head item is sent with its revisioned request ID.
  await expect(page.locator('#messages')).toContainText('queued alpha edited',{timeout:20000});
  await expect(items.nth(0).locator('.queue-state')).toHaveText(/Sending|Sent/);
  await until(async()=>(await api(`/api/sessions/${session.id}/queue`)).queue.items.every(i=>i.state==='sent'));
  const q=(await api(`/api/sessions/${session.id}/queue`)).queue;
  expect(q.items.map(i=>i.text)).toEqual(['queued alpha edited','queued beta']);
  const events=(await api(`/api/sessions/${session.id}`)).events.filter(e=>e.kind==='user').map(e=>e.data.requestId);
  expect(events.filter(id=>id.startsWith('queue:'))).toEqual(['queue:q1:r1','queue:q2:r0']);
  await expect(page.locator('#session-state')).toHaveText('Ready');
  await expect(indicator).toBeHidden();
});

test('duplicate request IDs, changed payloads, stale revisions and bounds',async()=>{
  const path=`/api/sessions/${session.id}/queue`;
  await api(`/api/sessions/${session.id}/prompt`,{text:'slow',requestId:'busy'});
  const first=await api(path,{text:'one',requestId:'r1'});
  expect(first.result.duplicate).toBe(false);
  expect((await api(path,{text:'one',requestId:'r1'})).result.duplicate).toBe(true);
  expect((await call(path,{text:'different',requestId:'r1'})).status).toBe(409);
  const id=first.result.id;
  await api(`${path}/${id}`,{text:'one v2',revision:0},'PATCH');
  expect((await call(`${path}/${id}`,{text:'stale',revision:0},'PATCH')).status).toBe(409);
  expect((await api(path,{text:'one',requestId:'r1'})).result.duplicate).toBe(true);
  expect((await call(path,{text:'one v2',requestId:'r1'})).status).toBe(409);
  expect((await call(path,{text:'x'.repeat(32769),requestId:'big'})).status).toBe(400);
  expect((await call(path,{text:'   ',requestId:'blank'})).status).toBe(400);
  expect((await api(path)).queue.items.length).toBe(1);
  await api(`/api/sessions/${session.id}/stop`,{});
});

test('Stop pauses the queue; nothing sends until a deliberate resume',async({page})=>{
  await openSession(page);
  await type(page,'slow work to stop');
  await expect(page.locator('#activity-indicator')).toBeVisible();
  await type(page,'after stop');
  await expect(page.locator('#input-queue .queue-item')).toHaveCount(1);
  await page.locator('#stop').click();
  await expect(page.locator('#session-state')).toHaveText('Ready');
  await expect(page.locator('.queue-paused')).toContainText('Cancelled');
  await expect(page.locator('#input-queue .queue-state')).toHaveText('Paused');
  await page.waitForTimeout(500);
  expect((await api(`/api/sessions/${session.id}`)).events.filter(e=>e.kind==='user').length).toBe(1);
  await page.getByRole('button',{name:'Resume & send next'}).click();
  await expect(page.locator('#messages')).toContainText('after stop',{timeout:10000});
  await until(async()=>(await api(`/api/sessions/${session.id}/queue`)).queue.items[0].state==='sent');
});

test('sent entries are collapsed history and an empty paused queue offers Resume queue',async({page})=>{
  await openSession(page);
  await type(page,'slow first turn');
  await type(page,'already sent');
  await expect(page.locator('#messages')).toContainText('already sent',{timeout:15000});
  await until(async()=>(await api(`/api/sessions/${session.id}/queue`)).queue.items[0].state==='sent');

  await type(page,'slow turn to cancel');
  await expect(page.locator('#activity-indicator')).toBeVisible();
  await type(page,'remove this pending input');
  await page.locator('#stop').click();
  await expect(page.locator('#session-state')).toHaveText('Ready');
  await expect(page.locator('.queue-paused')).toContainText('Cancelled');
  const queue=(await api(`/api/sessions/${session.id}/queue`)).queue;
  const pending=queue.items.find(item=>item.state==='paused');
  expect(pending).toBeTruthy();
  await api(`/api/sessions/${session.id}/queue/${pending.id}`,undefined,'DELETE');

  await expect(page.locator('#input-queue')).toBeVisible();
  await expect(page.locator('#input-queue .queue-head')).toContainText('Queued inputs (0)');
  await expect(page.locator('#input-queue > ol .queue-item')).toHaveCount(0);
  await expect(page.locator('.queue-history summary')).toHaveText('Recently sent (1)');
  await expect(page.getByRole('button',{name:'Resume queue'})).toBeVisible();
  await expect(page.getByRole('button',{name:'Resume & send next'})).toHaveCount(0);
  await page.getByRole('button',{name:'Resume queue'}).click();
  await expect(page.locator('.queue-paused')).toHaveCount(0);
  await expect(page.locator('#session-state')).toHaveText('Ready');
});

test('approval wait label, queued input dispatches after approval completes the turn',async({page})=>{
  await openSession(page);
  await type(page,'approval');
  await expect(page.locator('#activity-indicator [data-label]')).toHaveText('Waiting for approval');
  await type(page,'after approval');
  await page.getByRole('button',{name:'Allow once',exact:true}).click();
  await expect(page.locator('#messages')).toContainText('after approval',{timeout:15000});
});

test('session switching keeps drafts and per-session queues; replay shows unknown-start elapsed',async({page})=>{
  const other=await api('/api/sessions',{provider:'demo',cwd:fixture,title:'Queue other'});
  await until(async()=>await state(base,other.id)==='idle');
  await openSession(page);
  await type(page,'slow switching');
  await expect(page.locator('#activity-indicator')).toBeVisible();
  await type(page,'queued in A');
  await page.locator('#prompt').fill('unsent draft A');
  await page.locator('.session-item',{hasText:'Queue other'}).first().click();
  await expect(page.locator('#input-queue')).toBeHidden();
  await page.locator('.session-item',{hasText:'Queue test'}).first().click();
  await expect(page.locator('#prompt')).toHaveValue('unsent draft A');
  await expect(page.locator('#input-queue .queue-text')).toHaveText('queued in A');
  // Reload mid-turn: start time is unknown after replay, so elapsed is labelled as observed since reconnect.
  await page.reload();
  await expect(page.locator('#activity-indicator [data-elapsed]')).toContainText('since reconnect');
  await api(`/api/sessions/${session.id}/stop`,{});
  await api(`/api/sessions/${other.id}`,undefined,'DELETE');
});

test('mobile, keyboard and reduced motion',async({browser})=>{
  const context=await browser.newContext({viewport:{width:390,height:844},reducedMotion:'reduce',hasTouch:true});
  const page=await context.newPage();
  await openSession(page);
  await type(page,'slow mobile');
  await expect(page.locator('#activity-indicator')).toBeVisible();
  await type(page,'mobile queued');
  const dot=page.locator('.activity-dots i').first();
  expect(await dot.evaluate(n=>getComputedStyle(n).animationName)).toBe('none');
  const remove=page.getByRole('button',{name:/^Remove queued input/});
  const box=await remove.boundingBox();
  expect(box.height).toBeGreaterThanOrEqual(44);
  await remove.focus();await page.keyboard.press('Enter');
  await expect(page.locator('#input-queue')).toBeHidden();
  await api(`/api/sessions/${session.id}/stop`,{});
  await context.close();
});

test('restart during queued dispatch pauses as ambiguous and never replays',async()=>{
  const dir=await mkdtemp(resolve('.cache/input-queue-restart-'));const url='http://127.0.0.1:4512';
  let proc=launch(4512,dir);await ready(proc,url);
  const s=await api('/api/sessions',{provider:'demo',cwd:dir,title:'Restart'},undefined,url);
  await until(async()=>await state(url,s.id)==='idle');
  await api(`/api/sessions/${s.id}/prompt`,{text:'quick',requestId:'first'},undefined,url);
  await api(`/api/sessions/${s.id}/queue`,{text:'slow mutating queued work',requestId:'qa'},undefined,url);
  await api(`/api/sessions/${s.id}/queue`,{text:'later',requestId:'qb'},undefined,url);
  await until(async()=>(await api(`/api/sessions/${s.id}/queue`,undefined,undefined,url)).queue.items[0].state==='sending');
  proc.child.kill('SIGKILL');await until(()=>proc.exited);
  proc=launch(4512,dir);await ready(proc,url);
  try{
    const q=(await api(`/api/sessions/${s.id}/queue`,undefined,undefined,url)).queue;
    expect(q.items[0].state).toBe('error');
    expect(q.items[0].note).toContain('delivery unknown');
    expect(q.items[1].state).toBe('paused');
    expect(q.paused).toContain('restarted');
    await new Promise(r=>setTimeout(r,500));
    const users=(await api(`/api/sessions/${s.id}`,undefined,undefined,url)).events.filter(e=>e.kind==='user');
    expect(users.filter(e=>e.data.text==='slow mutating queued work').length).toBe(1);
    expect(users.some(e=>e.data.text==='later')).toBe(false);
    // Request bindings survive restart: original payload is a duplicate, changed payload is refused.
    expect((await api(`/api/sessions/${s.id}/queue`,{text:'later',requestId:'qb'},undefined,url)).result.duplicate).toBe(true);
    expect((await call(`/api/sessions/${s.id}/queue`,{text:'later changed',requestId:'qb'},undefined,url)).status).toBe(409);
  }finally{proc.child.kill();}
});

test('late idempotency: removed/sent IDs stay bound and direct prompts never duplicate queued IDs',async()=>{
  const path=`/api/sessions/${session.id}/queue`;
  const a=await api(path,{text:'bound text',requestId:'late-1'});
  await api(`${path}/${a.result.id}`,{text:'edited text',revision:0},'PATCH');
  expect((await call(path,{text:'some other text',requestId:'late-1'})).status).toBe(409);
  await api(`${path}/${a.result.id}`,undefined,'DELETE');
  const retry=await api(path,{text:'bound text',requestId:'late-1'});
  expect(retry.result.duplicate).toBe(true);
  expect(retry.queue.items.length).toBe(0);
  expect((await call(path,{text:'changed after remove',requestId:'late-1'})).status).toBe(409);
  // A late direct prompt with a queued request ID is a duplicate, not a new turn.
  const direct=await api(`/api/sessions/${session.id}/prompt`,{text:'bound text',requestId:'late-1'});
  expect(direct.duplicate).toBe(true);
  expect((await call(`/api/sessions/${session.id}/prompt`,{text:'changed via prompt',requestId:'late-1'})).status).toBe(409);
  // A request ID already sent directly cannot also be queued.
  await api(`/api/sessions/${session.id}/prompt`,{text:'hello direct',requestId:'direct-1'});
  const dup=await api(path,{text:'hello direct',requestId:'direct-1'});
  expect(dup.result.sentDirectly).toBe(true);
  await until(async()=>await state()==='idle');
  expect((await api(`/api/sessions/${session.id}`)).events.filter(e=>e.kind==='user').length).toBe(1);
});

test('quota failure of a queued item pauses the queue and never sends later items',async()=>{
  const path=`/api/sessions/${session.id}/queue`;
  await api(`/api/sessions/${session.id}/prompt`,{text:'slow before quota',requestId:'pre'});
  await api(path,{text:'quota known',requestId:'qq'});
  await api(path,{text:'after quota',requestId:'aq'});
  await until(async()=>(await api(path)).queue.paused!=null,25000);
  const q=(await api(path)).queue;
  expect(q.items[0].state).toBe('error');
  expect(q.items[1].state).toBe('paused');
  await new Promise(r=>setTimeout(r,800));
  const users=(await api(`/api/sessions/${session.id}`)).events.filter(e=>e.kind==='user').map(e=>e.data.text);
  expect(users).toEqual(['slow before quota','quota known']);
});

test('status-only probe turn does not trigger queued substantive work; a real turn does',async()=>{
  const path=`/api/sessions/${session.id}/queue`;
  await api(path,{text:'substantive queued',requestId:'sq'});
  await api(`/api/sessions/${session.id}/prompt`,{text:'/status',requestId:'probe'});
  await until(async()=>await state()==='idle');
  await new Promise(r=>setTimeout(r,800));
  expect((await api(path)).queue.items[0].state).toBe('queued');
  await api(`/api/sessions/${session.id}/prompt`,{text:'hello',requestId:'real'});
  await until(async()=>(await api(path)).queue.items[0].state==='sent',15000);
});

test('Stop and Suspend win over turn-end dispatch',async()=>{
  const path=`/api/sessions/${session.id}/queue`;
  // Stop while idle blocks pending items even though nothing is running.
  await api(path,{text:'idle pending',requestId:'ip'});
  await api(`/api/sessions/${session.id}/stop`,{});
  expect((await api(path)).queue.paused).toContain('Stop');
  await api(`${path}/q1`,undefined,'DELETE');
  // Stop on an empty-flight queue keeps its pause across a later successful turn.
  await api(`/api/sessions/${session.id}/prompt`,{text:'hello after stop',requestId:'has'});
  await until(async()=>await state()==='idle');
  await api(path,{text:'still paused',requestId:'sp'});
  await new Promise(r=>setTimeout(r,800));
  expect((await api(path)).queue.items.find(i=>i.text==='still paused').state).toBe('paused');
  await api(`${path}/q2`,undefined,'DELETE');
  // Suspend mid-turn: the turn ends but nothing is dispatched; resume of the queue is refused while suspended.
  await api(`/api/sessions/${session.id}/prompt`,{text:'slow suspend me',requestId:'sm'});
  await api(path,{text:'must not run',requestId:'mn'});
  await api(`/api/sessions/${session.id}/suspend`,{});
  await until(async()=>await state()==='suspended');
  await new Promise(r=>setTimeout(r,800));
  expect((await api(path)).queue.items[0].state).toBe('paused');
  expect((await call(`${path}/resume`,{})).status).toBe(409);
  expect((await api(`/api/sessions/${session.id}`)).events.filter(e=>e.kind==='user').map(e=>e.data.text)).toEqual(['hello after stop','slow suspend me']);
});

test('Send then Enter race queues the second input exactly once without losing it',async({page})=>{
  await openSession(page);
  await page.locator('#prompt').fill('slow race first');
  await page.locator('#send').click();
  await page.locator('#prompt').fill('race second');
  await page.locator('#prompt').press('Enter');
  await expect(page.locator('#input-queue .queue-text')).toHaveText(['race second']);
  await expect(page.locator('#prompt')).toHaveValue('');
  await until(async()=>(await api(`/api/sessions/${session.id}/queue`)).queue.items[0]?.state==='sent',25000);
  const users=(await api(`/api/sessions/${session.id}`)).events.filter(e=>e.kind==='user').map(e=>e.data.text);
  expect(users).toEqual(['slow race first','race second']);
});

test('storage fault: queue changes are not acknowledged or applied when persistence fails',async()=>{
  const {DatabaseSync}=await import('node:sqlite');
  const dir=await mkdtemp(resolve('.cache/input-queue-fault-'));const url='http://127.0.0.1:4513';
  const proc=launch(4513,dir);await ready(proc,url);
  try{
    const s=await api('/api/sessions',{provider:'demo',cwd:dir,title:'Fault'},undefined,url);
    await until(async()=>await state(url,s.id)==='idle');
    const path=`/api/sessions/${s.id}/queue`;
    const ok=await api(path,{text:'kept',requestId:'k'},undefined,url);
    // Inject a write fault into this isolated database only.
    const db=new DatabaseSync(join(dir,'sessions.sqlite3'));
    db.exec(`CREATE TRIGGER inject_fault BEFORE UPDATE ON sessions WHEN NEW.state LIKE '%fault-me%' BEGIN SELECT RAISE(ABORT,'injected'); END;`);
    expect((await call(path,{text:'fault-me',requestId:'f'},undefined,url)).status).toBe(500);
    expect((await call(`${path}/${ok.result.id}`,{text:'fault-me edit',revision:0},'PATCH',url)).status).toBe(500);
    const q=(await api(path,undefined,undefined,url)).queue;
    expect(q.items.map(i=>[i.text,i.revision])).toEqual([['kept',0]]);
    db.exec('DROP TRIGGER inject_fault');db.close();
    // After the fault clears, the same request ID is accepted normally (it was never bound).
    expect((await api(path,{text:'fault-me',requestId:'f'},undefined,url)).result.duplicate).toBe(false);
  }finally{proc.child.kill();}
});
