import {serverReady,stopChild} from './support/cleanup.mjs';
import {test,expect} from '@playwright/test';
import {spawn} from 'node:child_process';
import {mkdtemp,mkdir,rm,stat} from 'node:fs/promises';
import {resolve,join} from 'node:path';

// Chat attachments: isolated servers/databases on assigned ports 4661-4662; demo agent only.
// 4661 advertises image + embeddedContext; 4662 advertises no prompt capabilities.
const base='http://127.0.0.1:4661',bare='http://127.0.0.1:4662';
test.use({baseURL:base});
const PNG=Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=','base64');
let fixture,server,bareServer,session;

function launch(port,dir,env={}){
  const proc={log:'',exited:false};
  proc.child=spawn(resolve('target/debug/agentworks'),[],{env:{...process.env,
    AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:String(port),AGENTWORKS_ROOTS:dir,
    AGENTWORKS_CATALOG_PATH:join(dir,'catalog.json'),AGENTWORKS_DB_PATH:join(dir,'sessions.sqlite3'),AGENTWORKS_DEMO_STATE_DIR:join(dir,'demo'),...env},stdio:['ignore','pipe','pipe']});
  proc.child.on('error',e=>{proc.log+=e.message;proc.exited=true;});proc.child.on('exit',()=>proc.exited=true);
  for(const stream of [proc.child.stdout,proc.child.stderr])stream.on('data',s=>{proc.log+=s.toString();});
  return proc;
}
async function ready(proc,url){
  for(let i=0;i<100;i++){
    if(proc.exited)throw new Error(proc.log);
    if(await serverReady(proc.child,url))return;
    await new Promise(r=>setTimeout(r,100));
  }
  throw new Error('Isolated server readiness timed out: '+proc.log);
}
async function call(path,body,method,url=base){
  const response=await fetch(url+path,{method:method||(body?'POST':'GET'),headers:{Origin:url,'Content-Type':'application/json'},body:body?JSON.stringify(body):undefined});
  return {status:response.status,body:await response.text().then(t=>{try{return JSON.parse(t);}catch{return t;}})};
}
async function api(path,body,method,url=base){const r=await call(path,body,method,url);expect(r.status,JSON.stringify(r.body)).toBeLessThan(300);return r.body;}
async function upload(name,bytes,{url=base,id=session.id,origin=url}={}){
  const response=await fetch(`${url}/api/sessions/${id}/attachments?name=${encodeURIComponent(name)}`,{method:'POST',headers:{Origin:origin,'Content-Type':'application/octet-stream'},body:bytes});
  return {status:response.status,body:await response.text().then(t=>{try{return JSON.parse(t);}catch{return t;}})};
}
async function until(fn,ms=20000){const end=Date.now()+ms;while(Date.now()<end){if(await fn())return;await new Promise(r=>setTimeout(r,100));}throw new Error('Condition timed out');}
const snapshot=(id=session.id,url=base)=>api(`/api/sessions/${id}`,undefined,undefined,url);
const state=async(url=base,id=session.id)=>(await snapshot(id,url)).session.status;
const reply=async()=>(await snapshot()).events.filter(e=>e.kind==='acp').map(e=>e.data.update??e.data).filter(u=>u.sessionUpdate==='agent_message_chunk').map(u=>u.content?.text||'').join('');
async function newSession(url=base){const s=await api('/api/sessions',{provider:'demo',cwd:fixture,title:'Attachment test'},undefined,url);await until(async()=>await state(url,s.id)==='idle');return s;}

test.beforeAll(async()=>{
  await mkdir('.cache',{recursive:true});fixture=await mkdtemp(resolve('.cache/attachments-'));
  server=launch(4661,fixture);
  bareServer=launch(4662,fixture,{AGENTWORKS_DB_PATH:join(fixture,'bare.sqlite3'),AGENTWORKS_CATALOG_PATH:join(fixture,'bare.json'),AGENTWORKS_DEMO_PROMPT_CAPS:'{}'});
  await ready(server,base);await ready(bareServer,bare);
});
test.afterAll(async()=>{await stopChild(server?.child);await stopChild(bareServer?.child);if(fixture)await rm(fixture,{recursive:true,force:true});});
test.beforeEach(async()=>{session=null;session=await newSession();});
test.afterEach(async()=>{if(session)await call(`/api/sessions/${session.id}`,undefined,'DELETE');});

