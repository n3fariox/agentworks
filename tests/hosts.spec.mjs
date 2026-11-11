import {test,expect} from '@playwright/test';
import {openCreateSections,choosePlace} from './support/create-sections.mjs';
import {createServer} from 'node:http';
import {readFile} from 'node:fs/promises';
import {resolve,extname} from 'node:path';

openCreateSections(test);
// Static UI only: all API traffic is intercepted, no provider or live data access.
const base='http://127.0.0.1:4363';let server;
test.beforeAll(async()=>{
  server=createServer(async(req,res)=>{
    const name=req.url==='/'?'index.html':req.url.slice(1);
    if(!/^(?:vendor\/)?[\w.-]+$/.test(name)){res.writeHead(404);res.end();return;}
    try{const data=await readFile(resolve('web',name));res.setHeader('Content-Type',({'.js':'text/javascript','.mjs':'text/javascript','.css':'text/css','.html':'text/html'})[extname(name)]||'text/plain');res.end(data);}
    catch{res.writeHead(404);res.end();}
  });
  await new Promise((ok,fail)=>{server.once('error',fail);server.listen(4363,'127.0.0.1',ok);});
});
test.afterAll(async()=>{await new Promise(ok=>server.close(ok));});

const remote=(id,name,status,extra={})=>({id,name,status,kind:'remote',destination:`dev@${name}`,command:'agentworks',detail:'',since:Date.now(),worker:status==='connected'?{hostname:name,platform:'linux-x86_64',providers:['demo','claude'],tools:{codex:'/remote/bin/codex',claude:'/remote/bin/claude'},workspace:true,version:'0.1.0'}:null,...extra});
async function fixture(page,{build,r1={}}={}){
  const state={directories:[],hosts:[{id:'local',name:'Local',status:'connected',kind:'local'},remote('r1','build-box','connected',r1),remote('r2','old-box','offline',{detail:'ssh exited (exit status: 255). ssh: Permission denied (publickey). Retrying in 8s.'})],requests:[]};
  await page.routeWebSocket('**/api/sessions/*/events',()=>{});
  await page.route('**/api/**',async route=>{
    const request=route.request(),path=new URL(request.url()).pathname,method=request.method();
    const json=data=>route.fulfill({json:data});
    if(path==='/api/status')return json({roots:['/repo'],defaultModels:{},build,providers:[{id:'demo',name:'Demo agent',installed:true}]});
    if(path==='/api/directories'){const q=Object.fromEntries(new URL(request.url()).searchParams);state.directories.push(q);return json({root:'/home/dev',path:q.path||'/home/dev',parent:null,entries:[{name:'src',path:'/home/dev/src'}]});}
    if(path==='/api/catalog')return json({projects:[{id:'a',name:'Alpha',locationIds:[]}],hosts:state.hosts,locations:[]});
    if(path==='/api/sessions'&&method==='GET')return json([]);
    if(path==='/api/automations')return json([]);
    if(path.startsWith('/api/hosts'))state.requests.push({path,method,body:request.postData()?request.postDataJSON():null});
    if(path==='/api/hosts'&&method==='POST'){
      const body=request.postDataJSON();
      if(body.destination.startsWith('-'))return route.fulfill({status:400,body:'Use an SSH destination such as user@host'});
      const host=remote('r3',body.name,'connecting');state.hosts.push(host);return json(host);
    }
    const reconnect=path.match(/^\/api\/hosts\/([^/]+)\/reconnect$/);
    if(reconnect){const host=state.hosts.find(h=>h.id===reconnect[1]);Object.assign(host,{status:'connecting',detail:''});return json(host);}
    const roots=path.match(/^\/api\/hosts\/([^/]+)\/roots$/);
    if(roots&&method==='PUT'){const host=state.hosts.find(h=>h.id===roots[1]);host.roots=request.postDataJSON().roots;return json(host);}
    if(path==='/api/sessions'&&method==='POST'){state.created=request.postDataJSON();return route.fulfill({status:409,body:'stop here'});}
    const host=path.match(/^\/api\/hosts\/([^/]+)$/);
    if(host&&method==='DELETE'){state.hosts=state.hosts.filter(h=>h.id!==host[1]);return json({ok:true});}
    if(host&&method==='PATCH'){const h=state.hosts.find(h=>h.id===host[1]);Object.assign(h,request.postDataJSON());return json(h);}
    return json({});
  });
  await page.goto(base);await expect(page.locator('#provider option')).toHaveCount(1);
  return state;
}

