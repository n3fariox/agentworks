// Exercises ACP -> native stdio MCP -> private service bridge, without model calls.
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {mkdtemp} from 'node:fs/promises';
import {resolve} from 'node:path';
import {once} from 'node:events';
const dir=await mkdtemp(resolve('.cache/completion-'));
const port=process.env.AGENTWORKS_COMPLETION_PORT||'4341',base=`http://127.0.0.1:${port}`;
let server;
const delay=ms=>new Promise(r=>setTimeout(r,ms));
async function api(path,body,method=body===undefined?'GET':'POST',code=200){const r=await fetch(base+path,{method,headers:{Origin:base,'Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body)});const text=await r.text();assert.equal(r.status,code,text);return code===200?JSON.parse(text):text;}
async function start(){server=spawn(resolve('target/debug/agentworks'),[],{env:{...process.env,AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:port,AGENTWORKS_ROOTS:resolve('.'),AGENTWORKS_CATALOG_PATH:dir+'/catalog.json',AGENTWORKS_DB_PATH:dir+'/sessions.sqlite3',AGENTWORKS_DEMO_STATE_DIR:dir+'/demo'},stdio:['ignore','inherit','inherit']});for(let i=0;i<80;i++){try{if((await fetch(base+'/api/status')).ok)return;}catch{}await delay(100);}throw new Error('Startup failed');}
async function stop(){if(server.exitCode!==null){server=null;return;}const exit=once(server,'exit');server.kill('SIGINT');await exit;server=null;}
async function idle(id){for(let i=0;i<200;i++){const s=await api(`/api/sessions/${id}`);if(s.session.status==='idle')return s;assert.notEqual(s.session.status,'error',JSON.stringify(s));await delay(100);}throw new Error('Idle timed out '+id);}
async function enable(id,access={communicate:true}){await idle(id);await api(`/api/sessions/${id}/agent-access`,access,'PUT');await idle(id);}
async function call(id,name,args={}){await idle(id);const old=await api(`/api/sessions/${id}`);await api(`/api/sessions/${id}/prompt`,{text:`mcp ${name} ${JSON.stringify(args)}`,requestId:crypto.randomUUID()});const s=await idle(id);const result=s.events.filter(e=>e.seq>old.seq&&e.kind==='acp'&&e.data.sessionUpdate==='tool_call_update').at(-1)?.data.rawOutput;assert(result,'Missing MCP result '+name);return result;}
function value(result){assert(!result.isError,JSON.stringify(result));return JSON.parse(result.content[0].text);}
function denied(result,status=403){assert(result.isError,JSON.stringify(result));assert.equal(JSON.parse(result.content[0].text).status,status);}
try{
 await start();
 const {roots}=await api('/api/status');
 const project=await api('/api/projects',{name:'Completion tests'}),other=await api('/api/projects',{name:'Other'});
 const create=(title,projectId=project.id)=>api('/api/sessions',{provider:'demo',cwd:roots[0],title,projectId});
 const a=await create('Author'),b=await create('Peer'),c=await create('Outside',other.id);
 await Promise.all([idle(a.id),idle(b.id),idle(c.id)]);
 denied(await call(a.id,'get_completion'));denied(await call(a.id,'report_completion',{status:'blocked',summary:'No access',requestId:'no'}));
 await enable(a.id);await enable(b.id);await enable(c.id);
 const catalog=(await call(a.id,'tools/list')).tools.map(t=>t.name);assert(catalog.includes('report_completion'));assert(catalog.includes('get_completion'));
 assert.equal(value(await call(a.id,'get_completion')).report,null);
 const payload={status:'completed',summary:'<b>Durable agent claim</b>',branch:'batch/test',commit:'a'.repeat(40),checks:[{name:'Demo check',result:'passed <script>no execution</script>'}],requestId:'first'};
 const first=value(await call(a.id,'report_completion',payload));assert.equal(first.report.author,a.id);assert.equal(first.report.authorTitle,'Author');assert.equal(first.report.projectId,project.id);assert.equal(first.duplicate,false);
 assert.deepEqual(value(await call(b.id,'get_completion',{sessionId:a.id})).report,first.report);
 const count=async()=> (await api(`/api/sessions/${a.id}`)).events.filter(e=>e.kind==='completion_reported').length;
 const before=await count();const retry=value(await call(a.id,'report_completion',payload));assert.deepEqual(retry.report,first.report);assert(retry.duplicate);assert.equal(await count(),before);
 denied(await call(a.id,'report_completion',{...payload,summary:'Changed'}),409);
 for(const extra of [{actor:b.id},{author:b.id},{sessionId:b.id}])denied(await call(a.id,'report_completion',{...payload,requestId:crypto.randomUUID(),...extra}),400);
 denied(await call(b.id,'get_completion',{sessionId:c.id}));denied(await call(c.id,'get_completion',{sessionId:a.id}));denied(await call(a.id,'get_completion',{actor:b.id}),400);
 for(const bad of [{status:'success'},{summary:''},{summary:'x'.repeat(4097)},{summary:42},{commit:'abc123'},{commit:'z'.repeat(40)},{checks:[{name:'x',result:true}]},{checks:Array(33).fill({name:'x',result:'pass'})},{checks:Array(12).fill({name:'x',result:'x'.repeat(2048)})},{branch:'x'.repeat(257)},{requestId:'x'.repeat(101)}]){
   denied(await call(a.id,'report_completion',{...payload,requestId:crypto.randomUUID(),...bad}),400);
 }
 await enable(a.id,{});denied(await call(a.id,'report_completion',payload));denied(await call(a.id,'get_completion'));denied(await call(b.id,'get_completion',{sessionId:a.id}));
 await enable(b.id,{promptSessions:true});assert.deepEqual(value(await call(b.id,'get_completion',{sessionId:a.id})).report,first.report);denied(await call(b.id,'report_completion',payload));
 await enable(a.id);
 await api(`/api/sessions/${a.id}/project`,{projectId:other.id});
 assert.equal(value(await call(c.id,'get_completion',{sessionId:a.id})).report,null,'Historical project report must not move with author');
 await api(`/api/sessions/${a.id}/project`,{projectId:project.id});
 const latest=value(await call(a.id,'report_completion',{status:'blocked',summary:'Awaiting integration',checks:[],requestId:'second'}));assert(latest.report.id>first.report.id);assert.equal(latest.report.commit,null);
 assert.deepEqual((await api(`/api/sessions/${a.id}/completion`)).report,latest.report);
 // Verify the actual existing dialog uses safe text and keeps the report with an empty inbox.
 const {chromium}=await import('@playwright/test');const browser=await chromium.launch({headless:true});
 try{const page=await browser.newPage();await page.goto(base);await page.getByRole('button',{name:/Author/}).click();await page.setViewportSize({width:390,height:844});await page.emulateMedia({colorScheme:'dark'});await page.click('#session-agents');await page.locator('#agent-completion').getByText('Awaiting integration',{exact:true}).waitFor();assert(await page.locator('#agent-inbox').innerText().then(t=>t.includes('No messages yet')));
 // Latest update arrives through existing polling, with markup remaining text.
 value(await call(a.id,'report_completion',{...payload,requestId:'third'}));await page.locator('#agent-completion pre').filter({hasText:'<b>Durable agent claim</b>'}).waitFor();assert.equal(await page.locator('#agent-completion b, #agent-completion script').count(),0);assert(await page.locator('#agents-dialog').evaluate(n=>n.scrollWidth<=n.clientWidth),'Report must fit mobile dialog');
 }finally{await browser.close();}
 const final=(await api(`/api/sessions/${a.id}/completion`)).report;
 const audit=await api(`/api/sessions/${a.id}/agent-audit`);assert(audit.events.some(e=>e.action==='completion_reported'));assert(audit.events.some(e=>e.action==='report_completion'&&e.outcome==='rejected'));
 await stop();await start();assert.deepEqual((await api(`/api/sessions/${a.id}/completion`)).report,final);
 await api(`/api/sessions/${a.id}/resume`,{});await idle(a.id);
 assert.deepEqual(value(await call(a.id,'get_completion')).report,final);
 const oldRetry=value(await call(a.id,'report_completion',payload));assert.deepEqual(oldRetry.report,first.report);assert(oldRetry.duplicate);assert.deepEqual(value(await call(a.id,'get_completion')).report,final,'Old retry must not replace latest');
 console.log('PASS: completion MCP, validation, identity/project/grants, idempotency, audit/events, safe browser polling and restart persistence');
}finally{if(server)await stop();}
