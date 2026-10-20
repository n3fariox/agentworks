import {test,expect} from '@playwright/test';
import {spawn} from 'node:child_process';
import {mkdtemp,mkdir,writeFile,rm,symlink} from 'node:fs/promises';
import {resolve,join} from 'node:path';
const base='http://127.0.0.1:4498';
test.use({baseURL:base});
let fixture,server,log='',exited=false,session;
async function api(path,body,method='POST'){
 const response=await fetch(base+path,{method,headers:{'Content-Type':'application/json',Origin:base},body:body?JSON.stringify(body):undefined});
 expect(response.ok,await response.clone().text()).toBe(true);return response.json();
}
async function send(page,text){
 await page.addInitScript(id=>localStorage.setItem('agentworks-session',id),session.id);
 await page.goto('/');await expect(page.locator('#session-state')).toHaveText('Ready');
 await page.locator('#prompt').fill(text);await page.locator('#send').click();
 await expect(page.locator('.message.assistant .markdown')).toBeVisible();
}
test.beforeAll(async()=>{
  await mkdir('.cache',{recursive:true});fixture=await mkdtemp(resolve('.cache/chat-references-'));
  await writeFile(join(fixture,'sample.rs'),'pub fn sample() -> u8 {\n    42 // answer\n}\n');
  await writeFile(join(fixture,'notes.md'),'# Notes\n\n```python\ndef f():\n    return "x"\n```\n');
  await writeFile(join(fixture,'file with spaces.rs'),Array.from({length:90},(_,i)=>`// line ${i+1}`).join('\n'));
  await symlink('/etc/passwd',join(fixture,'escape.txt'));
  server=spawn(resolve('target/debug/agentworks'),[],{env:{...process.env,
    AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:'4498',AGENTWORKS_ROOTS:fixture,
    AGENTWORKS_CATALOG_PATH:join(fixture,'catalog.json'),AGENTWORKS_DB_PATH:join(fixture,'sessions.sqlite3'),AGENTWORKS_DEMO_STATE_DIR:join(fixture,'demo')},stdio:['ignore','pipe','pipe']});
  server.on('error',e=>{log+=e.message;exited=true;});server.on('exit',()=>exited=true);
  for(const stream of [server.stdout,server.stderr])stream.on('data',s=>{log+=s.toString();});
  for(let i=0;i<100;i++){
    if(exited)throw new Error(log);
    try{if((await fetch(base+'/api/status')).ok)break;}catch{}
    await new Promise(r=>setTimeout(r,100));
    if(i===99)throw new Error('Isolated server readiness timed out: '+log);
  }
});
test.beforeEach(async()=>{session=await api('/api/sessions',{provider:'demo',cwd:fixture,title:'Display preferences test'});});
test.afterEach(async()=>{if(session)await api(`/api/sessions/${session.id}`,undefined,'DELETE');});
test.afterAll(async()=>{
  if(server&&!exited){const stopped=new Promise(r=>server.once('exit',r));server.kill('SIGTERM');await stopped;}
  if(fixture)await rm(fixture,{recursive:true,force:true});
});