test('API: validated uploads reach the harness as embedded blocks once, without host paths',async()=>{
  const policy=(await snapshot()).attachments;
  expect(policy).toMatchObject({image:true,text:true,maxPerPrompt:4,maxImageBytes:5*1024*1024,maxTextBytes:256*1024});
  const image=(await upload('/home/someone/private/pixel.png',PNG)).body;
  expect(image).toMatchObject({name:'pixel.png',kind:'image',mime:'image/png',size:PNG.length});
  const notes=(await upload(join(fixture,'notes.md'),Buffer.from('# Release notes\nShip it.'))).body;
  expect(notes).toMatchObject({name:'notes.md',kind:'text',mime:'text/markdown'});
  // Content decides the type: a PDF, binary data or an oversized file is refused with a reason.
  const pdf=await upload('spec.pdf',Buffer.from('%PDF-1.7 binary'));
  expect(pdf.status).toBe(415);expect(pdf.body).toMatch(/PDF/);
  expect((await upload('blob.bin',Buffer.from([0,1,2,255]))).status).toBe(415);
  expect((await upload('big.txt',Buffer.alloc(256*1024+1,97))).status).toBe(415);
  expect((await upload('pixel.png',PNG,{origin:'http://evil.example'})).status).toBe(403);
  // Content is served back for previews with a fixed type; unknown IDs are not files.
  const served=await fetch(`${base}/api/sessions/${session.id}/attachments/${image.id}`);
  expect(served.headers.get('content-type')).toBe('image/png');expect(served.headers.get('x-content-type-options')).toBe('nosniff');
  expect(Buffer.from(await served.arrayBuffer()).equals(PNG)).toBe(true);
  expect((await call(`/api/sessions/${session.id}/attachments/..%2F..%2Fcatalog.json`)).status).toBe(404);

  const requestId='attach-1';
  expect(await api(`/api/sessions/${session.id}/prompt`,{text:'describe these',requestId,attachments:[image.id,notes.id]})).toEqual({accepted:true});
  await until(async()=>await state()==='idle');
  const text=await reply();
  const received=text.slice(text.indexOf('Received attachments:'),text.indexOf('This is the'));
  expect(received).toContain(`- image image/png ${PNG.length} bytes`);
  expect(received).toContain(`- resource text/markdown agentworks://attachments/${notes.id}/notes.md 24 chars: "# Release notes\\nShip it."`);
  expect(received).not.toContain(fixture);
  const user=(await snapshot()).events.filter(e=>e.kind==='user').at(-1);
  expect(user.data.attachments).toEqual([image,notes]);
  expect(JSON.stringify(user)).not.toContain(fixture);

  // Retrying the same request is a duplicate; any other request cannot replay consumed files.
  expect(await api(`/api/sessions/${session.id}/prompt`,{text:'describe these',requestId,attachments:[image.id,notes.id]})).toMatchObject({duplicate:true});
  const again=await call(`/api/sessions/${session.id}/prompt`,{text:'again',requestId:'attach-2',attachments:[image.id]});
  expect(again.status).toBe(409);expect(again.body).toMatch(/already sent/);
  expect((await call(`/api/sessions/${session.id}/attachments/${image.id}`,undefined,'DELETE')).status).toBe(409);
  const many=await Promise.all([1,2,3,4,5].map(i=>upload(`n${i}.txt`,Buffer.from('x'))));
  const tooMany=await call(`/api/sessions/${session.id}/prompt`,{text:'many',requestId:'attach-3',attachments:many.map(r=>r.body.id)});
  expect(tooMany.status).toBe(400);
  // Unsent uploads can be discarded; a missing ID fails the prompt instead of sending without it.
  expect(await api(`/api/sessions/${session.id}/attachments/${many[0].body.id}`,undefined,'DELETE')).toEqual({ok:true});
  expect((await call(`/api/sessions/${session.id}/prompt`,{text:'gone',requestId:'attach-4',attachments:[many[0].body.id]})).status).toBe(404);

  // Text-only prompts are unchanged: no attachment field and nothing extra reaches the harness.
  await api(`/api/sessions/${session.id}/prompt`,{text:'plain hello',requestId:'plain-1'});
  await until(async()=>await state()==='idle');
  const plain=(await snapshot()).events.filter(e=>e.kind==='user').at(-1);
  expect(plain.data.attachments).toBeUndefined();
  expect((await reply()).split('### Local session, turn 2')[1]).not.toContain('Received attachments');

  // Session deletion removes stored files.
  const dir=join(fixture,'sessions.attachments',session.id);
  await stat(dir);
  await api(`/api/sessions/${session.id}`,undefined,'DELETE');session=null;
  await expect(stat(dir)).rejects.toThrow();
});

