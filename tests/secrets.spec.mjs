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
    await page.locator('#lifecycle-menu > summary').click();
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

test('environment secrets reach the harness; changing them restarts an idle session',async({page})=>{
  const github=(await (await api('/api/secrets')).json()).secrets.find(s=>s.name==='GITHUB_TOKEN');
  const session=await (await api('/api/sessions',{provider:'demo',cwd:fixture,title:'Secret environment'})).json();
  const path=`/api/sessions/${session.id}/secrets`;
  const ask=async(expected)=>{
    await expect(page.locator('#session-state')).toHaveText('Ready');
    await page.locator('#prompt-editor .cm-content').fill('env? GITHUB_TOKEN');await page.locator('#prompt-editor .cm-content').press('Enter');
    await expect(page.locator('#conversation')).toContainText(expected);
  };
  try{
    // Tool-only secrets stay out of the harness environment.
    expect((await api(path,{assignments:[{secretId:github.id}]},'PUT')).ok).toBe(true);
    await page.addInitScript(id=>localStorage.setItem('agentworks-session',id),session.id);
    await page.goto('/');
    await ask('GITHUB_TOKEN is not set.');

    const changed=await (await api(path,{assignments:[{secretId:github.id,delivery:'environment'}]},'PUT')).json();
    expect(changed.environmentChanged).toBe(true);
    await ask(`GITHUB_TOKEN is set (${github.length} bytes).`);

    // A harness that prints the value has it replaced before storage or display.
    await page.locator('#prompt-editor .cm-content').fill('env! GITHUB_TOKEN');await page.locator('#prompt-editor .cm-content').press('Enter');
    await expect(page.locator('#conversation')).toContainText('GITHUB_TOKEN = [redacted:GITHUB_TOKEN] ([redacted:GITHUB_TOKEN]');
    await expect(page.locator('#conversation')).not.toContainText(REPLACED);
    expect((await stored()).includes(Buffer.from(REPLACED))).toBe(false);
    expect((await stored()).includes(Buffer.from(Buffer.from(REPLACED).toString('base64').replace(/=+$/,'')))).toBe(false);

    // Editing other settings does not restart the harness.
    expect((await (await api(path,{assignments:[{secretId:github.id,delivery:'environment',delegation:'passable'}]},'PUT')).json()).environmentChanged).toBe(false);

    expect((await api(path,{assignments:[]},'PUT')).ok).toBe(true);
    await expect(page.locator('#conversation')).toContainText('Session secrets removed');
    await page.locator('#prompt-editor .cm-content').fill('env? GITHUB_TOKEN');await page.locator('#prompt-editor .cm-content').press('Enter');
    await expect(page.locator('#conversation .message').last()).toContainText('GITHUB_TOKEN is not set.');
    expect((await stored()).includes(Buffer.from(REPLACED))).toBe(false);
  }finally{await api(`/api/sessions/${session.id}`,undefined,'DELETE');}
});