test('file references: absolute, encoded and spaced paths, line navigation, external links and root enforcement',async({page,context})=>{
 const spaced=join(fixture,'file with spaces.rs');
 await send(page,`[absolute](<${spaced}:42>) [encoded](${encodeURI(spaced)}:70) [relative](sample.rs:2) [file](file://${encodeURI(spaced)}:12) [outside](/etc/passwd:2) [escape](escape.txt:1) [external](https://example.com/a:42?q=x#part)`);
 const chat=page.locator('.message.assistant');
 for(const [label,line,path] of [['absolute',42,spaced],['encoded',70,spaced],['relative',2,'sample.rs'],['file',12,spaced]]){
  await chat.getByRole('link',{name:label,exact:true}).click();
  await expect(page.locator('#workspace-preview .workspace-preview-header strong')).toHaveText(path);
  const target=page.locator('#workspace-preview .referenced-line');
  await expect(target).toHaveAttribute('data-line',String(line));await expect(target).toBeInViewport();
  await expect(target).toContainText(line===2?'42 // answer':`// line ${line}`);
  if(line===2){
   await expect(page.locator('#workspace-preview .tok-keyword').first()).toHaveText('pub');
   expect(await page.locator('#workspace-preview pre code').textContent()).toBe('pub fn sample() -> u8 {\n    42 // answer\n}\n');
   expect(await page.locator('#workspace-preview .code-block').evaluate(n=>n.codeState.raw)).toBe('pub fn sample() -> u8 {\n    42 // answer\n}\n');
  }
 }
 for(const label of ['outside','escape']){
  await chat.getByRole('link',{name:label,exact:true}).click();
  await expect(page.locator('#workspace-preview')).toContainText('outside allowed workspace roots');
  await expect(page.locator('#workspace-preview')).not.toContainText('root:x:');
 }
 const external=chat.getByRole('link',{name:'external',exact:true});
 await expect(external).toHaveAttribute('href','https://example.com/a:42?q=x#part');await expect(external).toHaveAttribute('target','_blank');
 await context.route('https://example.com/**',route=>route.fulfill({body:'External destination'}));
 const opened=page.waitForEvent('popup');await external.click();const popup=await opened;await popup.waitForLoadState();expect(popup.url()).toBe('https://example.com/a:42?q=x#part');await popup.close();
 const response=await page.request.get(`/api/sessions/${session.id}/file?path=${encodeURIComponent('../'.repeat(15)+'etc/passwd')}`);expect(response.status()).toBe(403);
});

test('inline image viewer: fit, actual size, keyboard/wheel zoom, pan, focus and chat restoration',async({page})=>{
 // Serve a large raster without introducing a binary fixture into the repository.
 const png=await page.evaluate(()=>{const c=document.createElement('canvas');c.width=2000;c.height=1400;return c.toDataURL('image/png').split(',')[1];});
 await writeFile(join(fixture,'large image.png'),Buffer.from(png,'base64'));
 await page.setViewportSize({width:1100,height:800});
 await send(page,Array.from({length:30},(_,i)=>`Paragraph ${i}\n`).join('\n')+'\n![Inspection chart](<large image.png>)\n\nLast paragraph');
 const inline=page.getByRole('button',{name:'Enlarge image: Inspection chart'});
 await inline.scrollIntoViewIfNeeded();await expect(inline).toHaveJSProperty('complete',true);
 const before=await inline.boundingBox();const scroll=await page.locator('#messages').evaluate(n=>n.scrollTop);
 expect(before.height).toBeLessThanOrEqual(420);
 await inline.focus();await page.keyboard.press('Enter');
 const viewer=page.getByRole('dialog',{name:'Image viewer'}),output=viewer.locator('output');
 await expect(viewer).toBeVisible();await expect(viewer.getByRole('button',{name:'Close image viewer'})).toBeFocused();
 await page.keyboard.press('Tab');await expect(viewer.locator('.image-viewport')).toBeFocused();
 await page.keyboard.press('Tab');await expect(viewer.getByRole('button',{name:'Zoom out'})).toBeFocused();
 const fit=parseInt(await output.textContent());expect(fit).toBeLessThan(100);
 await viewer.getByRole('button',{name:'Actual size'}).click();await expect(output).toHaveText('100%');
 await page.keyboard.press('+');await expect(output).toHaveText('125%');
 await viewer.getByRole('button',{name:'Zoom out'}).click();await expect(output).toHaveText('100%');
 const viewport=viewer.locator('.image-viewport');await viewport.hover();await page.mouse.wheel(0,-200);await expect(output).not.toHaveText('100%');
 const image=viewport.locator('img'),initial=await image.evaluate(n=>n.style.transform);
 await page.keyboard.press('ArrowRight');expect(await image.evaluate(n=>n.style.transform)).not.toBe(initial);
 const beforeDrag=await image.evaluate(n=>n.style.transform);
 const box=await viewport.boundingBox();await page.mouse.move(box.x+150,box.y+150);await page.mouse.down();await page.mouse.move(box.x+220,box.y+200);await page.mouse.up();
 expect(await image.evaluate(n=>n.style.transform)).not.toBe(beforeDrag);
 await page.keyboard.press('0');expect(parseInt(await output.textContent())).toBe(fit);
 await page.keyboard.press('Escape');await expect(viewer).not.toBeVisible();await expect(inline).toBeFocused();
 expect(await page.locator('#messages').evaluate(n=>n.scrollTop)).toBeCloseTo(scroll,0);
 expect(await inline.boundingBox()).toEqual(before);
 await inline.click();await viewer.getByRole('button',{name:'Close image viewer'}).click();await expect(viewer).not.toBeVisible();
 // Native agent image content shares the accessible viewer with Markdown images.
 const native=page.getByRole('button',{name:'Enlarge image: Image from agent'});await expect(native).toBeVisible();await native.click();await expect(viewer).toBeVisible();await expect(viewer.locator('img')).toHaveAttribute('alt','Image from agent');await page.keyboard.press('Escape');await expect(native).toBeFocused();
});

