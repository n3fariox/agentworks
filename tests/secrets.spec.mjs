import {test,expect} from '@playwright/test';
import {spawn} from 'node:child_process';
import {mkdtemp,mkdir,readFile,readdir,rm,stat,writeFile} from 'node:fs/promises';
import {resolve,join} from 'node:path';

// Settings → Secrets against its own server, database, key file and port; never the live service.
const base='http://127.0.0.1:4545';
test.use({baseURL:base});
test.describe.configure({mode:'serial'});
let fixture,server,log='',exited=false;
const VALUE='ghp_fixture-value-that-must-never-be-stored-in-plaintext';
const REPLACED='replacement-fixture-value-also-private';

async function start(){
  log='';exited=false;
  server=spawn(resolve('target/debug/agentworks'),[],{env:{...process.env,
    AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:'4545',AGENTWORKS_ROOTS:fixture,
    AGENTWORKS_CATALOG_PATH:join(fixture,'catalog.json'),AGENTWORKS_DB_PATH:join(fixture,'sessions.sqlite3'),AGENTWORKS_DEMO_STATE_DIR:join(fixture,'demo')},stdio:['ignore','pipe','pipe']});
  server.on('error',e=>{log+=e.message;exited=true;});server.on('exit',()=>exited=true);
  for(const stream of [server.stdout,server.stderr])stream.on('data',s=>{log+=s.toString();});
  for(let i=0;i<100;i++){
    if(exited)throw new Error(log);
    try{if((await fetch(base+'/api/status')).ok)return;}catch{}
    await new Promise(r=>setTimeout(r,100));
  }
  throw new Error('Isolated server readiness timed out: '+log);
}
async function stop(){
  if(server&&!exited){const stopped=new Promise(r=>server.once('exit',r));server.kill('SIGTERM');await stopped;}
}
async function api(path,body,method){
  return fetch(base+path,{method:method||(body?'POST':'GET'),headers:{Origin:base,'Content-Type':'application/json'},body:body?JSON.stringify(body):undefined});
}
async function stored(){
  const files=(await readdir(fixture)).filter(f=>f.startsWith('sessions.sqlite3'));
  return Buffer.concat(await Promise.all(files.map(f=>readFile(join(fixture,f)))));
}
async function openPanel(page){
  await page.goto('/');
  await page.locator('#nav-settings').evaluate(n=>n.click());
  await page.locator('#settings-tab-secrets').click();
  return page.locator('#secret-controls');
}

test.beforeAll(async()=>{
  await mkdir('.cache',{recursive:true});fixture=await mkdtemp(resolve('.cache/secrets-'));
  await start();
});
test.afterAll(async()=>{
  await stop();
  if(fixture)await rm(fixture,{recursive:true,force:true});
});

test('adds, edits and replaces a secret without ever returning its value',async({page})=>{
  const panel=await openPanel(page);
  await expect(panel.locator('[data-key-note]')).toContainText('sessions.secrets.key');
  await expect(panel).toContainText('No secrets yet.');

  await panel.getByRole('button',{name:'＋ Add secret'}).click();
  await panel.getByLabel('Name').fill('github_token');
  await expect(panel.getByLabel('Name')).toHaveValue('GITHUB_TOKEN');
  await panel.getByLabel('Description').fill('Read-only API token');
  await panel.getByLabel('Value').fill(VALUE);
  await panel.getByRole('button',{name:'Save secret'}).click();
  const card=panel.locator('.model-provider-card',{hasText:'GITHUB_TOKEN'});
  await expect(card).toContainText('Read-only API token');
  await expect(card).toContainText(`Set · ${VALUE.length} bytes`);

  const listed=await (await api('/api/secrets')).text();
  expect(listed).not.toContain(VALUE);
  expect(JSON.parse(listed).secrets[0]).toMatchObject({name:'GITHUB_TOKEN',length:VALUE.length,readable:true});
  expect((await stored()).includes(Buffer.from(VALUE))).toBe(false);
  const key=await stat(join(fixture,'sessions.secrets.key'));
  expect(key.mode&0o777).toBe(0o600);
  const keyFile=JSON.parse(await readFile(join(fixture,'sessions.secrets.key'),'utf8'));
  expect(keyFile).toMatchObject({version:1,kdf:'argon2id',password:false});

  // Editing without a value keeps it; the name cannot change.
  await card.getByRole('button',{name:'Edit'}).click();
  await expect(panel.getByLabel('Name')).toHaveJSProperty('readOnly',true);
  await expect(panel.getByLabel('Value')).toHaveValue('');
  await panel.getByLabel('Description').fill('Org token');
  await panel.getByRole('button',{name:'Save secret'}).click();
  await expect(card).toContainText('Org token');
  await expect(card).toContainText(`${VALUE.length} bytes`);

  await card.getByRole('button',{name:'Edit'}).click();
  await panel.getByLabel('Value').fill(REPLACED);
  await panel.getByRole('button',{name:'Save secret'}).click();
  await expect(card).toContainText(`${REPLACED.length} bytes`);
  const bytes=await stored();
  expect(bytes.includes(Buffer.from(REPLACED))).toBe(false);
});

