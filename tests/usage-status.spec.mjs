import {test,expect} from '@playwright/test';
import {spawn} from 'node:child_process';
import {mkdtemp,mkdir,rm} from 'node:fs/promises';
import {resolve,join} from 'node:path';

// U-07 server-backed check. Own server, catalog, database and demo state on port 4463;
// usage_update events are injected by the deterministic demo agent (no model calls).
const base='http://127.0.0.1:4463';
test.use({baseURL:base,viewport:{width:390,height:844}});
let fixture,server,log='',exited=false;

async function api(path,body,method){
  const response=await fetch(base+path,{method:method||(body?'POST':'GET'),headers:{Origin:base,'Content-Type':'application/json'},body:body?JSON.stringify(body):undefined});
  expect(response.ok,await response.clone().text()).toBe(true);return response.json();
}
async function open(page,id){
  await page.evaluate(id=>localStorage.setItem('agentworks-session',id),id);
  await page.reload();
  await expect(page.locator('#session-state')).toHaveText('Ready');
}
async function inject(page,update){
  await page.locator('#prompt').fill('usage-status '+JSON.stringify(update));await page.locator('#send').click();
  await expect(page.locator('.message.assistant').last()).toContainText('usage reported');
  await expect(page.locator('#session-state')).toHaveText('Ready');
}
const toggle=page=>page.locator('.usage-status-toggle');
const details=page=>page.locator('#usage-status-details');

test.beforeAll(async()=>{
  await mkdir('.cache',{recursive:true});fixture=await mkdtemp(resolve('.cache/usage-status-'));
  server=spawn(resolve('target/debug/agentworks'),[],{env:{...process.env,
    AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:'4463',AGENTWORKS_ROOTS:fixture,
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
test.afterAll(async()=>{
  if(server&&!exited){const stopped=new Promise(r=>server.once('exit',r));server.kill('SIGTERM');await stopped;}
  if(fixture)await rm(fixture,{recursive:true,force:true});
});

test('composer usage statusline: live, replay, partial, attributed, session switch, mobile details',async({page})=>{
  const a=await api('/api/sessions',{provider:'demo',cwd:fixture,title:'Usage A'});
  const b=await api('/api/sessions',{provider:'demo',cwd:fixture,title:'Usage B'});
  expect((await fetch(base+'/usage-status.js')).ok).toBe(true);
  await page.goto('/');await open(page,a.id);
  await expect(toggle(page)).toHaveText('Context unknown');
  await inject(page,{used:50000,size:200000,cost:{amount:0.1,currency:'USD'},_meta:{'_claude/model':'demo-model'}});
  await expect(toggle(page)).toHaveText('50k/200k · 25%');
  await expect(details(page)).toBeHidden();
  await toggle(page).click();
  await expect(toggle(page)).toHaveAttribute('aria-expanded','true');
  await expect(details(page)).toBeVisible();
  await expect(details(page)).toContainText('browser-observed');
  await expect(details(page)).toContainText('Provider-reported cost: 0.1000 USD');
  await expect(details(page)).toContainText('5h quota: not reported');
  // Reload replays timestamp-less persisted events: freshness must be unknown, not "now".
  await page.reload();await expect(page.locator('#session-state')).toHaveText('Ready');
  await expect(toggle(page)).toHaveText('⚠ 50k/200k · 25%');
  await toggle(page).click();
  await expect(details(page)).toContainText('freshness unknown (replayed');
  await expect(details(page)).not.toContainText('browser-observed');
  // A live context-only update (ACP requires used/size) must not freshen replayed cost.
  await inject(page,{used:60000,size:200000});
  await expect(toggle(page)).toHaveText('60k/200k · 30%');
  await expect(details(page).locator('div',{hasText:'Provider-reported cost'})).toContainText('0.1000 USD · scope and account billing not verified · freshness unknown (replayed');
  // Subagent-attributed usage cannot overwrite the parent composer.
  await inject(page,{used:999,size:1000,_meta:{parentToolUseId:'toolu_demo'}});
  await expect(toggle(page)).toHaveText('60k/200k · 30%');
  await expect(details(page)).toContainText('1 subagent/parent-attributed ignored');
  // Switching sessions resets the statusline to that session's own events.
  await open(page,b.id);
  await expect(toggle(page)).toHaveText('Context unknown');
  await expect(details(page)).toBeHidden();
  await open(page,a.id);
  await expect(toggle(page)).toHaveText('⚠ 60k/200k · 30%');
  // Reported unified 5h/7d windows (verified live shape; fractions 0..1, resets in seconds).
  await open(page,b.id);
  const rateLimit={status:'allowed',rateLimitType:'five_hour',resetsAt:1792366200,unifiedWindows:{five_hour:{utilization:0.23,resetsAt:1792366200},seven_day:{utilization:0.1,resetsAt:1792803600}}};
  await inject(page,{used:68468,size:1000000,_meta:{'_claude/model':'demo-model','_claude/rateLimit':rateLimit}});
  await expect(toggle(page)).toHaveText('68k/1.0M · 7% · 5h 23% used/77% left · 7d 10% used/90% left');
  await toggle(page).click();
  await expect(details(page)).toContainText('5h quota (reported): 23% used / 77% remaining · status allowed');
  await expect(details(page)).toContainText('7d quota (reported): 10% used / 90% remaining');
  // Invalid fractions are rejected and do not overwrite reported windows.
  await inject(page,{used:70000,size:1000000,_meta:{'_claude/rateLimit':{unifiedWindows:{five_hour:{utilization:1.5},seven_day:{utilization:-0.2}}}}});
  await expect(toggle(page)).toHaveText('70k/1.0M · 7% · 5h 23% used/77% left · 7d 10% used/90% left');
  // Replay: quota and context freshness unknown; a live context update leaves quota stale.
  await page.reload();await expect(page.locator('#session-state')).toHaveText('Ready');
  await expect(toggle(page)).toHaveText('⚠ 70k/1.0M · 7% · 5h 23% used/77% left ⚠ · 7d 10% used/90% left ⚠');
  await inject(page,{used:80000,size:0});
  await expect(toggle(page)).toHaveText('80k ctx · size unknown · 5h 23% used/77% left ⚠ · 7d 10% used/90% left ⚠');
  await toggle(page).click();
  await expect(details(page).locator('div',{hasText:'5h quota'})).toContainText('freshness unknown (replayed');
});