test('phone: image pinch gesture zooms without changing inline layout',async({page})=>{
 await page.setViewportSize({width:390,height:844});
 const png=await page.evaluate(()=>{const c=document.createElement('canvas');c.width=1000;c.height=900;return c.toDataURL('image/png').split(',')[1];});
 await writeFile(join(fixture,'touch.png'),Buffer.from(png,'base64'));
 await send(page,'![Touch chart](touch.png)');
 const inline=page.getByRole('button',{name:'Enlarge image: Touch chart'});await inline.click();
 const viewer=page.getByRole('dialog',{name:'Image viewer'}),viewport=viewer.locator('.image-viewport');
 await expect(viewer).toBeVisible();const before=parseInt(await viewer.locator('output').textContent());
 // Browser-generated touch events exercise the same pointer capture path as a real pinch.
 const cdp=await page.context().newCDPSession(page),box=await viewport.boundingBox();
 const points=[{x:box.x+100,y:box.y+150},{x:box.x+200,y:box.y+150}];
 await cdp.send('Input.dispatchTouchEvent',{type:'touchStart',touchPoints:points});
 await cdp.send('Input.dispatchTouchEvent',{type:'touchMove',touchPoints:[points[0],{x:box.x+290,y:box.y+150}]});
 await cdp.send('Input.dispatchTouchEvent',{type:'touchEnd',touchPoints:[]});
 await expect.poll(async()=>parseInt(await viewer.locator('output').textContent())).toBeGreaterThan(before);
 await viewer.getByRole('button',{name:'Close image viewer'}).click();await expect(inline).toBeVisible();
 expect(await page.evaluate(()=>document.scrollingElement.scrollWidth<=innerWidth)).toBe(true);
});


test('session file access permits allowed sibling paths and rejects roots removed in settings',async({page})=>{
 const nested=join(fixture,'nested');await mkdir(nested,{recursive:true});
 const other=await api('/api/sessions',{provider:'demo',cwd:nested,title:'Nested workspace'});
 try{
  const path=encodeURIComponent(join(fixture,'sample.rs'));
  const response=await page.request.get(`/api/sessions/${other.id}/file?path=${path}`);expect(response.ok()).toBe(true);expect(await response.text()).toContain('pub fn sample');
  const settings=await api('/api/settings',undefined,'GET');
  try{
   await api('/api/settings',{...settings,roots:[nested]},'PUT');
   expect((await page.request.get(`/api/sessions/${other.id}/file?path=${path}`)).status()).toBe(403);
  }finally{await api('/api/settings',settings,'PUT');}
 }finally{await api(`/api/sessions/${other.id}`,undefined,'DELETE');}
});