test('rejects invalid, managed and duplicate names',async({page})=>{
  const panel=await openPanel(page);
  for(const [name,message] of [['PATH','cannot be a secret'],['OPENAI_API_KEY','cannot be a secret'],['GOOSE_MODEL','cannot be a secret'],['2FA','environment variable name'],['GITHUB_TOKEN','already exists']]){
    await panel.getByRole('button',{name:'＋ Add secret'}).click();
    await panel.getByLabel('Name').fill(name);
    await panel.getByLabel('Value').fill('x');
    await panel.getByRole('button',{name:'Save secret'}).click();
    await expect(panel.locator('[data-error]')).toContainText(message);
    await panel.getByRole('button',{name:'Cancel'}).click();
  }
  expect((await api('/api/secrets',{name:'EMPTY_VALUE',value:''})).status).toBe(400);
  expect((await (await api('/api/secrets')).json()).secrets.map(s=>s.name)).toEqual(['GITHUB_TOKEN']);
});

test('assigns secrets to a session; deleting a secret or the session removes assignments',async({page})=>{
  const npm=await (await api('/api/secrets',{name:'NPM_TOKEN',description:'Registry',value:'npm-fixture-value'})).json();
  const github=(await (await api('/api/secrets')).json()).secrets.find(s=>s.name==='GITHUB_TOKEN');
  const session=await (await api('/api/sessions',{provider:'demo',cwd:fixture,title:'Secrets test'})).json();
  const path=`/api/sessions/${session.id}/secrets`;
  await page.addInitScript(id=>localStorage.setItem('agentworks-session',id),session.id);
  await page.goto('/');
  await expect(page.locator('#session-state')).toHaveText('Ready');
  const openSettings=async()=>{
    if(await page.locator('#session-more').isVisible())await page.locator('#session-more').click();
    await page.locator('#session-settings').click();
    await expect(page.locator('#session-settings-dialog')).toBeVisible();
    return page.locator('#session-secret-controls');
  };
  let section=await openSettings();
  await expect(section).toContainText('No secrets assigned.');
  await section.getByLabel('Secret to add').selectOption(github.id);
  await section.getByRole('button',{name:'Add',exact:true}).click();
  await section.getByLabel('Secret to add').selectOption(npm.id);
  await section.getByRole('button',{name:'Add',exact:true}).click();
  await expect(section.getByLabel('Secret to add')).toBeDisabled();
  await expect(section.getByLabel('GITHUB_TOKEN delivery')).toHaveValue('tool');
  await expect(section.getByLabel('GITHUB_TOKEN delegation')).toHaveValue('restricted');
  await section.getByLabel('GITHUB_TOKEN delivery').selectOption('environment');
  await section.getByLabel('GITHUB_TOKEN delegation').selectOption('passable');
  await section.locator('.session-secret',{hasText:'NPM_TOKEN'}).getByLabel('Ask me before each tool use').check();
  await page.locator('#save-session-settings').click();
  await expect(page.locator('#session-settings-dialog')).toBeHidden();
  await expect(page.locator('#conversation')).toContainText('Session secrets: GITHUB_TOKEN (environment, passable), NPM_TOKEN (tool, ask each use)');

  const saved=await (await api(path)).json();
  expect(saved.assignments).toEqual([
    {secretId:github.id,name:'GITHUB_TOKEN',delivery:'environment',delegation:'passable',approval:'none',origin:'human'},
    {secretId:npm.id,name:'NPM_TOKEN',delivery:'tool',delegation:'restricted',approval:'eachUse',origin:'human'},
  ]);
  expect(JSON.stringify(saved)).not.toContain('npm-fixture-value');
  const used=(await (await api('/api/secrets')).json()).secrets;
  expect(used.map(s=>[s.name,s.sessions])).toEqual([['GITHUB_TOKEN',1],['NPM_TOKEN',1]]);

  section=await openSettings();
  await expect(section.getByLabel('GITHUB_TOKEN delivery')).toHaveValue('environment');
  await expect(section.locator('.session-secret',{hasText:'NPM_TOKEN'}).getByLabel('Ask me before each tool use')).toBeChecked();
  await page.locator('#dismiss-session-settings').click();

  expect((await api(path,{assignments:[{secretId:npm.id},{secretId:npm.id}]},'PUT')).status).toBe(400);
  expect((await api(path,{assignments:[{secretId:'missing'}]},'PUT')).status).toBe(404);
  expect((await api(path,{assignments:[{secretId:npm.id,delivery:'shell'}]},'PUT')).status).toBe(422);
  expect((await api(path,{assignments:[{secretId:npm.id,value:'x'}]},'PUT')).status).toBe(422);
  expect((await (await api(path)).json()).assignments).toHaveLength(2);

  expect((await api(`/api/secrets/${npm.id}`,undefined,'DELETE')).ok).toBe(true);
  expect((await (await api(path)).json()).assignments.map(a=>a.name)).toEqual(['GITHUB_TOKEN']);
  expect((await api(`/api/sessions/${session.id}`,undefined,'DELETE')).ok).toBe(true);
  expect((await (await api('/api/secrets')).json()).secrets.map(s=>[s.name,s.sessions])).toEqual([['GITHUB_TOKEN',0]]);
});

