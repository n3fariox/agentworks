// Real server + browser against an offline ACP fixture and a fake login shell.
// Also covers the harness environment: session markers in, service configuration out.
import {test,expect} from '@playwright/test';
import {spawn} from 'node:child_process';
import {mkdtemp,mkdir,copyFile,writeFile,readFile,chmod} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';
import {serverReady,stopChild,removeFixture} from './support/cleanup.mjs';

const base='http://127.0.0.1:4487';
let dir,server,output='';
async function api(path,body,method=body===undefined?'GET':'POST'){
  const response=await fetch(base+path,{method,headers:{Origin:base,'Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body)});
  expect(response.ok,await response.clone().text()).toBeTruthy();return response.json();
}
async function ready(id,status='idle'){
  await expect.poll(async()=>{const data=await api('/api/sessions/'+id);if(data.session.status==='error'&&status!=='error')throw Error(JSON.stringify(data.events));return data.session.status;}).toBe(status);
}
async function native(id){return (await api('/api/sessions/'+id)).events.findLast(e=>e.kind==='provider_session').data.id;}
async function saved(id){return JSON.parse(await readFile(join(dir,await native(id)+'.json'),'utf8'));}
async function logins(id){return (await saved(id)).logins;}
// The startup file prints noise, as real ones do; only the variables count.
async function rc(value){await writeFile(join(dir,'rc'),`echo "welcome"\nexport AGENTWORKS_LOGIN_FIXTURE_VALUE=${value}\n`);}
test.beforeAll(async()=>{
  dir=await mkdtemp(join(tmpdir(),'agentworks-harness-restart-'));
  await mkdir(join(dir,'node_modules/.bin'),{recursive:true});
  await copyFile('tests/fixtures/harness-restart-agent.mjs',join(dir,'opencode'));
  await chmod(join(dir,'opencode'),0o755);
  await writeFile(join(dir,'controls.json'),'{}');
  // Stands in for `$SHELL -l -i -c CMD`: read the startup file, then run CMD.
  await writeFile(join(dir,'shell'),'#!/bin/sh\n. "$AGENTWORKS_LOGIN_FIXTURE_RC"\nshift 3\nexec /bin/sh -c "$1"\n');
  await chmod(join(dir,'shell'),0o755);
  await rc('before');
  server=spawn(resolve(process.env.AGENTWORKS_TEST_BINARY||'target/debug/agentworks'),[],{env:{...process.env,SHELL:join(dir,'shell'),AGENTWORKS_LOGIN_FIXTURE_RC:join(dir,'rc'),AGENTWORKS_LOGIN_ENV:'',AGENTWORKS_SUPERVISOR_TOKEN:'not-for-harnesses',AGENTWORKS_OPENCODE_PATH:join(dir,'opencode'),AGENTWORKS_MCP_TOKEN:'not-for-harnesses',AGENTWORKS_MCP_TOKEN_FILE:join(dir,'mcp-token'),AGENTWORKS_MCP_ENDPOINT:'127.0.0.1:1',AGENTWORKS_MCP_ENDPOINT_FILE:join(dir,'mcp-endpoint'),NOTIFY_SOCKET:join(dir,'notify'),AGENTWORKS_RUNTIME_DIR:dir,AGENTWORKS_PERMISSION_FIXTURE:dir,AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:'4487',AGENTWORKS_ROOTS:dir,AGENTWORKS_CATALOG_PATH:join(dir,'catalog.json'),AGENTWORKS_DB_PATH:join(dir,'sessions.sqlite3')},stdio:['ignore','pipe','pipe']});
  server.stdout.on('data',b=>output+=b);server.stderr.on('data',b=>output+=b);
  let error;server.on('error',e=>error=e);
  await expect.poll(async()=>{if(error)throw error;if(server.exitCode!==null)throw Error(output);return serverReady(server,base);},{timeout:10000}).toBe(true);
});
test.afterAll(async()=>{await stopChild(server);await removeFixture(dir);});

test('Restart harness picks up an edited login environment and keeps the conversation',async({page})=>{
  const errors=[];page.on('pageerror',e=>errors.push(e.message));
  const s=await api('/api/sessions',{provider:'opencode',cwd:dir});await ready(s.id);
  expect(await logins(s.id)).toEqual(['before']);
  // The harness knows its session, but not the service's own configuration.
  expect((await saved(s.id)).environment).toEqual({AGENTWORKS_SESSION_ID:s.id,AGENTWORKS_SESSION_URI:'agentworks://sessions/'+s.id,AGENTWORKS_DB_PATH:null,AGENTWORKS_PORT:null,AGENTWORKS_SUPERVISOR_TOKEN:null,AGENTWORKS_OPENCODE_PATH:null,AGENTWORKS_MCP_TOKEN:null,AGENTWORKS_MCP_TOKEN_FILE:null,AGENTWORKS_MCP_ENDPOINT:null,AGENTWORKS_MCP_ENDPOINT_FILE:null,NOTIFY_SOCKET:null});
  const sid=await native(s.id);

  await rc('after');
  await page.goto(base);await page.evaluate(id=>localStorage.setItem('agentworks-session',id),s.id);await page.reload();
  await page.locator('#lifecycle-menu summary').click();
  await page.getByRole('button',{name:'Restart harness'}).click();
  await expect.poll(()=>logins(s.id)).toEqual(['before','after']);
  await ready(s.id);
  expect(await native(s.id)).toBe(sid);

  // A stopped harness restarts as a plain resume.
  await rc('suspended');
  await api('/api/sessions/'+s.id+'/suspend',{});await ready(s.id,'suspended');
  await api('/api/sessions/'+s.id+'/restart',{});await ready(s.id);
  expect((await logins(s.id)).at(-1)).toBe('suspended');
  expect(errors).toEqual([]);
});
