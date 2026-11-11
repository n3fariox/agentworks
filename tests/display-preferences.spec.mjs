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
const starlarkRaw='def build(ctx):\n    """Build docs.\n    # still a string\n    """\n    # comment\n    return True if ctx else None\n';
const starlarkFiles=['sample.star','rules.bzl','BUILD','BUILD.bazel','WORKSPACE','WORKSPACE.bazel','MODULE.bazel'];
const message=['Display check','','```rust',...rustRaw.slice(0,-1).split('\n'),'```','','```',prose,'```','','```text',longLine,'```','','[sample](sample.rs) and [notes](notes.md)'].join('\n');

async function api(path,body,method){
  const response=await fetch(base+path,{method:method||(body?'POST':'GET'),headers:{Origin:base,'Content-Type':'application/json'},body:body?JSON.stringify(body):undefined});
  expect(response.ok,await response.clone().text()).toBe(true);return response.json();
}
async function openSession(page,{formatted=false}={}){
  await page.addInitScript(({id,formatted})=>{if(!sessionStorage.getItem('display-test-init')){localStorage.setItem('agentworks-session',id);localStorage.setItem('agentworks-composer-source',String(!formatted));sessionStorage.setItem('display-test-init','1');}},{id:session.id,formatted});
  await page.goto('/');
  await expect(page.locator('#session-state')).toHaveText('Ready');
}
async function sendDisplayMessage(page){
  await page.locator('#prompt').fill(message);await page.locator('#send').click();
  await expect(page.locator('#session-state')).toHaveText('Ready');
  await expect(page.locator('.message.assistant .code-block')).toHaveCount(3);
  await expect(page.locator('.message.assistant')).toContainText('sample and notes');
  await expect(page.locator('#session-state')).toHaveText('Ready');
}
async function setPreference(page,key,value){
  await page.locator('#nav-settings').click();await page.locator('#settings-tab-display').click();
  await page.getByLabel({theme:'Syntax color theme',wrap:'Long lines in code blocks',highlight:'Syntax highlighting',prose:'Prose in unlabeled code blocks'}[key]).selectOption(value);
  await expect(page.locator('#display-preferences-note')).toContainText('saved in this browser');
  await page.locator('#dismiss-settings').click();
}
const blocks=page=>page.locator('.message.assistant .code-block');

