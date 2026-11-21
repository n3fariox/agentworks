import {test,expect} from '@playwright/test';
import {createServer} from 'node:http';
import {readFile,mkdir} from 'node:fs/promises';
import {resolve,extname} from 'node:path';

// Static application with fixture APIs. No harness, Codex binary or real rollout file.
const base='http://127.0.0.1:4549';let server;
test.beforeAll(async()=>{
  server=createServer(async(req,res)=>{
    const name=req.url==='/'?'index.html':req.url.slice(1);
    if(!/^[\w.-]+$/.test(name)){res.writeHead(404);res.end();return;}
    try{const data=await readFile(resolve('web',name));res.setHeader('Content-Type',({'.js':'text/javascript','.css':'text/css','.html':'text/html'})[extname(name)]||'text/plain');res.end(data);}
    catch{res.writeHead(404);res.end();}
  });
  await new Promise((ok,fail)=>{server.once('error',fail);server.listen(4549,'127.0.0.1',ok);});
});
test.afterAll(async()=>{await new Promise(ok=>server.close(ok));});

const MB=1048576;
async function fixture(page,{provider='codex',running=false}={}){
  const session={id:'codex-one',title:'Long Codex session',provider,cwd:'/repo',status:'suspended',hostId:'local'};
  const state={bytes:241*MB,checkpoints:61,compactedBytes:2.3*MB,backups:[],running};
  const calls=[];
  await page.addInitScript(()=>localStorage.setItem('agentworks-session','codex-one'));
  await page.routeWebSocket('**/api/sessions/*/events',ws=>ws.send(JSON.stringify({kind:'snapshot',seq:1,session,events:[],approvals:[],commands:[],permissionModes:{options:[]},toolProgress:{}})));
  await page.route('**/api/**',async route=>{
    const req=route.request(),path=new URL(req.url()).pathname,json=data=>route.fulfill({json:data});
    if(path==='/api/status')return json({roots:['/repo'],defaultModels:{},providers:[{id:provider,name:'Codex',installed:true}]});
    if(path==='/api/catalog')return json({projects:[],hosts:[{id:'local',name:'Local',status:'connected'}],locations:[]});
    if(path==='/api/sessions')return json([session]);
    if(path==='/api/automations')return json([]);
    if(path===`/api/sessions/${session.id}/codex-transcript`){
      calls.push(req.method());
      if(req.method()==='DELETE')state.backups=[];
      return json({available:true,path:'/home/u/.codex/sessions/rollout-x.jsonl',...state});
    }
    if(path===`/api/sessions/${session.id}/codex-transcript/compact`){
      calls.push('compact');
      const before=state.bytes;Object.assign(state,{bytes:state.compactedBytes,checkpoints:1,backups:[{path:'/b',bytes:before}]});
      return json({available:true,...state,result:{before,after:state.bytes,images:0}});
    }
    if(path===`/api/sessions/${session.id}`)return json({session});
    return json({});
  });
  await page.goto(base);await expect(page.locator('#header-title')).toHaveText('Long Codex session');
  return {calls,state};
}
// Phones keep the session actions inside the session menu.
async function openMenu(page){
  if(await page.locator('#session-more').isVisible())await page.locator('#session-more').click();
  await page.locator('#lifecycle-menu>summary').click();
}

for(const mobile of [false,true])test(`the lifecycle menu shows the Codex transcript size and compacts it ${mobile?'mobile':'desktop'}`,async({page})=>{
  if(mobile)await page.setViewportSize({width:390,height:844});
  const {calls}=await fixture(page);
  await openMenu(page);
  await expect(page.locator('#codex-transcript')).toHaveText('Codex transcript · 241 MB…');
  await page.locator('#codex-transcript').click();
  const dialog=page.locator('#codex-transcript-dialog');
  await expect(dialog).toBeVisible();
  await expect(dialog).toContainText('241 MB on disk · 61 compaction checkpoints.');
  await expect(dialog).toContainText('about 2.3 MB');
  await mkdir('.cache/screenshots',{recursive:true});
  await page.screenshot({path:`.cache/screenshots/codex-transcript-${mobile?'phone':'desktop'}.png`});
  await page.locator('#codex-transcript-compact').click();
  await expect(page.locator('#toast')).toContainText('Codex transcript compacted: 241 MB → 2.3 MB');
  await expect(dialog).toContainText('2.3 MB on disk · 1 compaction checkpoint.');
  await expect(dialog).toContainText('The file already starts at its latest checkpoint.');
  await expect(page.locator('#codex-transcript-compact')).toBeDisabled();
  await expect(page.locator('#codex-transcript-backup')).toContainText('backup (241 MB)');
  await page.locator('#codex-transcript-delete-backup').click();
  await expect(page.locator('#codex-transcript-backup')).toBeHidden();
  await expect(page.locator('#codex-transcript-delete-backup')).toBeHidden();
  expect(calls.filter(c=>c==='compact')).toHaveLength(1);
  expect(calls).toContain('DELETE');
  await page.locator('#codex-transcript-dismiss').click();await expect(dialog).toBeHidden();
  await openMenu(page);await expect(page.locator('#codex-transcript')).toHaveText('Codex transcript · 2.3 MB…');
});

test('a running harness must be suspended first',async({page})=>{
  await fixture(page,{running:true});
  await openMenu(page);await page.locator('#codex-transcript').click();
  await expect(page.locator('#codex-transcript-dialog')).toContainText('Suspend the harness first');
  await expect(page.locator('#codex-transcript-compact')).toBeDisabled();
});

test('other harnesses do not offer it',async({page})=>{
  await fixture(page,{provider:'demo'});
  await openMenu(page);
  await expect(page.locator('#suspend-session')).toBeVisible();
  await expect(page.locator('#codex-transcript')).toBeHidden();
});
