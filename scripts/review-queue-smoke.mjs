import {removeFixture} from '../tests/support/cleanup.mjs';
// Deterministic HTTP/native-MCP/browser coverage, isolated from the live service.
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {mkdtemp} from 'node:fs/promises';
import {resolve} from 'node:path';
import {tmpdir} from 'node:os';
import {once} from 'node:events';
const dir=await mkdtemp(resolve(tmpdir(),'review-queue-'));
const port=process.env.AGENTWORKS_REVIEW_QUEUE_PORT||'4495',base=`http://127.0.0.1:${port}`;
let server;
const delay=ms=>new Promise(r=>setTimeout(r,ms));
async function api(path,body,method=body===undefined?'GET':'POST',code=200){const r=await fetch(base+path,{method,headers:{Origin:base,'Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body)});const text=await r.text();assert.equal(r.status,code,text);return code===200?JSON.parse(text):text;}
async function start(){server=spawn(resolve('target/debug/agentworks'),[],{env:{...process.env,AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:port,AGENTWORKS_ROOTS:resolve('.'),AGENTWORKS_CATALOG_PATH:dir+'/catalog.json',AGENTWORKS_DB_PATH:dir+'/sessions.sqlite3',AGENTWORKS_DEMO_STATE_DIR:dir+'/demo',AGENTWORKS_SERVICE_DIR:'',AGENTWORKS_RELEASE_DIR:''},stdio:['ignore','inherit','inherit']});for(let i=0;i<80;i++){try{if((await fetch(base+'/api/status')).ok)return;}catch{}await delay(100);}throw Error('Startup failed');}
async function stop(){if(server.exitCode!==null){server=null;return;}const exit=once(server,'exit');server.kill('SIGINT');await exit;server=null;}
async function idle(id){for(let i=0;i<200;i++){const s=await api(`/api/sessions/${id}`);if(s.session.status==='idle')return s;assert.notEqual(s.session.status,'error',JSON.stringify(s));await delay(100);}throw Error('Idle timed out');}
async function call(id,name,args={}){await idle(id);const old=await api(`/api/sessions/${id}`);await api(`/api/sessions/${id}/prompt`,{text:`mcp ${name} ${JSON.stringify(args)}`,requestId:crypto.randomUUID()});const s=await idle(id);const result=s.events.filter(e=>e.seq>old.seq&&e.kind==='acp'&&e.data.sessionUpdate==='tool_call_update').at(-1)?.data.rawOutput;assert(result,'Missing MCP result '+name);return result;}
function value(result){assert(!result.isError,JSON.stringify(result));return JSON.parse(result.content[0].text);}
function denied(result,status=403){assert(result.isError,JSON.stringify(result));assert.equal(JSON.parse(result.content[0].text).status,status);}
try {
 await start();
 const project=await api('/api/projects',{name:'Queue tests'}),other=await api('/api/projects',{name:'Other'});
 const path=`/api/projects/${project.id}/review-queue`;
 const create=(title,p=project.id)=>api('/api/sessions',{provider:'demo',cwd:resolve('.'),title,projectId:p,agentAccess:{communicate:true}});
 const a=await create('Queue worker'),b=await create('Other coordinator',other.id);await idle(a.id);await idle(b.id);
 denied(await call(a.id,'get_review_queue'));
 await api(`/api/sessions/${a.id}/agent-access`,{communicate:true,startSessions:true,promptSessions:true,cancelSessions:true,runAutomations:true},'PUT');
 denied(await call(a.id,'update_review_queue',{action:'propose',requestId:'forbidden'}));
 assert(!(await call(a.id,'tools/list')).tools.some(t=>t.name==='update_review_queue'));
 await api(`/api/sessions/${a.id}/agent-access`,{communicate:true,manageReviewQueue:true},'PUT');
 await api(`/api/sessions/${b.id}/agent-access`,{manageReviewQueue:true},'PUT');
 assert((await call(a.id,'tools/list')).tools.some(t=>t.name==='update_review_queue'));
 const claim={status:'completed',summary:'Independent claim',commit:'a'.repeat(40),requestId:'claim'};
 value(await call(a.id,'report_completion',claim));assert.deepEqual((await api(path)).items,[],'Claims cannot create proposals');
 const proposal={action:'propose',requestId:'proposal',title:'<img src=x onerror=alert(1)>',branch:'worker',commits:['a'.repeat(40)],target:'main',base:'b'.repeat(40)};
 const first=value(await call(a.id,'update_review_queue',proposal));assert.equal(first.item.state,'queued');
 assert(value(await call(a.id,'update_review_queue',proposal)).duplicate);
 denied(await call(a.id,'update_review_queue',{...proposal,title:'changed'}),409);
 denied(await call(a.id,'update_review_queue',{...proposal,requestId:'spoof',actor:'human'}),400);
 let item=first.item;
 const operation=async(action,extra={})=>{const req={action,id:item.id,revision:item.revision,note:'Reviewed pinned code and isolated test outputs',requestId:crypto.randomUUID(),...extra};const response=await api(path,req);item=response.item;return req;};
 const stale={action:'review',id:item.id,revision:item.revision,note:'Stale',result:'approved',requestId:'stale'};
 await operation('review',{result:'approved'});await api(path,stale,'POST',409);
 await operation('test',{result:'passed'});
 denied(await call(b.id,'update_review_queue',{action:'begin',id:item.id,revision:item.revision,note:'Cross project',requestId:'cross'}),404);
 // Real browser opens the separate ledger and renders untrusted evidence as text.
 const {chromium}=await import('@playwright/test');const browser=await chromium.launch({headless:true});
 try {
  const page=await browser.newPage();await page.goto(base);await page.getByRole('button',{name:/Queue worker/}).click();await page.setViewportSize({width:390,height:844});await page.click('#session-more');await page.click('#lifecycle-menu > summary');await page.click('#session-agents');await page.locator('#review-queue summary').click();
  await page.locator('#review-queue article strong').filter({hasText:proposal.title}).waitFor();assert.equal(await page.locator('#review-queue img').count(),0);
  assert(await page.locator('#agents-dialog').evaluate(n=>n.scrollWidth<=n.clientWidth),'Fits mobile dialog');
  await page.selectOption('#review-queue [name=action]','begin');await page.fill('#review-queue [name=note]','Human begins manual integration; no Git command dispatched');await page.locator('#review-queue button[type=submit]').click();
  await page.locator('#review-queue article strong').filter({hasText:'applying'}).waitFor();
 }finally{await browser.close();}
 item=(await api(path)).items[0];assert.equal(item.state,'applying');
 await api(`/api/projects/${project.id}`,undefined,'DELETE',409);
 const second=await api(path,{...proposal,requestId:'second',title:'Second integration'});
 let secondItem=second.item;
 for(const [action,result] of [['review','approved'],['test','passed']]){secondItem=(await api(path,{action,result,id:secondItem.id,revision:secondItem.revision,note:'Independent evidence',requestId:crypto.randomUUID()})).item;}
 const competing={action:'begin',id:secondItem.id,revision:secondItem.revision,note:'Competing begin',requestId:'competing'};
 await api(path,competing,'POST',409);
 await stop();await start();assert.equal((await api(path)).items[0].state,'applying');await api(path,competing,'POST',409);
 // Revocation applies to cached tools and duplicate payloads.
 await api(`/api/sessions/${a.id}/resume`,{});await idle(a.id);
 await api(`/api/sessions/${a.id}/agent-access`,{communicate:true},'PUT');denied(await call(a.id,'update_review_queue',proposal));
 await api(`/api/sessions/${a.id}/agent-access`,{communicate:true,manageReviewQueue:true},'PUT');assert(value(await call(a.id,'update_review_queue',proposal)).duplicate);
 const finish=await operation('finish',{outcome:'integrated',commit:'c'.repeat(40)});assert.equal(item.state,'integrated');assert((await api(path,finish)).duplicate);
 await api(path,competing); // A rejected transaction never consumed this request ID.
 const ledger=await api(path);assert.equal(ledger.items[0].state,'integrated');assert.equal(ledger.items[1].state,'applying');assert(ledger.events.some(e=>e.actor===a.id));assert(ledger.events.some(e=>e.actor==='human'));
 assert.equal((await api(`/api/sessions/${a.id}/completion`)).report.summary,'Independent claim');
 const tail=await api(path+'?after='+ledger.nextCursor);assert.deepEqual(tail.events,[]);
 console.log('PASS: review queue HTTP, native MCP grants/scope/revocation, revisions/retries, independent claims, manual global slot, restart and mobile safe rendering');
} finally {if(server)await stop();await removeFixture(dir);}
