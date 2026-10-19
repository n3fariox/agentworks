import {test,expect} from '@playwright/test';
import {createServer} from 'node:http';
import {readFile} from 'node:fs/promises';
import {resolve,extname} from 'node:path';

// Offline UI fixture: no application server, native sessions, or turns.
const base='http://127.0.0.1:4368';let server;
test.beforeAll(async()=>{
  server=createServer(async(req,res)=>{
    const name=req.url==='/'?'index.html':req.url.slice(1);
    if(!/^[\w.-]+$/.test(name)){res.writeHead(404);res.end();return;}
    try{
      const data=await readFile(resolve('web',name));
      res.setHeader('Content-Type',({'.js':'text/javascript','.css':'text/css','.html':'text/html'})[extname(name)]||'text/plain');res.end(data);
    }catch{res.writeHead(404);res.end();}
  });
  await new Promise((ok,fail)=>{server.once('error',fail);server.listen(4368,'127.0.0.1',ok);});
});
test.afterAll(async()=>{await new Promise(ok=>server.close(ok));});

test('Claude default starts at Auto, saves beside its model, and survives reopening',async({page})=>{
  let settings={roots:['/repo'],defaultModels:{claude:'claude-model',codex:'codex-model',opencode:'free-model'}};
  const writes=[],unexpected=[];
  await page.route('**/api/**',async route=>{
    const request=route.request(),path=new URL(request.url()).pathname;
    const json=data=>route.fulfill({json:data});
    if(path==='/api/status')return json({...settings,providers:['claude','codex','opencode'].map(id=>({id,name:id==='claude'?'Claude':id,installed:true}))});
    if(path==='/api/settings'){
      if(request.method()==='PUT'){settings=request.postDataJSON();writes.push(settings);}
      return json(settings);
    }
    if(path==='/api/catalog')return json({projects:[],locations:[],hosts:[]});
    if(path==='/api/sessions'||path==='/api/automations'){
      if(request.method()!=='GET')unexpected.push(path);
      return json([]);
    }
    return json({});
  });
  await page.goto(base);
  await page.locator('#nav-settings').click();await page.locator('#settings-tab-models').click();
  const mode=page.getByLabel('Claude default permission mode');
  await expect(mode).toHaveValue('auto');
  await expect(page.locator('#default-model-claude')).toHaveValue('claude-model');
  await expect(page.locator('#default-model-fields select')).toHaveCount(1);
  await expect(page.locator('#default-model-fields')).toContainText('does not advertise your choice');
  await expect(mode.locator('option')).toHaveText(['Auto','Default','Accept edits','Plan']);
  await mode.selectOption('plan');
  // Category changes preserve edits, and native validation reveals an invalid hidden root.
  await page.locator('#settings-tab-directories').click();
  await expect(mode).toBeHidden();
  const root=page.getByRole('textbox',{name:'Allowed root directory'});
  await root.fill('');
  await page.locator('#settings-tab-models').click();
  await expect(mode).toHaveValue('plan');
  await page.locator('#save-settings').click();
  await expect(page.locator('#settings-tab-directories')).toHaveAttribute('aria-selected','true');
  await expect(root).toBeFocused();
  expect(writes).toHaveLength(0);
  await root.fill('/repo');
  await page.locator('#settings-tab-directories').focus();
  await page.keyboard.press('ArrowDown');
  await expect(page.locator('#settings-tab-models')).toBeFocused();
  await expect(mode).toBeVisible();
  await page.locator('#save-settings').click();
  await expect(page.locator('#settings-dialog')).not.toBeVisible();
  expect(writes).toHaveLength(1);
  expect(writes[0]).toEqual({roots:['/repo'],defaultModels:{claude:'claude-model',codex:'codex-model',opencode:'free-model'},claudeDefaultPermissionMode:'plan',defaultCommunicationRole:'participant'});
  await page.reload();await page.locator('#nav-settings').click();await page.locator('#settings-tab-models').click();
  await expect(mode).toHaveValue('plan');
  expect(unexpected).toEqual([]);
});
