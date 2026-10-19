import {test,expect} from '@playwright/test';
import {createServer} from 'node:http';
import {readFile} from 'node:fs/promises';
import {resolve,extname} from 'node:path';
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
async function fixture(page,{withProjects=false}={}){
  const sessions=[{id:'one',title:'Current session',provider:'demo',cwd:'/repo',status:'idle',hostId:'local'},
    {id:'two',title:'Other session',provider:'demo',cwd:'/repo',status:'idle',hostId:'local',model:'demo/quick'}];
  const projects=withProjects?[{id:"alpha",name:"Alpha"},{id:"beta",name:"Beta"}]:[];
  if(withProjects){sessions[0].projectId="alpha";sessions[1].projectId="beta";}
  const writes=[];
  const snapshot=session=>({kind:'snapshot',session,events:[{seq:1,kind:'user',data:{text:'Original question'}},{seq:2,kind:'acp',data:{sessionUpdate:'agent_message_chunk',content:{type:'text',text:'A useful answer'}}},{seq:3,kind:'turn_end',data:{stopReason:'end_turn'}}],approvals:[],commands:[]});
  await page.addInitScript(()=>localStorage.setItem('agentworks-session','one'));
  await page.routeWebSocket('**/api/sessions/*/events',ws=>ws.send(JSON.stringify(snapshot(sessions[0]))));
  await page.route('**/api/**',async route=>{
    const req=route.request(),path=new URL(req.url()).pathname,method=req.method();
    const json=data=>route.fulfill({json:data});
    if(path.startsWith('/api/projects/')&&(method==='POST'||method==='DELETE')){
      const id=path.split('/')[3],project=projects.find(p=>p.id===id),body=req.postData()?req.postDataJSON():null;
      writes.push({path,method,body,query:new URL(req.url()).search});
      const mode=body?.sessions||new URL(req.url()).searchParams.get('sessions')||'cascade';
      if(mode==='ungroup'){for(const session of sessions)if(session.projectId===id)session.projectId=null;}
      if(method==='DELETE'){
        projects.splice(projects.indexOf(project),1);
        if(mode==='cascade')for(let i=sessions.length-1;i>=0;i--)if(sessions[i].projectId===id)sessions.splice(i,1);
      }else {
        project.archived=body.archived;
        if(mode==='cascade')for(const session of sessions)if(session.projectId===id)session.archived=body.archived;
      }
      return json(project||{});
    }
    if(method==='PATCH'||method==='DELETE'){
      writes.push({path,method,body:req.postDataJSON()});const id=path.split('/').pop(),session=sessions.find(s=>s.id===id);
      if(method==='PATCH')Object.assign(session,req.postDataJSON());else sessions.splice(sessions.indexOf(session),1);
      return json(session||{});
    }
    if(path==='/api/status')return json({roots:['/repo'],providers:[{id:'demo',name:'Demo agent',installed:true}],defaultModels:{}});
    if(path==='/api/catalog')return json({projects,hosts:[{id:'local',name:'Local'}],locations:[]});
    if(path==='/api/sessions')return json(sessions);
    if(path==='/api/automations')return json([]);
    if(path==='/api/markdown')return json({html:'<p>A useful answer</p>'});
    const session=sessions.find(s=>path===`/api/sessions/${s.id}`);if(session)return json(snapshot(session));
    return json({});
  });
  await page.goto(base);await expect(page.locator('#messages')).toContainText('A useful answer');
  return writes;
}
const row=page=>page.locator('[data-session-id="two"]');
const menu=page=>page.getByRole('menu');
test('session menu archives and deletes the pressed session without switching chats',async({page})=>{
  const writes=await fixture(page);
  await row(page).click({button:'right'});await menu(page).getByRole('menuitem',{name:'Archive session',exact:true}).click();
  await expect.poll(()=>writes.length).toBe(1);expect(writes[0]).toMatchObject({path:'/api/sessions/two',method:'PATCH',body:{archived:true}});
  await expect(page.locator('#header-title')).toHaveText('Current session');
  await page.locator('#session-filter').selectOption('archived');await row(page).click({button:'right'});
  await expect(menu(page).getByRole('menuitem',{name:'Unarchive session'})).toBeVisible();
  page.once('dialog',dialog=>dialog.dismiss());await menu(page).getByRole('menuitem',{name:'Delete history…'}).click();expect(writes).toHaveLength(1);
  await row(page).click({button:'right'});page.once('dialog',dialog=>dialog.accept());await menu(page).getByRole('menuitem',{name:'Delete history…'}).click();
  await expect(row(page)).toHaveCount(0);expect(writes[1]).toMatchObject({path:'/api/sessions/two',method:'DELETE'});
  await expect(page.locator('#header-title')).toHaveText('Current session');
});
test('keyboard navigation forks the pressed session',async({page})=>{
  await fixture(page);await row(page).focus();await page.keyboard.press('Shift+F10');
  await expect(menu(page)).toBeVisible();await page.keyboard.press('End');await expect(menu(page).getByRole('menuitem',{name:'Delete history…'})).toBeFocused();
  await page.keyboard.press('Escape');await expect(row(page)).toBeFocused();
  await page.keyboard.press('Shift+F10');await page.keyboard.press('Enter');
  await expect(page.locator('#session-title')).toHaveValue('Other session (fork)');
  await expect(page.locator('#header-title')).toHaveText('Current session');
});
test('long press opens on mobile, suppresses activation, and movement cancels it',async({page})=>{
  await page.setViewportSize({width:390,height:844});await fixture(page);await page.locator('#session-home').click();
  await row(page).dispatchEvent('pointerdown',{pointerType:'touch',isPrimary:true,button:0,pointerId:5,clientX:240,clientY:800});
  await expect(menu(page)).toBeVisible();await row(page).dispatchEvent('pointerup',{pointerType:'touch',pointerId:5});await row(page).dispatchEvent('click');
  await expect(page.locator('#header-title')).toHaveText('Current session');
  const bounds=await menu(page).boundingBox();expect(bounds.x+bounds.width).toBeLessThanOrEqual(390);expect(bounds.y+bounds.height).toBeLessThanOrEqual(844);
  await page.keyboard.press('Escape');
  await row(page).dispatchEvent('pointerdown',{pointerType:'touch',isPrimary:true,button:0,pointerId:6,clientX:50,clientY:200});
  await row(page).dispatchEvent('pointermove',{pointerType:'touch',pointerId:6,clientX:50,clientY:230});
  await page.waitForTimeout(650);await expect(menu(page)).toBeHidden();
});
test('message menu copies source text and quotes while preserving a draft and native text selection',async({page,context})=>{
  await context.grantPermissions(['clipboard-read','clipboard-write']);await fixture(page);
  const message=page.locator('article.message.assistant');
  await message.click({button:'right'});await menu(page).getByRole('menuitem',{name:'Copy message'}).click();
  expect(await page.evaluate(()=>navigator.clipboard.readText())).toBe('A useful answer');
  await page.locator('#prompt').fill('Existing draft');await message.click({button:'right'});await menu(page).getByRole('menuitem',{name:'Quote in reply'}).click();
  await expect(page.locator('#prompt')).toHaveValue('Existing draft\n\n> A useful answer\n\n');await expect(page.locator('#prompt')).toBeFocused();
  await message.evaluate(node=>{const range=document.createRange();range.selectNodeContents(node.querySelector('.message-body'));getSelection().removeAllRanges();getSelection().addRange(range);});
  const intercepted=await message.evaluate(node=>{const event=new MouseEvent('contextmenu',{bubbles:true,cancelable:true,clientX:100,clientY:100});node.dispatchEvent(event);return event.defaultPrevented;});
  expect(intercepted).toBe(false);await expect(menu(page)).toBeHidden();
});

