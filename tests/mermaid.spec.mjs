import {test,expect} from '@playwright/test';
import {spawn} from 'node:child_process';
import {mkdtemp,mkdir,writeFile,rm} from 'node:fs/promises';
import {resolve,join} from 'node:path';
const base='http://127.0.0.1:4416';
test.use({baseURL:base});
let fixture,server,log='',exited=false,session;
const raw='flowchart LR\n  A[Start] --> B[Finish]\n';
const message=['# Diagrams','','```mermaid',raw.trimEnd(),'```','','```mermaid','flowchart LR\n A[broken','```','','```mermaid','not-a-diagram','```','','```js','const x = "<script>safe</script>";','```','','<pre><code class="language-mermaid">flowchart LR; X --> Y</code></pre>','','[notes](notes.md)'].join('\n');
async function api(path,body,method){
 const response=await fetch(base+path,{method:method||(body?'POST':'GET'),headers:{Origin:base,'Content-Type':'application/json'},body:body?JSON.stringify(body):undefined});
 expect(response.ok,await response.clone().text()).toBe(true);return response.json();
}
async function openChat(page){
 await page.addInitScript(id=>{if(window===window.top)localStorage.setItem('agentworks-session',id);},session.id);
 await page.goto('/');await expect(page.locator('#session-state')).toHaveText('Ready');
 await page.locator('#prompt').fill(message);await page.locator('#send').click();
 await expect(page.locator('.message.assistant .mermaid-block')).toHaveCount(3);
}
async function checkPreview(container){
 const blocks=container.locator('.mermaid-block'),first=blocks.first();
 await expect(first.locator('.mermaid-status')).toBeHidden();
 await expect(first.locator('pre')).toBeHidden();
 await expect(first.locator('iframe')).toBeVisible();
 await expect(first.locator('iframe')).toHaveAttribute('sandbox','allow-scripts');
 await expect(first.frameLocator('iframe').locator('svg')).toBeVisible();
 await expect(first.frameLocator('iframe').locator('svg')).toContainText('Start');
 for(const i of [1,2]){
  await expect(blocks.nth(i).locator('.mermaid-status')).toContainText('Unable to render Mermaid diagram:');
  await expect(blocks.nth(i).locator('pre')).toBeVisible();
  await expect(blocks.nth(i).locator('iframe')).toHaveCount(0);
 }
 await expect(container.locator('.code-block')).toHaveCount(2);
 await expect(container.locator('.code-block').first().locator('pre code')).toHaveText('const x = "<script>safe</script>";\n');
 await expect(container.locator('.code-block').last().locator('pre code')).toContainText('X --> Y');
 return first;
}
test.beforeAll(async()=>{
  await mkdir('.cache',{recursive:true});fixture=await mkdtemp(resolve('.cache/mermaid-'));
  await writeFile(join(fixture,'notes.md'),message);
  server=spawn(resolve('target/debug/agentworks'),[],{env:{...process.env,
    AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:'4416',AGENTWORKS_ROOTS:fixture,
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
test.beforeEach(async()=>{session=await api('/api/sessions',{provider:'demo',cwd:fixture,title:'Mermaid test'});});
test.afterEach(async()=>{if(session)await api(`/api/sessions/${session.id}`,undefined,'DELETE');});
test.afterAll(async()=>{
  if(server&&!exited){const stopped=new Promise(r=>server.once('exit',r));server.kill('SIGTERM');await stopped;}
  if(fixture)await rm(fixture,{recursive:true,force:true});
});


test('chat and disk use rendered diagrams, accessible source controls and readable failures',async({page})=>{
 const errors=[];page.on('pageerror',e=>errors.push(e.message));
 await openChat(page);
 const chat=page.locator('.message.assistant');
 const first=await checkPreview(chat),toggle=first.locator('.mermaid-toggle');
 await page.mouse.move(0,0);await expect(toggle).toHaveCSS('opacity','0');
 await first.hover();await expect(toggle).toHaveCSS('opacity','1');
 await page.mouse.move(0,0);await toggle.focus();await expect(toggle).toHaveCSS('opacity','1');
 await page.keyboard.press('Enter');await expect(first.locator('pre')).toBeVisible();
 expect(await first.locator('pre code').textContent()).toBe(raw);
 await expect(toggle).toHaveAttribute('aria-expanded','true');
 await expect(first.locator('iframe')).toBeHidden();
 await page.keyboard.press('Space');await expect(first.locator('iframe')).toBeVisible();
 await expect(toggle).toHaveAttribute('aria-expanded','false');
 await chat.getByRole('link',{name:'notes',exact:true}).click();
 const disk=page.locator('#workspace-preview');const diskFirst=await checkPreview(disk);
 await diskFirst.getByRole('button',{name:'Show source',exact:true}).click();
 expect(await diskFirst.locator('pre code').textContent()).toBe(raw);
 // Full-file Source stays literal and never renders fences.
 await disk.getByRole('button',{name:'Source',exact:true}).click();
 await expect(disk.locator('.mermaid-block')).toHaveCount(0);
 await expect(disk.locator('pre code')).toContainText('```mermaid');
 await page.locator('#side-close').click();
 await page.locator('#nav-files').click();
 await page.locator('.file-entry').filter({hasText:'notes.md'}).click();
 await checkPreview(page.locator('#workspace-file-detail'));
 expect(errors).toEqual([]);
});

test('touch users can toggle source and return to the diagram',async({browser})=>{
 const context=await browser.newContext({baseURL:base,hasTouch:true,isMobile:true,viewport:{width:390,height:844}});
 const page=await context.newPage();
 try{
  await openChat(page);const first=await checkPreview(page.locator('.message.assistant'));
  const toggle=first.locator('button');await expect(toggle).toHaveCSS('opacity','1');
  await toggle.tap();await expect(first.locator('pre')).toBeVisible();
  await toggle.tap();await expect(first.locator('iframe')).toBeVisible();
 }finally{await context.close();}
});

test('sandboxed SVG cannot execute HTML, navigate links or load external resources',async({page})=>{
 await page.goto('/');const requests=[];
 page.on('request',r=>{if(!r.url().startsWith(base)&&!r.url().startsWith('data:'))requests.push(r.url());});
 const source='flowchart LR\n A["&lt;img src=https://evil.invalid/pixel onerror=alert(1)&gt;"] --> B[Safe]\n click B "javascript:alert(1)"';
 await page.evaluate(async source=>{
  const {enhanceMermaid}=await import('/mermaid.js');
  const result=await (await fetch('/api/markdown',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({text:'```mermaid\n'+source+'\n```'})})).json();
  const container=document.createElement('div');container.id='safety';container.className='markdown';container.innerHTML=result.html;document.body.append(container);enhanceMermaid(container,result.mermaidClass);
 },source);
 const block=page.locator('#safety .mermaid-block');await expect(block.locator('.mermaid-status')).toBeHidden();
 const frame=block.frameLocator('iframe');await expect(frame.locator('svg')).toContainText('Safe');
 await expect(frame.locator('a,script:not([src]),foreignObject,image,svg [onload],svg [onclick]')).toHaveCount(0);
 expect(await frame.locator('svg').evaluate(n=>[...n.querySelectorAll('*')].flatMap(e=>[...e.attributes].filter(a=>/href$/i.test(a.name)&&!a.value.startsWith('#')).map(a=>a.value)))).toEqual([]);
 expect(requests).toEqual([]);
 // Application-owned configuration cannot be overridden by input.
 await page.evaluate(async()=>{
  const {enhanceMermaid}=await import('/mermaid.js');const marker='language-awmermaid-0123456789abcdef';
  const div=document.createElement('div'),pre=document.createElement('pre'),code=document.createElement('code');
  div.id='directive';code.className=marker;code.textContent='%%{init: {"securityLevel":"loose"}}%%\nflowchart LR; A-->B';pre.append(code);div.append(pre);document.body.append(div);enhanceMermaid(div,marker);
 });
 await expect(page.locator('#directive pre')).toBeVisible();
 await expect(page.locator('#directive .mermaid-status')).toContainText('configuration directives');
});


test('renderer load failure keeps source and a useful error',async({page})=>{
 await page.route('**/mermaid-frame.bundle.js',route=>route.abort());
 await openChat(page);
 const first=page.locator('.message.assistant .mermaid-block').first();
 await expect(first.locator('.mermaid-status')).toContainText('timed out or could not load',{timeout:18000});
 await expect(first.locator('pre')).toBeVisible();
 expect(await first.locator('pre code').textContent()).toBe(raw);
});