for(const mobile of [false,true]){
  test(`remote hosts show link status and controls ${mobile?'mobile':'desktop'}`,async({page})=>{
    if(mobile)await page.setViewportSize({width:390,height:844});
    const errors=[];page.on('pageerror',e=>errors.push(e.message));const s=await fixture(page);
    if(mobile)await page.locator('#menu').click();
    await page.locator('#view-hosts').click();
    await expect(page.locator('#section-title')).toHaveText('HOSTS');await expect(page.locator('#session-count')).toHaveText('3');
    const box=page.locator('[data-group="host:r1"]');
    await expect(box.locator('.tree-label').first()).toHaveText('build-box');
    await expect(box.locator('.connection-dot.connected')).toHaveAttribute('title','Connected · ssh dev@build-box · build-box · linux-x86_64 · providers: demo, claude · v0.1.0');
    await expect(box).toContainText('Add working directories');
    await expect(box.getByRole('button',{name:'New session in build-box'})).toHaveCount(0);
    // The offline reason is shown inline, not only on hover.
    const old=page.locator('[data-group="host:r2"]');
    await expect(old.locator('.connection-dot.offline')).toHaveCount(1);
    await expect(old.locator('.host-note')).toHaveText(/Permission denied \(publickey\)/);
    await expect(page.locator('[data-group="host:local"]').getByRole('button',{name:'New session in Local'})).toHaveCount(1);

    await old.getByRole('button',{name:'Reconnect old-box'}).click();
    await expect(old.locator('.connection-dot.connecting')).toHaveCount(1);
    page.once('dialog',d=>d.accept());
    await box.getByRole('button',{name:'Remove host build-box'}).click();
    await expect(box).toHaveCount(0);

    await page.getByRole('button',{name:'Add host'}).click();
    await page.locator('#host-mode').selectOption('manual');
    await page.locator('#host-name').fill('laptop');await page.locator('#host-destination').fill('-oProxyCommand=sh');
    await page.locator('#save-host').click();
    await expect(page.locator('#host-error')).toContainText('SSH destination');
    await page.locator('#host-destination').fill('me@laptop');await page.locator('#save-host').click();
    await expect(page.locator('#host-dialog')).toBeHidden();
    if(mobile&&!(await page.locator('[data-group="host:r3"]').isVisible()))await page.locator('#menu').click();
    await expect(page.locator('[data-group="host:r3"] .connection-dot.connecting')).toHaveCount(1);
    expect(s.requests.map(r=>`${r.method} ${r.path}`)).toEqual(['POST /api/hosts/r2/reconnect','DELETE /api/hosts/r1','POST /api/hosts','POST /api/hosts']);
    expect(s.requests.at(-1).body).toEqual({name:'laptop',destination:'me@laptop',command:''});
    expect(errors).toEqual([]);
  });
}

test('empty remote hosts stay out of the Projects view',async({page})=>{
  await fixture(page);
  await page.locator('#view-projects').click();
  await expect(page.getByRole('button',{name:'New project'})).toHaveCount(1);
  await expect(page.locator('#section-title')).toHaveText('SESSIONS');await expect(page.locator('#session-count')).toHaveText('0');
  await expect(page.locator('[data-group="project:a:host:local"]')).toHaveCount(1);
  await expect(page.locator('[data-group^="project:a:host:r"]')).toHaveCount(0);
});

