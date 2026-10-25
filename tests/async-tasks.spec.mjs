import {test,expect} from '@playwright/test';
import {spawn} from 'node:child_process';
import {mkdtemp,mkdir,copyFile,readFile} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';
import {serverReady,stopChild,removeFixture} from './support/cleanup.mjs';
const base='http://127.0.0.1:4502';
let dir,server,output='';
async function api(path,body){const r=await fetch(base+path,{method:body===undefined?'GET':'POST',headers:{Origin:base,'Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body)});expect(r.ok,await r.clone().text()).toBeTruthy();return r.json();}
async function snapshot(id){return api('/api/sessions/'+id);}
async function status(id,value){await expect.poll(async()=>(await snapshot(id)).session.status).toBe(value);}
async function open(page,id){await page.goto(base);await page.evaluate(id=>localStorage.setItem('agentworks-session',id),id);await page.reload();}
async function wire(){return (await readFile(join(dir,'wire.jsonl'),'utf8')).trim().split('\n').map(JSON.parse);}
async function start(){
 output='';server=spawn(resolve(process.env.AGENTWORKS_TEST_BINARY||'target/debug/agentworks'),[],{env:{...process.env,AGENTWORKS_NODE:process.execPath,AGENTWORKS_RUNTIME_DIR:dir,AGENTWORKS_ASYNC_LOG:join(dir,'wire.jsonl'),AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:'4502',AGENTWORKS_ROOTS:dir,AGENTWORKS_CATALOG_PATH:join(dir,'catalog.json'),AGENTWORKS_DB_PATH:join(dir,'sessions.sqlite3')},stdio:['ignore','pipe','pipe']});
 server.stdout.on('data',b=>output+=b);server.stderr.on('data',b=>output+=b);let error;server.on('error',e=>error=e);
 await expect.poll(async()=>{if(error)throw error;if(server.exitCode!==null)throw Error(output);return serverReady(server,base);},{timeout:10000}).toBe(true);
}
test.beforeAll(async()=>{dir=await mkdtemp(join(tmpdir(),'agentworks-async-'));await mkdir(join(dir,'node_modules/.bin'),{recursive:true});await copyFile('tests/fixtures/async-task-agent.mjs',join(dir,'node_modules/.bin/codex-acp'));await start();});
test.afterAll(async()=>{await stopChild(server);await removeFixture(dir);});

test('jobs survive turn completion and reload, report failures, and stop individually',async({page})=>{
 const errors=[];page.on('pageerror',e=>errors.push(e.message));
 const s=await api('/api/sessions',{provider:'codex',cwd:dir});await status(s.id,'idle');await open(page,s.id);
 await expect(page.locator('#background-jobs')).toBeHidden();
 await api(`/api/sessions/${s.id}/prompt`,{text:'jobs',requestId:'jobs'});await status(s.id,'idle');
 const panel=page.locator('#background-jobs');await expect(panel).toBeVisible();await panel.locator('summary').click();
 await expect(panel.locator('.background-job')).toHaveCount(4);await expect(panel).not.toContainText('Foreign job');
 const root=panel.locator('.background-job').filter({hasText:'Root build'}),child=panel.locator('.background-job').filter({hasText:'Child build'}),failed=panel.locator('.background-job').filter({hasText:'Failed build'});
 await expect(root.locator('.background-job-state')).toHaveText('Running');await expect(child).toContainText('Subagent job');await expect(failed.locator('.background-job-state')).toHaveText('Failed');await expect(failed.getByRole('button')).toBeHidden();
 await expect(page.locator('#messages')).toContainText('Background · Running');
 await page.reload();await expect(panel).toBeVisible();await panel.locator('summary').click();await expect(root.locator('.background-job-state')).toHaveText('Running');
 await child.getByRole('button',{name:'Stop Child build'}).click();await expect(child.locator('.background-job-state')).toHaveText('Stopped');await expect(root.locator('.background-job-state')).toHaveText('Running');
 await panel.getByRole('button',{name:'Stop Protected build'}).click();await expect(panel.getByRole('alert')).toContainText(['Fixture stop rejected']);
 const saved=await snapshot(s.id);expect(saved.asyncTasks.tasks).toHaveLength(4);expect(saved.asyncTasks.tasks.find(t=>t.asyncTaskId==='root-job').usage).toEqual({totalTokens:5});
 const log=await wire();expect(log.find(m=>m.method==='initialize').params.clientCapabilities._meta.jetbrains.air).toEqual({version:1,capabilities:['asyncTasks']});
 expect(log.filter(m=>m.method==='_session/async_task/stop').every(m=>m.params.sessionId===saved.asyncTasks.tasks.find(t=>t.asyncTaskId==='root-job').acpSessionId)).toBe(true);
 expect(log.filter(m=>m.method==='session/cancel')).toHaveLength(0);
 await page.setViewportSize({width:390,height:844});expect(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBe(true);
 expect(errors).toEqual([]);
});

test('individual stop works during a turn and does not cancel that turn',async({page})=>{
 const s=await api('/api/sessions',{provider:'codex',cwd:dir});await status(s.id,'idle');await open(page,s.id);
 await api(`/api/sessions/${s.id}/prompt`,{text:'busy',requestId:'busy'});await status(s.id,'running');
 const panel=page.locator('#background-jobs');await expect(panel).toBeVisible();await panel.locator('summary').click();await panel.getByRole('button',{name:'Stop Root build'}).click();
 await expect(panel.locator('.background-job-state')).toHaveText('Stopped');expect((await snapshot(s.id)).session.status).toBe('running');
 await page.locator('#stop').click();await status(s.id,'idle');
});

test('restart makes old jobs unknown and stale or foreign stop requests are rejected',async({page})=>{
 const s=await api('/api/sessions',{provider:'codex',cwd:dir});await status(s.id,'idle');await open(page,s.id);
 await api(`/api/sessions/${s.id}/prompt`,{text:'jobs',requestId:'restart'});await status(s.id,'idle');
 await stopChild(server);await start();await page.reload();
 const panel=page.locator('#background-jobs');await expect(panel).toBeVisible();await panel.locator('summary').click();
 const root=panel.locator('.background-job').filter({hasText:'Root build'});await expect(root.locator('.background-job-state')).toHaveText('Status unknown');await expect(root.getByRole('button')).toBeDisabled();
 const tasks=(await snapshot(s.id)).asyncTasks.tasks;const owner=tasks.find(t=>t.asyncTaskId==='root-job').acpSessionId;
 for(const acpSessionId of [owner,'foreign']){const r=await fetch(`${base}/api/sessions/${s.id}/async-tasks/stop`,{method:'POST',headers:{Origin:base,'Content-Type':'application/json'},body:JSON.stringify({asyncTaskId:'root-job',acpSessionId})});expect(r.status).toBe(409);}
 const failed=panel.locator('.background-job').filter({hasText:'Failed build'});await expect(failed.locator('.background-job-state')).toHaveText('Failed');
});


test('late completion updates an idle session and jobs survive truncated transcript replay',async({page})=>{
 const s=await api('/api/sessions',{provider:'codex',cwd:dir});await status(s.id,'idle');await open(page,s.id);
 await api(`/api/sessions/${s.id}/prompt`,{text:'late',requestId:'late'});await status(s.id,'idle');
 const panel=page.locator('#background-jobs');await expect(panel).toBeVisible();await panel.locator('summary').click();
 await expect(panel.locator('.background-job-state')).toHaveText('Completed');await expect(panel).toContainText('Checks passed');
 const trimmed=await api('/api/sessions',{provider:'codex',cwd:dir});await status(trimmed.id,'idle');await open(page,trimmed.id);
 await api(`/api/sessions/${trimmed.id}/prompt`,{text:'trim',requestId:'trim'});await status(trimmed.id,'idle');
 const saved=await snapshot(trimmed.id);expect(saved.truncated).toBe(true);expect(saved.events.some(e=>e.data?.sessionUpdate==='async_task_spawned')).toBe(false);expect(saved.asyncTasks.tasks[0].state).toBe('running');
 await page.reload();await expect(panel).toBeVisible();await panel.locator('summary').click();await expect(panel.locator('.background-job-state')).toHaveText('Running');
});
