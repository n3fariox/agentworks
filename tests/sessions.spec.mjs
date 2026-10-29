import {test,expect} from '@playwright/test';

async function create(page,name,directory){
  await page.locator('#new-session').click();
  await page.locator('#session-title').fill(name);
  if(directory)await page.locator('#directory').fill(directory);
  await page.locator('#create-submit').click();
  await expect(page.locator('#create-dialog')).not.toBeVisible();
  await expect(page.locator('#session-heading-title')).toHaveText(name);
  await expect(page.locator('#session-state')).toHaveText('Ready');
}
async function send(page,text){
  await page.locator('#prompt').fill(text);await page.locator('#send').click();
}
// Track this test's creations, including automation runs whose title differs
// from the launch title. Never sweep another run's "Browser test" resources.
const resources=new WeakMap();
test.beforeEach(async({page})=>{
  const owned={sessions:new Set(),projects:new Set(),automations:new Set(),pending:[]};
  resources.set(page,owned);
  page.on('response',response=>{
    if(response.request().method()!=='POST'||!response.ok())return;
    const path=new URL(response.url()).pathname;
    const kind=path==='/api/projects'?'projects':path==='/api/automations'?'automations':
      /^\/api\/sessions(?:\/import)?$/.test(path)||/^\/api\/automations\/[^/]+\/run$/.test(path)?'sessions':null;
    if(kind)owned.pending.push(response.json().then(value=>{const id=value.id||value.session?.id;if(id)owned[kind].add(id);}));
  });
});
test.afterEach(async({page,request})=>{
  const owned=resources.get(page);await Promise.all(owned.pending);
  for(const kind of ['sessions','automations','projects'])for(const id of owned[kind]){
    const response=await request.delete(`/api/${kind}/${id}`,{headers:{Origin:new URL(page.url()).origin}});
    expect([200,204,404]).toContain(response.status());
  }
});

test('desktop: sessions, Markdown, approvals across reload, cancellation, project files',async({page})=>{
  const errors=[];page.on('pageerror',e=>errors.push(e.message));
  await page.goto('/');
  await expect(page.locator('h1')).toContainText('big ideas');
  await page.screenshot({path:'.cache/prototype-desktop.png'});
  await create(page,'Browser test desktop');
  await send(page,'hello');
  await expect(page.locator('#session-state')).toHaveText('Ready');
  await expect(page.locator('.markdown h3')).toHaveText('Local session, turn 1');
  await expect(page.locator('#messages')).toContainText('Rust ACP client');
  // Local file links in the chat open in the side panel's Files tab.
  await page.getByRole('link',{name:'README.md'}).click();
  await expect(page.locator('#side-tab-files')).toHaveAttribute('aria-selected','true');
  await expect(page.locator('#workspace-preview .workspace-preview-header strong')).toHaveText('README.md');
  await expect(page.locator('#workspace-preview')).toContainText('Agentworks');
  await page.locator('#side-close').click();
  await send(page,'approval');
  await expect(page.locator('.approval')).toBeVisible();
  await page.reload();
  await expect(page.locator('.approval')).toBeVisible();
  await page.getByRole('button',{name:'Allow once',exact:true}).click();
  await expect(page.locator('.approval')).toHaveCount(0);
  await expect(page.locator('#session-state')).toHaveText('Ready');
  await expect(page.locator('#messages')).toContainText('approved');
  await send(page,'slow');
  await expect(page.locator('#stop')).toBeVisible();
  await expect(page.locator('#messages')).toContainText('turn 3');
  await page.reload();
  await expect(page.locator('#stop')).toBeVisible();
  await page.locator('#stop').click();
  await expect(page.locator('#session-state')).toHaveText('Ready');
  const {roots}=await page.evaluate(()=>fetch('/api/status').then(r=>r.json()));
  await create(page,'Browser test second project',roots[0]+'/web');
  await expect(page.locator('#project-path')).toHaveText(roots[0]+'/web');
  await page.getByRole('button',{name:/Browser test desktop/}).click();
  await expect(page.locator('#messages')).toContainText('turn 3');
  await page.screenshot({path:'.cache/prototype-session.png'});
  expect(errors).toEqual([]);
});

