import {test,expect} from '@playwright/test';
import {createServer} from 'node:http';
import {readFile} from 'node:fs/promises';
import {resolve,extname} from 'node:path';
// Static UI only: all API traffic is intercepted.
let server,base;
test.beforeAll(async()=>{
  server=createServer(async(req,res)=>{
    const name=req.url==='/'?'index.html':req.url.slice(1);
    if(!/^[\w.-]+$/.test(name)){res.writeHead(404);res.end();return;}
    try{res.setHeader('Content-Type',({'.js':'text/javascript','.css':'text/css','.html':'text/html'})[extname(name)]||'text/plain');res.end(await readFile(resolve('web',name)));}
    catch{res.writeHead(404);res.end();}
  });
  await new Promise(ok=>server.listen(0,'127.0.0.1',ok));base=`http://127.0.0.1:${server.address().port}`;
});
test.afterAll(async()=>{await new Promise(ok=>server.close(ok));});
const worktree={owned:true,path:'/repo/.worktrees/two',branch:'agentworks/two',removed:false};
const clean={staged:0,unstaged:0,untracked:0,paths:[],truncated:false};
function status({dirty=clean,cleanup,sharedWith=[]}){
  return {owned:true,state:'present',path:worktree.path,branch:worktree.branch,dirty,fingerprint:'fp-1',locked:false,sharedWith,cleanup};
}
async function fixture(page,{archived=false,inspection}){
  const sessions=[{id:'one',title:'Current session',provider:'demo',cwd:'/repo',status:'idle',hostId:'local'},
    {id:'two',title:'Worktree session',provider:'demo',cwd:worktree.path,status:'suspended',hostId:'local',archived,worktree:{...worktree}}];
  const writes=[];
  const snapshot=session=>({kind:'snapshot',session,events:[{seq:1,kind:'user',data:{text:'Hello'}}],approvals:[],commands:[]});
  await page.addInitScript(()=>localStorage.setItem('agentworks-session','one'));
  await page.routeWebSocket('**/api/sessions/*/events',ws=>ws.send(JSON.stringify(snapshot(sessions[0]))));
  await page.route('**/api/**',async route=>{
    const req=route.request(),path=new URL(req.url()).pathname,method=req.method();
    const json=data=>route.fulfill({json:data});
    if(path==='/api/sessions/two/worktree')return json(inspection);
    if(path==='/api/sessions/two/worktree/remove'){
      writes.push({path,body:req.postDataJSON()});sessions[1].worktree.removed=true;
      return json({outcome:'removed',forced:false,path:worktree.path,branch:worktree.branch});
    }
    if(method==='PATCH'){
      const body=req.postDataJSON();writes.push({path,body});Object.assign(sessions[1],{archived:body.archived});
      return json({...sessions[1],worktreeCleanup:{outcome:'removed',path:worktree.path,branch:worktree.branch}});
    }
    if(path==='/api/status')return json({roots:['/repo'],providers:[{id:'demo',name:'Demo agent',installed:true}],defaultModels:{}});
    if(path==='/api/catalog')return json({projects:[],hosts:[{id:'local',name:'Local'}],locations:[]});
    if(path==='/api/sessions')return json(sessions);
    if(path==='/api/automations')return json([]);
    const session=sessions.find(s=>path===`/api/sessions/${s.id}`);if(session)return json(snapshot(session));
    return json({});
  });
  await page.goto(base);await expect(page.locator('#messages')).toContainText('Hello');
  if(archived)await page.locator('#session-filter').selectOption('archived');
  return writes;
}
const row=page=>page.locator('[data-session-id="two"]');
const item=(page,name)=>page.getByRole('menu').getByRole('menuitem',{name,exact:true});
const dialog=page=>page.locator('#worktree-archive-dialog');

test('archiving a worktree shared with another session asks before removing it anyway',async({page})=>{
  const writes=await fixture(page,{inspection:status({sharedWith:[{id:'three',title:'Markdown tables'}],cleanup:{allowed:false,requiresConfirmation:false,overridable:true,reason:'Another session uses this worktree'}})});
  await row(page).click({button:'right'});await item(page,'Archive session').click();
  await expect(dialog(page).getByRole('heading')).toHaveText('Worktree still in use');
  await expect(page.locator('#worktree-archive-users')).toHaveText('Markdown tables');
  await expect(page.locator('#worktree-archive-paths')).toBeHidden();
  const remove=page.locator('#worktree-archive-remove');
  await expect(remove).toHaveText('Archive, remove worktree');await expect(remove).toBeDisabled();
  await page.locator('#worktree-archive-unused').check();await remove.click();
  await expect.poll(()=>writes.length).toBe(1);
  expect(writes[0]).toEqual({path:'/api/sessions/two',body:{archived:true,worktree:'remove',confirmUnused:['three']}});
  await expect(page.locator('#toast')).toContainText('Worktree removed');
});

test('archived sessions offer removal of a retained dirty worktree',async({page})=>{
  const dirty={staged:0,unstaged:2,untracked:1,paths:['a.txt','b.txt','new.txt'],truncated:false};
  const writes=await fixture(page,{archived:true,inspection:status({dirty,cleanup:{allowed:true,requiresConfirmation:true,overridable:false,reason:null}})});
  await row(page).click({button:'right'});await item(page,'Remove worktree…').click();
  await expect(dialog(page).getByRole('heading')).toHaveText('Uncommitted worktree changes');
  await expect(page.locator('#worktree-archive-keep')).toBeHidden();
  await expect(page.locator('#worktree-archive-in-use')).toBeHidden();
  await page.locator('#worktree-archive-remove').click();
  await expect.poll(()=>writes.length).toBe(1);
  expect(writes[0]).toEqual({path:'/api/sessions/two/worktree/remove',body:{confirmDirty:'fp-1'}});
  await expect(page.locator('#toast')).toContainText('branch agentworks/two retained');
  await row(page).click({button:'right'});await expect(item(page,'Remove worktree…')).toHaveCount(0);
});

test('unsafe blockers are reported without offering removal',async({page})=>{
  const writes=await fixture(page,{archived:true,inspection:status({cleanup:{allowed:false,requiresConfirmation:false,overridable:false,reason:'Worktree is locked'}})});
  await row(page).click({button:'right'});await item(page,'Remove worktree…').click();
  await expect(page.locator('#toast')).toHaveText('Worktree not removed: Worktree is locked');
  await expect(dialog(page)).not.toBeVisible();expect(writes).toHaveLength(0);
});
