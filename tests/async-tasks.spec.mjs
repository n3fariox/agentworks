import {test,expect} from '@playwright/test';
import {spawn} from 'node:child_process';
import {mkdtemp,readFile} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';
import {acpAgent} from './support/acp-agent.mjs';
import {serverReady,stopChild,removeFixture} from './support/cleanup.mjs';
const base='http://127.0.0.1:4502';
let dir,server,agent,output='';
async function api(path,body){const r=await fetch(base+path,{method:body===undefined?'GET':'POST',headers:{Origin:base,'Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body)});expect(r.ok,await r.clone().text()).toBeTruthy();return r.json();}
async function snapshot(id){return api('/api/sessions/'+id);}
async function status(id,value){await expect.poll(async()=>(await snapshot(id)).session.status).toBe(value);}
async function open(page,id){await page.goto(base);await page.evaluate(id=>localStorage.setItem('agentworks-session',id),id);await page.reload();}
async function wire(){return (await readFile(join(dir,'wire.jsonl'),'utf8')).trim().split('\n').map(JSON.parse);}
async function start(){
 output='';server=spawn(resolve(process.env.AGENTWORKS_TEST_BINARY||'target/debug/agentworks'),[],{env:{...process.env,AGENTWORKS_ACP_COMMAND:agent,AGENTWORKS_RUNTIME_DIR:dir,AGENTWORKS_ASYNC_LOG:join(dir,'wire.jsonl'),AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:'4502',AGENTWORKS_ROOTS:dir,AGENTWORKS_CATALOG_PATH:join(dir,'catalog.json'),AGENTWORKS_DB_PATH:join(dir,'sessions.sqlite3')},stdio:['ignore','pipe','pipe']});
 server.stdout.on('data',b=>output+=b);server.stderr.on('data',b=>output+=b);let error;server.on('error',e=>error=e);
 await expect.poll(async()=>{if(error)throw error;if(server.exitCode!==null)throw Error(output);return serverReady(server,base);},{timeout:10000}).toBe(true);
}
test.beforeAll(async()=>{dir=await mkdtemp(join(tmpdir(),'agentworks-async-'));agent=await acpAgent(dir,'tests/fixtures/async-task-agent.mjs');await start();});
test.afterAll(async()=>{await stopChild(server);await removeFixture(dir);});

const jobCard=(page,name)=>page.locator('#messages .job-result').filter({hasText:name});
async function openLogs(page){const closed=page.locator('#messages details.work-log:not([open]) > .work-log-summary');while(await closed.count())await closed.first().click();}

test('active jobs survive turn completion and reload; finished jobs move to the work log; stop works individually',async({page})=>{
 const errors=[];page.on('pageerror',e=>errors.push(e.message));
 const s=await api('/api/sessions',{provider:'codex-rust',cwd:dir});await status(s.id,'idle');await open(page,s.id);
 await expect(page.locator('#background-jobs')).toBeHidden();
 await api(`/api/sessions/${s.id}/prompt`,{text:'jobs',requestId:'jobs'});await status(s.id,'idle');
 const panel=page.locator('#background-jobs');await expect(panel).toBeVisible();await panel.locator('summary').click();
 // The failed job is not listed as active: its outcome is in the conversation instead.
 await expect(panel.locator('.background-job')).toHaveCount(3);await expect(panel.locator('summary')).toHaveText('Background jobs · 3 active');
 await expect(panel).not.toContainText('Foreign job');await expect(panel).not.toContainText('Failed build');
 const root=panel.locator('.background-job').filter({hasText:'Root build'}),child=panel.locator('.background-job').filter({hasText:'Child build'});
 await expect(root.locator('.background-job-state')).toHaveText('Running');await expect(child).toContainText('Subagent job');
 await openLogs(page);
 const failed=jobCard(page,'Failed build');await expect(failed).toHaveCount(1);await expect(failed.locator('.tool-state')).toHaveText('Background · Failed');
 await failed.locator('summary').first().click();await expect(failed).toContainText('Build failed');await expect(failed).toContainText('Output file: /tmp/fixture-job.output');
 // It folds with the turn's tool calls, inside the same work log.
 await expect(page.locator('#messages details.work-log').filter({has:page.locator('.job-result',{hasText:'Failed build'})}).locator('.work-log-counts')).toContainText('1 background job ended · 1 job failed');
 await expect(page.locator('#messages')).toContainText('Background · Running');
 await page.reload();await expect(panel).toBeVisible();await panel.locator('summary').click();await expect(root.locator('.background-job-state')).toHaveText('Running');
 await expect(panel.locator('.background-job')).toHaveCount(3);await openLogs(page);await expect(jobCard(page,'Failed build')).toHaveCount(1);
 await child.getByRole('button',{name:'Stop Child build'}).click();await expect(child).toHaveCount(0);await expect(root.locator('.background-job-state')).toHaveText('Running');
 // A child's job outcome stays attributed to the child transcript, not the root conversation.
 await expect(jobCard(page,'Child build')).toHaveCount(0);
 await page.getByRole('tab',{name:'Sub: Worker',exact:true}).click();await expect(page.locator('#subagent-view')).toContainText('Child build · Stopped');
 await page.getByRole('tab',{name:'Chat',exact:true}).click();
 await panel.getByRole('button',{name:'Stop Protected build'}).click();await expect(panel.getByRole('alert')).toContainText(['Fixture stop rejected']);
 const saved=await snapshot(s.id);expect(saved.asyncTasks.tasks).toHaveLength(4);expect(saved.asyncTasks.tasks.find(t=>t.asyncTaskId==='root-job').usage).toEqual({totalTokens:5});
 const finished=saved.events.filter(e=>e.kind==='async_task_finished');
 expect(finished.map(e=>[e.data.asyncTaskId,e.data.state])).toEqual([['fail-job','failed'],['child-job','stopped']]);
 expect(finished[1].data.parentSessionId).toBe(saved.asyncTasks.tasks.find(t=>t.asyncTaskId==='root-job').acpSessionId);
 for(const e of finished)expect(saved.asyncTasks.tasks.find(t=>t.asyncTaskId===e.data.asyncTaskId).resultSeq).toBe(e.seq);
 const log=await wire();expect(log.find(m=>m.method==='initialize').params.clientCapabilities._meta.jetbrains.air).toEqual({version:1,capabilities:['asyncTasks']});
 expect(log.filter(m=>m.method==='_session/async_task/stop').every(m=>m.params.sessionId===saved.asyncTasks.tasks.find(t=>t.asyncTaskId==='root-job').acpSessionId)).toBe(true);
 expect(log.filter(m=>m.method==='session/cancel')).toHaveLength(0);
 await page.setViewportSize({width:390,height:844});expect(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBe(true);
 expect(errors).toEqual([]);
});

test('individual stop works during a turn, does not cancel that turn and records the outcome',async({page})=>{
 const s=await api('/api/sessions',{provider:'codex-rust',cwd:dir});await status(s.id,'idle');await open(page,s.id);
 await api(`/api/sessions/${s.id}/prompt`,{text:'busy',requestId:'busy'});await status(s.id,'running');
 const panel=page.locator('#background-jobs');await expect(panel).toBeVisible();await panel.locator('summary').click();await panel.getByRole('button',{name:'Stop Root build'}).click();
 await expect(panel).toBeHidden();await expect(jobCard(page,'Root build').locator('.tool-state')).toHaveText('Background · Stopped');
 expect((await snapshot(s.id)).session.status).toBe('running');
 await page.locator('#stop').click();await status(s.id,'idle');
});

test('restart resolves old jobs as unknown in the work log and stale or foreign stop requests are rejected',async({page})=>{
 const s=await api('/api/sessions',{provider:'codex-rust',cwd:dir});await status(s.id,'idle');await open(page,s.id);
 await api(`/api/sessions/${s.id}/prompt`,{text:'jobs',requestId:'restart'});await status(s.id,'idle');
 await stopChild(server);await start();await page.reload();
 // Reconnecting ends control of the old process's jobs; their outcome is unknown, not "still running".
 const panel=page.locator('#background-jobs'),root=jobCard(page,'Root build');
 await expect(root).toHaveCount(1);await expect(panel).toBeHidden();
 await expect(page.locator('#messages .work-log-counts').last()).toHaveText('2 background jobs ended · 2 with unknown status');
 await openLogs(page);await expect(root.locator('.tool-state')).toHaveText('Background · Status unknown');
 await root.locator('summary').first().click();await expect(root).toContainText('The agent process ended before this job reported an outcome');
 const tasks=(await snapshot(s.id)).asyncTasks.tasks;const owner=tasks.find(t=>t.asyncTaskId==='root-job').acpSessionId;
 for(const acpSessionId of [owner,'foreign']){const r=await fetch(`${base}/api/sessions/${s.id}/async-tasks/stop`,{method:'POST',headers:{Origin:base,'Content-Type':'application/json'},body:JSON.stringify({asyncTaskId:'root-job',acpSessionId})});expect(r.status).toBe(409);}
 await expect(jobCard(page,'Failed build').locator('.tool-state')).toHaveText('Background · Failed');
 // Replaying the stored history shows each outcome once, and the panel stays empty.
 await page.reload();await expect(jobCard(page,'Root build')).toHaveCount(1);await expect(panel).toBeHidden();
 await expect(jobCard(page,'Failed build')).toHaveCount(1);await expect(jobCard(page,'Protected build')).toHaveCount(1);
 expect((await snapshot(s.id)).events.filter(e=>e.kind==='async_task_finished').map(e=>e.data.state)).toEqual(['failed','unknown','unknown','unknown']);
});

test('late completion of an idle session is shown in a work log after the turn and survives replay',async({page})=>{
 const s=await api('/api/sessions',{provider:'codex-rust',cwd:dir});await status(s.id,'idle');await open(page,s.id);
 await api(`/api/sessions/${s.id}/prompt`,{text:'late',requestId:'late'});await status(s.id,'idle');
 const panel=page.locator('#background-jobs');
 const late=page.locator('#messages details.work-log').filter({hasText:'Background job · Root build · completed'});
 await expect(late).toHaveCount(1);await expect(panel).toBeHidden();
 // The result comes after the turn's own work log, in event order.
 const order=await page.locator('#messages > details.work-log .work-log-counts').allTextContents();
 expect(order.at(-1)).toBe('Background job · Root build · completed');expect(order.length).toBe(2);
 await late.locator('.work-log-summary').click();const card=jobCard(page,'Root build');await card.locator('summary').first().click();
 await expect(card).toContainText('Checks passed');await expect(card).toContainText('Output file: /tmp/fixture-job.output');await expect(card).toContainText('5 tokens');
 await card.getByRole('button',{name:'Show launching tool call'}).click();
 await expect(page.locator('#messages details.work-log').first()).toHaveAttribute('open','');
 await page.reload();await expect(late).toHaveCount(1);await expect(panel).toBeHidden();
 const trimmed=await api('/api/sessions',{provider:'codex-rust',cwd:dir});await status(trimmed.id,'idle');await open(page,trimmed.id);
 await api(`/api/sessions/${trimmed.id}/prompt`,{text:'trim',requestId:'trim'});await status(trimmed.id,'idle');
 const saved=await snapshot(trimmed.id);expect(saved.truncated).toBe(true);expect(saved.events.some(e=>e.data?.sessionUpdate==='async_task_spawned')).toBe(false);expect(saved.asyncTasks.tasks[0].state).toBe('running');
 await page.reload();await expect(panel).toBeVisible();await panel.locator('summary').click();await expect(panel.locator('.background-job-state')).toHaveText('Running');
});
