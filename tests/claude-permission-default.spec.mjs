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

test('Per-harness sections save models, permission defaults and picker visibility',async({page})=>{
  let settings={roots:['/repo'],defaultModels:{'claude-rust':'claude-model','codex-rust':'codex-model',opencode:'free-model'}};
  const writes=[],unexpected=[];
  await page.route('**/api/**',async route=>{
    const request=route.request(),path=new URL(request.url()).pathname;
    const json=data=>route.fulfill({json:data});
    if(path==='/api/status')return json({...settings,providers:[['demo','Demo agent'],['claude-rust','Claude'],['codex-rust','Codex'],['opencode','OpenCode']].map(([id,name])=>({id,name,installed:true}))});
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
  const section=name=>page.getByRole('region',{name,exact:true});
  await expect(page.locator('#default-model-fields h3')).toHaveText(['Demo agent','Claude','Codex','OpenCode']);
  const mode=section('Claude').getByLabel('Default permission mode');
  await expect(mode).toHaveValue('auto');
  const codexMode=section('Codex').getByLabel('Default sandbox / permission mode');
  await expect(codexMode).toHaveValue('agent-full-access');
  await expect(codexMode.locator('option')).toHaveText(['Full access','Read-only','Workspace access','Auto review']);
  await expect(section('Codex').getByLabel('Default model')).toHaveValue('codex-model');
  await expect(section('Demo agent').getByLabel('Default model')).toHaveCount(0);
  const demoShown=page.getByRole('switch',{name:'Show Demo agent in the new session picker'});
  await expect(demoShown).toBeChecked();
  // Switching a harness off collapses its defaults until it is switched back on.
  const opencodePower=page.getByRole('switch',{name:'Show OpenCode in the new session picker'});
  await opencodePower.click();await expect(section('OpenCode').getByLabel('Default model')).toBeHidden();
  await expect(section('OpenCode')).toContainText('Hidden from new sessions');
  await opencodePower.click();await expect(section('OpenCode').getByLabel('Default model')).toHaveValue('free-model');
  await demoShown.click();await expect(demoShown).not.toBeChecked();
  await expect(page.locator('#default-model-fields')).toContainText('MCP/controller grants or host controls');
  await codexMode.selectOption('workspace-write');
  await expect(section('Claude').getByLabel('Default model')).toHaveValue('claude-model');
  await expect(page.locator('#default-model-fields select')).toHaveCount(2);
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
  await page.keyboard.press('ArrowDown');await page.keyboard.press('ArrowDown');
  await expect(page.locator('#settings-tab-models')).toBeFocused();
  await expect(mode).toBeVisible();
  await page.locator('#save-settings').click();
  await expect(page.locator('#settings-dialog')).not.toBeVisible();
  expect(writes).toHaveLength(1);
  expect(writes[0]).toMatchObject({roots:['/repo'],defaultModels:{'claude-rust':'claude-model','codex-rust':'codex-model',opencode:'free-model'},claudeDefaultPermissionMode:'plan',codexDefaultPermissionMode:'workspace-write',hiddenProviders:['demo'],defaultCommunicationRole:'participant'});
  await page.reload();await page.locator('#nav-settings').click();await page.locator('#settings-tab-models').click();
  await expect(mode).toHaveValue('plan');
  await expect(codexMode).toHaveValue('workspace-write');
  await expect(demoShown).not.toBeChecked();
  expect(unexpected).toEqual([]);
});

test('New session picker hides harnesses and lists the most recently used first',async({page})=>{
  await page.route('**/api/**',route=>{
    const path=new URL(route.request().url()).pathname;
    if(path==='/api/status')return route.fulfill({json:{roots:['/repo'],defaultModels:{},providers:[['demo','Demo agent',true],['claude-rust','Claude',false],['codex-rust','Codex',false],['opencode','OpenCode',false]].map(([id,name,hidden])=>({id,name,hidden,installed:true}))}});
    if(path==='/api/catalog')return route.fulfill({json:{projects:[],locations:[],hosts:[]}});
    if(path==='/api/sessions'||path==='/api/automations'||path==='/api/roles')return route.fulfill({json:[]});
    return route.fulfill({json:{}});
  });
  await page.addInitScript(()=>localStorage.setItem('agentworks-provider-used',JSON.stringify({'claude-rust':1,'codex-rust':2,demo:3})));
  await page.goto(base);
  await page.keyboard.press('n');
  const picker=page.locator('#provider');
  await expect(picker.locator('option')).toHaveText(['Codex','Claude','OpenCode']);
  await expect(picker).toHaveValue('codex-rust');
});