test('agents discover secrets and use them through the facade without seeing values',async({page})=>{
  const value='tool-only-fixture-value';
  const secret=await (await api('/api/secrets',{name:'API_TOKEN',description:'Fixture API token',value})).json();
  const session=await (await api('/api/sessions',{provider:'demo',cwd:fixture,title:'Secret tool'})).json();
  const path=`/api/sessions/${session.id}/secrets`;
  const conversation=page.locator('#conversation');
  const send=async text=>{await page.locator('#prompt-editor .cm-content').fill(text);await page.locator('#prompt-editor .cm-content').press('Enter');};
  const run=argv=>'mcp run_with_secrets '+JSON.stringify({argv});
  const measure=['sh','-c','printf "%s" "$1" | wc -c; echo "$1"','sh','{{secret:API_TOKEN}}'];
  try{
    await page.addInitScript(id=>localStorage.setItem('agentworks-session',id),session.id);
    await page.goto('/');
    await send('mcp list_session_secrets {}');
    await expect(conversation).toContainText('secrets\\":[]');
    await expect(page.locator('#session-state')).toHaveText('Ready');

    // Assigning a tool-only secret needs no restart: the same harness sees it next call.
    expect((await (await api(path,{assignments:[{secretId:secret.id}]},'PUT')).json()).environmentChanged).toBe(false);
    await send('mcp list_session_secrets {}');
    await expect(conversation).toContainText('Fixture API token');
    await expect(conversation).toContainText('Use run_with_secrets with {{secret:API_TOKEN}}');
    await expect(page.locator('#session-state')).toHaveText('Ready');

    await send(run(measure));
    await expect(conversation).toContainText(new RegExp(`${value.length}\\\\+n\\[redacted:API_TOKEN\\]`));
    await expect(conversation).not.toContainText(value);
    await expect(page.locator('#session-state')).toHaveText('Ready');

    // The internal release operation and unassigned names are refused.
    await send('mcp resolve_secrets {"names":["API_TOKEN"]}');
    await expect(conversation).toContainText('values are not available to agents directly');
    await expect(page.locator('#session-state')).toHaveText('Ready');
    await send(run(['echo','{{secret:GITHUB_TOKEN}}']));
    await expect(conversation).toContainText('GITHUB_TOKEN is not assigned to this session');
    await expect(page.locator('#session-state')).toHaveText('Ready');

    // Ask each use: deny, allow once (asks again), then allow for this session (stops asking).
    expect((await api(path,{assignments:[{secretId:secret.id,approval:'eachUse'}]},'PUT')).ok).toBe(true);
    const card=page.locator('#approval-area .approval');
    const answer=async name=>{await expect(card).toContainText('Run a command with API_TOKEN');await expect(card).toContainText('{{secret:API_TOKEN}}');await expect(card).not.toContainText(value);await card.getByRole('button',{name}).click();await expect(card).toHaveCount(0);};
    await send(run(['echo','denied-run','{{secret:API_TOKEN}}']));
    await answer('Deny');
    await expect(conversation).toContainText('The user denied this use');
    await expect(page.locator('#session-state')).toHaveText('Ready');
    await send(run(['echo','once-run','{{secret:API_TOKEN}}']));
    await answer('Allow once');
    await expect(conversation).toContainText('once-run [redacted:API_TOKEN]');
    await expect(conversation).toContainText('The user approved this use.');
    await expect(page.locator('#session-state')).toHaveText('Ready');
    await send(run(['echo','session-run','{{secret:API_TOKEN}}']));
    await answer('Allow for this session');
    await expect(conversation).toContainText('session-run [redacted:API_TOKEN]');
    await expect(page.locator('#session-state')).toHaveText('Ready');
    await send(run(['echo','no-prompt-run','{{secret:API_TOKEN}}']));
    await expect(conversation).toContainText('no-prompt-run [redacted:API_TOKEN]');
    await expect(card).toHaveCount(0);

    // Changing the assignment withdraws "allow for this session": the next use asks again.
    expect((await api(path,{assignments:[{secretId:secret.id,approval:'eachUse',delegation:'passable'}]},'PUT')).ok).toBe(true);
    await send(run(['echo','asks-again-run','{{secret:API_TOKEN}}']));
    await answer('Allow once');
    await expect(conversation).toContainText('asks-again-run [redacted:API_TOKEN]');
    await expect(page.locator('#session-state')).toHaveText('Ready');

    // An assignment revoked while its approval is pending is not released when approved.
    await send(run(['echo','revoked-run','{{secret:API_TOKEN}}']));
    await expect(card).toContainText('Run a command with API_TOKEN');
    expect((await api(path,{assignments:[]},'PUT')).ok).toBe(true);
    await card.getByRole('button',{name:'Allow once'}).click();
    await expect(conversation).toContainText('API_TOKEN is not assigned to this session');
    await expect(conversation).not.toContainText('revoked-run [redacted');
    await expect(page.locator('#session-state')).toHaveText('Ready');

    const db=await stored();
    expect(db.includes(Buffer.from(value))).toBe(false);
    const audit=await (await api(`/api/sessions/${session.id}/agent-audit`)).text();
    expect(audit).toContain('secret.use');
    expect(audit).toContain('denied');
    expect(audit).toContain('released');
    expect(audit).not.toContain(value);
  }finally{
    await api(`/api/sessions/${session.id}`,undefined,'DELETE');
    await api(`/api/secrets/${secret.id}`,undefined,'DELETE');
  }
});

