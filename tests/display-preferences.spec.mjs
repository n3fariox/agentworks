import {test,expect} from '@playwright/test';
import {spawn} from 'node:child_process';
import {mkdtemp,mkdir,writeFile,rm} from 'node:fs/promises';
import {resolve,join} from 'node:path';

// Own server, catalog, database and demo state on a dedicated port; never the live service.
const base='http://127.0.0.1:4411';
test.use({baseURL:base});
let fixture,server,log='',exited=false,session;

const rustRaw='fn main() {\n    let greeting = "hi   there";  \n    // keep   spacing\n\tlet tab = 1;\n}\n';
const prose='This paragraph was placed in a code block by the agent, but it is ordinary prose. It should read as text, not code.';
const longLine='LONG:'+'abcdefghij'.repeat(40);
const message=['Display check','','```rust',...rustRaw.slice(0,-1).split('\n'),'```','','```',prose,'```','','```text',longLine,'```','','[sample](sample.rs) and [notes](notes.md)'].join('\n');

async function api(path,body,method){
  const response=await fetch(base+path,{method:method||(body?'POST':'GET'),headers:{Origin:base,'Content-Type':'application/json'},body:body?JSON.stringify(body):undefined});
  expect(response.ok,await response.clone().text()).toBe(true);return response.json();
}
async function openSession(page){
  await page.addInitScript(id=>{if(!sessionStorage.getItem('display-test-init')){localStorage.setItem('agentworks-session',id);sessionStorage.setItem('display-test-init','1');}},session.id);
  await page.goto('/');
  await expect(page.locator('#session-state')).toHaveText('Ready');
}
async function sendDisplayMessage(page){
  await page.locator('#prompt').fill(message);await page.locator('#send').click();
  await expect(page.locator('#session-state')).toHaveText('Ready');
  await expect(page.locator('.message.assistant .code-block')).toHaveCount(3);
}
async function setPreference(page,key,value){
  await page.locator('#open-settings').click();
  await page.getByLabel({wrap:'Long lines in code blocks',highlight:'Syntax highlighting',prose:'Prose in unlabeled code blocks'}[key]).selectOption(value);
  await expect(page.locator('#display-preferences-note')).toContainText('saved in this browser');
  await page.locator('#dismiss-settings').click();
}
const blocks=page=>page.locator('.message.assistant .code-block');