test('host working directories enable remote sessions in the new-session dialog',async({page})=>{
  const errors=[];page.on('pageerror',e=>errors.push(e.message));const s=await fixture(page);
  await page.locator('#view-hosts').click();
  const box=page.locator('[data-group="host:r1"]');
  await box.getByRole('button',{name:'Settings for build-box'}).click();
  const dialog=page.locator('#host-settings-dialog');
  await expect(page.locator('#host-settings-title')).toHaveText('build-box');
  await expect(dialog.getByRole('tab',{name:'General'})).toHaveAttribute('aria-selected','true');
  await expect(page.locator('#host-edit-name')).toHaveValue('build-box');
  await dialog.getByRole('tab',{name:'Harness tools'}).click();
  await expect(page.locator('#host-tools')).toContainText('/remote/bin/codex');
  await expect(page.locator('#host-tools')).toContainText('codex');
  await expect(page.locator('#host-tools')).toContainText('dev@build-box');
  await dialog.getByRole('tab',{name:'Connection'}).click();
  await expect(page.locator('#host-connection')).toContainText('Connected');
  await expect(page.locator('#host-connection').getByRole('button',{name:'Reconnect'})).toBeVisible();
  // Edits survive switching categories, as in Settings.
  await dialog.getByRole('tab',{name:'Directories'}).click();
  await page.locator('#host-roots input').first().fill('~/projects');
  await page.locator('#add-host-root').click();
  await page.locator('#add-host-root').click();
  await page.locator('#host-roots input').last().fill('/srv/app');
  await dialog.getByRole('tab',{name:'Connection'}).click();await dialog.getByRole('tab',{name:'Directories'}).click();
  await expect(page.locator('#host-roots input').last()).toHaveValue('/srv/app');
  expect(await dialog.evaluate(e=>e.scrollWidth<=e.clientWidth)).toBe(true);
  await page.locator('#save-host-settings').click();
  await expect(dialog).toBeHidden();
  expect(s.requests.at(-1)).toEqual({path:'/api/hosts/r1/roots',method:'PUT',body:{roots:['~/projects','/srv/app']}});
  await expect(box).toContainText('No sessions yet');
  // Offline hosts cannot take new sessions; connected ones with directories can.
  await expect(page.locator('[data-group="host:r2"]').getByRole('button',{name:'New session in old-box'})).toHaveCount(0);
  await box.getByRole('button',{name:'New session in build-box'}).click();
  await expect(page.locator('#create-host')).toHaveValue('r1');
  await expect(page.locator('#create-host option[value="r2"]')).toBeDisabled();
  await expect(page.locator('#directory')).toHaveValue('~/projects');
  await expect(page.locator('#load-models')).toBeEnabled();
  await expect(page.locator('#roots-note')).toContainText('Working directories on build-box: ~/projects, /srv/app');
  await page.locator('#create-submit').click();
  await expect(page.locator('#create-error')).toContainText('stop here');
  expect(s.created).toMatchObject({hostId:'r1',cwd:'~/projects',workspace:{mode:'directory'}});
  // Switching back to Local restores local roots and model discovery.
  await choosePlace(page,{host:'local'});
  await expect(page.locator('#load-models')).toBeEnabled();
  await expect(page.locator('#roots-note')).toContainText('Allowed roots: /repo');
  expect(errors).toEqual([]);
});