test.beforeAll(async()=>{
  await mkdir('.cache',{recursive:true});fixture=await mkdtemp(resolve('.cache/display-prefs-'));
  await writeFile(join(fixture,'sample.rs'),'pub fn sample() -> u8 {\n    42 // answer\n}\n');
  for(const file of starlarkFiles)await writeFile(join(fixture,file),starlarkRaw);
  await writeFile(join(fixture,'binary.rs'),Buffer.from([0,1,2]));
  await writeFile(join(fixture,'large.rs'),'// original\r\n'+'let x = 42;  \t\r\n'.repeat(7000));
  await writeFile(join(fixture,'unknown.txt'),'<img src=x onerror=alert(1)>\n  exact\ttext  \n');
  await writeFile(join(fixture,'notes.md'),'# Notes\n\n```python\ndef f():\n    return "x"\n```\n');
  server=spawn(process.env.AGENTWORKS_TEST_BINARY||resolve('target/debug/agentworks'),[],{env:{...process.env,
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
  await setPreference(page,'theme','monokai');
  await setPreference(page,'wrap','wrap');
  expect(await pre.evaluate(n=>getComputedStyle(n).whiteSpace)).toBe('pre-wrap');
  expect(await pre.evaluate(n=>n.scrollWidth<=n.clientWidth)).toBe(true);
  await setPreference(page,'prose','code');
  await expect(proseBlock).not.toHaveClass(/plain/);
  await page.reload();await expect(page.locator('#session-state')).toHaveText('Ready');
  await expect(blocks(page)).toHaveCount(3);
  expect(await page.evaluate(()=>document.body.dataset.codeWrap)).toBe('wrap');
  await expect(page.locator('body')).toHaveAttribute('data-code-theme','monokai');
  await expect(blocks(page).nth(0).locator('[class^=tok-]')).toHaveCount(0);
  await expect(blocks(page).nth(1)).not.toHaveClass(/plain/);
  await page.locator('#nav-settings').click();await page.locator('#settings-tab-display').click();
  await expect(page.getByLabel('Syntax highlighting')).toHaveValue('off');
  await expect(page.getByLabel('Long lines in code blocks')).toHaveValue('wrap');
  await expect(page.getByLabel('Prose in unlabeled code blocks')).toHaveValue('code');
  await page.getByLabel('Syntax highlighting').selectOption('on');await page.locator('#dismiss-settings').click();
  await expect(blocks(page).nth(0).locator('.tok-keyword').first()).toHaveText('fn');

  // File previews: plain source by extension and Markdown fences use the same blocks.
  await page.locator('.message.assistant').getByRole('link',{name:'sample'}).click();
  await expect(page.locator('#workspace-preview .workspace-preview-header strong')).toHaveText('sample.rs');
  const fileBlock=page.locator('#workspace-preview .code-block');
  await expect(fileBlock.locator('.code-language')).toHaveText('rs');
  await expect(fileBlock.locator('.tok-keyword').first()).toHaveText('pub');
  expect(await fileBlock.locator('pre code').evaluate(n=>n.textContent)).toBe('pub fn sample() -> u8 {\n    42 // answer\n}\n');
  await page.locator('#side-close').click();
  await page.locator('.message.assistant').getByRole('link',{name:'notes'}).click();
  await expect(page.locator('#workspace-preview .code-block .code-language')).toHaveText('python');
  await expect(page.locator('#workspace-preview .code-block .tok-keyword').first()).toHaveText('def');
  await page.locator('#side-close').click();

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

test('Starlark file previews, multiline strings, fenced blocks and highlighting preference',async({page})=>{
  await openSession(page);
  await page.locator('#prompt').fill(['```starlark',starlarkRaw,'```','',...starlarkFiles.map(file=>`[${file}](${file})`)].join('\n'));
  await page.locator('#send').click();
  await expect(page.locator('#session-state')).toHaveText('Ready');
  const fence=page.locator('.message.assistant .code-block');
  await expect(fence.locator('.tok-keyword').first()).toHaveText('def');
  for(const file of starlarkFiles){
    await page.locator('.message.assistant').getByRole('link',{name:file,exact:true}).click();
    const code=page.locator('#workspace-preview pre code');
    await expect(code).toHaveClass(/highlighted/);
    expect(await code.textContent()).toBe(starlarkRaw);
    await expect(code.locator('.tok-keyword')).toHaveText(['def','return','True','if','else','None']);
    await expect(code.locator('.tok-comment')).toHaveText(['# comment']);
    await expect(code.locator('.tok-string')).toHaveText(['"""Build docs.\n','    # still a string\n','    """']);
    await page.locator('#side-close').click();
  }
  await setPreference(page,'highlight','off');
  await expect(fence.locator('[class^=tok-]')).toHaveCount(0);
  await page.locator('.message.assistant').getByRole('link',{name:'sample.star',exact:true}).click();
  await expect(page.locator('#workspace-preview [class^=tok-]')).toHaveCount(0);
  expect(await page.locator('#workspace-preview pre code').textContent()).toBe(starlarkRaw);
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

test('workspace and beside-chat previews share display preferences',async({page})=>{
  await openSession(page);
  await page.locator('#prompt').fill('Keep this draft');
  await page.locator('#nav-files').click();
  await page.locator('.file-entry').filter({hasText:'sample.rs'}).click();
  await expect(page.locator('#workspace-file-detail .tok-keyword').first()).toHaveText('pub');
  await page.getByRole('button',{name:'Open beside chat',exact:true}).click();
  await expect(page.locator('#workspace-preview .tok-keyword').first()).toHaveText('pub');
  await setPreference(page,'highlight','off');
  await expect(page.locator('#workspace-preview [class^=tok-]')).toHaveCount(0);
  await setPreference(page,'wrap','wrap');
  expect(await page.locator('#workspace-preview pre').evaluate(n=>getComputedStyle(n).whiteSpace)).toBe('pre-wrap');
  await expect(page.locator('#prompt')).toHaveValue('Keep this draft');
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
  await page.locator('#nav-settings').click();await page.locator('#settings-tab-display').click();
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
  await page.locator('#nav-settings').click();await page.locator('#settings-tab-display').click();await page.locator('#save-settings').click();
  await expect(page.locator('#settings-error')).toContainText('Host settings were not saved: disk full');
  await expect(page.locator('#settings-error')).toContainText('Display preferences are stored separately');
  await expect(page.locator('#settings-dialog')).toBeVisible();
  await expect(toast).toBeHidden();
  await page.locator('#dismiss-settings').click();

  // Errors keep the error tone and assertive role.
  await page.route('**/api/settings',route=>route.request().method()==='GET'?route.fulfill({status:503,body:'settings unavailable'}):route.continue());
  await page.locator('#nav-settings').click();
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
  await page.locator('#nav-settings').evaluate(n=>n.click());await page.locator('#settings-tab-display').click();
  const select=page.getByLabel('Long lines in code blocks');await expect(select).toBeVisible();
  const [dialogBox,selectBox]=[await page.locator('#settings-dialog').boundingBox(),await select.boundingBox()];
  expect(selectBox.x+selectBox.width).toBeLessThanOrEqual(dialogBox.x+dialogBox.width);
  expect(selectBox.height).toBeGreaterThanOrEqual(40);
  await select.selectOption('scroll');await page.locator('#dismiss-settings').click();
  expect(await pre.evaluate(n=>getComputedStyle(n).whiteSpace)).toBe('pre');
  expect(await pre.evaluate(n=>n.scrollWidth>n.clientWidth)).toBe(true);
  expect(await page.evaluate(()=>document.scrollingElement.scrollWidth<=innerWidth)).toBe(true);
});


test('source rows, safe unknown/large fallback and exact full-file copy',async({page})=>{
  await openSession(page);
  await page.evaluate(()=>{window.copies=[];Object.defineProperty(navigator,'clipboard',{configurable:true,value:{writeText:async text=>window.copies.push(text)}});});
  await page.locator('#nav-files').click();
  await page.locator('.file-entry').filter({hasText:'sample.rs'}).click();
  const detail=page.locator('#workspace-file-detail');
  await detail.getByRole('button',{name:'Source',exact:true}).click();
  await expect(detail.locator('.source-line')).toHaveCount(3);
  await expect(detail.locator('.source-line').nth(1)).toHaveAttribute('data-line','2');
  await expect(detail.locator('.tok-number')).toHaveText('42');
  await detail.getByRole('button',{name:'Copy rs block',exact:true}).click();
  expect(await page.evaluate(()=>window.copies.at(-1))).toBe('pub fn sample() -> u8 {\n    42 // answer\n}\n');
  await page.locator('.file-entry').filter({hasText:'notes.md'}).click();
  await expect(detail.locator('.tok-keyword').first()).toHaveText('def');
  await detail.getByRole('button',{name:'Source',exact:true}).click();
  expect(await detail.locator('pre').textContent()).toBe('# Notes\n\n```python\ndef f():\n    return "x"\n```\n');
  await expect(detail.locator('.source-line').first()).toHaveAttribute('data-line','1');
  await page.locator('.file-entry').filter({hasText:'binary.rs'}).click();
  await expect(detail).toContainText('Preview is unavailable for this format');
  await expect(detail.locator('pre, [class^=tok-]')).toHaveCount(0);
  await page.locator('.file-entry').filter({hasText:'unknown.txt'}).click();
  await expect(detail.locator('[class^=tok-], img')).toHaveCount(0);
  await expect(detail.locator('pre')).toContainText('<img src=x onerror=alert(1)>');
  await page.locator('.file-entry').filter({hasText:'large.rs'}).click();
  await expect(detail.locator('pre')).toContainText('// original');
  await expect(detail.locator('[class^=tok-], .source-line')).toHaveCount(0);
  await expect(detail).toContainText('Preview shortened');
  await detail.getByRole('button',{name:'Copy rs block',exact:true}).click();
  expect(await page.evaluate(()=>window.copies.at(-1))).toBe('// original\r\n'+'let x = 42;  \t\r\n'.repeat(7000));
});

test('line renderer keeps multiline tokens, patch identity and bounded binary/large fallback',async({page})=>{
  await page.goto('/');
  const result=await page.evaluate(async()=>{
    const {renderLines}=await import('/display-preferences.js');
    const render=(raw,lang,options)=>{const code=document.createElement('code');renderLines(code,raw,lang,options);return {exact:code.textContent===raw,tokens:code.querySelectorAll('[class^=tok-]').length,rows:code.querySelectorAll('.source-line').length,comments:[...code.querySelectorAll('.tok-comment')].map(n=>n.textContent)};};
    return {
      multiline:render('/* comment\n * const hidden = 2;\n */\nconst x = 1;\n','js'),
      unknown:render('<script>alert(1)</script>\n','unknown'),
      prototype:render('constructor prototype\n','constructor'),
      binary:render('const a=1;\0','js'),large:render('const a=1;'.repeat(7000),'js'),
      many:render('x\n'.repeat(3001),'js'),
      patch:render('diff --git a/x.js b/x.js\n--- a/x.js\n+++ b/x.js\n@@ -2,2 +2,2 @@\n-const old = 1;\n+const next = 2;\n // end\n','js',{diff:true}),
      binaryPatch:render('Binary files a/x.js and b/x.js differ\n','js',{diff:true}),
    };
  });
  for(const value of Object.values(result))expect(value.exact).toBe(true);
  expect(result.multiline.comments.join('')).toBe('/* comment\n * const hidden = 2;\n */');
  expect(result.patch.tokens).toBeGreaterThan(0);
  for(const name of ['unknown','prototype','binary','large','many','binaryPatch'])expect(result[name].tokens).toBe(0);
  for(const name of ['binary','large','many','binaryPatch'])expect(result[name].rows).toBe(0);
});

for(const mobile of [false,true])test(`user Markdown edits inline, preserves source, and toggles immediately (${mobile?'phone':'desktop'})`,async({page})=>{
  const errors=[];page.on('pageerror',e=>errors.push(e.message));
  await page.setViewportSize(mobile?{width:390,height:844}:{width:1280,height:900});
  await openSession(page,{formatted:true});
  const source='# My heading\n\n**Bold** and *italic* and `literal`\n\n- First\n- Second\n\n```js\nconst x = 1;\n```\n\n<img src=x onerror="window.userMarkdownUnsafe=true">';
  const prompt=page.locator('#prompt'),editor=page.locator('#prompt-editor .cm-content');
  await expect(prompt).toBeHidden();await expect(editor).toBeVisible();
  await editor.fill(source);
  await expect(page.locator('#prompt-editor .cm-md-h1')).toHaveText('My heading');
  await expect(page.locator('#prompt-editor .cm-md-strong')).toHaveText('Bold');
  await expect(page.locator('#prompt-editor .cm-md-em')).toHaveText('italic');
  await expect(page.locator('#prompt-editor .cm-md-inline-code')).toHaveText('literal');
  await expect(page.locator('#prompt-editor .cm-md-bullet')).toHaveCount(2);
  await expect(page.locator('#prompt-preview')).toHaveCount(0);
  expect(await page.evaluate(()=>window.userMarkdownUnsafe)).toBeUndefined();
  await expect(page.locator('#prompt-editor img:not(.cm-widgetBuffer)')).toHaveCount(0);
  await expect(prompt).toHaveValue(source);
  // Source and formatted editing are the same document, with no lossy round-trip.
  await page.getByRole('button',{name:'Edit Markdown source',exact:true}).click();
  await expect(prompt).toBeVisible();await expect(prompt).toHaveValue(source);
  await page.getByRole('button',{name:'Use formatted Markdown editor',exact:true}).click();
  await expect(editor).toBeVisible();await expect(prompt).toHaveValue(source);
  const submitted=page.waitForRequest(r=>r.url().endsWith(`/api/sessions/${session.id}/prompt`)&&r.method()==='POST');
  if(mobile)await page.locator('#send').click();else await editor.press('Enter');
  expect((await submitted).postDataJSON().text).toBe(source);
  const body=page.locator('.message.user .message-body');
  await expect(body.locator('h1')).toHaveText('My heading');
  await expect(prompt).toHaveValue('');
  // Sending resets undo history so Undo cannot resurrect a previous prompt.
  await editor.press('Control+z');await expect(prompt).toHaveValue('');
  await editor.fill('**Draft**');await expect(page.locator('#prompt-editor .cm-md-strong')).toHaveText('Draft');
  const preference=async value=>{
    await page.locator('#nav-settings').evaluate(n=>n.click());await page.locator('#settings-tab-display').click();
    await page.getByLabel('Markdown in your messages').selectOption(value);
    await page.locator('#dismiss-settings').click();
  };
  await preference('off');
  await expect(body).toHaveText(source);await expect(body.locator('h1')).toHaveCount(0);
  await expect(editor).toBeHidden();await expect(prompt).toBeVisible();await expect(prompt).toHaveValue('**Draft**');
  await page.reload();await expect(page.locator('#session-state')).toHaveText('Ready');
  await expect(page.locator('.message.user .message-body')).toHaveText(source);
  await expect(prompt).toBeVisible();
  await preference('on');
  await expect(page.locator('.message.user h1')).toHaveText('My heading');
  await expect(page.locator('#prompt-editor .cm-md-strong')).toHaveText('Draft');
  // A late transcript render cannot undo the switch back to plain text.
  await preference('off');
  await page.route('**/api/markdown',async route=>{
    const response=await route.fetch();await new Promise(r=>setTimeout(r,500));await route.fulfill({response});
  });
  await preference('on');await preference('off');await page.waitForTimeout(600);
  await expect(editor).toBeHidden();
  await expect(page.locator('.message.user .message-body strong')).toHaveCount(0);
  expect(errors).toEqual([]);
});

test('inline editor handles selection, undo, multiline input, IME and draft restoration',async({page})=>{
  await openSession(page,{formatted:true});
  const editor=page.locator('#prompt-editor .cm-content'),prompt=page.locator('#prompt');
  await editor.pressSequentially('**hello**');
  await expect(page.locator('#prompt-editor .cm-md-strong')).toHaveText('hello');
  await editor.press('ArrowLeft');
  await expect(page.locator('#prompt-editor .cm-md-strong')).toContainText('**hello**');
  await editor.press('End');await editor.press('Shift+Enter');
  await editor.pressSequentially('next line');await expect(prompt).toHaveValue('**hello**\nnext line');
  await editor.press('Control+z');await expect(prompt).not.toHaveValue('**hello**\nnext line');
  await editor.press('Control+y');await expect(prompt).toHaveValue('**hello**\nnext line');
  await editor.dispatchEvent('keydown',{key:'Enter',code:'Enter',isComposing:true});
  await expect(page.locator('.message.user')).toHaveCount(0);
  const draft=await prompt.inputValue();
  await page.reload();await expect(page.locator('#session-state')).toHaveText('Ready');
  await expect(prompt).toHaveValue(draft);
  await expect(page.locator('#prompt-editor .cm-md-strong')).toHaveText('hello');
});


test('mobile keyboard keeps the inline editor, caret and send button in the visual viewport',async({page})=>{
  await page.setViewportSize({width:390,height:844});
  // Headless browsers have no OS keyboard. Model its independent viewport
  // resize and pan while leaving the layout viewport at the full screen size.
  await page.addInitScript(()=>{
    const viewport=window.visualViewport;
    let keyboard=null;
    for(const key of ['height','offsetTop','scale']){
      const native=Object.getOwnPropertyDescriptor(Object.getPrototypeOf(viewport),key).get.bind(viewport);
      Object.defineProperty(viewport,key,{get:()=>keyboard?.[key]??native()});
    }
    window.setKeyboardViewport=value=>{keyboard=value;viewport.dispatchEvent(new Event('resize'));viewport.dispatchEvent(new Event('scroll'));};
  });
  await openSession(page,{formatted:true});
  const editor=page.locator('#prompt-editor .cm-content'),source=page.locator('#prompt');
  const text=Array.from({length:35},(_,i)=>`Line ${i+1}: **keep typing** here`).join('\n');
  await editor.fill(text);
  const bounds=async()=>page.evaluate(()=>{
    const root=document.querySelector('.layout').getBoundingClientRect(),send=document.getElementById('send').getBoundingClientRect();
    const scroller=document.querySelector('#prompt-editor .cm-scroller').getBoundingClientRect();
    const selection=getSelection(),caret=selection.rangeCount?selection.getRangeAt(0).getBoundingClientRect():null;
    return {top:root.top,bottom:root.bottom,sendTop:send.top,sendBottom:send.bottom,editorTop:scroller.top,editorBottom:scroller.bottom,caretTop:caret?.top,caretBottom:caret?.bottom};
  });
  await page.evaluate(()=>window.setKeyboardViewport({height:480,offsetTop:0,scale:1}));
  await expect.poll(async()=>(await bounds()).bottom).toBe(480);
  await expect.poll(async()=>(await bounds()).sendBottom).toBeLessThanOrEqual(480);
  await expect.poll(async()=>(await bounds()).caretBottom).toBeLessThanOrEqual(480);
  await expect.poll(async()=>{const r=await bounds();return r.caretTop>=r.editorTop-1&&r.caretBottom<=r.editorBottom+1;}).toBe(true);
  // Safari may pan the visual viewport as well as shrinking it.
  await page.evaluate(()=>window.setKeyboardViewport({height:400,offsetTop:70,scale:1}));
  await expect.poll(async()=>(await bounds()).top).toBe(70);
  await expect.poll(async()=>(await bounds()).bottom).toBe(470);
  await expect.poll(async()=>(await bounds()).sendBottom).toBeLessThanOrEqual(470);
  await expect.poll(async()=>(await bounds()).caretBottom).toBeLessThanOrEqual(470);
  await expect(source).toHaveValue(text);
  // Source mode shares the same keyboard-aware layout without losing the draft.
  await page.getByRole('button',{name:'Edit Markdown source',exact:true}).click();
  await expect(source).toBeVisible();await expect(source).toHaveValue(text);
  expect((await source.boundingBox()).y+(await source.boundingBox()).height).toBeLessThanOrEqual(470);
  await page.getByRole('button',{name:'Use formatted Markdown editor',exact:true}).click();
  await page.evaluate(()=>window.setKeyboardViewport({height:320,offsetTop:0,scale:1}));
  await expect.poll(async()=>(await bounds()).sendBottom).toBeLessThanOrEqual(320);
  // Keyboard dismissal restores the available height, and pinch zoom stays native.
  await page.evaluate(()=>window.setKeyboardViewport({height:844,offsetTop:0,scale:1}));
  await expect.poll(async()=>(await bounds()).bottom).toBe(844);
  await page.evaluate(()=>window.setKeyboardViewport({height:422,offsetTop:30,scale:2}));
  await expect(page.locator('html')).not.toHaveAttribute('data-composer-viewport');
  await page.evaluate(()=>window.setKeyboardViewport({height:480,offsetTop:0,scale:1}));
  await expect(page.locator('html')).toHaveAttribute('data-composer-viewport','');
  await page.locator('#send').focus();await page.locator('#session-more').focus();
  await expect(page.locator('html')).not.toHaveAttribute('data-composer-viewport');
  await expect(source).toHaveValue(text);
});


for(const formatted of [true,false])test(`mobile Enter makes newlines and only Send submits (${formatted?'formatted':'source'})`,async({browser})=>{
  const context=await browser.newContext({viewport:{width:390,height:844},isMobile:true,hasTouch:true});
  try{
    const page=await context.newPage();await openSession(page,{formatted});
    const input=page.locator(formatted?'#prompt-editor .cm-content':'#prompt');
    const source=page.locator('#prompt');
    const submissions=[];page.on('request',request=>{if(request.method()==='POST'&&request.url().endsWith(`/api/sessions/${session.id}/prompt`))submissions.push(request.postDataJSON());});
    await expect(input).toHaveAttribute('enterkeyhint','enter');
    await expect(page.locator('#composer-hint')).toHaveText('Enter for a new line · Tap Send to send');
    await input.fill('First line');await input.press('Enter');
    await expect(source).toHaveValue('First line\n');
    await input.pressSequentially('Second line');await input.press('Shift+Enter');
    await expect(source).toHaveValue('First line\nSecond line\n');
    // Touch devices still require the button when a wider viewport is used.
    await page.setViewportSize({width:900,height:600});
    await input.pressSequentially('Third line');await input.press('Enter');
    await expect(source).toHaveValue('First line\nSecond line\nThird line\n');
    await expect(page.locator('.message.user')).toHaveCount(0);expect(submissions).toEqual([]);
    await page.locator('#send').click();
    await expect(source).toHaveValue('');
    expect(submissions.map(s=>s.text)).toEqual(['First line\nSecond line\nThird line\n']);
    await expect(page.locator('.message.user')).toHaveCount(1);
  }finally{await context.close();}
});

for(const formatted of [true,false])test(`mobile Send keeps typing active after send and queue (${formatted?'formatted':'source'})`,async({browser})=>{
  const context=await browser.newContext({viewport:{width:390,height:844},isMobile:true,hasTouch:true});
  try{
    const page=await context.newPage();await openSession(page,{formatted});
    const input=page.locator(formatted?'#prompt-editor .cm-content':'#prompt'),source=page.locator('#prompt');
    await input.fill('slow first mobile message');
    await input.evaluate(node=>{window.typingTarget=node;window.composerBlurCount=0;node.addEventListener('blur',()=>window.composerBlurCount++);});
    await page.locator('#send').tap();
    await expect(source).toHaveValue('');await expect(input).toBeFocused();
    // Use the existing keyboard target: locator.fill/press would silently focus
    // the editor again and conceal the mobile keyboard handoff regression.
    await page.keyboard.insertText('second message without reopening keyboard');
    await expect(source).toHaveValue('second message without reopening keyboard');
    await expect(page.locator('#send')).toHaveAttribute('aria-label','Queue message');
    await page.locator('#send').tap();
    await expect(source).toHaveValue('');await expect(input).toBeFocused();
    await expect(page.locator('#input-queue .queue-text')).toHaveText('second message without reopening keyboard');
    await page.keyboard.insertText('third draft with the same keyboard');
    await expect(source).toHaveValue('third draft with the same keyboard');
    expect(await input.evaluate(node=>node===window.typingTarget)).toBe(true);
    expect(await page.evaluate(()=>window.composerBlurCount)).toBe(0);
  }finally{await context.close();}
});
