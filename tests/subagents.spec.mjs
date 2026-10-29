import {test,expect} from '@playwright/test';
import {spawn} from 'node:child_process';
import {mkdtemp,mkdir,readFile} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';
import {acpAgent} from './support/acp-agent.mjs';
import {serverReady,stopChild,removeFixture} from './support/cleanup.mjs';
const base='http://127.0.0.1:4496';
let dir,server,agent,output='';
async function api(path,body){
  const response=await fetch(base+path,{method:body===undefined?'GET':'POST',headers:{Origin:base,'Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body)});
  expect(response.ok,await response.clone().text()).toBeTruthy();return response.json();
}
async function state(id,status){await expect.poll(async()=>(await api('/api/sessions/'+id)).session.status).toBe(status);}
async function open(page,id){await page.goto(base);await page.evaluate(id=>localStorage.setItem('agentworks-session',id),id);await page.reload();}
async function screenshots(page,name){
  const folder=process.env.AGENTWORKS_SUBAGENT_SCREENSHOTS;if(!folder)return;
  await mkdir(folder,{recursive:true});const viewport=page.viewportSize();
  for(const [device,width,height]of [['desktop',1440,1000],['mobile',390,1000]]){
    await page.setViewportSize({width,height});
    if(await page.locator('body').evaluate(node=>node.classList.contains('menu-open')))await page.locator('.session-item.selected').click();
    await page.screenshot({path:join(folder,`${device}-${name}.png`),animations:'disabled'});
  }
  await page.setViewportSize(viewport);
}
test.beforeAll(async()=>{
  dir=await mkdtemp(join(tmpdir(),'agentworks-subagents-'));
  
  agent=await acpAgent(dir,'tests/fixtures/subagent-agent.mjs');
  await startServer();
});
async function startServer(){
  output='';
  server=spawn(resolve(process.env.AGENTWORKS_TEST_BINARY||'target/debug/agentworks'),[],{env:{...process.env,AGENTWORKS_ACP_COMMAND:agent,AGENTWORKS_RUNTIME_DIR:dir,AGENTWORKS_SUBAGENT_LOG:join(dir,'wire.jsonl'),AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:'4496',AGENTWORKS_ROOTS:dir,AGENTWORKS_CATALOG_PATH:join(dir,'catalog.json'),AGENTWORKS_DB_PATH:join(dir,'sessions.sqlite3')},stdio:['ignore','pipe','pipe']});
  server.stdout.on('data',b=>output+=b);server.stderr.on('data',b=>output+=b);
  let error;server.on('error',e=>error=e);
  await expect.poll(async()=>{if(error)throw error;if(server.exitCode!==null)throw Error(output);return serverReady(server,base);},{timeout:10000}).toBe(true);
}
test.afterAll(async()=>{await stopChild(server);await removeFixture(dir);});

test('subagent tabs separate transcripts, retain approvals, and survive replay',async({page})=>{
  const errors=[];page.on('pageerror',e=>errors.push(e.message));
  await page.setViewportSize({width:1440,height:1000});
  const s=await api('/api/sessions',{provider:'codex-rust',cwd:dir});await state(s.id,'idle');
  await open(page,s.id);
  await api('/api/sessions/'+s.id+'/prompt',{text:'inspect',requestId:'subagents'});await state(s.id,'waiting');
  const researcher=page.getByRole('tab',{name:'Sub: Researcher',exact:true}),reviewer=page.getByRole('tab',{name:'Sub: Reviewer',exact:true});
  const panel=page.locator('#subagent-view');
  await expect(researcher).toBeVisible();await expect(reviewer).toBeVisible();
  await expect(page.getByRole('tab',{name:'Chat',exact:true})).toHaveAttribute('aria-selected','true');
  await expect(page.locator('#messages')).not.toContainText('Legacy working');
  await expect(page.locator('#messages')).not.toContainText('Modern working');
  await expect(page.locator('#approval-area')).toContainText('Reviewer');
  await researcher.click();await expect(researcher).toHaveAttribute('aria-selected','true');
  await expect(panel).toContainText('Legacy working');await expect(panel).not.toContainText('Modern working');
  await expect(page.locator('#messages')).toBeHidden();
  await reviewer.click();await expect(panel).toContainText('Modern working');
  await expect(panel).toContainText('Verify the code carefully');await expect(panel).toContainText('Delegated by Researcher');
  await expect(page.locator('#approval-area')).toContainText('Reviewer');
  await expect(page.locator('#model-picker')).toBeHidden();
  await screenshots(page,'subagent-tabs');
  const snapshot=await api('/api/sessions/'+s.id);
  expect(snapshot.commands.some(c=>c.name==='child-only')).toBe(false);
  expect(snapshot.events.filter(e=>e.kind==='acp'&&e.data.parentSessionId).length).toBeGreaterThan(5);
  await reviewer.focus();await page.keyboard.press('ArrowLeft');await expect(researcher).toBeFocused();
  await expect(panel).toContainText('Legacy working');
  await page.keyboard.press('End');await expect(reviewer).toBeFocused();
  await expect(panel).toContainText('Modern working');
  await page.keyboard.press('Home');await expect(page.getByRole('tab',{name:'Chat',exact:true})).toBeFocused();
  await expect(panel).toBeHidden();
  await page.reload();await expect(reviewer).toBeVisible();await reviewer.click();
  await expect(page.locator('#approval-area')).toContainText('Reviewer');
  await page.getByRole('button',{name:'Allow once',exact:true}).click();await state(s.id,'idle');
  await expect(reviewer).toHaveAttribute('title','Reviewer · Completed');
  await expect(panel).toContainText('Modern result');await expect(panel.locator('.tool-card')).toHaveCount(1);
  await expect(panel).not.toContainText('Legacy result');
  await researcher.click();await expect(panel).toContainText('Legacy result');await expect(panel.locator('.tool-card')).toHaveCount(1);
  await page.getByRole('tab',{name:'Chat',exact:true}).click();
  await expect(page.locator('#messages > .message.assistant')).toContainText(['Root working','Root result']);
  await expect(page.locator('#messages .tool-card').filter({hasText:'Root tool'})).toHaveCount(1);
  await expect(page.locator('#messages .subagent-card')).toHaveCount(0);
  await screenshots(page,'parent-chat');
  const wire=(await readFile(join(dir,'wire.jsonl'),'utf8')).trim().split('\n').map(JSON.parse);
  expect(wire.find(m=>m.method==='initialize').params.clientCapabilities.subagents).toEqual({});
  expect(wire.filter(m=>m.method==='session/prompt')).toHaveLength(1);
  expect(wire.find(m=>m.id===1000&&!m.method).result.outcome.optionId).toBe('allow');
  await stopChild(server);await startServer();await page.reload();
  await expect(researcher).toHaveAttribute('title','Researcher · Completed');
  await reviewer.click();await expect(panel).toContainText('Modern result');
  await page.getByRole('tab',{name:'Terminal',exact:true}).click();await expect(panel).toBeHidden();
  await expect(page.locator('#session-terminal')).toBeVisible();
  await reviewer.click();await expect(panel).toBeVisible();await expect(page.locator('#session-terminal')).toBeHidden();
  expect(errors).toEqual([]);
});

test('mobile strip scrolls, child Stop targets the root, and tabs stay within the viewport',async({page})=>{
  await page.setViewportSize({width:390,height:844});
  const s=await api('/api/sessions',{provider:'codex-rust',cwd:dir});await state(s.id,'idle');await open(page,s.id);
  await api('/api/sessions/'+s.id+'/prompt',{text:'slow',requestId:'stop-subagents'});await state(s.id,'running');
  const reviewer=page.getByRole('tab',{name:'Sub: Reviewer',exact:true});
  await reviewer.click();await expect(page.locator('#subagent-view')).toContainText('Modern working');
  await expect(reviewer).toHaveAttribute('aria-selected','true');
  await expect(page.locator('#prompt')).toBeVisible();
  await page.locator('#stop').click();await state(s.id,'idle');
  await expect(page.getByRole('tab',{name:'Sub: Researcher',exact:true})).toHaveAttribute('title','Researcher · Cancelled');
  await expect(page.locator('#subagent-view .subagent-state')).toHaveText('Cancelled');
  const wire=(await readFile(join(dir,'wire.jsonl'),'utf8')).trim().split('\n').map(JSON.parse);
  expect(wire.filter(m=>m.method==='session/cancel').every(m=>m.params.sessionId.startsWith('root-'))).toBe(true);
  expect(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBe(true);
  expect(await page.locator('#session-tabs').evaluate(node=>node.scrollWidth>node.clientWidth)).toBe(true);
});