test('a password locks secrets until unlocked; commands wait and locked sessions restart with them',async({page})=>{
  const value='password-protected-fixture-value';
  const password='correct horse';
  const secret=await (await api('/api/secrets',{name:'LOCKED_TOKEN',description:'Fixture',value})).json();
  let session;
  try{
    const panel=await openPanel(page);
    const section=panel.locator('[data-password-section]');
    await expect(section).toContainText('No password');
    await section.getByRole('button',{name:'Set password'}).click();
    await page.locator('#secret-password-new').fill('short');
    await page.locator('#secret-password-confirm').fill('short');
    await section.getByRole('button',{name:'Save password'}).click();
    await expect(panel.locator('[data-error]')).toContainText('at least 8 characters');
    await page.locator('#secret-password-new').fill(password);
    await page.locator('#secret-password-confirm').fill(password);
    await section.getByRole('button',{name:'Save password'}).click();
    await expect(section).toContainText('Unlocked');
    await page.locator('#secret-lock-minutes').selectOption('15');
    await expect.poll(async()=>(await (await api('/api/secrets/status')).json()).lockMinutes).toBe(15);
    const key=await readFile(join(fixture,'sessions.secrets.key'),'utf8');
    expect(JSON.parse(key).password).toBe(true);
    expect(key).not.toContain(password);

    // A password-protected key file starts locked; nothing can be sealed or opened.
    await stop();await start();
    const locked=await (await api('/api/secrets/status')).json();
    expect([locked.locked,locked.password,locked.lockMinutes]).toEqual([true,true,15]);
    expect((await api('/api/secrets',{name:'OTHER_TOKEN',value:'other-value'})).status).toBe(423);

    // A session started while locked runs without its environment secret.
    session=await (await api('/api/sessions',{provider:'demo',cwd:fixture,title:'Locked secrets'})).json();
    expect((await api(`/api/sessions/${session.id}/secrets`,{assignments:[{secretId:secret.id,delivery:'environment'}]},'PUT')).ok).toBe(true);
    await page.addInitScript(id=>localStorage.setItem('agentworks-session',id),session.id);
    await page.goto('/');
    const conversation=page.locator('#conversation'),banner=page.locator('#secrets-banner');
    const send=async text=>{await page.locator('#prompt-editor .cm-content').fill(text);await page.locator('#prompt-editor .cm-content').press('Enter');};
    await expect(banner).toBeVisible();
    await expect(conversation).toContainText('Secrets were locked when this harness started: LOCKED_TOKEN');
    await expect(page.locator('#session-state')).toHaveText('Ready');
    await send('env? LOCKED_TOKEN');
    await expect(conversation).toContainText('LOCKED_TOKEN is not set.');
    await expect(page.locator('#session-state')).toHaveText('Ready');

    // A command that needs a secret waits for the unlock instead of failing.
    await send('mcp run_with_secrets '+JSON.stringify({argv:['sh','-c','echo "unlocked-run $1"','sh','{{secret:LOCKED_TOKEN}}']}));
    await expect(conversation).toContainText('Waiting for you to unlock secrets to run a command with LOCKED_TOKEN');
    await banner.getByRole('button',{name:'Unlock secrets'}).click();
    const dialog=page.locator('#secrets-unlock-dialog');
    await dialog.getByLabel('Password').fill('wrong password');
    await dialog.getByRole('button',{name:'Unlock'}).click();
    await expect(dialog).toContainText('The password is not correct');
    await page.waitForTimeout(1100);
    await dialog.getByLabel('Password').fill(password);
    await dialog.getByRole('button',{name:'Unlock'}).click();
    await expect(dialog).toBeHidden();
    await expect(banner).toBeHidden();
    await expect(conversation).toContainText('unlocked-run [redacted:LOCKED_TOKEN]');
    await expect(conversation).not.toContainText(value);

    // Unlocking restarts the session with its environment secret once the turn ends.
    let reply='';
    for(let attempt=0;attempt<20&&!reply.includes('is set');attempt++){
      await expect(page.locator('#session-state')).toHaveText('Ready');
      await page.waitForTimeout(500);
      await send('env? LOCKED_TOKEN');
      await expect(conversation.locator('.message').last()).toContainText('LOCKED_TOKEN is');
      reply=await conversation.locator('.message').last().innerText();
    }
    expect(reply).toContain(`LOCKED_TOKEN is set (${value.length} bytes).`);

    // Changing the password needs the current one; locking and removing it work.
    expect((await api('/api/secrets/password',{current:'wrong password',password:'battery staple'},'PUT')).status).toBe(403);
    expect((await (await api('/api/secrets/lock',{})).json()).locked).toBe(true);
    expect((await api('/api/secrets/password',{current:password,password:''},'PUT')).status).toBe(423);
    expect((await (await api('/api/secrets/unlock',{password})).json()).locked).toBe(false);
    expect((await api('/api/secrets/password',{current:password,password:''},'PUT')).ok).toBe(true);
    await stop();await start();
    const open=await (await api('/api/secrets/status')).json();
    expect([open.locked,open.password]).toEqual([false,false]);
    const list=await (await api('/api/secrets')).json();
    expect(list.secrets.find(s=>s.name==='LOCKED_TOKEN').readable).toBe(true);
    expect((await stored()).includes(Buffer.from(value))).toBe(false);
  }finally{
    if(session)await api(`/api/sessions/${session.id}`,undefined,'DELETE');
    await api(`/api/secrets/${secret.id}`,undefined,'DELETE');
  }
});

test('values stored with a lost key file need re-entry after restart',async({page})=>{
  await stop();
  await rm(join(fixture,'sessions.secrets.key'));
  await start();
  // A session whose environment secret cannot be decrypted still starts, without it.
  const github=(await (await api('/api/secrets')).json()).secrets.find(s=>s.name==='GITHUB_TOKEN');
  const session=await (await api('/api/sessions',{provider:'demo',cwd:fixture,title:'Unreadable secret'})).json();
  expect((await api(`/api/sessions/${session.id}/secrets`,{assignments:[{secretId:github.id,delivery:'environment'}]},'PUT')).ok).toBe(true);
  await page.addInitScript(id=>localStorage.setItem('agentworks-session',id),session.id);
  await page.goto('/');
  await page.locator('#prompt-editor .cm-content').fill('env? GITHUB_TOKEN');await page.locator('#prompt-editor .cm-content').press('Enter');
  await expect(page.locator('#conversation')).toContainText('Not available to this harness: GITHUB_TOKEN');
  await expect(page.locator('#conversation')).toContainText('GITHUB_TOKEN is not set.');
  await api(`/api/sessions/${session.id}`,undefined,'DELETE');
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
