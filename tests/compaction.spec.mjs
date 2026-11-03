import {test,expect} from '@playwright/test';
import {createServer} from 'node:http';
import {readFile} from 'node:fs/promises';
import {resolve,extname} from 'node:path';

// Static application with fixture APIs and WebSocket. No provider, live data or model calls.
const base='http://127.0.0.1:4527';let server;
test.beforeAll(async()=>{
  server=createServer(async(req,res)=>{
    const name=req.url==='/'?'index.html':req.url.slice(1);
    if(!/^[\w.-]+$/.test(name)){res.writeHead(404);res.end();return;}
    try{const data=await readFile(resolve('web',name));res.setHeader('Content-Type',({'.js':'text/javascript','.css':'text/css','.html':'text/html'})[extname(name)]||'text/plain');res.end(data);}
    catch{res.writeHead(404);res.end();}
  });
  await new Promise((ok,fail)=>{server.once('error',fail);server.listen(4527,'127.0.0.1',ok);});
});
test.afterAll(async()=>{await new Promise(ok=>server.close(ok));});

const acp=(seq,data)=>({kind:'acp',seq,data});
const update=(seq,status,extra={})=>acp(seq,{sessionUpdate:'compaction_update',compactionId:'c1',status,...extra});

test('a compaction shows as a divider that fills in, expands to its summary and survives replay',async({page})=>{
  const session={id:'compaction-fixture',title:'Compaction fixture',provider:'demo',cwd:'/fixture',status:'running',hostId:'local'};
  const history=[];let socket;
  const snapshot=()=>({kind:'snapshot',seq:100,session,events:history,approvals:[],commands:[],permissionModes:{options:[]},toolProgress:{}});
  await page.addInitScript(()=>localStorage.setItem('agentworks-session','compaction-fixture'));
  await page.routeWebSocket('**/api/sessions/*/events',ws=>{socket=ws;ws.send(JSON.stringify(snapshot()));});
  await page.route('**/api/**',async route=>{
    const path=new URL(route.request().url()).pathname,json=data=>route.fulfill({json:data});
    if(path==='/api/status')return json({roots:['/fixture'],defaultModels:{},providers:[{id:'demo',name:'Demo agent',installed:true}]});
    if(path==='/api/catalog')return json({projects:[],hosts:[{id:'local',name:'Local',status:'connected'}],locations:[]});
    if(path==='/api/sessions')return json([session]);
    if(path==='/api/automations')return json([]);
    if(path==='/api/markdown')return json({html:`<p>${route.request().postDataJSON().text}</p>`});
    if(path===`/api/sessions/${session.id}`)return json(snapshot());
    return json({});
  });
  const errors=[];page.on('pageerror',e=>errors.push(e.message));
  const send=e=>{history.push(e);socket.send(JSON.stringify(e));};
  await page.goto(base);await expect(page.locator('#header-title')).toHaveText('Compaction fixture');
  send({kind:'user',seq:1,data:{text:'Keep going'}});
  send(acp(2,{sessionUpdate:'tool_call',toolCallId:'t1',title:'Read file',kind:'read',status:'completed'}));
  send(update(3,'in_progress'));
  const card=page.locator('.compaction-card'),label=card.locator('summary');
  await expect(label).toHaveText('Compacting context…');
  send(update(4,'completed',{_meta:{'_claude/compaction':{trigger:'auto',preTokens:243295,postTokens:9575}}}));
  await expect(label).toHaveText('Context compacted · automatic · 243k → 9.6k tokens');
  await label.click();await expect(card).not.toHaveAttribute('open','');
  send(update(5,'completed',{summary:[{type:'text',text:'1. Primary Request: finish the parser'}]}));
  await label.click();await expect(card.locator('.compaction-summary')).toHaveText('1. Primary Request: finish the parser');
  send(acp(6,{sessionUpdate:'agent_message_chunk',content:{type:'text',text:'Continuing after compaction.'}}));
  send({kind:'status',seq:7,data:{status:'idle'}});session.status='idle';
  await expect(page.locator('.message.assistant')).toContainText('Continuing after compaction.');
  await expect(card).toHaveCount(1);
  await page.screenshot({path:'.cache/screenshots/compaction-divider.png'});
  await page.reload();
  await expect(page.locator('.compaction-card summary')).toHaveText('Context compacted · automatic · 243k → 9.6k tokens');
  // Never folded into the turn's work log, and the answer after it stays visible.
  await expect(page.locator('.work-log .compaction-card')).toHaveCount(0);
  await expect(page.locator('.message.assistant')).toBeVisible();
  expect(errors).toEqual([]);
});
