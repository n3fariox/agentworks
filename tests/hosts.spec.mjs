import {test,expect} from '@playwright/test';
import {createServer} from 'node:http';
import {readFile} from 'node:fs/promises';
import {resolve,extname} from 'node:path';

// Static UI only: all API traffic is intercepted, no provider or live data access.
const base='http://127.0.0.1:4363';let server;
test.beforeAll(async()=>{
  server=createServer(async(req,res)=>{
    const name=req.url==='/'?'index.html':req.url.slice(1);
    if(!/^[\w.-]+$/.test(name)){res.writeHead(404);res.end();return;}
    try{const data=await readFile(resolve('web',name));res.setHeader('Content-Type',({'.js':'text/javascript','.css':'text/css','.html':'text/html'})[extname(name)]||'text/plain');res.end(data);}
    catch{res.writeHead(404);res.end();}
  });
  await new Promise((ok,fail)=>{server.once('error',fail);server.listen(4363,'127.0.0.1',ok);});
});
test.afterAll(async()=>{await new Promise(ok=>server.close(ok));});

const remote=(id,name,status,extra={})=>({id,name,status,kind:'remote',destination:`dev@${name}`,command:'agentworks',detail:'',since:Date.now(),worker:status==='connected'?{hostname:name,platform:'linux-x86_64',providers:['demo','claude'],version:'0.1.0'}:null,...extra});
async function fixture(page){
  const state={hosts:[{id:'local',name:'Local',status:'connected',kind:'local'},remote('r1','build-box','connected'),remote('r2','old-box','offline',{detail:'ssh exited (exit status: 255). ssh: Permission denied (publickey). Retrying in 8s.'})],requests:[]};
  await page.routeWebSocket('**/api/sessions/*/events',()=>{});
  await page.route('**/api/**',async route=>{
    const request=route.request(),path=new URL(request.url()).pathname,method=request.method();
    const json=data=>route.fulfill({json:data});
    if(path==='/api/status')return json({roots:['/repo'],defaultModels:{},providers:[{id:'demo',name:'Demo agent',installed:true}]});
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
    const host=path.match(/^\/api\/hosts\/([^/]+)$/);
    if(host&&method==='DELETE'){state.hosts=state.hosts.filter(h=>h.id!==host[1]);return json({ok:true});}
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
    await expect(box).toContainText('not supported yet');
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
