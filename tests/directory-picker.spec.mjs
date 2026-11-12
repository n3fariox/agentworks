import {test,expect} from '@playwright/test';
import {openCreateSections,override,choosePlace} from './support/create-sections.mjs';
import {createServer} from 'node:http';
import {readFile} from 'node:fs/promises';
import {resolve,extname} from 'node:path';

openCreateSections(test);
// Static UI only: all API traffic is intercepted, including the folder tree.
const port=Number(process.env.AGENTWORKS_DIRECTORY_UI_PORT||4364),base=`http://127.0.0.1:${port}`;let server;
test.beforeAll(async()=>{
  server=createServer(async(req,res)=>{
    const name=req.url==='/'?'index.html':req.url.slice(1);
    if(!/^[\w.-]+$/.test(name)){res.writeHead(404);res.end();return;}
    try{const data=await readFile(resolve('web',name));res.setHeader('Content-Type',({'.js':'text/javascript','.css':'text/css','.html':'text/html'})[extname(name)]||'text/plain');res.end(data);}
    catch{res.writeHead(404);res.end();}
  });
  await new Promise((ok,fail)=>{server.once('error',fail);server.listen(port,'127.0.0.1',ok);});
});
test.afterAll(async()=>{await new Promise(ok=>server.close(ok));});
async function fixture(page,{remembered,sessions=[]}={}){
  const state={
    tree:{'/repo':['existing','other'],'/repo/existing':[],'/repo/other':[]},
    projects:[{id:'a',name:'Alpha',locationIds:['loc-a'],defaultLocations:{local:'loc-a'}},{id:'b',name:'Beta',locationIds:[]}],
    locations:[{id:'loc-a',hostId:'local',path:'/repo/other'}],
    hosts:[{id:'local',name:'Local',status:'connected',kind:'local'},{id:'box',name:'Box',status:'connected',kind:'remote',roots:['~/work'],worker:{providers:['demo']}}],
    creates:[],projectWrites:[],folders:[],browses:[],
  };
  if(remembered)await page.addInitScript(value=>localStorage.setItem('agentworks-project-locations',JSON.stringify(value)),remembered);
  await page.routeWebSocket('**/api/sessions/*/events',()=>{});
  await page.route('**/api/**',async route=>{
    const request=route.request(),url=new URL(request.url()),path=url.pathname,method=request.method();
    const body=request.postData()?request.postDataJSON():null;
    const json=data=>route.fulfill({json:data});
    if(path==='/api/status')return json({roots:['/repo'],defaultModels:{},providers:[{id:'demo',name:'Demo agent',installed:true}]});
    if(path==='/api/catalog')return json({projects:state.projects,hosts:state.hosts,locations:state.locations});
    if(path==='/api/sessions'&&method==='GET')return json(sessions);
    if(path==='/api/automations')return json([]);
    if(path.endsWith('/workspace-policy'))return json({defaultMode:'directory',requireWorktree:false});
    if(path==='/api/directories'&&method==='GET'){
      const host=url.searchParams.get('hostId')||'local',at=url.searchParams.get('path');state.browses.push({host,at});
      if(host!=='local')return route.fulfill({status:409,body:'Update the worker on this host to browse and create folders there.'});
      if(!at)return json({path:null,root:null,parent:null,entries:[{name:'/repo',path:'/repo'}]});
      if(!state.tree[at])return route.fulfill({status:404,body:'No such directory inside the allowed roots'});
      return json({path:at,root:'/repo',parent:at==='/repo'?null:at.slice(0,at.lastIndexOf('/'))||null,entries:state.tree[at].map(name=>({name,path:`${at}/${name}`})),truncated:false});
    }
    if(path==='/api/directories'&&method==='POST'){
      state.folders.push(body);const created=`${body.parent}/${body.name}`;
      if(state.tree[created])return route.fulfill({status:409,body:'A file or folder with that name already exists'});
      state.tree[body.parent].push(body.name);state.tree[created]=[];return json({path:created});
    }
    if(path==='/api/projects'&&method==='POST'){
      state.projectWrites.push({method,body});
      const project={id:'new',name:body.name,locationIds:[]};
      for(const [hostId,path] of Object.entries(body.defaultLocations||{})){const loc={id:'loc-new-'+hostId,hostId,path};state.locations.push(loc);(project.defaultLocations??={})[hostId]=loc.id;project.locationIds.push(loc.id);}
      state.projects.push(project);return json(project);
    }
    if(path.startsWith('/api/projects/')&&method==='PATCH'){
      state.projectWrites.push({method,body});const project=state.projects.find(p=>p.id===path.split('/').at(-1));project.name=body.name;
      for(const [hostId,path] of Object.entries(body.defaultLocations||{})){if(path===null){delete project.defaultLocations?.[hostId];continue;}const loc={id:'loc-'+hostId+'-'+state.locations.length,hostId,path};state.locations.push(loc);(project.defaultLocations??={})[hostId]=loc.id;project.locationIds.push(loc.id);}
      return json(project);
    }
    if(path==='/api/sessions'&&method==='POST'){
      state.creates.push(body);
      const cwd=body.workspace?.mode==='newWorktree'?'/repo/.worktrees/created':body.cwd;
      return json({id:'created-'+state.creates.length,...body,cwd,locationId:'x',status:'idle',createdAt:Date.now()});
    }
    return json({});
  });
  await page.goto(base);await expect(page.locator('#provider option')).toHaveCount(1);
  return state;
}
for(const mobile of [false,true]){
  test(`new project: browse, create a folder, and start sessions there ${mobile?'mobile':'desktop'}`,async({page})=>{
    if(mobile)await page.setViewportSize({width:390,height:844});
    const errors=[];page.on('pageerror',e=>errors.push(e.message));const s=await fixture(page);
    await page.locator('#welcome-new').click();await page.locator('#create-project-new').click();
    await expect(page.locator('#project-directory')).toHaveValue('');
    await page.locator('#project-name').fill('Thing');
    await page.locator('#project-directory-browse').click();
    await expect(page.locator('#directory-picker-dialog')).toBeVisible();
    await expect(page.locator('#directory-picker-select')).toBeDisabled();
    await page.locator('#directory-picker-list button',{hasText:'/repo'}).click();
    await expect(page.locator('#directory-picker-current')).toHaveText('/repo');
    await page.locator('#directory-picker-list button',{hasText:'existing'}).click();
    await expect(page.locator('#directory-picker-crumbs')).toContainText('existing');
    await page.locator('#directory-picker-name').fill('  thing  ');await page.locator('#directory-picker-create').click();
    await expect(page.locator('#directory-picker-current')).toHaveText('/repo/existing/thing');
    expect(s.folders).toEqual([{parent:'/repo/existing',name:'thing'}]);
    // A duplicate name is reported, not silently reused.
    await page.locator('#directory-picker-crumbs button',{hasText:'existing'}).click();
    await expect(page.locator('#directory-picker-current')).toHaveText('/repo/existing');
    await page.locator('#directory-picker-name').fill('thing');await page.locator('#directory-picker-name').press('Enter');
    await expect(page.locator('#directory-picker-error')).toContainText('already exists');
    await page.locator('#directory-picker-list button',{hasText:'thing'}).click();
    if(!mobile)expect(await page.locator('#directory-picker-dialog').evaluate(n=>{const r=n.getBoundingClientRect();return r.left>=0&&r.right<=innerWidth;})).toBe(true);
    await page.locator('#directory-picker-select').click();
    await expect(page.locator('#directory-picker-dialog')).toBeHidden();
    await expect(page.locator('#project-directory')).toHaveValue('/repo/existing/thing');
    await page.locator('#save-project').click();await expect(page.locator('#project-dialog')).toBeHidden();
    expect(s.projectWrites[0].body).toMatchObject({name:'Thing',defaultLocations:{local:'/repo/existing/thing'}});
    await expect(page.locator('#create-project')).toHaveValue('new');
    await expect(page.locator('#directory')).toHaveValue('/repo/existing/thing');
    await page.locator('#create-submit').click();
    expect(s.creates[0]).toMatchObject({cwd:'/repo/existing/thing',projectId:'new',hostId:'local'});
    expect(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBe(true);expect(errors).toEqual([]);
  });
}
test('project directory beats the browser-remembered path; remembered path is the source, not the worktree',async({page})=>{
  const s=await fixture(page,{remembered:{a:'/repo/remembered',b:'/repo/existing'}});
  await page.locator('#welcome-new').click();
  await choosePlace(page,{project:'a'});await expect(page.locator('#directory')).toHaveValue('/repo/other');
  await choosePlace(page,{project:'b'});await expect(page.locator('#directory')).toHaveValue('/repo/existing');
  await page.locator('#workspace-mode').selectOption('newWorktree');await page.locator('#create-submit').click();
  await expect(page.locator('#create-dialog')).toBeHidden();
  expect(s.creates[0]).toMatchObject({cwd:'/repo/existing',projectId:'b',workspace:{mode:'newWorktree'}});
  const stored=await page.evaluate(()=>JSON.parse(localStorage.getItem('agentworks-project-locations')));
  expect(stored.b).toBe('/repo/existing');
});
test('switching projects moves the directory to each project root, never a worktree',async({page})=>{
  // Beta has no saved directory or remembered path; its newest session ran in a worktree.
  const worktree='/repo/existing/.worktrees/old';
  await fixture(page,{sessions:[{id:'s1',provider:'demo',title:'Old',status:'idle',created:1,cwd:worktree,projectId:'b',hostId:'local',worktree:{owned:true,path:worktree,branch:'old',mainTop:'/repo/existing',removed:false}}]});
  await page.locator('#welcome-new').click();
  await choosePlace(page,{project:'a'});await expect(page.locator('#directory')).toHaveValue('/repo/other');
  await page.locator('#directory').fill('/repo/other/deep/subfolder');
  await choosePlace(page,{project:'b'});await expect(page.locator('#directory')).toHaveValue('/repo/existing');
  await choosePlace(page,{project:''});await expect(page.locator('#directory')).toHaveValue('/repo');
  await choosePlace(page,{project:'a'});await expect(page.locator('#directory')).toHaveValue('/repo/other');
});
test('one field picks project and host: used places first, letters-in-order search, Escape keeps the dialog',async({page})=>{
  const s=await fixture(page);
  await page.locator('#welcome-new').click();
  await page.locator('#create-place').click();
  const options=page.locator('#create-place-list .create-place-option');
  // Alpha has a saved directory on Local, so it leads; every other pair is still offered.
  await expect(page.locator('#create-place-list .create-place-group').first()).toHaveText('Used before');
  await expect(options.first()).toHaveText('Alpha · Local');
  await expect(options).toHaveCount(6);
  await page.locator('#create-place').fill('bet bx');
  await expect(options).toHaveText(['Beta · Box']);
  await page.keyboard.press('Enter');
  await expect(page.locator('#create-place')).toHaveValue('Beta · Box');
  await expect(page.locator('#create-project')).toHaveValue('b');await expect(page.locator('#create-host')).toHaveValue('box');
  await expect(page.locator('#directory')).toHaveValue('~/work');
  await page.locator('#create-place').click();await page.keyboard.press('Escape');
  await expect(page.locator('#create-place-list')).toBeHidden();await expect(page.locator('#create-dialog')).toBeVisible();
  await expect(page.locator('#create-place')).toHaveValue('Beta · Box');
});
test('sidebar + prefills the place field from the project, or the project and host',async({page})=>{
  await fixture(page,{sessions:[{id:'s1',provider:'demo',title:'Remote',status:'idle',created:1,cwd:'~/work/beta',projectId:'b',hostId:'box'}]});
  const add=page.locator('button[aria-label^="New session in"]');
  // A project's own + keeps the default host.
  await page.getByRole('button',{name:'New session in Alpha',exact:true}).click();
  await expect(page.locator('#create-place')).toHaveValue('Alpha · Local');await expect(page.locator('#directory')).toHaveValue('/repo/other');
  await page.locator('#dismiss-create').click();
  // A host row under a project carries both; the directory is that project's root there.
  await add.nth(4).click();
  await expect(page.locator('#create-place')).toHaveValue('Beta · Box');
  await expect(page.locator('#create-project')).toHaveValue('b');await expect(page.locator('#create-host')).toHaveValue('box');
  await expect(page.locator('#directory')).toHaveValue('~/work/beta');
  await page.locator('#dismiss-create').click();
  // Typing in the field and leaving the dialog does not leak into the next prefill.
  await add.nth(2).click();await page.locator('#create-place').fill('zzz');await page.locator('#dismiss-create').click();
  await page.getByRole('button',{name:'New session in Alpha',exact:true}).click();
  await expect(page.locator('#create-place')).toHaveValue('Alpha · Local');
});
test('secondary new-session fields start collapsed with their current choice and remember being opened',async({page})=>{
  // Start this tab with nothing remembered instead of the shared all-open state.
  await fixture(page);await page.evaluate(key=>sessionStorage.setItem(key,'{}'),override);await page.reload();
  await page.locator('#welcome-new').click();
  for(const id of ['workspace','model','access'])await expect(page.locator(`#create-section-${id}`)).not.toHaveAttribute('open','');
  await expect(page.locator('#workspace-mode')).toBeHidden();
  await expect(page.locator('#create-section-workspace-value')).toHaveText('Use project directory');
  await expect(page.locator('#create-section-model-value')).toHaveText('Harness default');
  await expect(page.locator('#create-section-access-value')).toContainText('default');
  await page.locator('#create-section-workspace summary').click();
  await page.locator('#workspace-mode').selectOption('newWorktree');await page.locator('#worktree-branch').fill('feat/x');
  await expect(page.locator('#create-section-workspace-value')).toHaveText('Create new worktree · feat/x');
  await page.evaluate(key=>sessionStorage.setItem(key,localStorage.getItem('agentworks-create-sections')),override);
  await page.reload();await page.locator('#welcome-new').click();
  await expect(page.locator('#create-section-workspace')).toHaveAttribute('open','');
  await expect(page.locator('#create-section-model')).not.toHaveAttribute('open','');
});
test('editing a project leaves its directory alone unless changed, and Clear sends null',async({page})=>{
  const s=await fixture(page);
  await page.getByRole('button',{name:'Edit project Alpha',exact:true}).click();
  await expect(page.locator('#project-directory')).toHaveValue('/repo/other');
  await expect(page.locator('#project-directory-host')).toHaveValue('local');
  await page.locator('#project-name').fill('Alpha renamed');await page.locator('#save-project').click();
  await expect(page.locator('#project-dialog')).toBeHidden();
  expect(s.projectWrites[0]).toMatchObject({method:'PATCH'});expect(s.projectWrites[0].body).not.toHaveProperty('defaultLocations');
  // A project without a directory stays without one.
  await page.getByRole('button',{name:'Edit project Beta',exact:true}).click();
  await expect(page.locator('#project-directory')).toHaveValue('');
  await page.locator('#save-project').click();await expect(page.locator('#project-dialog')).toBeHidden();
  expect(s.projectWrites[1].body).not.toHaveProperty('defaultLocations');
  await page.getByRole('button',{name:'Edit project Alpha renamed',exact:true}).click();
  await page.locator('#project-directory-clear').click();
  await expect(page.locator('#project-directory-note')).toContainText('will be cleared');
  await page.locator('#save-project').click();await expect(page.locator('#project-dialog')).toBeHidden();
  expect(s.projectWrites[2].body.defaultLocations).toEqual({local:null});
});
test('each host keeps its own project directory',async({page})=>{
  const s=await fixture(page);
  await page.getByRole('button',{name:'Edit project Alpha',exact:true}).click();
  await expect(page.locator('#project-directory')).toHaveValue('/repo/other');
  // Switching hosts shows that host's directory and keeps the others.
  await page.locator('#project-directory-host').selectOption('box');
  await expect(page.locator('#project-directory')).toHaveValue('');
  await expect(page.locator('#project-directory-note')).toContainText('Also set on: Local');
  await page.locator('#project-directory').fill('~/work/alpha');
  await page.locator('#project-directory-host').selectOption('local');
  await expect(page.locator('#project-directory')).toHaveValue('/repo/other');
  await expect(page.locator('#project-directory-note')).toContainText('Also set on: Box');
  await page.locator('#save-project').click();await expect(page.locator('#project-dialog')).toBeHidden();
  expect(s.projectWrites[0].body.defaultLocations).toEqual({box:'~/work/alpha'});
  // New sessions on each host start in that host's directory.
  await page.locator('#welcome-new').click();
  await choosePlace(page,{project:'a'});await expect(page.locator('#directory')).toHaveValue('/repo/other');
  await choosePlace(page,{project:'a',host:'box'});await expect(page.locator('#directory')).toHaveValue('~/work/alpha');
  await page.locator('#dismiss-create').click();
  // Clearing one host sends only that host.
  await page.getByRole('button',{name:'Edit project Alpha',exact:true}).click();
  await page.locator('#project-directory-host').selectOption('box');
  await expect(page.locator('#project-directory')).toHaveValue('~/work/alpha');
  await page.locator('#project-directory-clear').click();
  await expect(page.locator('#project-directory-note')).toContainText('will be cleared');
  await page.locator('#save-project').click();await expect(page.locator('#project-dialog')).toBeHidden();
  expect(s.projectWrites[1].body.defaultLocations).toEqual({box:null});
});
test('session Browse uses the selected host and shows its limits',async({page})=>{
  const s=await fixture(page);
  await page.locator('#welcome-new').click();await choosePlace(page,{project:'b'});
  await page.locator('#directory').fill('/repo/missing');
  await page.locator('#directory-browse').click();
  // A start path the server rejects falls back to the roots list.
  await expect(page.locator('#directory-picker-list button',{hasText:'/repo'})).toBeVisible();
  await page.locator('#directory-picker-list button',{hasText:'/repo'}).click();
  await page.locator('#directory-picker-list button',{hasText:'other'}).click();
  await page.locator('#directory-picker-select').click();
  await expect(page.locator('#directory')).toHaveValue('/repo/other');
  await choosePlace(page,{host:'box'});
  await page.locator('#directory-browse').click();
  await expect(page.locator('#directory-picker-host')).toContainText('Box');
  await expect(page.locator('#directory-picker-error')).toContainText('Update the worker');
  await page.keyboard.press('Escape');await expect(page.locator('#directory-picker-dialog')).toBeHidden();
  await expect(page.locator('#create-dialog')).toBeVisible();
  expect(s.browses.some(b=>b.host==='box')).toBe(true);
});
test('Windows host paths render and navigate with backslash breadcrumbs',async({page})=>{
  const s=await fixture(page);
  // Canonical answers from a Windows worker: verbatim root, backslash separators.
  const root='\\\\?\\C:\\Users\\me\\projects';
  const tree={[root]:['new thing'],[root+'\\new thing']:['src'],[root+'\\new thing\\src']:[]};
  const seen=[];
  await page.route('**/api/directories**',route=>{
    const url=new URL(route.request().url()),at=url.searchParams.get('path');seen.push(at);
    if(!at)return route.fulfill({json:{path:null,root:null,parent:null,entries:[{name:root,path:root}]}});
    if(!tree[at])return route.fulfill({status:404,body:'No such directory inside the allowed roots'});
    const parent=at===root?null:at.slice(0,at.lastIndexOf('\\'));
    return route.fulfill({json:{path:at,root,parent,entries:tree[at].map(name=>({name,path:`${at}\\${name}`})),truncated:false}});
  });
  await page.locator('#welcome-new').click();
  await page.locator('#directory-browse').click();
  await page.locator('#directory-picker-list button',{hasText:'projects'}).click();
  await page.locator('#directory-picker-list button',{hasText:'new thing'}).click();
  await page.locator('#directory-picker-list button',{hasText:'src'}).click();
  await expect(page.locator('#directory-picker-current')).toHaveText(root+'\\new thing\\src');
  const crumbs=page.locator('#directory-picker-crumbs button');
  await expect(crumbs).toHaveText(['Allowed roots',root,'new thing','src']);
  // A middle crumb navigates to the exact canonical Windows path, not a `/`-joined one.
  await crumbs.nth(2).click();
  await expect(page.locator('#directory-picker-current')).toHaveText(root+'\\new thing');
  expect(seen.at(-1)).toBe(root+'\\new thing');
  // Every Windows path the picker requested used backslashes only (the first request is the dialog's POSIX default start path).
  const windows=seen.filter(p=>p?.startsWith(root));expect(windows.length).toBeGreaterThan(3);expect(windows.filter(p=>p.includes('/'))).toEqual([]);
  await page.locator('#directory-picker-list button',{hasText:'↑ Up'}).click();
  await expect(page.locator('#directory-picker-current')).toHaveText(root);
  await page.locator('#directory-picker-list button',{hasText:'new thing'}).click();
  await page.locator('#directory-picker-select').click();
  await expect(page.locator('#directory')).toHaveValue(root+'\\new thing');
  expect(s.creates).toHaveLength(0);
});
test('breadcrumb paths use the host separator',async({page})=>{
  await page.goto(base);
  const result=await page.evaluate(async()=>{const {breadcrumbs}=await import('./directory-picker.js');return {
    posix:breadcrumbs('/home/captain/projects','/home/captain/projects/thing/src'),
    slashRoot:breadcrumbs('/','/srv/x'),
    drive:breadcrumbs('C:\\','C:\\work\\x'),
    verbatim:breadcrumbs('\\\\?\\C:\\Users\\me','\\\\?\\C:\\Users\\me\\a b\\c'),
    atRoot:breadcrumbs('D:\\code','D:\\code'),
  };});
  expect(result.posix).toEqual([['/home/captain/projects','/home/captain/projects'],['thing','/home/captain/projects/thing'],['src','/home/captain/projects/thing/src']]);
  expect(result.slashRoot).toEqual([['/','/'],['srv','/srv'],['x','/srv/x']]);
  expect(result.drive).toEqual([['C:\\','C:\\'],['work','C:\\work'],['x','C:\\work\\x']]);
  expect(result.verbatim).toEqual([['\\\\?\\C:\\Users\\me','\\\\?\\C:\\Users\\me'],['a b','\\\\?\\C:\\Users\\me\\a b'],['c','\\\\?\\C:\\Users\\me\\a b\\c']]);
  expect(result.atRoot).toEqual([['D:\\code','D:\\code']]);
});
