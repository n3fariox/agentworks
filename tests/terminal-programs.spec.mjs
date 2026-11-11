import {test,expect} from '@playwright/test';
import {spawn} from 'node:child_process';
import {mkdtemp,mkdir,writeFile,readFile,chmod,rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';
import {serverReady,stopChild} from './support/cleanup.mjs';
const base='http://127.0.0.1:4460';
const binary=resolve(process.env.AGENTWORKS_TEST_BINARY||'target/debug/agentworks');
let root,server,remote;
const sessions={};
async function api(path,body,method){
  const response=await fetch(base+path,{method:method||(body===undefined?'GET':'POST'),headers:{Origin:base,'Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body)});
  if(!response.ok)throw Error(await response.text());return response.json();
}
const records=async()=>JSON.parse('['+(await readFile(join(root,'launches.jsonl'),'utf8')).trim().split('\n').join(',')+']');
async function startServer(){
  const executable=join(root,'agy');
  server=spawn(binary,[],{env:{...process.env,HOME:join(root,'home'),SHELL:'/bin/bash',AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:'4460',AGENTWORKS_ROOTS:root,AGENTWORKS_DB_PATH:join(root,'sessions.sqlite3'),AGENTWORKS_CATALOG_PATH:join(root,'catalog.json'),AGENTWORKS_DEMO_STATE_DIR:join(root,'demo'),AGENTWORKS_SSH:resolve('scripts/fake-ssh.sh'),AGENTWORKS_ANTIGRAVITY_PATH:executable},stdio:['ignore','pipe','pipe']});
  let log='';server.stdout.on('data',b=>log+=b);server.stderr.on('data',b=>log+=b);
  await expect.poll(async()=>{if(server.exitCode!==null)throw Error(log);return serverReady(server,base);}).toBe(true);
}
test.beforeAll(async()=>{
  root=await mkdtemp(join(tmpdir(),'agentworks-interactive-'));await mkdir(join(root,'home'));
  const executable=join(root,'agy');
  await writeFile(executable,`#!/usr/bin/env python3
import sys,os,json
with open('launches.jsonl','a') as log: log.write(json.dumps({'args':sys.argv[1:],'cwd':os.getcwd()})+'\\n')
print('ANTIGRAVITY_INTERACTIVE_READY',flush=True)
for line in sys.stdin:
 if line.strip()=='exit': break
 print('NATIVE_INPUT:'+line.strip(),flush=True)
`);await chmod(executable,0o700);
  await startServer();
  remote=await api('/api/hosts',{name:'CLI host',destination:'terminal-program-fixture',command:binary});
  await expect.poll(async()=>(await api('/api/catalog')).hosts.find(h=>h.id===remote.id)?.status).toBe('connected');
  await api(`/api/hosts/${remote.id}/roots`,{roots:[root]},'PUT');
});
test.afterAll(async()=>{await stopChild(server,{signal:'SIGINT'});await rm(root,{recursive:true,force:true});});
for(const hostId of ['local','remote'])test(`${hostId}: run the interactive CLI and resume only an explicit conversation`,async({page})=>{
  const session=await api('/api/sessions',{provider:'demo',hostId:hostId==='local'?'local':remote.id,cwd:root,title:'Interactive '+hostId});sessions[hostId]=session;
  await page.goto(base);await page.evaluate(id=>localStorage.setItem('agentworks-session',id),session.id);await page.reload();
  await page.getByRole('tab',{name:'Terminal',exact:true}).click();
  await expect(page.locator('#terminal-status')).toHaveText('Shell running');
  await page.locator('#terminal-stop').click();await expect(page.locator('#terminal-program')).toBeEnabled();
  await page.locator('#terminal-program').selectOption('antigravity');
  await page.getByRole('button',{name:'Start Antigravity',exact:true}).click();
  await expect(page.locator('#terminal-status')).toHaveText('Antigravity running');
  await expect(page.locator('#terminal-surface')).toContainText('ANTIGRAVITY_INTERACTIVE_READY');
  expect((await records()).at(-1)).toEqual({args:[],cwd:root});
  const count=(await records()).length;
  await page.reload();await page.getByRole('tab',{name:'Terminal',exact:true}).click();
  await expect(page.locator('#terminal-program')).toHaveValue('antigravity');
  await expect(page.locator('#terminal-surface')).toContainText('ANTIGRAVITY_INTERACTIVE_READY');
  expect((await records()).length).toBe(count);
  await page.locator('#terminal-surface .xterm-helper-textarea').focus();await page.keyboard.insertText('hello native CLI');await page.keyboard.press('Enter');
  await expect(page.locator('#terminal-surface')).toContainText('NATIVE_INPUT:hello native CLI');
  await page.locator('#terminal-stop').click();await expect(page.locator('#terminal-conversation')).toBeEnabled();
  await page.locator('#terminal-conversation').fill('native-'+hostId+'-123');
  await page.getByRole('button',{name:'Start Antigravity',exact:true}).click();
  await expect(page.locator('#terminal-surface')).toContainText('ANTIGRAVITY_INTERACTIVE_READY');
  expect((await records()).at(-1).args).toEqual(['--conversation','native-'+hostId+'-123']);
  await page.locator('#terminal-stop').click();
  await page.reload();await page.getByRole('tab',{name:'Terminal',exact:true}).click();
  await expect(page.locator('#terminal-conversation')).toHaveValue('native-'+hostId+'-123');
  const denied=await fetch(base+`/api/sessions/${session.id}/terminal`,{method:'POST',headers:{Origin:base,'Content-Type':'application/json'},body:JSON.stringify({restart:true,launch:{program:'antigravity',conversationId:'--prompt'}})});
  // A leading option is not a native conversation ID.
  expect(denied.status).toBe(400);
});

test('terminal launch and explicit resume ID survive a service restart',async({page})=>{
  const count=(await records()).length;
  await stopChild(server,{signal:'SIGINT'});await startServer();
  await page.goto(base);await page.evaluate(id=>localStorage.setItem('agentworks-session',id),sessions.local.id);await page.reload();
  await page.getByRole('tab',{name:'Terminal',exact:true}).click();
  await expect(page.locator('#terminal-program')).toHaveValue('antigravity');
  await expect(page.locator('#terminal-conversation')).toHaveValue('native-local-123');
  expect((await records()).length).toBe(count);
  await page.getByRole('button',{name:'Start Antigravity',exact:true}).click();
  await expect(page.locator('#terminal-surface')).toContainText('ANTIGRAVITY_INTERACTIVE_READY');
  expect((await records()).at(-1).args).toEqual(['--conversation','native-local-123']);
  await page.locator('#terminal-stop').click();
});
