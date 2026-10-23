import {test,expect} from '@playwright/test';
import {spawn} from 'node:child_process';
import {mkdtemp,mkdir,writeFile} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';
import {serverReady,stopChild,removeFixture} from './support/cleanup.mjs';
// The Rust Claude bridge against the fake stream-json CLI: no Node adapter, no inference.
const base='http://127.0.0.1:4497';
let dir,server,output='';
async function api(path,body){
  const response=await fetch(base+path,{method:body===undefined?'GET':'POST',headers:{Origin:base,'Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body)});
  expect(response.ok,await response.clone().text()).toBeTruthy();return response.json();
}
async function state(id,status){await expect.poll(async()=>(await api('/api/sessions/'+id)).session.status).toBe(status);}
test.beforeAll(async()=>{
  dir=await mkdtemp(join(tmpdir(),'agentworks-claude-rust-'));
  server=spawn(resolve(process.env.AGENTWORKS_TEST_BINARY||'target/debug/agentworks'),[],{env:{...process.env,CLAUDE_CODE_EXECUTABLE:resolve('tests/claude-acp.test.py'),CLAUDE_CONFIG_DIR:join(dir,'claude'),AGENTWORKS_RUNTIME_DIR:dir,AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:'4497',AGENTWORKS_ROOTS:dir,AGENTWORKS_CATALOG_PATH:join(dir,'catalog.json'),AGENTWORKS_DB_PATH:join(dir,'sessions.sqlite3')},stdio:['ignore','pipe','pipe']});
  server.stdout.on('data',b=>output+=b);server.stderr.on('data',b=>output+=b);
  let error;server.on('error',e=>error=e);
  await expect.poll(async()=>{if(error)throw error;if(server.exitCode!==null)throw Error(output);return serverReady(server,base);},{timeout:10000}).toBe(true);
});
test.afterAll(async()=>{await stopChild(server);await removeFixture(dir);});

test('claude-rust background subagents get their own tab and approvals',async({page})=>{
  const errors=[];page.on('pageerror',e=>errors.push(e.message));
  const s=await api('/api/sessions',{provider:'claude-rust',cwd:dir});await state(s.id,'idle');
  await page.goto(base);await page.evaluate(id=>localStorage.setItem('agentworks-session',id),s.id);await page.reload();
  await api('/api/sessions/'+s.id+'/prompt',{text:'agent',requestId:'claude-rust-agent'});
  // The subagent asks after Claude's first result; the turn is still open, so it waits.
  await state(s.id,'waiting');
  const tab=page.getByRole('tab',{name:'Sub: Look around',exact:true});
  await expect(tab).toBeVisible();
  await expect(page.locator('#approval-area')).toContainText('Look around');
  await expect(page.locator('#messages')).toContainText('Waiting for the agent');
  await expect(page.locator('#messages')).not.toContainText('Found 3 files');
  await tab.click();
  await expect(page.locator('#subagent-view')).toContainText('Found 3 files');
  await page.locator('#approval-area').getByRole('button',{name:'Yes',exact:true}).click();
  await state(s.id,'idle');
  await page.getByRole('tab',{name:'Chat',exact:true}).click();
  await expect(page.locator('#messages')).toContainText('done');
  await page.reload();
  await expect(page.getByRole('tab',{name:'Sub: Look around',exact:true})).toBeVisible();
  expect(errors).toEqual([]);
});

// A saved Claude Code conversation: a background subagent that started a nested one.
async function savedConversation(cwd,session){
  const project=join(dir,'claude/projects',cwd.replace(/[^a-zA-Z0-9]/g,'-')),agents=join(project,session,'subagents');
  await mkdir(agents,{recursive:true});
  const row=(type,uuid,parentUuid,message,extra={})=>({type,uuid,parentUuid,sessionId:session,cwd,isSidechain:false,timestamp:'2026-10-22T10:00:00.000Z',message,...extra});
  const lines=rows=>rows.map(r=>JSON.stringify(r)+'\n').join('');
  const side=(...a)=>row(...a,{isSidechain:true});
  await writeFile(join(project,session+'.jsonl'),lines([
    row('user','m1',null,{role:'user',content:'go'}),
    row('assistant','m2','m1',{id:'msg_root',role:'assistant',content:[{type:'tool_use',id:'toolu_A',name:'Agent',input:{description:'Look',prompt:'Inspect the code'}}]}),
    row('user','m3','m2',{role:'user',content:[{type:'tool_result',tool_use_id:'toolu_A',content:'Async agent launched successfully'}]}),
    row('assistant','m4','m3',{id:'msg_done',role:'assistant',content:[{type:'text',text:'all done'}]}),
  ]));
  await writeFile(join(agents,'agent-aaa.meta.json'),JSON.stringify({toolUseId:'toolu_A',description:'Look',agentType:'general-purpose'}));
  await writeFile(join(agents,'agent-aaa.jsonl'),lines([
    side('user','c1',null,{role:'user',content:'Inspect the code'}),
    side('assistant','c2','c1',{id:'msg_c2',role:'assistant',content:[{type:'text',text:'Child says hi'}]}),
    side('assistant','c3','c2',{id:'msg_c3',role:'assistant',content:[{type:'tool_use',id:'toolu_B',name:'Agent',input:{description:'Nested',prompt:'Go deeper'}}]}),
    side('user','c4','c3',{role:'user',content:[{type:'tool_result',tool_use_id:'toolu_B',content:'nested done'}]}),
  ]));
  await writeFile(join(agents,'agent-bbb.meta.json'),JSON.stringify({toolUseId:'toolu_B',description:'Nested',agentType:'Explore'}));
  await writeFile(join(agents,'agent-bbb.jsonl'),lines([side('assistant','n2',null,{id:'msg_n2',role:'assistant',content:[{type:'text',text:'Nested says hi'}]})]));
}

test('importing a saved Claude conversation rebuilds its subagent tabs',async({page})=>{
  const errors=[];page.on('pageerror',e=>errors.push(e.message));
  const session='33333333-3333-4333-8333-333333333333';
  await savedConversation(dir,session);
  const imported=await api('/api/sessions/import',{provider:'claude-rust',providerSessionId:session,cwd:dir,title:'Imported'});
  const id=imported.session?.id??imported.id;await state(id,'idle');
  await page.goto(base);await page.evaluate(id=>localStorage.setItem('agentworks-session',id),id);await page.reload();
  await expect(page.locator('#messages')).toContainText('all done');
  await expect(page.locator('#messages')).not.toContainText('Child says hi');
  const look=page.getByRole('tab',{name:'Sub: Look',exact:true}),nested=page.getByRole('tab',{name:'Sub: Nested',exact:true});
  await expect(look).toBeVisible();await expect(nested).toBeVisible();
  await look.click();await expect(page.locator('#subagent-view')).toContainText('Child says hi');
  await nested.click();await expect(page.locator('#subagent-view')).toContainText('Nested says hi');
  await page.reload();await expect(page.getByRole('tab',{name:'Sub: Nested',exact:true})).toBeVisible();
  expect(errors).toEqual([]);
});

test('a crashed Claude CLI is reported and the session resumes deliberately',async({page})=>{
  const s=await api('/api/sessions',{provider:'claude-rust',cwd:dir});await state(s.id,'idle');
  await api('/api/sessions/'+s.id+'/prompt',{text:'hello',requestId:'before-crash'});await state(s.id,'idle');
  await api('/api/sessions/'+s.id+'/prompt',{text:'exit',requestId:'crash'});
  await state(s.id,'error');
  await page.goto(base);await page.evaluate(id=>localStorage.setItem('agentworks-session',id),s.id);await page.reload();
  await expect(page.locator('#messages')).toContainText('not retried');
  await api('/api/sessions/'+s.id+'/resume',{});await state(s.id,'idle');
  await api('/api/sessions/'+s.id+'/prompt',{text:'hello',requestId:'after-resume'});await state(s.id,'idle');
  const events=(await api('/api/sessions/'+s.id)).events||[];
  expect(JSON.stringify(events)).toContain('Claude Code exited unexpectedly');
});