test('API: a cancelled turn keeps its attachments consumed and reports them for recovery',async()=>{
  const notes=(await upload('todo.txt',Buffer.from('one\ntwo'))).body;
  await api(`/api/sessions/${session.id}/prompt`,{text:'slow please',requestId:'slow-1',attachments:[notes.id]});
  await until(async()=>(await reply()).includes('This is the'));
  await api(`/api/sessions/${session.id}/cancel`,{});
  await until(async()=>await state()==='idle');
  const stop=(await snapshot()).events.find(e=>e.kind==='stop_requested');
  expect(stop.data).toMatchObject({requestId:'slow-1',attachments:1});
  expect((await call(`/api/sessions/${session.id}/prompt`,{text:'retry',requestId:'slow-2',attachments:[notes.id]})).status).toBe(409);
});

test('API and UI: an agent without prompt capabilities refuses attachments with a reason',async({page})=>{
  const s=await newSession(bare);
  try{
    const policy=(await snapshot(s.id,bare)).attachments;
    expect(policy).toMatchObject({image:false,text:false});
    const refused=await upload('a.txt',Buffer.from('hi'),{url:bare,id:s.id});
    expect(refused.status).toBe(415);expect(refused.body).toMatch(/does not accept text/);
    await page.addInitScript(id=>localStorage.setItem('agentworks-session',id),s.id);
    await page.goto(bare);
    await expect(page.locator('#session-state')).toHaveText('Ready');
    await expect(page.locator('#attach')).toBeDisabled();
    await expect(page.locator('#attach')).toHaveAttribute('title',/does not accept/);
  }finally{await call(`/api/sessions/${s.id}`,undefined,'DELETE',bare);}
});

test('UI: attach, preview, send and render; unsupported files and busy turns are explained',async({page})=>{
  await page.addInitScript(id=>localStorage.setItem('agentworks-session',id),session.id);
  await page.goto('/');
  await expect(page.locator('#session-state')).toHaveText('Ready');
  await expect(page.locator('#attach')).toBeEnabled();
  await expect(page.locator('#attach')).toHaveAttribute('title',/PNG, JPEG, GIF, WebP/);
  const input=page.locator('#attach-input');
  await input.setInputFiles({name:'report.pdf',mimeType:'application/pdf',buffer:Buffer.from('%PDF-1.7')});
  await expect(page.locator('#toast')).toContainText('PDFs and other binary files are not supported');
  await expect(page.locator('#attachment-list')).toBeHidden();
  await input.setInputFiles([{name:'pixel.png',mimeType:'image/png',buffer:PNG},{name:'notes.txt',mimeType:'text/plain',buffer:Buffer.from('remember the milk')}]);
  const chips=page.locator('#attachment-list .attachment-chip');
  await expect(chips).toHaveCount(2);
  await expect(page.locator('#attachment-list .attachment-chip.ready')).toHaveCount(2);
  await expect(chips.first().locator('img.attachment-thumb')).toBeVisible();
  // Remove and re-add: a removed upload is discarded on the server.
  await chips.nth(1).getByRole('button',{name:'Remove notes.txt'}).click();
  await expect(chips).toHaveCount(1);
  await input.setInputFiles({name:'notes.txt',mimeType:'text/plain',buffer:Buffer.from('remember the milk')});
  await expect(page.locator('#attachment-list .attachment-chip.ready')).toHaveCount(2);
  await page.locator('#prompt').fill('what is attached?');
  await page.locator('#prompt').press('Enter');
  await expect(page.locator('#attachment-list')).toBeHidden();
  const sent=page.locator('.message.user .message-attachments');
  await expect(sent.locator('img.attachment-sent-image')).toHaveAttribute('alt','pixel.png');
  await expect(sent.locator('a.attachment-chip')).toContainText('notes.txt');
  await expect(page.locator('.message.agent, .message:not(.user)').last()).toContainText('image image/png');
  await expect(page.locator('#session-state')).toHaveText('Ready');
  const events=(await snapshot()).events.filter(e=>e.kind==='user');
  expect(events.at(-1).data.attachments.map(a=>a.name)).toEqual(['pixel.png','notes.txt']);
  // Reloading renders sent attachments from metadata only.
  await page.reload();
  await expect(page.locator('.message.user .message-attachments img')).toHaveCount(1);
  // While busy, attachments are not queued; the draft and chips stay.
  await page.locator('#prompt').fill('slow turn');await page.locator('#prompt').press('Enter');
  await expect(page.locator('#stop')).toBeVisible();
  await input.setInputFiles({name:'later.txt',mimeType:'text/plain',buffer:Buffer.from('later')});
  await expect(page.locator('#attachment-list .attachment-chip.ready')).toHaveCount(1);
  await page.locator('#prompt').fill('follow up with file');await page.locator('#prompt').press('Enter');
  await expect(page.locator('#toast')).toContainText('Attachments can’t be queued');
  await expect(page.locator('#prompt')).toHaveValue('follow up with file');
  await expect(page.locator('#attachment-list .attachment-chip')).toHaveCount(1);
  await expect(page.locator('#input-queue')).toBeHidden();
});
