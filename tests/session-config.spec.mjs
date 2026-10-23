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

async function fixture(page,{providerDelete=true}={}){
  const session={id:'one',title:'Settings test',provider:'codex',cwd:'/repo',status:'idle',hostId:'local'};
  const choices=(id,name,currentValue,values,category)=>({id,name,type:'select',category,currentValue,options:values.map(value=>({value,name:value}))});
  const configOptions=[
    choices('model','Model','a',['a','b'],'model'),choices('mode','Permissions','ask',['ask','code'],'mode'),
    {...choices('reasoning_effort','Reasoning effort','low',[],'thought_level'),options:[{group:'levels',name:'Levels',options:[{value:'low',name:'Low'},{value:'high',name:'High'}]}]},
    choices('collaboration_mode','Plan mode','default',['default','plan']),
    {id:'fast-mode',name:'Fast mode',type:'boolean',currentValue:false},
    choices('future-setting','Future setting','one',['one','two'],'future-category'),
    {id:'future-type',name:'Unsupported',type:'future',currentValue:'x'}
  ];
  let socket,seq=0,reject=false;
  const writes=[],deletes=[];
  const send=(kind,data)=>socket.send(JSON.stringify({seq:++seq,kind,data}));
  const snapshot=()=>({kind:'snapshot',seq,session,events:[],approvals:[],commands:[],sessionConfig:{configOptions},
    providerCapabilities:providerDelete?{close:{},delete:{}}:{},models:{configId:'model',currentValue:'a',options:configOptions[0].options},permissionModes:{configId:'mode',currentValue:'ask',options:configOptions[1].options}});
  await page.addInitScript(()=>localStorage.setItem('agentworks-session','one'));
  await page.routeWebSocket('**/api/sessions/*/events',ws=>{socket=ws;ws.send(JSON.stringify(snapshot()));});
  await page.route('**/api/**',async route=>{
    const req=route.request(),path=new URL(req.url()).pathname;
    const json=data=>route.fulfill({json:data});
    if(path==='/api/status')return json({roots:['/repo'],providers:[{id:'codex',name:'Codex',installed:true}],defaultModels:{}});
    if(path==='/api/catalog')return json({projects:[],hosts:[{id:'local',name:'Local'}],locations:[]});
    if(path==='/api/sessions')return json([session]);
    if(path==='/api/automations')return json([]);
    if(path==='/api/sessions/one'){if(req.method()==='DELETE'){deletes.push(new URL(req.url()).search);return json({ok:true});}return json(snapshot());}
    if(path==='/api/sessions/one/configuration'){
      const body=req.postDataJSON();writes.push(body);
      if(reject)return route.fulfill({status:400,body:'Choice unavailable'});
      send('status',{status:'configuring'});
      configOptions.find(o=>o.id===body.configId).currentValue=body.value;
      send('session_config',{configOptions});send('status',{status:'idle'});
      return json({accepted:true});
    }
    return json({});
  });
  await page.goto(base);await expect(page.locator('#session-state')).toHaveText('Ready');
  if(await page.locator('#session-more').isVisible())await page.locator('#session-more').click();
  await page.locator('#session-config-options>summary').click();
  return {writes,deletes,send,configOptions,reject:()=>{reject=true;}};
}

for(const mobile of [false,true])test(`advertised settings are editable and stay synchronized${mobile?' on mobile':''}`,async({page})=>{
  if(mobile)await page.setViewportSize({width:390,height:844});
  const f=await fixture(page);
  await expect(page.getByLabel('Reasoning effort',{exact:true})).toHaveValue('low');
  await expect(page.locator('#session-config-fields optgroup')).toHaveAttribute('label','Levels');
  await expect(page.locator('#session-config-fields').getByText('Model',{exact:true})).toHaveCount(0);
  await expect(page.getByLabel('Unsupported',{exact:true})).toHaveCount(0);
  await page.getByLabel('Reasoning effort',{exact:true}).selectOption('high');
  await expect.poll(()=>f.writes.length).toBe(1);
  expect(f.writes[0]).toEqual({configId:'reasoning_effort',value:'high'});
  await page.getByLabel('Fast mode',{exact:true}).check();
  await expect.poll(()=>f.writes.length).toBe(2);expect(f.writes[1]).toEqual({configId:'fast-mode',value:true});
  await page.getByLabel('Plan mode',{exact:true}).selectOption('plan');
  await expect.poll(()=>f.writes.length).toBe(3);
  await page.getByLabel('Future setting',{exact:true}).selectOption('two');
  await expect.poll(()=>f.writes.length).toBe(4);
  f.send('status',{status:'running'});
  await expect(page.getByLabel('Reasoning effort',{exact:true})).toBeDisabled();
  await expect(page.getByLabel('Fast mode',{exact:true})).toBeDisabled();
  f.send('status',{status:'idle'});
  f.reject();await page.getByLabel('Reasoning effort',{exact:true}).selectOption('low');
  await expect(page.getByLabel('Reasoning effort',{exact:true})).toHaveValue('high');
  f.configOptions.splice(2,1);f.send('session_config',{configOptions:f.configOptions});
  await expect(page.getByLabel('Reasoning effort',{exact:true})).toHaveCount(0);
  await page.reload();await expect(page.locator('#session-state')).toHaveText('Ready');
  if(await page.locator('#session-more').isVisible())await page.locator('#session-more').click();
  await page.locator('#session-config-options>summary').click();
  await expect(page.getByLabel('Fast mode',{exact:true})).toBeChecked();
  await expect(page.getByLabel('Plan mode',{exact:true})).toHaveValue('plan');
});

test('provider-history deletion is explicit, capability gated, and cancelable',async({page})=>{
  const f=await fixture(page);
  await page.locator('#session-config-options>summary').click();
  await page.locator('#lifecycle-menu>summary').click();
  await expect(page.locator('#delete-provider-history')).toBeVisible();
  page.once('dialog',dialog=>{expect(dialog.message()).toContain('provider’s conversation history');dialog.dismiss();});
  await page.locator('#delete-provider-history').click();expect(f.deletes).toEqual([]);
  // The mobile flyout closes on choosing an action; reopen only when needed.
  if(await page.locator('#session-more').isVisible())await page.locator('#session-more').click();
  page.once('dialog',dialog=>dialog.accept());
  await page.locator('#delete-provider-history').click();
  await expect.poll(()=>f.deletes.length).toBe(1);expect(f.deletes).toEqual(['?providerHistory=true']);
});

test('ordinary delete preserves provider history and unsupported adapters hide delete everywhere',async({page})=>{
  const f=await fixture(page,{providerDelete:false});
  await page.locator('#session-config-options>summary').click();
  await page.locator('#lifecycle-menu>summary').click();
  await expect(page.locator('#delete-provider-history')).toBeHidden();
  page.once('dialog',dialog=>{expect(dialog.message()).toContain('Provider conversation history, worktree files and branches remain');dialog.accept();});
  await page.locator('#close-session').click();
  await expect.poll(()=>f.deletes.length).toBe(1);expect(f.deletes).toEqual(['']);
});