test('host settings rename, repoint, browse for roots, flag outdated workers and remove',async({page})=>{
  const errors=[];page.on('pageerror',e=>errors.push(e.message));
  const s=await fixture(page,{build:'0.1.0+bbbbbbbbbbbb',r1:{outdated:true,worker:{...remote('r1','build-box','connected').worker,directories:true,build:'0.1.0+aaaaaaaaaaaa'}}});
  page.on('dialog',d=>d.accept());
  await page.locator('#view-hosts').click();
  const box=page.locator('[data-group="host:r1"]');
  // Out-of-date workers are flagged in the sidebar and on the Connection tab.
  await expect(box.locator('.connection-dot.host-update')).toHaveAttribute('aria-label',/worker update available/);
  await expect(box.locator('.tree-label').first()).toBeVisible();
  expect(await box.locator('.tree-label').first().evaluate(e=>e.getBoundingClientRect().width)).toBeGreaterThan(20);
  await expect(page.locator('[data-group="host:r2"] .host-update')).toHaveCount(0);
  await expect(page.locator('#host-address')).toContainText('v0.1.0 (bbbbbbbbbbbb)');
  await box.getByRole('button',{name:'Settings for build-box'}).click();
  const dialog=page.locator('#host-settings-dialog');
  await dialog.getByRole('tab',{name:'Connection'}).click();
  await expect(page.locator('#host-connection')).toContainText('v0.1.0 (aaaaaaaaaaaa)');
  await expect(page.locator('#host-connection .update-badge')).toHaveText('Update available');
  await expect(page.locator('#host-connection')).toContainText('v0.1.0 (bbbbbbbbbbbb)');
  // Browse… starts in the host's home directory, not inside the current roots.
  await dialog.getByRole('tab',{name:'Directories'}).click();
  await page.locator('#host-roots').getByRole('button',{name:'Browse for working directory'}).click();
  await expect(page.locator('#directory-picker-host')).toContainText('inside its home directory');
  await page.locator('#directory-picker-list').getByRole('button',{name:'src'}).click();
  await page.locator('#directory-picker-select').click();
  await expect(page.locator('#host-roots input').first()).toHaveValue('/home/dev/src');
  expect(s.directories.every(q=>q.scope==='home'&&q.hostId==='r1')).toBe(true);
  // General: rename and repoint; a changed destination asks before reconnecting.
  await dialog.getByRole('tab',{name:'General'}).click();
  await expect(page.locator('#host-edit-command')).toHaveValue('agentworks');
  await page.locator('#host-edit-name').fill('builder');
  await page.locator('#host-edit-destination').fill('dev@builder.lan');
  await page.locator('#save-host-settings').click();
  await expect(dialog).toBeHidden();
  expect(s.requests.slice(-2)).toEqual([
    {path:'/api/hosts/r1',method:'PATCH',body:{name:'builder',destination:'dev@builder.lan',command:'agentworks'}},
    {path:'/api/hosts/r1/roots',method:'PUT',body:{roots:['/home/dev/src']}},
  ]);
  await expect(page.locator('[data-group="host:r1"]')).toContainText('builder');
  // Remove lives in General; a host without sessions can be removed there.
  await page.locator('[data-group="host:r2"]').getByRole('button',{name:'Settings for old-box'}).click();
  await expect(page.locator('#host-remove-note')).toContainText('Nothing on the host is deleted');
  await page.locator('#remove-host').click();
  await expect(dialog).toBeHidden();
  expect(s.requests.at(-1)).toEqual({path:'/api/hosts/r2',method:'DELETE',body:null});
  await expect(page.locator('[data-group="host:r2"]')).toHaveCount(0);
  expect(errors).toEqual([]);
});

test('remote availability and directories are independent of local provider configuration',async({page})=>{
  const s=await fixture(page);
  s.hosts.find(h=>h.id==='r1').roots=['/remote/project'];
  await page.route('**/api/status',route=>route.fulfill({json:{roots:['/repo'],providers:[{id:'demo',name:'Demo agent',installed:false,authMode:'api_key',keyPresent:false}]}}));
  await page.reload();await page.locator('#view-hosts').click();
  await page.locator('[data-group="host:r1"]').getByRole('button',{name:'New session in build-box'}).click();
  await expect(page.locator('#directory')).toHaveValue('/remote/project');
  await expect(page.locator('#create-submit')).toBeEnabled();
  await expect(page.locator('#workspace-mode')).toBeEnabled();
  await expect(page.locator('#create-resume-policy')).toBeEnabled();
  await choosePlace(page,{host:'local'});
  await expect(page.locator('#directory')).toHaveValue('/repo');
  await expect(page.locator('#create-submit')).toBeDisabled();
  await expect(page.locator('#workspace-mode')).toBeEnabled();
});


test('remote selection respects required worktrees and restores local policy on return',async({page})=>{
  const s=await fixture(page);s.hosts.find(h=>h.id==='r1').roots=['/remote/project'];
  await page.route('**/api/projects/a/workspace-policy',route=>route.fulfill({json:{defaultMode:'newWorktree',requireWorktree:true}}));
  await page.locator('#view-hosts').click();
  await page.locator('[data-group="host:r1"]').getByRole('button',{name:'New session in build-box'}).click();
  await choosePlace(page,{project:'a'});
  await expect(page.locator('#workspace-policy-note')).toContainText('Isolated worktree required');
  await expect(page.locator('#workspace-mode')).toHaveValue('newWorktree');
  await expect(page.locator('#create-submit')).toBeEnabled();
  await choosePlace(page,{host:'local'});
  await expect(page.locator('#workspace-mode')).toHaveValue('newWorktree');
  await expect(page.locator('#workspace-policy-note')).toContainText('Isolated worktree required');
  await expect(page.locator('#create-submit')).toBeEnabled();
});