test('touch messages offer quoting, links retain native context menus, and outside press dismisses',async({page})=>{
  await fixture(page);
  const message=page.locator('article.message.assistant');
  await message.dispatchEvent('pointerdown',{pointerType:'touch',isPrimary:true,button:0,pointerId:2,clientX:350,clientY:300});
  await expect(menu(page).getByRole('menuitem',{name:'Quote in reply'})).toBeVisible();
  await message.dispatchEvent('pointerup',{pointerType:'touch',pointerId:2});
  await page.locator('#prompt').click();await expect(menu(page)).toBeHidden();
  const intercepted=await message.evaluate(node=>{
    const link=document.createElement('a');link.href='https://example.com';link.textContent='Example';node.querySelector('.message-body').append(link);
    const event=new MouseEvent('contextmenu',{bubbles:true,cancelable:true});link.dispatchEvent(event);return event.defaultPrevented;
  });
  expect(intercepted).toBe(false);await expect(menu(page)).toBeHidden();
});

const projectHeading=page=>page.locator('.tree-heading[data-project-id="beta"]');
test('project context menu confirms archive, supports cancellation and unarchive',async({page})=>{
  const writes=await fixture(page,{withProjects:true});
  await projectHeading(page).click({button:'right'});
  await menu(page).getByRole('menuitem',{name:'Archive project…',exact:true}).click();
  await expect(page.locator('#project-remove-dialog')).toBeVisible();
  await expect(page.locator('#project-remove-title')).toHaveText('Archive Beta');
  await expect(page.locator('#project-remove-description')).toContainText('archives its 1 session');expect(writes).toHaveLength(0);
  await page.keyboard.press('Escape');expect(writes).toHaveLength(0);
  await projectHeading(page).click({button:'right'});await menu(page).getByRole('menuitem',{name:'Archive project…',exact:true}).click();
  await page.locator('#confirm-project-remove').click();
  await expect(projectHeading(page)).toHaveCount(0);
  expect(writes).toEqual([{path:'/api/projects/beta/archive',method:'POST',body:{archived:true,sessions:'cascade'},query:''}]);
  await expect(page.locator('#header-title')).toHaveText('Current session');
  await page.locator('#session-filter').selectOption('archived');await projectHeading(page).click({button:'right'});
  await menu(page).getByRole('menuitem',{name:'Unarchive project',exact:true}).click();
  await expect(projectHeading(page)).toHaveCount(0);expect(writes[1].body).toEqual({archived:false});
});
test('project delete requires confirmation, retries errors and can keep sessions in Ungrouped',async({page})=>{
  const writes=await fixture(page,{withProjects:true});
  let fail=true;
  await page.route('**/api/projects/beta?*',route=>{if(fail){fail=false;return route.fulfill({status:500,body:'Could not delete project'});}return route.fallback();});
  await projectHeading(page).focus();await page.keyboard.press('Shift+F10');await page.keyboard.press('End');await page.keyboard.press('Enter');
  await expect(page.locator('#project-remove-title')).toHaveText('Delete Beta');
  await expect(page.locator('#project-remove-description')).toContainText('saved history of its 1 session');expect(writes).toHaveLength(0);
  await page.locator('#confirm-project-remove').click();
  await expect(page.locator('#project-remove-error')).toContainText('Could not delete project');await expect(page.locator('#confirm-project-remove')).toBeEnabled();
  await page.locator('#project-remove-ungroup').check();
  await expect(page.locator('#project-remove-description')).toContainText('move to Ungrouped and keep running');
  await page.locator('#confirm-project-remove').click();await expect(projectHeading(page)).toHaveCount(0);
  expect(writes).toEqual([{path:'/api/projects/beta',method:'DELETE',body:null,query:'?sessions=ungroup'}]);
  await expect(row(page)).toBeVisible();await expect(page.locator('#header-title')).toHaveText('Current session');
});
test('project long press works in host view without toggling the group; edit uses the same confirmations',async({page})=>{
  await page.setViewportSize({width:390,height:844});const writes=await fixture(page,{withProjects:true});await page.locator('#session-home').click();
  await page.locator('#view-hosts').click();
  const heading=projectHeading(page),wasOpen=await heading.evaluate(node=>node.parentElement.open);
  await heading.dispatchEvent('pointerdown',{pointerType:'touch',isPrimary:true,button:0,pointerId:9,clientX:100,clientY:300});
  await expect(menu(page)).toBeVisible();await heading.dispatchEvent('pointerup',{pointerType:'touch',pointerId:9});await heading.dispatchEvent('click');
  expect(await heading.evaluate(node=>node.parentElement.open)).toBe(wasOpen);
  await menu(page).getByRole('menuitem',{name:'Edit project…'}).click();await expect(page.locator('#project-name')).toHaveValue('Beta');
  await page.locator('#delete-project').click();await expect(page.locator('#project-remove-title')).toHaveText('Delete Beta');expect(writes).toHaveLength(0);
  await page.locator('#dismiss-project-remove').click();await expect(page.locator('#project-dialog')).toBeVisible();
  await page.locator('#archive-project').click();await expect(page.locator('#project-remove-title')).toHaveText('Archive Beta');expect(writes).toHaveLength(0);
});