test('values stored with a lost key file need re-entry after restart',async({page})=>{
  await stop();
  await rm(join(fixture,'sessions.secrets.key'));
  await start();
  const panel=await openPanel(page);
  const card=panel.locator('.model-provider-card',{hasText:'GITHUB_TOKEN'});
  await expect(card).toContainText('Needs re-entry');
  await card.getByRole('button',{name:'Edit'}).click();
  await expect(panel.locator('[data-value-note]')).toContainText('Enter it again');
  await panel.getByLabel('Value').fill(VALUE);
  await panel.getByRole('button',{name:'Save secret'}).click();
  await expect(card).toContainText(`Set · ${VALUE.length} bytes`);

  page.once('dialog',dialog=>dialog.accept());
  await card.getByRole('button',{name:'Delete'}).click();
  await expect(panel).toContainText('No secrets yet.');
});

test('a damaged key file leaves secrets unavailable and is not replaced',async({page})=>{
  await stop();
  await writeFile(join(fixture,'sessions.secrets.key'),'damaged');
  await start();
  const panel=await openPanel(page);
  await expect(panel.locator('[data-key-note]')).toContainText('Secret storage is unavailable');
  await expect(panel.getByRole('button',{name:'＋ Add secret'})).toBeDisabled();
  expect((await api('/api/secrets',{name:'NEW_TOKEN',value:'x'})).status).toBe(503);
  expect(await readFile(join(fixture,'sessions.secrets.key'),'utf8')).toBe('damaged');
});