for(const mobile of [false,true])test(`guided enrollment terminal ${mobile?'mobile':'desktop'}`,async({page})=>{
  if(mobile)await page.setViewportSize({width:390,height:844});
  const state=await fixture(page);let typed='';
  await page.routeWebSocket('**/api/hosts/enroll/events',ws=>{ws.onMessage(data=>{
    const value=JSON.parse(data);
    if(value.destination){expect(value).toEqual({name:'New box',destination:'dev@box',os:'unix'});ws.send(JSON.stringify({type:'stage',message:'Enter your SSH password in the terminal'}));ws.send(new TextEncoder().encode('Password: '));}
    if(value.type==='input')typed+=value.data;
    if(typed==='test-password'){
      const host=remote('enrolled','New box','connected',{managed:true});state.hosts.push(host);
      ws.send(JSON.stringify({type:'complete',host}));
    }
  });});
  if(mobile)await page.locator('#menu').click();
  await page.locator('#view-hosts').click();await page.getByRole('button',{name:'Add host'}).click();
  await expect(page.locator('#host-mode')).toHaveValue('enroll');await expect(page.locator('#host-manual')).toBeHidden();
  await page.locator('#host-name').fill('New box');await page.locator('#host-destination').fill('dev@box');await page.locator('#save-host').click();
  await expect(page.locator('#host-stage')).toContainText('SSH password');
  await expect(page.locator('#host-name')).toBeDisabled();
  const input=page.locator('#host-terminal .xterm-helper-textarea');await input.focus();await page.keyboard.type('test-password');
  await expect.poll(()=>typed).toBe('test-password');
  await expect(page.locator('#host-stage')).toContainText('Connected');
  await expect(page.locator('#host-terminal')).not.toContainText('test-password');
  expect(await page.locator('#host-dialog').evaluate(e=>e.scrollWidth<=e.clientWidth)).toBe(true);
  await page.locator('#host-done').click();await expect(page.locator('#host-settings-dialog')).toBeVisible();
  await expect(page.locator('#host-settings-title')).toHaveText('New box');
  await expect(page.locator('#host-settings-panel-directories')).toBeVisible();
});

test('enrollment error allows retry and close cancels the terminal',async({page})=>{
 await fixture(page);let count=0,closed=false;
 await page.routeWebSocket('**/api/hosts/enroll/events',ws=>{ws.onClose(()=>closed=true);ws.onMessage(data=>{if(JSON.parse(data).destination){count++;if(count===1)ws.send(JSON.stringify({type:'error',message:'tar is required on this host'}));else ws.send(JSON.stringify({type:'stage',message:'Connecting…'}));}});});
 await page.locator('#view-hosts').click();await page.getByRole('button',{name:'Add host'}).click();
 await page.locator('#host-name').fill('Box');await page.locator('#host-destination').fill('dev@box');await page.locator('#save-host').click();
 await expect(page.locator('#host-error')).toContainText('tar is required');await expect(page.locator('#save-host')).toBeEnabled();
 await page.locator('#save-host').click();await expect(page.locator('#host-stage')).toContainText('Connecting');
 closed=false;await page.locator('#dismiss-host').click();await expect.poll(()=>closed).toBe(true);await expect(page.locator('#host-dialog')).toBeHidden();
});

 test('model and worktree discovery target the selected remote host',async({page})=>{
  const s=await fixture(page);s.hosts.find(h=>h.id==='r1').roots=['/remote/project'];
  let modelRequest,treeRequest;
  await page.route('**/api/models',route=>{modelRequest=route.request().postDataJSON();return route.fulfill({json:{options:[{value:'remote/model',name:'Remote model'}]}});});
  await page.route('**/api/worktrees/list',route=>{treeRequest=route.request().postDataJSON();return route.fulfill({json:{worktrees:[{path:'/remote/project/.worktrees/one',branch:'one',isMain:false}]}});});
  await page.locator('#view-hosts').click();
  await page.locator('[data-group="host:r1"]').getByRole('button',{name:'New session in build-box'}).click();
  await page.locator('#load-models').click();
  await expect(page.locator('#create-model option[value="remote/model"]')).toHaveCount(1);
  expect(modelRequest).toEqual({provider:'demo',cwd:'/remote/project',hostId:'r1',modelProvider:null});
  await page.locator('#workspace-mode').selectOption('existingWorktree');
  await page.locator('#discover-worktrees').click();
  await expect(page.locator('#existing-worktree option')).toHaveCount(2);
  expect(treeRequest).toEqual({cwd:'/remote/project',hostId:'r1'});
  await page.locator('#existing-worktree').selectOption('/remote/project/.worktrees/one');
  await page.locator('#create-submit').click();
  await expect(page.locator('#create-error')).toContainText('stop here');
  expect(s.created).toMatchObject({hostId:'r1',cwd:'/remote/project/.worktrees/one',workspace:{mode:'existingWorktree'}});
});

