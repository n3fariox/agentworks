import {test,expect} from '@playwright/test';
import {createServer} from 'node:http';
import {readFile} from 'node:fs/promises';
import {resolve,extname} from 'node:path';

// Static UI only: all API traffic is intercepted, no provider or live data access.
const port=Number(process.env.AGENTWORKS_FORK_UI_PORT||4363),base=`http://127.0.0.1:${port}`;let server;
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
const ordinary={defaultMode:'directory',requireWorktree:false};
const isolated={defaultMode:'newWorktree',requireWorktree:true};
const parent={id:'parent-1',title:'Refactor parser',provider:'demo',cwd:'/repo/app',projectId:'a',status:'idle',created:1,hostId:'local',locationId:'root:/repo'};
const snapshot=()=>({kind:'snapshot',seq:40,session:{...parent,fork:{}},events:[
  {seq:1,kind:'user',data:{text:'Parse the config'}},{seq:2,kind:'acp',data:{sessionUpdate:'agent_message_chunk',content:{type:'text',text:'Parsed.'}}},
  {seq:3,kind:'turn_end',data:{stopReason:'end_turn'}},{seq:4,kind:'user',data:{text:'Add tests'}},{seq:5,kind:'turn_end',data:{stopReason:'end_turn'}},
],permissionModes:{options:[]},models:{currentValue:'demo/steady',options:[{value:'demo/steady',name:'Steady'},{value:'demo/quick',name:'Quick'}]},commands:[],approvals:[]});
async function fixture(page,{policies={a:ordinary,b:isolated},success=true}={}){
  const state={creates:[],forks:[],policies,success};
  await page.addInitScript(id=>localStorage.setItem('agentworks-session',id),parent.id);
  await page.routeWebSocket('**/api/sessions/*/events',()=>{});
  await page.route('**/api/**',async route=>{
    const request=route.request(),path=new URL(request.url()).pathname,method=request.method();
    const body=request.postData()?request.postDataJSON():null;
    const json=data=>route.fulfill({json:data});
    if(path==='/api/status')return json({roots:['/repo'],defaultModels:{demo:'saved-model',codex:'codex-default'},providers:[
      {id:'demo',name:'Demo agent',installed:true},{id:'codex',name:'Codex',installed:true}].map(p=>({...p,authMode:'existing',keyPresent:true}))});
    if(path==='/api/catalog')return json({projects:[{id:'a',name:'Alpha',locationIds:[]},{id:'b',name:'Beta',locationIds:[]}],hosts:[{id:'local',name:'Local',status:'connected'}],locations:[]});
    if(path==='/api/settings')return json({roots:['/repo'],defaultModels:{}});
    if(path==='/api/sessions'&&method==='GET')return json([parent]);
    if(path==='/api/automations')return json([]);
    if(path===`/api/sessions/${parent.id}`)return json(snapshot());
    if(path===`/api/sessions/${parent.id}/workspace`)return json({cwd:parent.cwd,status:'git',branch:'feature/parser',commit:'abc1234',linkedWorktree:true});
    const policy=path.match(/^\/api\/projects\/([^/]+)\/workspace-policy$/);
    if(policy)return json(state.policies[policy[1]]||ordinary);
    if(path==='/api/models')return json({options:[{value:'demo/quick',name:'Quick'}]});
    const fork=path.match(/^\/api\/sessions\/([^/]+)\/fork$/);
    if(fork&&method==='POST'){
      state.forks.push({sessionId:fork[1],body});
      if(!state.success)return route.fulfill({status:409,body:'Previous creation did not complete; inspect retained files'});
      return json({id:'forked',...body,cwd:'/repo/.worktrees/x',status:'starting',locationId:'root:/repo',fork:{parentId:fork[1],boundaryTurn:2}});
    }
    if(path==='/api/sessions'&&method==='POST'){state.creates.push(body);return json({id:'plain',...body,status:'starting',locationId:'root:/repo'});}
    return json({});
  });
  await page.goto(base);await expect(page.locator('#provider option')).toHaveCount(2);
  await expect(page.locator('#header-title')).toHaveText(parent.title);
  return state;
}
async function openFork(page,{mobile=false}={}){
  if(mobile)await page.locator('#session-more').click();
  await page.locator('#lifecycle-menu > summary').click();
  await page.locator('#fork-session').click();
  await expect(page.locator('#create-dialog')).toBeVisible();
  await expect(page.locator('#create-fork-note')).toBeVisible();
}
for(const mobile of [false,true]){
  test(`fork pre-fills from the current session ${mobile?'mobile':'desktop'}`,async({page})=>{
    if(mobile)await page.setViewportSize({width:390,height:844});
    const errors=[];page.on('pageerror',e=>errors.push(e.message));
    const state=await fixture(page);
    await openFork(page,{mobile});
    // Name, agent, model, project and directory all come from the session being forked.
    await expect(page.locator('#create-heading')).toHaveText('Fork session');
    await expect(page.locator('#session-title')).toHaveValue('Refactor parser (fork)');
    await expect(page.locator('#provider')).toHaveValue('demo');
    await expect(page.locator('#create-project')).toHaveValue('a');
    await expect(page.locator('#directory')).toHaveValue('/repo/app');
    await expect(page.locator('#create-model')).toHaveValue('demo/steady');
    await expect(page.locator('#create-model option')).toContainText(['From “Refactor parser” · demo/steady']);
    await expect(page.locator('#create-fork-note')).toContainText('2 finished turns from “Refactor parser”');
    await expect(page.locator('#create-fork-note')).toContainText('not as hidden harness state');
    // A new worktree branched from where this session is now.
    await expect(page.locator('#workspace-mode')).toHaveValue('newWorktree');
    await expect(page.locator('#worktree-base')).toHaveValue('abc1234');
    await page.locator('#create-submit').click();
    await expect.poll(()=>state.forks.length).toBe(1);
    expect(state.forks[0]).toMatchObject({sessionId:parent.id});
    expect(state.forks[0].body).toMatchObject({provider:'demo',title:'Refactor parser (fork)',model:'demo/steady',projectId:'a',cwd:'/repo/app',workspace:{mode:'newWorktree',baseRef:'abc1234'}});
    expect(state.forks[0].body.requestId).toBeTruthy();
    // Forking grants no coordinator powers of its own.
    expect(state.forks[0].body.agentAccess).toEqual({communicate:false,startSessions:false,promptSessions:false,cancelSessions:false,runAutomations:false});
    expect(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBe(true);expect(errors).toEqual([]);
  });
}
test('a fork can move to another agent and a different model',async({page})=>{
  const state=await fixture(page);await openFork(page);
  await page.locator('#provider').selectOption('codex');
  await expect(page.locator('#create-model option')).not.toContainText(['From “Refactor parser”']);
  await page.locator('#load-models').click();await page.locator('#create-model').selectOption('demo/quick');
  await page.locator('#session-title').fill('Port to Codex');
  await page.locator('#create-submit').click();await expect.poll(()=>state.forks.length).toBe(1);
  expect(state.forks[0].body).toMatchObject({provider:'codex',model:'demo/quick',title:'Port to Codex'});
});
test('a fork can reuse the same directory instead of a new worktree',async({page})=>{
  const state=await fixture(page);await openFork(page);
  await page.locator('#workspace-mode').selectOption('directory');
  await page.locator('#create-submit').click();await expect.poll(()=>state.forks.length).toBe(1);
  expect(state.forks[0].body.workspace).toEqual({mode:'directory'});
});
test('a project that requires isolation cannot be forked down to a plain directory',async({page})=>{
  await fixture(page);await openFork(page);
  await page.locator('#create-project').selectOption('b');
  await expect(page.locator('#workspace-mode')).toHaveValue('newWorktree');
  await expect(page.locator('#workspace-mode option[value=directory]')).toBeDisabled();
});
test('a failed fork keeps its request ID and shows the reason',async({page})=>{
  const state=await fixture(page,{success:false});await openFork(page);
  await page.locator('#create-submit').click();await expect(page.locator('#create-error')).toContainText('Previous creation did not complete');
  await page.locator('#create-submit').click();await expect.poll(()=>state.forks.length).toBe(2);
  expect(state.forks[0].body).toEqual(state.forks[1].body);
  await page.locator('#session-title').fill('Different name');
  await page.locator('#create-submit').click();await expect.poll(()=>state.forks.length).toBe(3);
  expect(state.forks[2].body.requestId).not.toBe(state.forks[0].body.requestId);
});
test('an ordinary new session is unaffected by the fork affordance',async({page})=>{
  const state=await fixture(page);
  await page.locator('#lifecycle-menu > summary').click();await page.locator('#fork-session').click();
  await page.locator('#dismiss-create').click();
  await page.locator('#new-session').click();
  await expect(page.locator('#create-heading')).toHaveText('New session');
  await expect(page.locator('#create-fork-note')).toBeHidden();
  await expect(page.locator('#create-import')).toBeVisible();
  await expect(page.locator('#workspace-mode')).toHaveValue('directory');
  await expect(page.locator('#session-title')).toHaveValue('');
  await expect(page.locator('#create-model')).toHaveValue('saved-model');
  await page.locator('#create-submit').click();await expect.poll(()=>state.creates.length).toBe(1);
  expect(state.forks).toHaveLength(0);
  expect(state.creates[0]).toMatchObject({workspace:{mode:'directory'}});
});
test('a late fork reply cannot overwrite a dialog that has moved on',async({page})=>{
  let release;const waiting=new Promise(r=>release=r);
  await fixture(page);
  await page.route('**/api/sessions/parent-1/workspace',async route=>{await waiting;return route.fulfill({json:{commit:'STALE',status:'git'}});});
  await openFork(page);
  await page.locator('#dismiss-create').click();
  await page.locator('#new-session').click();
  await page.locator('#workspace-mode').selectOption('newWorktree');
  await page.locator('#worktree-base').fill('mine');
  release();
  await page.waitForTimeout(200);
  await expect(page.locator('#worktree-base')).toHaveValue('mine');
  await expect(page.locator('#create-fork-note')).toBeHidden();
});
test('the command palette offers forking the selected session',async({page})=>{
  await fixture(page);await page.keyboard.press('Control+k');
  await page.locator('#palette-input').fill('fork');
  await page.locator('#palette-list').getByText('Fork session',{exact:true}).click();
  await expect(page.locator('#create-dialog')).toBeVisible();
  await expect(page.locator('#session-title')).toHaveValue('Refactor parser (fork)');
});
test('select an earlier completed boundary and an explicit summary',async({page})=>{
  const state=await fixture(page);await openFork(page);
  await expect(page.locator('#fork-boundary option')).toHaveCount(3);
  await page.locator('#fork-boundary').selectOption('3');
  await page.locator('#fork-context').selectOption('summary');
  await page.locator('#fork-summary').fill('Keep the parser design; inspect source paths.');
  await page.locator('#create-submit').click();await expect.poll(()=>state.forks.length).toBe(1);
  expect(state.forks[0].body).toMatchObject({boundarySeq:3,context:'summary',summary:'Keep the parser design; inspect source paths.'});
});

test('carried Markdown references retain their original directory',async({page})=>{
  await fixture(page);
  const s=snapshot();s.events=[{seq:1,kind:'acp',data:{sessionUpdate:'agent_message_chunk',forkSourceCwd:'/repo/original',content:{type:'text',text:'[Inspect file](src/config.rs)'}}}];
  await page.routeWebSocket('**/api/sessions/*/events',socket=>socket.send(JSON.stringify(s)));
  await page.route('**/api/markdown',route=>route.fulfill({json:{html:'<p><a href="src/config.rs">Inspect file</a></p>'}}));
  let filePath;
  await page.route('**/api/sessions/*/file?*',route=>{filePath=new URL(route.request().url()).searchParams.get('path');return route.fulfill({body:'source file',contentType:'text/plain'});});
  await page.reload();await page.locator('#messages a').filter({hasText:'Inspect file'}).click();
  await expect.poll(()=>filePath).toBe('/repo/original/src/config.rs');
});