test('mobile: drawer, composer, image output, approval denial',async({page})=>{
  await page.addInitScript(() => Object.defineProperty(crypto, 'randomUUID', {value:undefined, configurable:true}));
  await page.setViewportSize({width:390,height:844});
  await page.goto('/');
  await expect(page.locator('#welcome-new')).toBeVisible();
  await page.locator('#welcome-new').click();
  await page.locator('#session-title').fill('Browser test mobile');
  await page.locator('#create-submit').click();
  await expect(page.locator('#session-state')).toHaveText('Ready');
  await send(page,'image');
  await expect(page.locator('#session-state')).toHaveText('Ready');
  await expect(page.locator('.agent-image')).toBeVisible();
  await send(page,'approval');
  await expect(page.locator('.approval')).toBeVisible();
  await page.getByRole('button',{name:'Deny',exact:true}).click();
  await expect(page.locator('#session-state')).toHaveText('Ready');
  await expect(page.locator('#messages')).toContainText('denied');
  // On phones the chat bar's ☰ opens the full-screen session browser.
  await page.locator('#session-home').click();
  await expect(page.locator('#new-session')).toBeInViewport();
  await page.getByRole('button',{name:/Browser test mobile/}).click();
  await expect(page.locator('#prompt')).toBeInViewport();
  // The system Back button returns from a chat to the session browser, and
  // reopening a session makes Back work again.
  await page.goBack();
  await expect(page.locator('#new-session')).toBeInViewport();
  await page.getByRole('button',{name:/Browser test mobile/}).click();
  await expect(page.locator('#prompt')).toBeInViewport();
  await page.goBack();
  await expect(page.locator('#new-session')).toBeInViewport();
  await page.goForward();
  await expect(page.locator('#prompt')).toBeInViewport();
  // A quota chip and the stop control can coexist with both composer buttons.
  // Keep the essential stop action reachable even when its label is long.
  await page.evaluate(()=>{
    const info=document.querySelector('#composer-info');info.hidden=false;
    info.textContent='Context 82% · 5h quota 74% · weekly quota 61%';
    document.querySelector('#stop').hidden=false;
  });
  await expect(page.locator('#stop')).toBeInViewport();
  expect(await page.locator('#stop').evaluate(el=>el.getBoundingClientRect().right<=innerWidth)).toBe(true);
  await page.setViewportSize({width:320,height:844});
  expect(await page.locator('#stop').evaluate(el=>el.getBoundingClientRect().right<=innerWidth)).toBe(true);
  expect(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBe(true);
  await expect(page.locator('body')).not.toHaveClass(/menu-open/);
  await expect(page.locator('#messages')).toContainText('denied');
  await page.waitForTimeout(250);
  await page.screenshot({path:'.cache/prototype-mobile.png'});
});


test('OpenCode: native session and advertised free model picker', async ({page}) => {
  test.skip(!process.env.AGENTWORKS_TEST_OPENCODE, 'Opt-in check requiring the host OpenCode installation');
  await page.goto('/');
  await page.locator('#new-session').click();
  await page.locator('#provider').selectOption('opencode');
  await expect(page.locator('#provider-note')).toContainText('Native ACP');
  await page.locator('#session-title').fill('Browser test OpenCode');
  await page.locator('#create-submit').click();
  await expect(page.locator('#session-state')).toHaveText('Ready', {timeout:60000});
  await expect(page.locator('#model-picker')).toBeVisible();
  const values=await page.locator('#model-picker option').evaluateAll(nodes=>nodes.map(n=>n.value));
  expect(values.length).toBeGreaterThan(0);
  expect(values.every(v=>v.startsWith('opencode/')&&(v.endsWith('-free')||v==='opencode/big-pickle'))).toBe(true);
  await page.locator('#model-picker').selectOption(values[0]);
  await expect(page.locator('#session-state')).toHaveText('Ready');
  await page.reload();
  await expect(page.locator('#model-picker')).toHaveValue(values[0]);
});


test('Claude and Codex use existing sign-in without an API-key gate', async ({page}) => {
  await page.goto('/');
  await page.locator('#new-session').click();
  for (const provider of ['claude-rust','codex-rust']) {
    await page.locator('#provider').selectOption(provider);
    await expect(page.locator('#provider-note')).toContainText('existing agent sign-in');
    await expect(page.locator('#create-submit')).toBeEnabled();
    await expect(page.locator(`#provider option[value="${provider}"]`)).not.toContainText('API key needed');
  }
});


test('project hierarchies preserve draft, stream, selection and independent expansion',async({page})=>{
  let sockets=0;page.on('websocket',()=>sockets++);
  await page.goto('/');
  await page.locator('#new-project').click();
  await page.locator('#project-name').fill('Browser test Alpha');
  await page.locator('#save-project').click();
  await expect(page.locator('#project-dialog')).not.toBeVisible();
  await page.locator('#new-session').click();
  await page.locator('#create-project').selectOption({label:'Browser test Alpha'});
  await page.locator('#session-title').fill('Browser test grouped');
  await page.locator('#create-submit').click();
  await expect(page.locator('#session-state')).toHaveText('Ready');
  await send(page,'hello');
  await expect(page.locator('#session-state')).toHaveText('Ready');
  await page.locator('#prompt').fill('A draft stays here');
  const currentSockets=sockets;
  await page.locator('#view-hosts').click();
  await expect(page.locator('#view-hosts')).toHaveAttribute('aria-selected','true');
  await expect(page.locator('#prompt')).toHaveValue('A draft stays here');
  await expect(page.locator('#messages')).toContainText('turn 1');
  await page.locator('#view-projects').click();
  expect(sockets).toBe(currentSockets);
  const project=page.locator('#sessions > details').filter({has:page.locator(':scope > summary .tree-label',{hasText:'Browser test Alpha'})});
  await project.locator(':scope > summary .tree-label').click();
  await expect(project).not.toHaveAttribute('open');
  await page.locator('#view-hosts').click();
  await expect(page.getByRole('button',{name:/Browser test grouped/})).toBeVisible();
  await page.locator('#view-projects').click();
  await expect(project).not.toHaveAttribute('open');
  await project.locator(':scope > summary .tree-label').click();
  await page.locator('#new-project').click();await page.locator('#project-name').fill('Browser test Beta');await page.locator('#save-project').click();
  await expect(page.locator('#project-dialog')).not.toBeVisible();
  await page.locator('#lifecycle-menu > summary').click();await page.locator('#session-project').selectOption({label:'Browser test Beta'});
  await expect(page.locator('#session-project')).toHaveValue(await page.locator('#create-project option',{hasText:'Browser test Beta'}).getAttribute('value'));
  await expect(page.locator('#prompt')).toHaveValue('A draft stays here');
  expect(sockets).toBe(currentSockets);
  await page.getByRole('button',{name:'Edit project Browser test Beta',exact:true}).click();
  await page.locator('#project-name').fill('Browser test Renamed');await page.locator('#save-project').click();
  await expect(page.locator('#session-project option:checked')).toHaveText('Browser test Renamed');
  await page.reload();
  await expect(page.locator('#prompt')).toHaveValue('A draft stays here');
  await expect(page.locator('#session-project option:checked')).toHaveText('Browser test Renamed');
  await page.locator('#view-hosts').click();
  const {roots}=await page.evaluate(()=>fetch('/api/status').then(r=>r.json()));
  await create(page,'Browser test other checkout',roots[0]+'/web');
  await expect(page.locator('#session-project option:checked')).toHaveText('Browser test Renamed');
  await page.getByRole('button',{name:/Browser test grouped/}).click();
  await expect(page.locator('#prompt')).toHaveValue('A draft stays here');
  await expect(page.locator('#messages')).toContainText('turn 1');
  await page.screenshot({path:'.cache/prototype-projects.png'});
});


test('desktop session browser and composer: model label, resizable sidebar, usage popover',async({page})=>{
  await page.setViewportSize({width:1280,height:900});
  await page.goto('/');await page.evaluate(()=>localStorage.removeItem('agentworks-sidebar-width'));await page.reload();
  await page.locator('#new-session').click();await page.locator('#provider').selectOption('demo');
  await page.locator('#load-models').click();await page.locator('#create-model').selectOption('demo/quick');
  await page.locator('#session-title').fill('Browser test desktop chrome');await page.locator('#create-submit').click();
  await expect(page.locator('#session-state')).toHaveText('Ready');
  // The session browser shows the model, not the working directory name.
  const item=page.locator('.session-item[aria-current=true] small');
  await expect(item).toHaveText('Demo agent · Quick (local demo)');
  // Drag the sidebar edge wider; the width persists across reloads and resets on double-click.
  const sidebar=page.locator('#sidebar'),handle=page.locator('#sidebar-resize');
  const before=(await sidebar.boundingBox()).width,box=await handle.boundingBox();
  await page.mouse.move(box.x+box.width/2,box.y+200);await page.mouse.down();await page.mouse.move(box.x+box.width/2+150,box.y+200,{steps:5});await page.mouse.up();
  const wider=(await sidebar.boundingBox()).width;expect(wider).toBeGreaterThan(before+120);
  await page.reload();await expect(page.locator('#session-state')).toHaveText('Ready');
  expect(Math.abs((await sidebar.boundingBox()).width-wider)).toBeLessThan(2);
  await handle.focus();await page.keyboard.press('ArrowLeft');expect((await sidebar.boundingBox()).width).toBeLessThan(wider);
  await handle.dblclick();expect(Math.abs((await sidebar.boundingBox()).width-before)).toBeLessThan(2);
  // Desktop composer: no inline statusline or footer; the ⋯ popover holds usage and actions.
  await expect(page.locator('.usage-status-toggle')).toBeHidden();
  await expect(page.locator('.composer-foot')).toHaveCount(0);
  await expect(page.locator('#composer-hint')).toBeVisible();
  await page.locator('#composer-more').click();
  await expect(page.locator('.usage-status-toggle')).toBeVisible();await expect(page.locator('#show-commands')).toBeVisible();
  await page.keyboard.press('Escape');await expect(page.locator('#composer-menu')).toBeHidden();
});
test('models at creation, command and skill discovery, host settings',async({page})=>{
  await page.goto('/');
  const original=await page.evaluate(()=>fetch('/api/settings').then(r=>r.json()));
  try{
    await page.locator('#nav-settings').click();
    await expect(page.locator('#settings-roots input')).toHaveCount(original.roots.length);
    await expect(page.locator('#settings-roots input').first()).toHaveValue(original.roots[0]);
    await page.locator('#add-root').click();
    await expect(page.locator('#settings-roots input')).toHaveCount(original.roots.length+1);
    await page.locator('#settings-roots input').last().fill(original.roots[0]);
    await page.getByRole('button',{name:'Remove root directory'}).last().click();
    await expect(page.locator('#settings-roots input')).toHaveCount(original.roots.length);
    await page.locator('#settings-tab-models').click();
    await page.locator('#default-model-codex-rust').fill('test-default-model');
    await page.locator('#save-settings').click();
    await expect(page.locator('#settings-dialog')).not.toBeVisible();
    await page.locator('#new-session').click();
    await page.locator('#provider').selectOption('codex-rust');
    await expect(page.locator('#create-model')).toHaveValue('test-default-model');
    await page.locator('#dismiss-create').click();
    await page.evaluate(settings=>fetch('/api/settings',{method:'PUT',headers:{'Content-Type':'application/json'},body:JSON.stringify(settings)}),original);
    await page.reload();
    await page.locator('#new-session').click();
    await page.locator('#provider').selectOption('demo');
    await page.locator('#load-models').click();
    await expect(page.locator('#create-model option')).toHaveCount(3);
    await page.locator('#create-model').selectOption('demo/quick');
    await page.locator('#session-title').fill('Browser test model and commands');
    await page.locator('#create-submit').click();
    await expect(page.locator('#session-state')).toHaveText('Ready');
    await expect(page.locator('#model-picker')).toHaveValue('demo/quick');
    // Commands & skills live in the composer ⋯ popover at every width.
    await page.locator('#composer-more').click();await page.locator('#show-commands').click();
    await expect(page.locator('#command-list')).toContainText('$demo-skill');
    await page.locator('#command-list button').filter({hasText:'/help'}).click();
    await expect(page.locator('#prompt')).toHaveValue('/help ');
    await page.locator('#send').click();
    await expect(page.locator('#session-state')).toHaveText('Ready');
    await page.reload();
    await page.locator('#prompt').fill('$demo');
    await expect(page.locator('#command-list')).toContainText('$demo-skill');
    await page.locator('#command-list button').click();
    await expect(page.locator('#prompt')).toHaveValue('$demo-skill ');
    await page.setViewportSize({width:390,height:844});
    await page.locator('#menu:visible,#session-home:visible').click();await page.locator('#nav-settings').click();
    await expect(page.locator('#settings-dialog')).toBeVisible();
    await page.screenshot({path:'.cache/prototype-settings.png'});
    expect(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBe(true);
  }finally{
    await page.evaluate(settings=>fetch('/api/settings',{method:'PUT',headers:{'Content-Type':'application/json'},body:JSON.stringify(settings)}),original);
  }
});

test('resume picker loads on open and agent changes, clears old filters, and ignores stale responses',async({page})=>{
  await page.route('**/api/status',async route=>{
    const response=await route.fetch(),config=await response.json();
    for(const provider of config.providers)provider.installed=true;
    await route.fulfill({response,json:config});
  });
  let releaseSlow,startedSlow;
  const slowStarted=new Promise(resolve=>{startedSlow=resolve;});
  const slowRelease=new Promise(resolve=>{releaseSlow=resolve;});
  let delayCodex=false;
  await page.route('**/api/external-sessions',async route=>{
    const {provider,hostProfile}=route.request().postDataJSON();
    if(provider==='codex-rust'&&delayCodex){startedSlow();await slowRelease;}
    const title=provider==='opencode'?`opencode ${hostProfile?'host':'agentworks'}`:provider;
    await route.fulfill({json:{sessions:[{sessionId:title,cwd:'/project',title}],nextCursor:null}});
  });
  try{
    await page.goto('/');await page.locator('#new-session').click();await page.locator('#create-import').click();
    await expect(page.locator('.external-session')).toContainText('claude');
    await page.locator('#import-search').fill('claude');
    await page.locator('#import-provider').selectOption('codex-rust');
    await expect(page.locator('#import-search')).toHaveValue('');
    await expect(page.locator('.external-session')).toContainText('codex');
    await page.locator('#import-provider').selectOption('claude-rust');
    await expect(page.locator('.external-session')).toContainText('claude');
    delayCodex=true;await page.locator('#import-provider').selectOption('codex-rust');await slowStarted;
    await page.locator('#import-provider').selectOption('claude-rust');
    await expect(page.locator('#external-sessions')).toContainText('Loading sessions');
    const stale=page.waitForResponse(response=>response.url().endsWith('/api/external-sessions')&&response.request().postDataJSON().provider==='codex-rust');
    releaseSlow();await stale;
    await expect(page.locator('.external-session')).toContainText('claude');
    await page.locator('#import-provider').selectOption('opencode');
    await page.locator('#import-source').selectOption('host');
    await expect(page.locator('.external-session')).toContainText('opencode host');
    await page.locator('#import-source').selectOption('agentworks');
    await expect(page.locator('.external-session')).toContainText('opencode agentworks');
  }finally{releaseSlow();}
});

test('resume picker imports external history, paginates, and opens existing entries',async({page})=>{
  const {mkdir,writeFile,unlink,rm}=await import('node:fs/promises');
  const {resolve,join}=await import('node:path');
  const {randomUUID}=await import('node:crypto');
  const cwd=resolve('.cache/browser-import-'+randomUUID()),stateDir=process.env.AGENTWORKS_DEMO_STATE_DIR||resolve('.agentworks/demo');
  await mkdir(cwd,{recursive:true});await mkdir(stateDir,{recursive:true});
  const ids=Array.from({length:12},()=>randomUUID()),outside=randomUUID();
  try{
    for(const [index,id] of ids.entries())await writeFile(join(stateDir,id+'.json'),JSON.stringify({cwd,turn:1,cancelled:false,model:'demo/quick',title:'Browser test external '+index,updatedAt:new Date(Date.UTC(2026,0,1,0,index)).toISOString(),history:[{sessionUpdate:'user_message_chunk',content:{type:'text',text:'A question from the CLI'}},{sessionUpdate:'agent_message_chunk',content:{type:'text',text:'An answer from before Agentworks'}}]}));
    await writeFile(join(stateDir,outside+'.json'),JSON.stringify({cwd:'/tmp',turn:0,model:'demo/steady',title:'Outside allowed roots',updatedAt:'2099-01-01T00:00:00Z'}));
    await page.goto('/');await page.locator('#new-session').click();await page.locator('#create-import').click();
    await page.locator('#import-provider').selectOption('demo');await page.locator('#import-directory').fill(cwd);
    await page.locator('#browse-sessions').click();
    await expect(page.locator('.external-session')).toHaveCount(10);
    await page.locator('#more-external').click();await expect(page.locator('.external-session')).toHaveCount(12);
    await page.locator('#import-search').fill(ids[0]);await expect(page.locator('.external-session')).toHaveCount(1);
    await page.locator('.external-session input').check();await page.locator('#import-submit').click();
    await expect(page.locator('#session-state')).toHaveText('Ready');
    await expect(page.locator('#messages')).toContainText('A question from the CLI');
    await expect(page.locator('#messages')).toContainText('An answer from before Agentworks');
    await expect(page.locator('#model-picker')).toHaveValue('demo/quick');
    const selected=await page.evaluate(()=>localStorage.getItem('agentworks-session'));
    await page.reload();await expect(page.locator('#messages')).toContainText('An answer from before Agentworks');
    await page.locator('#new-session').click();await page.locator('#create-import').click();await page.locator('#import-provider').selectOption('demo');
    await page.locator('#import-directory').fill(cwd);await page.locator('#browse-sessions').click();await page.locator('#more-external').click();
    await page.locator('#import-search').fill(ids[0]);await expect(page.locator('.external-session')).toContainText('Already in Agentworks');
    await page.locator('.external-session input').check();await page.locator('#import-submit').click();
    expect(await page.evaluate(()=>localStorage.getItem('agentworks-session'))).toBe(selected);
    const result=await page.evaluate(async()=>fetch('/api/external-sessions',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({provider:'demo'})}).then(r=>r.json()));
    expect(result.sessions.some(s=>s.sessionId===outside)).toBe(false);
    await page.setViewportSize({width:390,height:844});await page.locator('#menu:visible,#session-home:visible').click();await page.locator('#new-session').click();await page.locator('#create-import').click();
    await page.locator('#import-provider').selectOption('demo');await page.locator('#import-directory').fill(cwd);await page.locator('#browse-sessions').click();
    await expect(page.locator('.external-session')).toHaveCount(10);
    await page.screenshot({path:'.cache/prototype-import.png'});
    expect(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBe(true);
  }finally{
    for(const id of [...ids,outside])await unlink(join(stateDir,id+'.json')).catch(()=>{});
    await rm(cwd,{recursive:true,force:true});
  }
});

test('side panel: files and diff fill the conversation area, with Back on phones',async({page})=>{
  const {mkdir,writeFile,rm}=await import('node:fs/promises'),{resolve,join}=await import('node:path'),{randomUUID}=await import('node:crypto');
  const cwd=resolve('.cache/browser-side-panel-'+randomUUID());await mkdir(join(cwd,'src'),{recursive:true});
  try{
  await writeFile(join(cwd,'notes.md'),'# Side notes');await writeFile(join(cwd,'src/main.js'),'export const answer=42;\n');await writeFile(join(cwd,'.hidden-note.md'),'# Hidden note');
  await writeFile(join(cwd,'pic.png'),Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII=','base64'));
  const errors=[];page.on('pageerror',e=>errors.push(e.message));
  await page.setViewportSize({width:1400,height:900});
  await page.goto('/');await page.evaluate(()=>{localStorage.removeItem('agentworks-side-panel');localStorage.removeItem('agentworks-side-panel-width');});
  await create(page,'Browser test side panel',cwd);await page.locator('#prompt').fill('Keep the side panel draft');
  const panel=page.locator('#side-panel');await expect(panel).toBeHidden();
  // Files fills the conversation area and keeps the chat mounted.
  await page.locator('#session-files').click();await expect(panel).toBeVisible();await expect(page.locator('#session-files')).toHaveAttribute('aria-expanded','true');
  await expect(page.locator('#workspace-chat-column')).toBeHidden();
  expect(await panel.evaluate(n=>n.clientWidth/n.parentElement.clientWidth)).toBeGreaterThan(.95);
  await expect(page.locator('#side-panel-resize')).toBeHidden();
  // Hidden files stay invisible until the side panel's own toggle reveals them; both toggles share the flag.
  await expect(page.locator('#side-file-entries')).not.toContainText('.hidden-note.md');
  await page.locator('#side-files-hidden').check();await expect(page.locator('#side-file-entries')).toContainText('.hidden-note.md');await expect(page.locator('#files-hidden')).toBeChecked();
  await page.locator('#side-files-hidden').uncheck();await expect(page.locator('#side-file-entries')).not.toContainText('.hidden-note.md');
  await page.locator('#side-file-entries .file-entry').filter({hasText:'src'}).click();await page.locator('#side-file-entries .file-entry').filter({hasText:'main.js'}).click();
  await expect(page.locator('#workspace-preview')).toContainText('answer');await expect(page.locator('#side-file-browser')).toBeHidden();
  // The toggle switches to Diff and back without losing the open file.
  await page.locator('#side-tab-diff').click();await expect(page.locator('#side-diff')).toBeVisible();await expect(page.locator('#diff-status')).not.toHaveText('Loading diff…');
  await expect(page.locator('#side-files')).toBeHidden();await expect(page.locator('#session-diff')).toHaveAttribute('aria-expanded','true');
  await page.locator('#side-tab-files').click();await expect(page.locator('#workspace-preview')).toContainText('answer');
  // Chat links open image files in Files; inline images use the image viewer.
  await page.locator('#side-close').click();
  await page.locator('#prompt').fill('See [the picture](pic.png) and ![inline picture](pic.png)');await page.locator('#send').click();
  await expect(page.locator('#session-state')).toHaveText('Ready');await expect(page.locator('#prompt')).toHaveValue('');await page.locator('#prompt').fill('Keep the side panel draft');
  await page.locator('.message.assistant').getByRole('link',{name:'the picture'}).click();
  await expect(panel).toBeVisible();await expect(page.locator('#side-tab-files')).toHaveAttribute('aria-selected','true');
  await expect(page.locator('#workspace-preview .workspace-preview-header strong')).toHaveText('pic.png');await expect(page.locator('#workspace-preview img.file-image')).toBeVisible();
  await page.locator('#side-close').click();await page.locator('.message.assistant img[alt="inline picture"]').click();
  const viewer=page.getByRole('dialog',{name:'Image viewer'});
  await expect(viewer).toBeVisible();await expect(panel).toBeHidden();
  await page.keyboard.press('Escape');await expect(viewer).toBeHidden();
  await page.locator('#session-files').click();await expect(panel).toBeVisible();
  // The full-size panel and its open state survive reload.
  await page.reload();await expect(page.locator('#session-state')).toHaveText('Ready');await expect(panel).toBeVisible();
  expect(await panel.evaluate(n=>n.clientWidth/n.parentElement.clientWidth)).toBeGreaterThan(.95);
  // Closing the panel restores the chat.
  await page.locator('#side-close').click();await expect(panel).toBeHidden();
  await expect(page.locator('#prompt')).toHaveValue('Keep the side panel draft');
  // Phone: the panel replaces the chat, and Back returns to the chat.
  await page.setViewportSize({width:390,height:844});
  await page.locator('#session-more').click();await page.locator('#session-diff').click();
  await expect(panel).toBeVisible();await expect(page.locator('#messages')).toBeHidden();
  expect(await panel.evaluate(n=>{const r=n.getBoundingClientRect();return r.left===0&&Math.round(r.right)===innerWidth;})).toBe(true);
  await page.locator('#side-tab-files').click();await expect(page.locator('#side-file-entries')).toContainText('notes.md');
  await page.goBack();await expect(panel).toBeHidden();await expect(page.locator('#messages')).toBeVisible();await expect(page.locator('body')).not.toHaveClass(/menu-open/);
  await page.locator('#session-more').click();await page.locator('#session-files').click();await expect(panel).toBeVisible();
  await page.locator('#side-close').click();await expect(panel).toBeHidden();await expect(page.locator('#messages')).toBeVisible();
  expect(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBe(true);
  expect(errors).toEqual([]);
  }finally{await rm(cwd,{recursive:true,force:true});}
});
test('workspace: file browsing, side preview, drafts, and manual automation runs',async({page})=>{
  const {mkdir,writeFile,rm}=await import('node:fs/promises'),{resolve,join}=await import('node:path'),{randomUUID}=await import('node:crypto');
  const cwd=resolve('.cache/browser-workspace-'+randomUUID());await mkdir(join(cwd,'docs'),{recursive:true});
  await writeFile(join(cwd,'README.md'),'# Workspace files\n\nOpen [the notes](docs/note.md).');await writeFile(join(cwd,'docs/note.md'),'# A project note\n\nThis file belongs to the selected directory.');
  let defId;const errors=[];page.on('pageerror',e=>errors.push(e.message));
  try{
    await page.goto('/');await create(page,'Browser test workspace',cwd);await page.locator('#prompt').fill('Keep my existing draft.');
    const original=await page.evaluate(()=>localStorage.getItem('agentworks-session'));
    await page.locator('#nav-files').click();await page.locator('.file-entry').filter({hasText:'README.md'}).click();
    await expect(page.locator('#workspace-file-detail h1')).toHaveText('Workspace files');
    await page.locator('#workspace-file-detail a').filter({hasText:'the notes'}).click();await expect(page.locator('#workspace-file-detail h1')).toHaveText('A project note');
    await page.locator('#workspace-file-detail button').filter({hasText:'Source',exact:true}).click();await expect(page.locator('.workspace-source')).toContainText('# A project note');
    await page.locator('#workspace-file-detail').getByRole('navigation',{name:'File path'}).getByRole('button',{name:'docs',exact:true}).click();
    await expect(page.locator('#file-breadcrumbs')).toContainText('docs');await page.locator('#file-entries .file-entry').filter({hasText:'note.md'}).click();
    await expect(page.locator('#workspace-file-detail h1')).toHaveText('A project note');
    await page.getByRole('button',{name:'Open beside chat',exact:true}).click();await expect(page.locator('#workspace-chat-column')).toBeHidden();await expect(page.locator('#workspace-preview')).toContainText('A project note');
    await expect(page.locator('#prompt')).toHaveValue('Keep my existing draft.');
    await page.locator('#workspace-preview').getByRole('button',{name:'Ask about this file'}).click();
    expect(await page.locator('#prompt').inputValue()).toContain('docs/note.md');expect(await page.locator('#prompt').inputValue()).toContain('Keep my existing draft.');
    await page.screenshot({path:'.cache/prototype-workspace-files.png'});
    // The beside-chat preview lives in the side panel's Files tab; ← Files returns to its listing.
    await expect(page.locator('#side-tab-files')).toHaveAttribute('aria-selected','true');
    await page.locator('#workspace-preview').getByRole('button',{name:'← Files'}).click();
    await expect(page.locator('#side-file-entries')).toContainText('README.md');await expect(page.locator('#workspace-preview')).toBeHidden();
    await page.locator('#side-close').click();await expect(page.locator('#side-panel')).toBeHidden();
    await page.locator('#nav-automations').click();await page.locator('#new-automation').click();await page.locator('#automation-name').fill('Browser test manual review');await page.locator('#automation-provider').selectOption('demo');await page.locator('#automation-model').fill('demo/quick');await page.locator('#automation-directory').fill(cwd);await page.getByLabel('Step 1 instructions').fill('approval');await page.locator('#save-automation').click();
    await expect(page.locator('#automation-detail h1')).toHaveText('Browser test manual review');
    defId=await page.evaluate(async()=>{const defs=await fetch('/api/automations').then(r=>r.json());return defs.find(d=>d.name==='Browser test manual review').id;});
    await page.locator('#run-automation').click();await expect(page.locator('#session-state')).toHaveText('Needs your approval');
    await expect(page.locator('#automation-source')).toContainText('Browser test manual review');
    const runSession=await page.evaluate(()=>localStorage.getItem('agentworks-session'));expect(runSession).not.toBe(original);
    await page.getByRole('button',{name:'Allow once',exact:true}).click();await expect(page.locator('#session-state')).toHaveText('Ready');
    await page.locator('#automation-source').click();await expect(page.locator('.automation-run .run-status').first()).toHaveText('completed');
    await page.screenshot({path:'.cache/prototype-workspace-automations.png'});
    await page.locator('.automation-run').first().click();expect(await page.evaluate(()=>localStorage.getItem('agentworks-session'))).toBe(runSession);
    expect(errors).toEqual([]);
  }finally{
    if(defId)await page.evaluate(id=>fetch('/api/automations/'+id,{method:'DELETE'}),defId).catch(()=>{});
    await rm(cwd,{recursive:true,force:true});
  }
});

test('workspace mobile: browse without an agent, ask in a new session, preserve context',async({page})=>{
  await page.setViewportSize({width:390,height:844});
  const {mkdir,writeFile,rm}=await import('node:fs/promises'),{resolve,join}=await import('node:path'),{randomUUID}=await import('node:crypto');
  const cwd=resolve('.cache/browser-mobile-files-'+randomUUID());await mkdir(cwd,{recursive:true});await writeFile(join(cwd,'README.md'),'# Mobile files\n\nPreview without starting an agent.');
  try{
    await page.goto('/');await page.locator('#nav-files').click();await page.locator('#add-location').click();await page.locator('#location-path').fill(cwd);await page.locator('#save-location').click();
    await page.locator('.file-entry').filter({hasText:'README.md'}).click();await expect(page.locator('#workspace-file-detail h1')).toHaveText('Mobile files');
    await page.getByRole('button',{name:'Ask about this file'}).click();await expect(page.locator('#directory')).toHaveValue(cwd);await page.locator('#provider').selectOption('demo');await page.locator('#session-title').fill('Browser test mobile files');await page.locator('#create-submit').click();
    await expect(page.locator('#session-state')).toHaveText('Ready');await expect(page.locator('#prompt')).toBeVisible();expect(await page.locator('#prompt').inputValue()).toContain('README.md');
    const id=await page.evaluate(()=>localStorage.getItem('agentworks-session'));const count=await page.evaluate(id=>fetch('/api/sessions/'+id).then(r=>r.json()).then(s=>s.events.filter(e=>e.kind==='user').length),id);expect(count).toBe(0);
    await page.locator('#session-home').click();await page.locator('#nav-files').click();await page.locator('.file-entry').filter({hasText:'README.md'}).click();await page.reload();await expect(page.locator('#workspace-files')).toBeVisible();await expect(page.locator('#workspace-file-detail h1')).toHaveText('Mobile files');await expect(page.locator('#workspace-location')).toHaveValue(await page.evaluate(()=>JSON.parse(localStorage.getItem('agentworks-workspace')).locationKey));
    expect(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBe(true);
    await page.screenshot({path:'.cache/prototype-workspace-mobile.png'});
  }finally{await rm(cwd,{recursive:true,force:true});}
});

test('agent access and inbox: grants, safe drafts, activity, mobile layout',async({page})=>{
  await page.goto('/');
  const setup=await page.evaluate(async()=>{
    const api=async(path,body,method=body?'POST':'GET')=>fetch(path,{method,headers:{'Content-Type':'application/json'},body:body?JSON.stringify(body):undefined}).then(r=>r.json());
    const {roots}=await api('/api/status');const p=await api('/api/projects',{name:'Browser test communication'});
    const a=await api('/api/sessions',{provider:'demo',cwd:roots[0],title:'Browser test sender',projectId:p.id});
    const b=await api('/api/sessions',{provider:'demo',cwd:roots[0],title:'Browser test inbox',projectId:p.id});
    const idle=async id=>{for(let i=0;i<100;i++){if((await api('/api/sessions/'+id)).session.status==='idle')return;await new Promise(r=>setTimeout(r,100));}throw Error('not idle');};
    await idle(a.id);await idle(b.id);
    await api(`/api/sessions/${a.id}/agent-access`,{communicate:true},'PUT');await idle(a.id);
    await api(`/api/sessions/${b.id}/agent-access`,{communicate:true},'PUT');await idle(b.id);
    const message=await api(`/api/sessions/${a.id}/mailbox`,{recipient:b.id,text:'Review this peer message <script>unsafe()</script>',requestId:'browser-message'});
    return {a,b,message};
  });
  await page.reload();await page.getByRole('button',{name:/Browser test inbox/}).click();
  await expect(page.locator('#session-state')).toHaveText('Ready');
  await page.locator('#prompt').fill('Existing draft');
  await page.locator('#lifecycle-menu > summary').click();await page.locator('#session-agents').click();
  await expect(page.locator('#agent-preset')).toHaveValue('participant');
  await expect(page.locator('#agent-inbox')).toContainText('Review this peer message <script>unsafe()</script>');
  await expect(page.locator('#agent-inbox script')).toHaveCount(0);
  await page.getByRole('button',{name:'Acknowledge',exact:true}).click();
  await expect(page.locator('#agent-inbox')).toContainText('Acknowledged');
  await page.getByRole('button',{name:'Use as next turn',exact:true}).click();
  await expect(page.locator('#agents-dialog')).not.toBeVisible();
  await expect(page.locator('#prompt')).toHaveValue(/Existing draft/);
  await expect(page.locator('#prompt')).toHaveValue(/peer-provided context/);
  await expect(page.locator('#messages')).not.toContainText('Local session, turn');
  await page.locator('#lifecycle-menu > summary').click();await page.locator('#session-agents').click();await page.locator('#agent-preset').selectOption('coordinator');
  await page.locator('#save-agent-access').click();await expect(page.locator('#save-agent-access')).toBeEnabled();
  await page.locator('#dismiss-agents').click();await expect(page.locator('#session-state')).toHaveText('Ready');
  await page.locator('#lifecycle-menu > summary').click();await page.locator('#session-agents').click();await expect(page.locator('#agent-preset')).toHaveValue('coordinator');
  await page.setViewportSize({width:390,height:844});
  const overflow=await page.locator('#agents-dialog').evaluate(n=>n.scrollWidth>n.clientWidth+2);expect(overflow).toBeFalsy();
  await page.screenshot({path:'.cache/prototype-agent-inbox-mobile.png'});
  await page.locator('#agent-grant-startSessions').uncheck();await expect(page.locator('#agent-preset')).toHaveValue('custom');
  await page.locator('#agent-preset').selectOption('off');await page.locator('#save-agent-access').click();await expect(page.locator('#save-agent-access')).toBeEnabled();
  await expect(page.locator('#agent-grant-communicate')).not.toBeChecked();await page.locator('#dismiss-agents').click();
  const access=await page.evaluate(id=>fetch(`/api/sessions/${id}/agent-access`).then(r=>r.json()),setup.b.id);expect(Object.values(access).some(Boolean)).toBeFalsy();
});