test('remote history picker sends its host when browsing and importing',async({page})=>{
  const s=await fixture(page);s.hosts.find(h=>h.id==='r1').roots=['/remote/project'];
  let browse,imported;
  await page.route('**/api/external-sessions',route=>{browse=route.request().postDataJSON();return route.fulfill({json:{sessions:[{sessionId:'native-remote',cwd:'/remote/project',title:'Remote history'}]}});});
  await page.route('**/api/sessions/import',route=>{imported=route.request().postDataJSON();return route.fulfill({status:409,body:'import captured'});});
  await page.locator('#view-hosts').click();
  await page.locator('[data-group="host:r1"]').getByRole('button',{name:'New session in build-box'}).click();
  await page.locator('#create-import').click();
  await expect(page.locator('#import-host')).toHaveValue('r1');
  await expect(page.locator('#external-sessions')).toContainText('Remote history');
  expect(browse).toMatchObject({hostId:'r1',provider:'demo'});
  await page.locator('#external-sessions input[type=radio]').first().check();
  await page.locator('#import-submit').click();
  await expect(page.locator('#import-error')).toHaveText('import captured');
  expect(imported).toMatchObject({hostId:'r1',provider:'demo',providerSessionId:'native-remote',cwd:'/remote/project'});
});

test('automation editor selects a remote host for models and saved execution',async({page})=>{
  const s=await fixture(page);s.hosts.find(h=>h.id==='r1').roots=['/remote/project'];
  await page.route('**/api/catalog',route=>route.fulfill({json:{projects:[{id:'a',name:'Alpha',locationIds:[]}],hosts:s.hosts,locations:[]}}));
  let models,saved;
  await page.route('**/api/models',route=>{models=route.request().postDataJSON();return route.fulfill({json:{options:[{value:'remote/model',name:'Remote model'}]}});});
  await page.route('**/api/automations',route=>{if(route.request().method()==='GET')return route.fulfill({json:[]});saved=route.request().postDataJSON();return route.fulfill({status:409,body:'automation captured'});});
  await page.reload();await page.locator('#nav-automations').click();await page.locator('#new-automation').click();
  await page.locator('#automation-host').selectOption('r1');
  await expect(page.locator('#automation-directory')).toHaveValue('/remote/project');
  await page.locator('#automation-provider').selectOption('demo');
  await page.locator('#automation-load-models').click();
  await expect(page.locator('#automation-models option')).toHaveCount(1);
  expect(models).toMatchObject({hostId:'r1',provider:'demo',cwd:'/remote/project'});
  await page.locator('#automation-name').fill('Remote task');await page.getByLabel('Step 1 instructions').fill('/status');
  await page.locator('#save-automation').click();await expect(page.locator('#automation-error')).toHaveText('automation captured');
  expect(saved).toMatchObject({hostId:'r1',provider:'demo',cwd:'/remote/project',workflow:{version:1,trigger:{kind:'manual'},steps:[{kind:'agent',text:'/status'}]}});
});
