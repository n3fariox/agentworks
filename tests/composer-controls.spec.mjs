import {test,expect} from '@playwright/test';
import {createServer} from 'node:http';
import {readFile} from 'node:fs/promises';
import {resolve,extname} from 'node:path';
const base='http://127.0.0.1:4523';let server;
test.beforeAll(async()=>{
  server=createServer(async(req,res)=>{
    const name=req.url==='/'?'index.html':req.url.slice(1);
    if(!/^[\w.-]+$/.test(name)){res.writeHead(404);res.end();return;}
    try{const data=await readFile(resolve('web',name));res.setHeader('Content-Type',({'.js':'text/javascript','.css':'text/css','.html':'text/html'})[extname(name)]||'text/plain');res.end(data);}catch{res.writeHead(404);res.end();}
  });await new Promise(ok=>server.listen(4523,'127.0.0.1',ok));
});
test.afterAll(async()=>{await new Promise(ok=>server.close(ok));});
async function fixture(page,{delaySend=false,finishFirst=false}={}){
  const sessions=['one','two'].map(id=>({id,title:id,provider:'demo',cwd:'/fixture',status:'idle',hostId:'local'}));
  const history=new Map(sessions.map(s=>[s.id,[]])),sockets=new Map(),writes=[];let release,seq=0;
  const send=(kind,data,id='one')=>{const event={kind,data,seq:++seq};history.get(id).push(event);if(kind==='status')sessions.find(s=>s.id===id).status=data.status;sockets.get(id)?.send(JSON.stringify(event));return event;};
  const snapshot=s=>({kind:'snapshot',session:s,events:history.get(s.id),approvals:[],commands:[]});
  await page.addInitScript(()=>localStorage.setItem('agentworks-session','one'));
  await page.routeWebSocket('**/api/sessions/*/events',ws=>{const s=sessions.find(s=>ws.url().includes('/'+s.id+'/'));sockets.set(s.id,ws);ws.send(JSON.stringify(snapshot(s)));});
  await page.route('**/api/**',async route=>{
    const req=route.request(),url=new URL(req.url()),path=url.pathname,json=data=>route.fulfill({json:data});
    if(path==='/api/status')return json({roots:['/fixture'],defaultModels:{},providers:[{id:'demo',name:'Demo agent',installed:true}]});
    if(path==='/api/catalog')return json({projects:[],hosts:[{id:'local',name:'Local',status:'connected'}],locations:[]});
    if(path==='/api/sessions')return json(sessions);
    if(path==='/api/automations')return json([]);
    if(path.endsWith('/prompt')){
      const body=req.postDataJSON();writes.push({path,body});send('user',body);send('status',{status:'running'});send('acp',{sessionUpdate:'agent_message_chunk',content:{type:'text',text:'Partial answer'}});
      if(delaySend)await new Promise(ok=>release=ok);return json({accepted:true});
    }
    if(path.endsWith('/cancel')){
      const user=history.get('one').findLast(e=>e.kind==='user');writes.push({path,seq:url.searchParams.get('userSeq')});
      if(finishFirst){send('turn_end',{stopReason:'end_turn'});send('status',{status:'idle'});}else send('status',{status:'cancelling'});
      send('stop_requested',{userSeq:user.seq,...user.data,alreadyFinished:finishFirst,historyRewound:false});return json({ok:true});
    }
    const s=sessions.find(s=>path===`/api/sessions/${s.id}`);if(s)return json(snapshot(s));
    if(path==='/api/markdown')return json({html:'<p>Partial answer</p>'});return json({});
  });
  await page.goto(base);await expect(page.locator('#session-state')).toHaveText('Ready');
  return {send,writes,release:()=>release?.()};
}
const prompt=page=>page.locator('#prompt');
const recovery=page=>page.locator('.notice-card').filter({hasText:'History'});
test('Stop restores exact whitespace, retains partial history and waits for deliberate retry',async({page})=>{
  const f=await fixture(page),text='  Original prompt\n\nwith trailing spaces  \n';
  await prompt(page).fill(text);await page.locator('#send').click();await expect(prompt(page)).toHaveValue('');
  await page.locator('#stop').click();await expect(prompt(page)).toHaveValue(text);await expect(recovery(page)).toContainText('Stop requested');
  await expect(page.locator('#send')).toBeDisabled();await expect(page.locator('#messages')).toContainText('Partial answer');
  f.send('acp',{sessionUpdate:'agent_message_chunk',content:{type:'text',text:' late'}});
  f.send('turn_end',{stopReason:'cancelled'});f.send('status',{status:'idle'});
  await expect(recovery(page)).toContainText('Cancelled.');await expect(prompt(page)).toHaveValue(text);expect(f.writes.filter(w=>w.path.endsWith('/prompt'))).toHaveLength(1);
  await prompt(page).fill('Edited retry');await page.locator('#send').click();expect(f.writes[2].body.requestId).not.toBe(f.writes[0].body.requestId);
});
test('completion winning Stop is explicit; replay does not replace edited draft',async({page})=>{
  await fixture(page,{finishFirst:true});await prompt(page).fill('original');await page.locator('#send').click();await expect(prompt(page)).toHaveValue('');
  await page.locator('#stop').click();await expect(prompt(page)).toHaveValue('original');await expect(recovery(page)).toContainText('completed while Stop');
  await prompt(page).fill('edited draft');await page.reload();await expect(prompt(page)).toHaveValue('edited draft');
});
test('a newer draft is preserved and recovery swaps it without discarding it',async({page})=>{
  const f=await fixture(page);await prompt(page).fill('original');await page.locator('#send').click();await expect(prompt(page)).toHaveValue('');
  await prompt(page).fill('newer draft');await page.locator('#stop').click();await expect(prompt(page)).toHaveValue('newer draft');
  await recovery(page).getByRole('button',{name:/Edit recovered prompt/}).click();await expect(prompt(page)).toHaveValue('original');
  await recovery(page).getByRole('button',{name:/Edit recovered prompt/}).click();await expect(prompt(page)).toHaveValue('newer draft');expect(f.writes).toHaveLength(2);
});
test('late POST acknowledgement cannot clear the restored prompt or reuse its identity',async({page})=>{
  const f=await fixture(page,{delaySend:true});await prompt(page).fill('original');await page.locator('#send').click();await expect(page.locator('#stop')).toBeVisible();
  await page.locator('#stop').click();await expect(recovery(page)).toContainText('Stop requested');f.release();
  await expect(page.locator('#send')).toBeDisabled();await expect(prompt(page)).toHaveValue('original');
  f.send('turn_end',{stopReason:'cancelled'});f.send('status',{status:'idle'});await expect(page.locator('#send')).toBeEnabled();await page.locator('#send').click();
  await expect.poll(()=>f.writes.length).toBe(3);expect(f.writes[2].body.requestId).not.toBe(f.writes[0].body.requestId);f.release();
});
for(const mobile of [false,true])test(`textarea grows, caps, shrinks and respects manual size ${mobile?'phone':'desktop'}`,async({page})=>{
  if(mobile)await page.setViewportSize({width:390,height:844});await fixture(page);
  const height=()=>prompt(page).evaluate(n=>n.getBoundingClientRect().height),initial=await height();
  await prompt(page).fill(Array(8).fill('line').join('\n'));await expect.poll(height).toBeGreaterThan(initial+50);
  await prompt(page).fill(Array(100).fill('line').join('\n'));await expect.poll(height).toBeLessThanOrEqual(mobile?338:322);
  await expect(prompt(page)).toHaveCSS('resize','vertical');expect(await prompt(page).evaluate(n=>n.scrollHeight>n.clientHeight)).toBe(true);
  await prompt(page).fill('short');await expect.poll(height).toBe(initial);
  // Chromium's native resize handle writes inline height. Exercise its observer
  // contract directly since headless platforms expose different drag hitboxes.
  await prompt(page).evaluate(n=>n.style.height='180px');await expect.poll(height).toBe(180);
  await prompt(page).fill('small');await expect.poll(height).toBe(180);
  await prompt(page).fill(Array(30).fill('line').join('\n'));await expect.poll(height).toBeGreaterThan(180);
  await prompt(page).fill('small');await expect.poll(height).toBe(180);
});
test('exact editor text survives mention expansion and a later queued submission',async({page})=>{
  await fixture(page);
  const result=await page.evaluate(async()=>{
    const {setupPromptRecovery}=await import('/input-queue.js');
    const form=document.createElement('form'),area=document.createElement('textarea');form.append(area);document.body.append(form);
    const values=new Map(),storage={getItem:k=>values.get(k),setItem:(k,v)=>values.set(k,v)};
    const recovery=setupPromptRecovery({area,storage,getSession:()=> 'fixture',onChange:()=>{}});
    const draft='  @two\nPlease inspect this  \n';
    recovery.submitted('fixture',{requestId:'active',text:'[expanded peer context]'},draft);
    recovery.submitted('fixture',{requestId:'queued',text:'queued follow-up'},'queued follow-up');
    recovery.consume('fixture',{kind:'user',seq:1,data:{requestId:'active'}});
    recovery.consume('fixture',{kind:'stop_requested',data:{userSeq:1,requestId:'active',text:'[expanded peer context]'}});
    return {value:area.value,recovered:recovery.recovered('fixture','active'),draft};
  });expect(result.value).toBe(result.draft);expect(result.recovered).toBe(true);
});
test('a stale cancellation marker in replay cannot recover a prompt from an older turn',async({page})=>{
  const f=await fixture(page);await prompt(page).fill('old prompt');await page.locator('#send').click();await expect(prompt(page)).toHaveValue('');
  await page.locator('#stop').click();f.send('turn_end',{stopReason:'cancelled'});f.send('status',{status:'idle'});await expect(page.locator('#send')).toBeEnabled();
  await prompt(page).fill('new prompt');await page.locator('#send').click();await expect(prompt(page)).toHaveValue('');
  // A fresh tab has not acknowledged old recovery markers. The later user event
  // still makes the older marker ineligible to restore anything.
  await page.evaluate(()=>{for(const key of Object.keys(sessionStorage))if(/recovered-stops|stop-recovery/.test(key))sessionStorage.removeItem(key);});
  await page.reload();await expect(prompt(page)).toHaveValue('');await expect(recovery(page)).toHaveCount(0);
});
test('cancellation failure leaves the prompt and history available for inspection',async({page})=>{
  const f=await fixture(page);await prompt(page).fill('original');await page.locator('#send').click();await expect(prompt(page)).toHaveValue('');await page.locator('#stop').click();
  f.send('error',{message:'Provider did not stop within grace period'});f.send('status',{status:'error'});
  await expect(prompt(page)).toHaveValue('original');await expect(page.locator('#messages')).toContainText('Partial answer');await expect(recovery(page)).toContainText('ended with an error');
});
test('repeated cancellations preserve previously recovered drafts',async({page})=>{
  const f=await fixture(page);await prompt(page).fill('first prompt');await page.locator('#send').click();await expect(prompt(page)).toHaveValue('');
  await prompt(page).fill('separate draft');await page.locator('#stop').click();f.send('turn_end',{stopReason:'cancelled'});f.send('status',{status:'idle'});await expect(page.locator('#send')).toBeEnabled();
  await prompt(page).fill('second prompt');await page.locator('#send').click();await expect(prompt(page)).toHaveValue('');await page.locator('#stop').click();await expect(prompt(page)).toHaveValue('second prompt');
  await recovery(page).getByRole('button',{name:/Edit recovered prompt/}).click();await expect(prompt(page)).toHaveValue('first prompt');
  await recovery(page).getByRole('button',{name:/Edit recovered prompt/}).click();await expect(prompt(page)).toHaveValue('second prompt');
});
test('native textarea drag retains the chosen height when typing resumes',async({page})=>{
  await fixture(page);const box=await prompt(page).boundingBox();
  await page.mouse.move(box.x+box.width-3,box.y+box.height-3);await page.mouse.down();await page.mouse.move(box.x+box.width-3,box.y+box.height+90,{steps:8});await page.mouse.up();
  const height=await prompt(page).evaluate(n=>n.getBoundingClientRect().height);expect(height).toBeGreaterThan(box.height+40);
  await prompt(page).fill('Writing after a manual drag');await expect.poll(()=>prompt(page).evaluate(n=>n.getBoundingClientRect().height)).toBe(height);
});
