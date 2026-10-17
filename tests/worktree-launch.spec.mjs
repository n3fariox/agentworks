import {test,expect} from '@playwright/test';
import {createServer} from 'node:http';
import {readFile} from 'node:fs/promises';
import {resolve,extname} from 'node:path';

// Static UI only: all API traffic is intercepted, no provider or live data access.
const base='http://127.0.0.1:4362';let server;
test.beforeAll(async()=>{
  server=createServer(async(req,res)=>{
    const name=req.url==='/'?'index.html':req.url.slice(1);
    if(!/^[\w.-]+$/.test(name)){res.writeHead(404);res.end();return;}
    try{const data=await readFile(resolve('web',name));res.setHeader('Content-Type',({'.js':'text/javascript','.css':'text/css','.html':'text/html'})[extname(name)]||'text/plain');res.end(data);}
    catch{res.writeHead(404);res.end();}
  });
  await new Promise((ok,fail)=>{server.once('error',fail);server.listen(4362,'127.0.0.1',ok);});
});
test.afterAll(async()=>{await new Promise(ok=>server.close(ok));});
const ordinary={defaultMode:'directory',requireWorktree:false};
const isolated={defaultMode:'newWorktree',requireWorktree:true};
async function fixture(page,options={}){
  const state={projects:[{id:'a',name:'Alpha',locationIds:[]},{id:'b',name:'Beta',locationIds:[]}],creates:[],mutations:[],lists:[],policies:{a:ordinary,b:isolated},policyFailure:false};
  await page.routeWebSocket('**/api/sessions/*/events',()=>{});
  await page.route('**/api/**',async route=>{
    const request=route.request(),path=new URL(request.url()).pathname,method=request.method();
    const body=request.postData()?request.postDataJSON():null;
    const json=data=>route.fulfill({json:data});
    if(path==='/api/status')return json({roots:['/repo'],defaultModels:{demo:'saved-model'},providers:[{id:'demo',name:'Demo agent',installed:true}]});
    if(path==='/api/catalog')return json({projects:state.projects,hosts:[{id:'local',name:'Local',status:'connected'}],locations:[]});
    if(path==='/api/sessions'&&method==='GET')return json([]);
    if(path==='/api/automations')return json([]);
    const policy=path.match(/^\/api\/projects\/([^/]+)\/workspace-policy$/);
    if(policy){
      if(method==='GET'){
        if(options.policy) return options.policy({route,id:policy[1],json,state});
        return json(state.policies[policy[1]]||ordinary);
      }
      state.mutations.push({path,method,body});
      if(state.policyFailure)return route.fulfill({status:503,body:'Policy save unavailable'});
      state.policies[policy[1]]=body;return json(body);
    }
    if(path==='/api/projects'&&method==='POST'){
      const project={id:'new',name:body.name,locationIds:[]};state.projects.push(project);state.mutations.push({path,method,body});return json(project);
    }
    if(path.startsWith('/api/projects/')&&method==='PATCH'){
      const project=state.projects.find(p=>p.id===path.split('/').at(-1));project.name=body.name;state.mutations.push({path,method,body});return json(project);
    }
    if(path==='/api/worktrees/list'){
      state.lists.push(body);
      if(options.list)return options.list({route,json});
      return json({worktrees:[
        {path:'/repo',branch:'main',isMain:true},
        {path:'/repo/.worktrees/locked',branch:'locked',locked:true},
        {path:'/repo/.worktrees/prunable',branch:'pruned',prunable:true},
        {path:'/repo/.worktrees/linked',branch:'<img src=x onerror=alert(1)>',isMain:false,locked:false,prunable:false},
      ]});
    }
    if(path==='/api/sessions'&&method==='POST'){
      state.creates.push(body);
      if(options.success)return json({id:'created',...body,locationId:'root:/repo',status:'idle',createdAt:Date.now()});
      return route.fulfill({status:503,body:'Launch unavailable; retry'});
    }
    if(path==='/api/models')return json({options:[{value:'chosen-model',name:'Chosen model'}]});
    return json({});
  });
  await page.goto(base);await expect(page.locator('#provider option')).toHaveCount(1);
  await page.locator('#welcome-new').click();return state;
}
for(const mobile of [false,true]){
  test(`workspace controls and discovery ${mobile?'mobile':'desktop'}`,async({page})=>{
    if(mobile)await page.setViewportSize({width:390,height:844});
    const errors=[];page.on('pageerror',e=>errors.push(e.message));const s=await fixture(page);
    await expect(page.locator('#workspace-mode')).toHaveValue('directory');
    await expect(page.locator('#workspace-new-fields')).toBeHidden();
    await page.locator('#create-project').selectOption('b');
    await expect(page.locator('#workspace-mode')).toHaveValue('newWorktree');
    await expect(page.locator('#workspace-mode option[value=directory]')).toBeDisabled();
    await expect(page.locator('#workspace-policy-note')).toContainText('Isolated worktree required');
    await expect(page.locator('#workspace-new-fields')).toContainText('Uncommitted changes stay');
    await page.locator('#worktree-branch').fill('feature/ui');await page.locator('#worktree-base').fill('main');
    await page.locator('#create-submit').click();
    expect(s.creates[0]).toMatchObject({cwd:'/repo',projectId:'b',workspace:{mode:'newWorktree',branch:'feature/ui',baseRef:'main'}});
    await page.locator('#workspace-mode').selectOption('existingWorktree');
    await expect(page.locator('#create-submit')).toBeDisabled();
    await page.locator('#discover-worktrees').click();
    await expect(page.locator('#existing-worktree option')).toHaveCount(5);
    expect(s.creates).toHaveLength(1);expect(s.lists).toEqual([{cwd:'/repo'}]);
    for(const path of ['/repo','/repo/.worktrees/locked','/repo/.worktrees/prunable'])await expect(page.locator(`#existing-worktree option[value="${path}"]`)).toBeDisabled();
    await expect(page.locator('#existing-worktree')).toContainText('<img src=x onerror=alert(1)>');expect(await page.locator('#create-dialog img').count()).toBe(0);
    await page.locator('#existing-worktree').selectOption('/repo/.worktrees/linked');
    await page.locator('#create-submit').click();
    expect(s.creates[1]).toMatchObject({cwd:'/repo/.worktrees/linked',workspace:{mode:'existingWorktree'}});
    await page.locator('#directory').fill('/other');await expect(page.locator('#create-submit')).toBeDisabled();
    await page.locator('#create-project').selectOption('a');
    await expect(page.locator('#workspace-mode')).toHaveValue('directory');
    await expect(page.locator('#worktree-branch')).toHaveValue('');await expect(page.locator('#worktree-base')).toHaveValue('');
    expect(await page.locator('#create-dialog').evaluate(n=>{const r=n.getBoundingClientRect();return r.left>=0&&r.right<=innerWidth&&r.height<=innerHeight;})).toBe(true);
    expect(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBe(true);expect(errors).toEqual([]);
  });
}
test('identical retry keeps request ID, edited payload gets a new ID; harness and model preserved',async({page})=>{
  const s=await fixture(page);await page.locator('#load-models').click();await page.locator('#create-model').selectOption('chosen-model');
  await page.locator('#create-project').selectOption('b');await expect(page.locator('#workspace-mode')).toHaveValue('newWorktree');
  await expect(page.locator('#create-model')).toHaveValue('chosen-model');
  await page.locator('#session-title').fill('Retry');await page.locator('#create-submit').click();await expect(page.locator('#create-error')).toContainText('retry');
  await page.locator('#create-submit').click();await expect.poll(()=>s.creates.length).toBe(2);
  expect(s.creates[0]).toEqual(s.creates[1]);expect(s.creates[0].requestId).toBeTruthy();
  expect(s.creates[0]).toMatchObject({provider:'demo',model:'chosen-model',workspace:{mode:'newWorktree'}});
  await page.locator('#session-title').fill('Different');await page.locator('#create-submit').click();await expect.poll(()=>s.creates.length).toBe(3);
  expect(s.creates[2].requestId).not.toBe(s.creates[0].requestId);
});
test('ordinary directory creation stays compatible',async({page})=>{
  const s=await fixture(page,{success:true});await page.locator('#directory').fill('/repo/subdir');await page.locator('#create-submit').click();
  await expect(page.locator('#create-dialog')).toBeHidden();await expect(page.locator('#project-path')).toHaveText('/repo/subdir');
  expect(s.creates[0]).toMatchObject({cwd:'/repo/subdir',projectId:null,workspace:{mode:'directory'}});
  await page.locator('#prompt').fill('Keep my draft');await page.locator('#new-session').click();await page.locator('#workspace-mode').selectOption('newWorktree');await page.locator('#dismiss-create').click();
  await expect(page.locator('#prompt')).toHaveValue('Keep my draft');
  let wording='';page.on('dialog',async d=>{wording=d.message();await d.dismiss();});await page.locator('#close-session').click();expect(wording).toContain('Worktree files and branches remain');
});
test('slow policy response cannot overwrite the next project, and errors block launch',async({page})=>{
  let release,started;const waiting=new Promise(r=>release=r),seen=new Promise(r=>started=r);
  await fixture(page,{policy:async({id,json,route})=>{if(id==='a'){started();await waiting;return json(isolated);}return route.fulfill({status:503,body:'Unavailable'});}});
  await page.locator('#create-project').selectOption('a');await seen;await expect(page.locator('#create-submit')).toBeDisabled();
  await page.locator('#create-project').selectOption('b');await expect(page.locator('#workspace-policy-note')).toContainText('Cannot load');release();
  await expect(page.locator('#workspace-mode')).toHaveValue('directory');await expect(page.locator('#create-submit')).toBeDisabled();
  await page.locator('#create-project').selectOption('');await expect(page.locator('#create-submit')).toBeEnabled();
});
test('stale discovery cannot repopulate after source/project switch',async({page})=>{
  let release,started;const waiting=new Promise(r=>release=r),seen=new Promise(r=>started=r);
  const s=await fixture(page,{list:async({json})=>{started();await waiting;return json({worktrees:[{path:'/stale',branch:'stale'}]});}});
  await page.locator('#workspace-mode').selectOption('existingWorktree');await page.locator('#discover-worktrees').click();await seen;
  await page.locator('#create-project').selectOption('b');release();await expect(page.locator('#workspace-mode')).toHaveValue('newWorktree');
  await expect(page.locator('#existing-worktree option')).toHaveCount(1);expect(s.creates).toHaveLength(0);
});
test('project creation policy defaults, enforcement and partial save retry',async({page})=>{
  const s=await fixture(page);s.policyFailure=true;await page.locator('#create-project-new').click();
  await expect(page.locator('#project-default-mode')).toHaveValue('directory');await expect(page.locator('#project-require-worktree')).not.toBeChecked();
  await page.locator('#project-name').fill('New group');await page.locator('#project-require-worktree').check();
  await expect(page.locator('#project-default-mode')).toHaveValue('newWorktree');await expect(page.locator('#project-default-mode option[value=directory]')).toBeDisabled();
  await page.locator('#save-project').click();await expect(page.locator('#project-error')).toContainText('Project name saved');
  expect(s.mutations.filter(m=>m.path==='/api/projects')).toHaveLength(1);s.policyFailure=false;
  await page.locator('#save-project').click();await expect(page.locator('#project-dialog')).toBeHidden();
  expect(s.mutations.filter(m=>m.path==='/api/projects')).toHaveLength(1);
  expect(s.policies.new).toEqual(isolated);await expect(page.locator('#create-project')).toHaveValue('new');await expect(page.locator('#workspace-mode')).toHaveValue('newWorktree');
});
test('existing project editor loads and accurately saves name plus policy',async({page})=>{
  const s=await fixture(page);await page.locator('#dismiss-create').click();await page.getByRole('button',{name:'Edit project Beta',exact:true}).click();
  await expect(page.locator('#project-require-worktree')).toBeChecked();await expect(page.locator('#project-default-mode')).toHaveValue('newWorktree');
  await page.locator('#project-name').fill('Renamed');await page.locator('#project-require-worktree').uncheck();await page.locator('#project-default-mode').selectOption('existingWorktree');
  await page.locator('#save-project').click();await expect(page.locator('#project-dialog')).toBeHidden();
  expect(s.projects.find(p=>p.id==='b').name).toBe('Renamed');expect(s.policies.b).toEqual({defaultMode:'existingWorktree',requireWorktree:false});
});
test('existing policy load is guarded across close/reopen and failed reads cannot save',async({page})=>{
  let release,started;const waiting=new Promise(r=>release=r),seen=new Promise(r=>started=r);
  const s=await fixture(page,{policy:async({id,json,route})=>{if(id==='a'){started();await waiting;return json(isolated);}return route.fulfill({status:503,body:'<script>bad()</script>'});}});
  await page.locator('#dismiss-create').click();await page.getByRole('button',{name:'Edit project Alpha',exact:true}).click();await seen;
  await expect(page.locator('#save-project')).toBeDisabled();await page.locator('#dismiss-project').click();
  await page.getByRole('button',{name:'Edit project Beta',exact:true}).click();await expect(page.locator('#project-error')).toContainText('Cannot load workspace policy');release();
  await expect(page.locator('#save-project')).toBeDisabled();await expect(page.locator('#project-name')).toHaveValue('Beta');
  expect(await page.locator('#project-error script').count()).toBe(0);expect(s.mutations).toHaveLength(0);
});
test('failed launch retry retains its request ID after reload in this tab',async({page})=>{
  const s=await fixture(page);await page.locator('#workspace-mode').selectOption('newWorktree');await page.locator('#create-submit').click();await expect(page.locator('#create-error')).toContainText('retry');
  const first=s.creates[0];await page.reload();await page.locator('#welcome-new').click();await page.locator('#workspace-mode').selectOption('newWorktree');await page.locator('#create-submit').click();
  await expect.poll(()=>s.creates.length).toBe(2);expect(s.creates[1]).toEqual(first);
});
test('discovery error stays visible and retry is read-only',async({page})=>{
  let fail=true;const s=await fixture(page,{list:({route,json})=>fail?route.fulfill({status:503,body:'Discovery temporarily unavailable'}):json({worktrees:[]})});
  await page.locator('#workspace-mode').selectOption('existingWorktree');await page.locator('#discover-worktrees').click();
  await expect(page.locator('#worktree-discovery-note')).toContainText('Discovery temporarily unavailable');await expect(page.locator('#create-submit')).toBeDisabled();
  fail=false;await page.locator('#discover-worktrees').click();await expect(page.locator('#worktree-discovery-note')).toContainText('No worktrees found');
  expect(s.creates).toHaveLength(0);await expect(page.locator('#create-submit')).toBeDisabled();
});
