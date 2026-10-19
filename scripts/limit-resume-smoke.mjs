// Deterministic local ACP/MCP/browser/SQLite checks. No provider model calls.
import assert from 'node:assert/strict';
import {spawn,execFileSync} from 'node:child_process';
import {mkdtemp} from 'node:fs/promises';
import {resolve} from 'node:path';
import {once} from 'node:events';
const dir=await mkdtemp(resolve('.cache/limit-resume-'));
const port=process.env.AGENTWORKS_LIMIT_PORT||'4371',base=`http://127.0.0.1:${port}`;
let server;
const delay=ms=>new Promise(r=>setTimeout(r,ms));
async function api(path,body,method=body===undefined?'GET':'POST',code=200){const r=await fetch(base+path,{method,headers:{Origin:base,'Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body)});const text=await r.text();assert.equal(r.status,code,text);return code===200?JSON.parse(text):text;}
async function start(){server=spawn(resolve('target/debug/agentworks'),[],{env:{...process.env,AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:port,AGENTWORKS_ROOTS:resolve('.'),AGENTWORKS_CATALOG_PATH:dir+'/catalog.json',AGENTWORKS_DB_PATH:dir+'/sessions.sqlite3',AGENTWORKS_DEMO_STATE_DIR:dir+'/demo'},stdio:['ignore','inherit','inherit']});for(let i=0;i<80;i++){try{if((await fetch(base+'/api/status')).ok)return;}catch{}await delay(100);}throw new Error('Startup failed');}
async function stop(signal='SIGINT'){const exit=once(server,'exit');server.kill(signal);await exit;server=null;}
const snapshot=id=>api(`/api/sessions/${id}`);
async function waitFor(id,predicate){for(let i=0;i<250;i++){const s=await snapshot(id);if(predicate(s))return s;await delay(80);}throw new Error('Wait timed out '+id+' '+JSON.stringify((await snapshot(id)).session));}
const idle=id=>waitFor(id,s=>s.session.status==='idle');
const policy=(id,p)=>api(`/api/sessions/${id}/continuation`,p,'PUT');
async function prompt(id,text){await idle(id);await api(`/api/sessions/${id}/prompt`,{text,requestId:crypto.randomUUID()});return idle(id);}
async function call(id,name,args={}){const old=(await snapshot(id)).seq;const s=await prompt(id,`mcp ${name} ${JSON.stringify(args)}`);const result=s.events.filter(e=>e.seq>old&&e.kind==='acp'&&e.data.sessionUpdate==='tool_call_update').at(-1)?.data.rawOutput;assert(result,'Missing MCP result');return result;}
function value(r){assert(!r.isError,JSON.stringify(r));return JSON.parse(r.content[0].text);}
const count=s=>s.events.filter(e=>e.kind==='user'&&e.data.initiatedBy==='usage-limit scheduler').length;
try{
 await start();const project=await api('/api/projects',{name:'Quota fixtures'});
 const create=async(title,resumePolicy)=>{const s=await api('/api/sessions',{provider:'demo',cwd:resolve('.'),title,projectId:project.id,...(resumePolicy?{resumePolicy}:{})});await idle(s.id);return s;};
 const a=await create('Off by default');assert.equal(a.continuation.enabled,false);
 let s=await prompt(a.id,'quota known');assert(s.session.continuation.limited);assert.equal(s.session.continuation.dueAt,null);await delay(4500);assert.equal(count(await snapshot(a.id)),0);
 await policy(a.id,{enabled:true});s=await prompt(a.id,'quota unknown');assert.equal(s.session.continuation.dueAt,null);assert.match(s.session.continuation.source,/Claude/);
 await policy(a.id,{enabled:true,manualDueAt:Date.now()+2000});assert((await api('/api/activity')).scheduledContinuations.some(t=>t.id===a.id));
 await api(`/api/sessions/${a.id}/cancel`,{});s=await snapshot(a.id);assert.equal(s.session.continuation.enabled,false);assert.equal(s.session.continuation.dueAt,null);await delay(2500);assert.equal(count(await snapshot(a.id)),0);
 await policy(a.id,{enabled:true});s=await prompt(a.id,'quota native-error');assert.match(s.session.continuation.source,/Codex ACP/);assert.equal(s.session.continuation.dueAt,null);
 await api(`/api/sessions/${a.id}/prompt`,{text:'quota active-approval',requestId:crypto.randomUUID()});await waitFor(a.id,s=>s.approvals.length>0);await delay(2500);assert.equal((await snapshot(a.id)).session.continuation.dueAt,null);await api(`/api/sessions/${a.id}/cancel`,{});await idle(a.id);assert.equal((await snapshot(a.id)).session.continuation.dueAt,null);
 await policy(a.id,{enabled:true});s=await prompt(a.id,'quota generic');assert.equal(s.session.continuation.limited,false);assert.equal(s.session.continuation.dueAt,null);
 s=await prompt(a.id,'quota known recovered');assert.equal(s.session.continuation.dueAt,null,'Normal completion must not schedule despite prior quota evidence');
 s=await prompt(a.id,'quota multi');assert(s.session.continuation.dueAt>Date.now()+5000);const native=s.events.find(e=>e.kind==='provider_session').data.id;
 // New human input cancels pending timer and has precedence.
 await prompt(a.id,'Human replacement task');assert.equal((await snapshot(a.id)).session.continuation.dueAt,null);
 s=await prompt(a.id,'quota known');const due=s.session.continuation.dueAt;assert(due);await stop('SIGKILL');await delay(Math.max(0,due-Date.now()+100));await start();
 s=await waitFor(a.id,s=>count(s)===1&&s.session.status==='idle');assert.equal(s.events.filter(e=>e.kind==='provider_session').at(-1).data.id,native);assert.equal(s.session.continuation.dueAt,null);assert.match(s.events.find(e=>e.kind==='user'&&e.data.initiatedBy==='usage-limit scheduler').data.text,/Inspect.*tool results/);assert.equal(s.events.filter(e=>e.kind==='user'&&e.data.text==='quota known').length,2);
 await stop();await start();await api(`/api/sessions/${a.id}/resume`,{});await idle(a.id);assert.equal(count(await snapshot(a.id)),1);
 // Child inheritance: disabled/default, explicit enabled, payload conflict, no grant copy, revocation.
 const parent=await create('Parent');await api(`/api/sessions/${parent.id}/agent-access`,{communicate:true,startSessions:true},'PUT');await idle(parent.id);
 let r=await call(parent.id,'start_session',{provider:'demo',cwd:resolve('.'),requestId:'disabled',inheritResumePolicy:true});assert(r.isError);
 await policy(parent.id,{enabled:true,maxAttempts:2});
 const args={provider:'demo',cwd:resolve('.'),requestId:'inherit',inheritResumePolicy:true};const child=value(await call(parent.id,'start_session',args));await idle(child.id);assert(child.continuation.enabled);assert.equal(child.continuation.origin,'inherited');assert.equal(child.continuation.maxAttempts,2);assert.equal(child.continuation.parent,parent.id);
 assert.equal(value(await call(parent.id,'start_session',args)).id,child.id);assert((await call(parent.id,'start_session',{...args,inheritResumePolicy:false})).isError);
 const off=value(await call(parent.id,'start_session',{...args,requestId:'off',inheritResumePolicy:false}));assert.equal(off.continuation.enabled,false);
 const access=await api(`/api/sessions/${child.id}/agent-access`);assert(!JSON.stringify(access).includes('"startSessions":true'));
 await prompt(child.id,'quota known');await api(`/api/sessions/${parent.id}/agent-access`,{},'PUT');await delay(5000);assert.equal(count(await snapshot(child.id)),0);assert.equal((await snapshot(child.id)).session.continuation.dueAt,null);
 // Browser mobile and dark: controls, local countdown, no page exceptions.
 const {chromium}=await import('@playwright/test');const browser=await chromium.launch({headless:true});
 try{const page=await browser.newPage({colorScheme:'dark'});page.setDefaultTimeout(10000);const errors=[];page.on('pageerror',e=>errors.push(e.message));await page.goto(base);await page.getByRole('button',{name:/Off by default/}).click();await page.setViewportSize({width:390,height:844});await page.locator('#composer-more').click();await page.locator('#continuation-controls > summary').click();await page.locator('#continuation-enabled').check();await page.waitForTimeout(300);assert(await page.locator('#continuation-enabled').isChecked());assert(await page.locator('#continuation-status').innerText().then(t=>t.includes('human')));await page.locator('#continuation-controls details summary').click();await page.locator('#continuation-attempts').fill('6');const bound=new Date(Date.now()+8*86400000);await page.locator('#continuation-deadline').fill(new Date(bound.getTime()-bound.getTimezoneOffset()*60000).toISOString().slice(0,16));await page.locator('#continuation-save-bounds').click();await page.waitForTimeout(200);assert.equal((await snapshot(a.id)).session.continuation.maxAttempts,6);assert((await snapshot(a.id)).session.continuation.deadline>Date.now()+7*86400000);assert(await page.locator('#continuation-controls').evaluate(n=>n.scrollWidth<=n.clientWidth));await page.locator('#continuation-now').scrollIntoViewIfNeeded();assert(await page.locator('#continuation-now').evaluate(n=>{const r=n.getBoundingClientRect();return r.top>=0&&r.bottom<=innerHeight;}),'All timer controls remain reachable on mobile');assert(await page.locator('#composer').evaluate(n=>{const r=n.getBoundingClientRect();return r.top>=0&&r.bottom<=innerHeight;}),'Expanded bounds must not push the composer off screen');await page.locator('#continuation-cancel').click();await page.waitForTimeout(200);assert(!(await page.locator('#continuation-enabled').isChecked()));assert.deepEqual(errors,[]);}finally{await browser.close();}
 // Simulate both durable crash boundaries while server is down; never touch live storage.
 await stop();
 for(const phase of ['claimed','dispatched']){
  execFileSync('python',['-c',`import sqlite3,json,sys
c=sqlite3.connect(sys.argv[1]);r=c.execute('select state from sessions where id=?',(sys.argv[2],)).fetchone();s=json.loads(r[0]);p=s['continuation'];p.update(enabled=True,limited=True,phase=sys.argv[3],dueAt=1,reason='fixture crash boundary');c.execute('update sessions set state=? where id=?',(json.dumps(s),sys.argv[2]));c.commit()`,dir+'/sessions.sqlite3',a.id,phase]);
  await start();const restored=await snapshot(a.id);assert.equal(restored.session.continuation.phase,'uncertain');assert.equal(restored.session.continuation.dueAt,null);assert.equal(count(restored),1);await stop();
 }
 await start();
 const repeated=await create('Repeated quota',{enabled:true,maxAttempts:1});await prompt(repeated.id,'quota repeat');
 const exhausted=await waitFor(repeated.id,s=>count(s)===1&&s.session.status==='idle');assert.equal(exhausted.session.continuation.attempts,1);assert.equal(exhausted.session.continuation.dueAt,null);assert(exhausted.session.continuation.limited);
 console.log('PASS: L-09 local ACP quota/default-off/unknown/multiwindow/injected failure/Stop/human race, overdue crash+restart/native context+no replay, MCP inheritance/idempotency/revocation/grants, mobile dark controls');
}finally{if(server)await stop();}