test.beforeAll(async()=>{
  await mkdir('.cache',{recursive:true});fixture=await mkdtemp(resolve('.cache/display-prefs-'));
  await writeFile(join(fixture,'sample.rs'),'pub fn sample() -> u8 {\n    42 // answer\n}\n');
  await writeFile(join(fixture,'notes.md'),'# Notes\n\n```python\ndef f():\n    return "x"\n```\n');
  server=spawn(resolve('target/debug/agentworks'),[],{env:{...process.env,
    AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:'4411',AGENTWORKS_ROOTS:fixture,
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

test('desktop: highlighting, exact raw copy, prose toggle, persisted preferences, tool and file literals',async({page,context})=>{
  const errors=[];page.on('pageerror',e=>errors.push(e.message));
  await context.grantPermissions(['clipboard-read','clipboard-write'],{origin:base});
  await page.setViewportSize({width:1280,height:900});
  await openSession(page);await sendDisplayMessage(page);
  const rust=blocks(page).nth(0),proseBlock=blocks(page).nth(1),long=blocks(page).nth(2);

  // Highlighting uses DOM text spans; text content stays byte-identical to the source.
  await expect(rust.locator('.code-language')).toHaveText('rust');
  await expect(rust).toHaveClass(/highlighted/);
  await expect(rust.locator('.tok-keyword').first()).toHaveText('fn');
  await expect(rust.locator('.tok-string')).toHaveText('"hi   there"');
  await expect(rust.locator('.tok-comment')).toHaveText('// keep   spacing');
  expect(await rust.locator('pre code').evaluate(n=>n.textContent)).toBe(rustRaw);
  expect(await rust.locator('pre code').evaluate(n=>n.innerHTML)).not.toContain('<script');
  // Unsupported/unlabeled languages are not decorated.
  await expect(long.locator('[class^=tok-]')).toHaveCount(0);

  await rust.getByRole('button',{name:'Copy rust block'}).click();
  await expect(rust.locator('.code-copy-status')).toHaveText('Copied');
  expect(await page.evaluate(()=>navigator.clipboard.readText())).toBe(rustRaw);

  // Prose in an unlabeled block renders as plain text by default, with a per-block toggle.
  await expect(proseBlock).toHaveClass(/plain/);
  const proseFont=await proseBlock.locator('pre').evaluate(n=>getComputedStyle(n).fontFamily);
  expect(proseFont).not.toMatch(/monospace/);
  const toggle=proseBlock.getByRole('button',{name:'Show as code'});
  await expect(toggle).toHaveAttribute('aria-pressed','true');
  await toggle.click();
  await expect(proseBlock).not.toHaveClass(/plain/);
  await expect(proseBlock.getByRole('button',{name:'Show as text'})).toHaveAttribute('aria-pressed','false');
  await proseBlock.getByRole('button',{name:'Copy text block'}).click();
  expect(await page.evaluate(()=>navigator.clipboard.readText())).toBe(prose+'\n');
  await rust.getByRole('button',{name:'Show as text'}).click();
  await expect(rust).toHaveClass(/plain/);await expect(rust.locator('.tok-keyword')).toHaveCount(0);
  expect(await rust.locator('pre code').evaluate(n=>n.textContent)).toBe(rustRaw);
  await rust.getByRole('button',{name:'Show as code'}).click();

  // Desktop default: long lines scroll inside the block, page does not widen.
  const pre=long.locator('pre');
  expect(await pre.evaluate(n=>getComputedStyle(n).whiteSpace)).toBe('pre');
  expect(await pre.evaluate(n=>n.scrollWidth>n.clientWidth)).toBe(true);

  // Preferences apply immediately and survive reload.
  await setPreference(page,'highlight','off');
  await expect(rust.locator('[class^=tok-]')).toHaveCount(0);
  expect(await rust.locator('pre code').evaluate(n=>n.textContent)).toBe(rustRaw);
  await setPreference(page,'wrap','wrap');
  expect(await pre.evaluate(n=>getComputedStyle(n).whiteSpace)).toBe('pre-wrap');
  expect(await pre.evaluate(n=>n.scrollWidth<=n.clientWidth)).toBe(true);
  await setPreference(page,'prose','code');
  await expect(proseBlock).not.toHaveClass(/plain/);
  await page.reload();await expect(page.locator('#session-state')).toHaveText('Ready');
  await expect(blocks(page)).toHaveCount(3);
  expect(await page.evaluate(()=>document.body.dataset.codeWrap)).toBe('wrap');
  await expect(blocks(page).nth(0).locator('[class^=tok-]')).toHaveCount(0);
  await expect(blocks(page).nth(1)).not.toHaveClass(/plain/);
  await page.locator('#open-settings').click();
  await expect(page.getByLabel('Syntax highlighting')).toHaveValue('off');
  await expect(page.getByLabel('Long lines in code blocks')).toHaveValue('wrap');
  await expect(page.getByLabel('Prose in unlabeled code blocks')).toHaveValue('code');
  await page.getByLabel('Syntax highlighting').selectOption('on');await page.locator('#dismiss-settings').click();
  await expect(blocks(page).nth(0).locator('.tok-keyword').first()).toHaveText('fn');

  // File previews: plain source by extension and Markdown fences use the same blocks.
  await page.locator('.message.assistant').getByRole('link',{name:'sample'}).click();
  await expect(page.locator('#file-title')).toHaveText('sample.rs');
  const fileBlock=page.locator('#file-content .code-block');
  await expect(fileBlock.locator('.code-language')).toHaveText('rs');
  await expect(fileBlock.locator('.tok-keyword').first()).toHaveText('pub');
  expect(await fileBlock.locator('pre code').evaluate(n=>n.textContent)).toBe('pub fn sample() -> u8 {\n    42 // answer\n}\n');
  await page.locator('#dismiss-file').click();
  await page.locator('.message.assistant').getByRole('link',{name:'notes'}).click();
  await expect(page.locator('#file-content .code-block .code-language')).toHaveText('python');
  await expect(page.locator('#file-content .code-block .tok-keyword').first()).toHaveText('def');
  await page.locator('#dismiss-file').click();

  // Tool literals follow the wrap preference and stay unhighlighted readable JSON/text.
  await page.locator('#prompt').fill('approval');await page.locator('#send').click();
  await page.getByRole('button',{name:'Allow once',exact:true}).click();
  await expect(page.locator('#session-state')).toHaveText('Ready');
  const toolPre=page.locator('.tool-card pre').first();
  expect(await toolPre.evaluate(n=>getComputedStyle(n).whiteSpace)).toBe('pre-wrap');
  await setPreference(page,'wrap','scroll');
  expect(await toolPre.evaluate(n=>getComputedStyle(n).whiteSpace)).toBe('pre');
  await expect(page.locator('.tool-card [class^=tok-]')).toHaveCount(0);
  expect(errors).toEqual([]);
});

test('tokenizer: real categories, exact reconstruction, bounded on pathological input, prose detection',async({page})=>{
  await page.goto('/');
  const result=await page.evaluate(async()=>{
    const {tokenize,looksLikeProse,supportedLanguage}=await import('/display-preferences.js');
    const js='const count = 42; // hello\nreturn `a${b}` + "q\\"x";',py='def f(x):\n    # note\n    return "s" if x else None\n';
    const join=t=>t.map(([,text])=>text).join('');
    const nasty='/*'+'"\''.repeat(30000),started=performance.now(),big=tokenize(nasty,'js');
    return {js:tokenize(js,'js'),jsExact:join(tokenize(js,'js'))===js,py:tokenize(py,'python').filter(([k])=>k),pyExact:join(tokenize(py,'python'))===py,
      bigExact:join(big)===nasty,bigMs:performance.now()-started,
      prose:looksLikeProse('This paragraph was placed in a code block by the agent, but it is ordinary prose. It reads as text.'),
      code:looksLikeProse('fn main() {\n    let value = compute(1, 2);\n    println!("{value}");\n}'),
      langs:[supportedLanguage('RS'),supportedLanguage('tsx'),supportedLanguage('text'),supportedLanguage('')]};
  });
  expect(result.js).toEqual([['keyword','const'],['',' count = '],['number','42'],['','; '],['comment','// hello'],['','\n'],['keyword','return'],['',' '],['string','`a${b}`'],['',' + '],['string','"q\\"x"'],['',';']]);
  expect(result.py).toEqual([['keyword','def'],['comment','# note'],['keyword','return'],['string','"s"'],['keyword','if'],['keyword','else'],['keyword','None']]);
  expect([result.jsExact,result.pyExact,result.bigExact]).toEqual([true,true,true]);
  expect(result.bigMs).toBeLessThan(1000);
  expect([result.prose,result.code]).toEqual([true,false]);
  expect(result.langs).toEqual(['rust','js',null,null]);
});

test('copy fallback selects the raw block when clipboard access is unavailable',async({page})=>{
  await page.addInitScript(()=>{
    Object.defineProperty(navigator,'clipboard',{value:{writeText:()=>Promise.reject(new Error('insecure origin'))},configurable:true});
    document.execCommand=()=>false;
  });
  await openSession(page);await sendDisplayMessage(page);
  const rust=blocks(page).nth(0);
  await rust.getByRole('button',{name:'Copy rust block'}).click();
  await expect(rust.locator('.code-copy-status')).toContainText('Raw text selected');
  const field=rust.getByRole('textbox',{name:'Raw rust block for copying'});
  await expect(field).toBeVisible();await expect(field).toBeFocused();await expect(field).toHaveJSProperty('readOnly',true);
  expect(await field.evaluate(n=>[n.value,n.selectionStart,n.selectionEnd])).toEqual([rustRaw,0,rustRaw.length]);
});

test('settings toast: success tone for saved settings, error tone and inline detail for failures',async({page})=>{
  await openSession(page);
  await page.locator('#open-settings').click();
  await page.locator('#save-settings').click();
  const toast=page.locator('#toast');
  await expect(toast).toHaveText('Settings saved. They apply to new sessions.');
  await expect(toast).toHaveAttribute('data-tone','success');
  await expect(toast).toHaveAttribute('role','status');
  const background=await toast.evaluate(n=>getComputedStyle(n).backgroundColor);
  const [r,g]=background.match(/\d+/g).map(Number);expect(g).toBeGreaterThan(r);

  // Failed host save keeps the dialog open with an explicit inline error; no success toast.
  await page.evaluate(()=>{document.getElementById('toast').hidden=true;});
  await page.route('**/api/settings',route=>route.request().method()==='PUT'?route.fulfill({status:500,body:'disk full'}):route.continue());
  await page.locator('#open-settings').click();await page.locator('#save-settings').click();
  await expect(page.locator('#settings-error')).toContainText('Host settings were not saved: disk full');
  await expect(page.locator('#settings-error')).toContainText('Display preferences are stored separately');
  await expect(page.locator('#settings-dialog')).toBeVisible();
  await expect(toast).toBeHidden();
  await page.locator('#dismiss-settings').click();

  // Errors keep the error tone and assertive role.
  await page.route('**/api/settings',route=>route.request().method()==='GET'?route.fulfill({status:503,body:'settings unavailable'}):route.continue());
  await page.locator('#open-settings').click();
  await expect(toast).toHaveText('settings unavailable');
  await expect(toast).toHaveAttribute('data-tone','error');
  await expect(toast).toHaveAttribute('role','alert');
  const [er,eg]=(await toast.evaluate(n=>getComputedStyle(n).backgroundColor)).match(/\d+/g).map(Number);expect(er).toBeGreaterThan(eg);
});

test('phone: long lines wrap by default, touch targets and settings fit the viewport',async({page})=>{
  await page.setViewportSize({width:390,height:844});
  await openSession(page);await sendDisplayMessage(page);
  const long=blocks(page).nth(2),pre=long.locator('pre');
  expect(await pre.evaluate(n=>getComputedStyle(n).whiteSpace)).toBe('pre-wrap');
  expect(await pre.evaluate(n=>n.scrollWidth<=n.clientWidth)).toBe(true);
  expect(await page.evaluate(()=>document.scrollingElement.scrollWidth<=innerWidth)).toBe(true);
  for(const button of await blocks(page).nth(0).locator('.code-actions button').all()){
    const box=await button.boundingBox();expect(box.height).toBeGreaterThanOrEqual(40);expect(box.width).toBeGreaterThanOrEqual(44);
  }
  await blocks(page).nth(0).scrollIntoViewIfNeeded();await page.screenshot({path:'.cache/display-preferences-phone.png'});
  // Explicit "never wrap" still keeps horizontal scrolling inside the block, not the page.
  await page.locator('#open-settings').evaluate(n=>n.click());
  const select=page.getByLabel('Long lines in code blocks');await expect(select).toBeVisible();
  const [dialogBox,selectBox]=[await page.locator('#settings-dialog').boundingBox(),await select.boundingBox()];
  expect(selectBox.x+selectBox.width).toBeLessThanOrEqual(dialogBox.x+dialogBox.width);
  expect(selectBox.height).toBeGreaterThanOrEqual(40);
  await select.selectOption('scroll');await page.locator('#dismiss-settings').click();
  expect(await pre.evaluate(n=>getComputedStyle(n).whiteSpace)).toBe('pre');
  expect(await pre.evaluate(n=>n.scrollWidth>n.clientWidth)).toBe(true);
  expect(await page.evaluate(()=>document.scrollingElement.scrollWidth<=innerWidth)).toBe(true);
});
