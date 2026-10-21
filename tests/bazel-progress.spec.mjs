import {test,expect} from '@playwright/test';
import {createServer} from 'node:http';
import {readFile} from 'node:fs/promises';
import {resolve,extname} from 'node:path';
import {events,progressEvents,view} from './fixtures/bazel-helpers.mjs';

// Static application with isolated fixture APIs and WebSockets. No provider, live data or model calls.
// The fixture server publishes the service's golden `tool_progress` events for the U-05 fixture.
const base='http://127.0.0.1:4521';let server;
test.beforeAll(async()=>{
  server=createServer(async(req,res)=>{
    const name=req.url==='/'?'index.html':req.url.slice(1);
    if(!/^[\w.-]+$/.test(name)){res.writeHead(404);res.end();return;}
    try{const data=await readFile(resolve('web',name));res.setHeader('Content-Type',({'.js':'text/javascript','.css':'text/css','.html':'text/html'})[extname(name)]||'text/plain');res.end(data);}
    catch{res.writeHead(404);res.end();}
  });
  await new Promise((ok,fail)=>{server.once('error',fail);server.listen(4521,'127.0.0.1',ok);});
});
test.afterAll(async()=>{await new Promise(ok=>server.close(ok));});
async function fixture(page){
  const sessions=[{id:'bazel-fixture',title:'Bazel fixture',provider:'demo',cwd:'/fixture',status:'running',hostId:'local'},
    {id:'other-fixture',title:'Other fixture',provider:'demo',cwd:'/fixture',status:'running',hostId:'local'}];
  const history=new Map(sessions.map(s=>[s.id,[]]));const published=new Map(sessions.map(s=>[s.id,[]]));const sockets=new Map();
  // Like the service: the latest derived view per tool, rebuilt from the retained history.
  const toolProgress=id=>{
    const last=Math.max(0,...history.get(id).map(e=>e.seq));const views={};
    for(const p of published.get(id))if(p.seq<=last)views[p.data.toolCallId]={...p.data,seq:p.seq};
    return views;
  };
  const snapshot=session=>({kind:'snapshot',seq:100,session,events:history.get(session.id),approvals:[],commands:[],permissionModes:{options:[]},toolProgress:toolProgress(session.id)});
  await page.addInitScript(()=>localStorage.setItem('agentworks-session','bazel-fixture'));
  await page.routeWebSocket('**/api/sessions/*/events',ws=>{
    const session=sessions.find(s=>ws.url().includes('/'+s.id+'/'));
    sockets.set(session.id,ws);ws.send(JSON.stringify(snapshot(session)));
  });
  await page.route('**/api/**',async route=>{
    const path=new URL(route.request().url()).pathname;
    const json=data=>route.fulfill({json:data});
    if(path==='/api/status')return json({roots:['/fixture'],defaultModels:{},providers:[{id:'demo',name:'Demo agent',installed:true}]});
    if(path==='/api/catalog')return json({projects:[],hosts:[{id:'local',name:'Local',status:'connected'}],locations:[]});
    if(path==='/api/sessions')return json(sessions);
    if(path==='/api/automations')return json([]);
    const session=sessions.find(s=>path===`/api/sessions/${s.id}`);if(session)return json(snapshot(session));
    return json({});
  });
  await page.goto(base);await expect(page.locator('#header-title')).toHaveText('Bazel fixture');
  await expect(page.locator('#session-state')).toHaveText('Running');
  const progress=(p,id)=>{published.get(id).push(p);sockets.get(id)?.send(JSON.stringify(p));};
  return {send(event,id='bazel-fixture'){
    history.get(id).push(event);if(event.kind==='status')sessions.find(s=>s.id===id).status=event.data.status;
    sockets.get(id)?.send(JSON.stringify(event));
    if(id==='bazel-fixture'&&events.includes(event))for(const p of progressEvents)if(p.seq===event.seq)progress(p,id);
  },progress(seq,data,id='bazel-fixture'){progress({kind:'tool_progress',seq,data:{toolCallId:'build',...data}},id);},replace(list,id='bazel-fixture'){history.set(id,list);sockets.get(id)?.send(JSON.stringify(snapshot(sessions.find(s=>s.id===id))));}};
}
for(const mobile of [false,true])test(`live progress, raw output, failed result and replay ${mobile?'phone':'desktop'}`,async({page})=>{
  if(mobile)await page.setViewportSize({width:390,height:844});
  const errors=[];page.on('pageerror',e=>errors.push(e.message));
  const f=await fixture(page);
  for(const e of events.slice(0,4))f.send(e);
  const card=page.locator('.tool-card'),activity=card.locator('.tool-activity');
  await expect(card).toHaveCount(1);
  await expect(activity).toContainText('Bazel · Testing · 2 / 6 actions reported · 1 / 3 tests reported');
  await expect(activity).toBeVisible();await expect(activity).toHaveClass(/is-running/);
  await card.locator('summary').click();
  const raw=await card.locator('pre').textContent();
  expect(raw).toContain('\x1b[32mLoading:');expect(raw).toContain('\r\x1b[KAnalyzing:');
  f.send(events[3]);await expect(card).toHaveCount(1);
  f.send(events[4]);await expect(activity).toContainText('3 / 8 actions reported · 2 / 5 tests reported');
  expect(await card.locator('pre').textContent()).not.toContain('Compiling fixture.cc');
  expect(await card.locator('pre').textContent()).toContain('"kept": true');
  f.send(events[5]);await expect(activity).toContainText('4 / 9 actions reported · 3 / 5 tests reported');
  // A successful-looking line cannot settle the call or override the subsequent failure.
  await expect(card.locator('.tool-state')).toHaveText('in_progress');await expect(activity).toHaveClass(/is-running/);
  f.send(events[6]);await expect(card.locator('.tool-state')).toHaveText('failed');await expect(activity).not.toHaveClass(/is-running/);
  await expect(activity).toContainText('Exit 1');
  f.send(events[7]);f.send({kind:'status',seq:9,data:{status:'idle'}});
  await expect(page.locator('.work-log-counts')).toContainText('1 failed');
  await page.reload();await expect(page.locator('.work-log-counts')).toHaveText('1 tool call · 1 failed');
  await page.locator('.work-log-summary').click();
  await expect(activity).toHaveText('Bazel · Testing · 4 / 9 actions reported · 3 / 5 tests reported · Exit 1');
  await card.locator('summary').click();
  const replayRaw=await card.locator('pre').textContent();expect(replayRaw).toContain('failure after progress');expect(replayRaw).toContain('Build completed successfully');
  expect(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBe(true);
  expect(errors).toEqual([]);
  if(mobile)await page.screenshot({path:'.cache/b009-u05-mobile.png'});
});
test('quiet commands show ticking elapsed activity; final-only output makes no streaming claim',async({page})=>{
  await page.clock.install();const f=await fixture(page);
  f.send(events[0]);f.send(events[2]);
  const activity=page.locator('.tool-activity');
  await expect(activity).toHaveText('In progress · 0s');
  await page.clock.fastForward(2100);await expect(activity).toHaveText('In progress · 2s');
  f.send({kind:'acp',seq:4,data:{sessionUpdate:'tool_call_update',toolCallId:'build',status:'completed',content:[{type:'content',content:{type:'text',text:'[2 / 2] fixture\nINFO: Build completed successfully'}}]}});
  f.progress(4,view('Executing',[[2,2,'actions']]));
  await expect(activity).toHaveText('Bazel · Executing · 2 / 2 actions reported · 2s');
  await expect(activity).not.toHaveClass(/is-running/);
});
test('snapshot rebuild and switching sessions isolate same tool IDs and ordinary output',async({page})=>{
  const f=await fixture(page);for(const e of events.slice(0,4))f.send(e);
  await expect(page.locator('.tool-activity')).toContainText('2 / 6 actions reported');
  f.replace([events[0],events[2]]);await expect(page.locator('.tool-activity')).toHaveText('In progress');
  await page.getByRole('button',{name:/Other fixture/}).click();
  await expect(page.locator('#header-title')).toHaveText('Other fixture');await expect(page.locator('.tool-card')).toHaveCount(0);
  f.send({kind:'acp',seq:3,data:{...events[2].data,title:'make fixture',rawInput:{command:'make fixture'}}},'other-fixture');
  f.send(events[3],'other-fixture');await expect(page.locator('.tool-activity')).toContainText('In progress');
  await expect(page.locator('.tool-activity')).not.toContainText('Bazel');
});
test('unfinished calls remain unresolved across a later turn instead of restarting elapsed activity',async({page})=>{
  await page.clock.install();const f=await fixture(page);
  f.send(events[0]);f.send(events[2]);await expect(page.locator('.tool-activity')).toHaveText('In progress · 0s');
  await page.clock.fastForward(2100);
  f.send({kind:'turn_end',seq:4,data:{stopReason:'cancelled'}});
  await expect(page.locator('.tool-activity')).toHaveText('No final status · 2s');
  f.send({kind:'user',seq:5,data:{text:'Next fixture turn'}});
  await page.clock.fastForward(3100);
  await expect(page.locator('.tool-activity')).toHaveText('No final status · 2s');
  await expect(page.locator('.tool-activity')).not.toHaveClass(/is-running/);
});
