import {serverReady,stopChild} from './support/cleanup.mjs';
import {test,expect} from '@playwright/test';
import {spawn} from 'node:child_process';
import {mkdtemp,mkdir,rm} from 'node:fs/promises';
import {resolve,join} from 'node:path';
import {DatabaseSync} from 'node:sqlite';

// Older transcript pages behind a bounded snapshot. Isolated server, store and demo state on
// port 4535; demo agent only, no model inference. The store is seeded offline with more stored
// events than the 3000-event snapshot window so the browser has to page backwards.
const base='http://127.0.0.1:4535';
const TURNS=400,PER_TURN=11;
let fixture,server,log='',exited=false,session,seeded;
const db=()=>join(fixture,'sessions.sqlite3');
async function call(path,body){
  return fetch(base+path,{method:body?'POST':'GET',headers:{Origin:base,'Content-Type':'application/json'},body:body?JSON.stringify(body):undefined});
}
async function api(path,body){const r=await call(path,body);expect(r.ok,await r.clone().text()).toBe(true);return r.json();}
async function start(){
  exited=false;
  server=spawn(process.env.AGENTWORKS_TEST_BINARY||resolve('target/debug/agentworks'),[],{env:{...process.env,AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:'4535',AGENTWORKS_ROOTS:fixture,
    AGENTWORKS_CATALOG_PATH:join(fixture,'catalog.json'),AGENTWORKS_DB_PATH:db(),AGENTWORKS_DEMO_STATE_DIR:join(fixture,'demo')},stdio:['ignore','pipe','pipe']});
  server.on('exit',()=>exited=true);for(const s of [server.stdout,server.stderr])s.on('data',c=>{log+=c;});
  for(let i=0;i<100;i++){if(exited)throw new Error(log);if(await serverReady(server,base))return;await new Promise(r=>setTimeout(r,100));}
  throw new Error('readiness timed out: '+log);
}
// Per seeded turn: user, tool_call, 6 output deltas (the last completes it), a reply in two chunks, turn_end.
function turn(i){
  const acp=data=>({kind:'acp',data});
  return [{kind:'user',data:{text:`Prompt ${i}`,requestId:`seed-${i}`}},
    acp({sessionUpdate:'tool_call',toolCallId:`tool-${i}`,title:`Seed tool ${i}`,kind:'execute',status:'in_progress'}),
    ...[1,2,3,4,5,6].map(k=>acp({sessionUpdate:'tool_call_update',toolCallId:`tool-${i}`,...(k===6?{status:'completed'}:{}),_meta:{terminal_output_delta:{terminal_id:`term-${i}`,data:`line ${i}.${k}\n`}}})),
    acp({sessionUpdate:'agent_message_chunk',messageId:`msg-${i}`,content:{type:'text',text:`Reply ${i} `}}),
    acp({sessionUpdate:'agent_message_chunk',messageId:`msg-${i}`,content:{type:'text',text:'done'}}),
    {kind:'turn_end',data:{stopReason:'end_turn'}}];
}
async function idle(){
  for(let i=0;i<150;i++){const s=await api(`/api/sessions/${session.id}`);if(s.session.status==='idle')return s;await new Promise(r=>setTimeout(r,100));}
  throw new Error('session did not become idle');
}
const rows=()=>{const d=new DatabaseSync(db(),{readOnly:true});try{return d.prepare('SELECT COUNT(*) AS n FROM events WHERE session_id=?').get(session.id).n;}finally{d.close();}};

test.describe.configure({mode:'serial'});
test.beforeAll(async()=>{
  await mkdir('.cache',{recursive:true});fixture=await mkdtemp(resolve('.cache/transcript-history-'));await mkdir(join(fixture,'project'));
  await start();
  session=await api('/api/sessions',{provider:'demo',cwd:join(fixture,'project'),title:'Long history'});
  await idle();
  await stopChild(server);
  const d=new DatabaseSync(db());
  try{
    let seq=JSON.parse(d.prepare('SELECT state FROM sessions WHERE id=?').get(session.id).state).seq;
    const insert=d.prepare('INSERT INTO events(session_id,seq,payload) VALUES (?,?,?)');
    d.exec('BEGIN');
    for(let i=1;i<=TURNS;i++)for(const e of turn(i)){seq++;insert.run(session.id,seq,JSON.stringify({seq,...e}));}
    d.prepare("UPDATE sessions SET state=json_set(state,'$.seq',CAST(? AS INTEGER)) WHERE id=?").run(seq,session.id);
    d.exec('COMMIT');seeded=seq;
  }finally{d.close();}
  await start();
});
test.afterAll(async()=>{await stopChild(server);if(fixture)await rm(fixture,{recursive:true,force:true});});

test('API pages older events with an exclusive cursor until the beginning, without gaps or overlap',async()=>{
  const snapshot=await api(`/api/sessions/${session.id}`);
  expect(snapshot.truncated).toBe(true);
  expect(snapshot.events.length).toBe(3000);
  const first=snapshot.events[0].seq;expect(snapshot.events.at(-1).seq).toBeGreaterThanOrEqual(seeded);
  const before=rows();
  let cursor=first,pages=[],page;
  do{
    page=await api(`/api/sessions/${session.id}/history?before=${cursor}`);
    const seqs=page.events.map(e=>e.seq);
    expect(page.before).toBe(cursor);
    expect(seqs.length).toBeGreaterThan(0);
    // Ascending, contiguous and ending immediately before the cursor.
    expect(seqs.at(-1)).toBe(cursor-1);
    expect(seqs).toEqual(seqs.map((_,i)=>seqs[0]+i));
    expect(page.earliestSeq).toBe(seqs[0]);
    pages.push(page);cursor=page.earliestSeq;
  }while(page.hasMore);
  expect(pages.length).toBe(3);
  expect(pages.slice(0,-1).every(p=>p.events.length===500)).toBe(true);
  expect(cursor).toBe(1);
  const all=[...pages.reverse().flatMap(p=>p.events),...snapshot.events].map(e=>e.seq);
  expect(all).toEqual(all.map((_,i)=>i+1));
  // Nothing is earlier than the beginning; the cursor contract still answers explicitly.
  expect(await api(`/api/sessions/${session.id}/history?before=1`)).toEqual({before:1,events:[],hasMore:false,earliestSeq:null});
  const small=await api(`/api/sessions/${session.id}/history?before=${first}&limit=7`);
  expect(small.events.map(e=>e.seq)).toEqual([...Array(7)].map((_,i)=>first-7+i));
  expect(small.hasMore).toBe(true);
  for(const query of ['','?before=0','?before=-3',`?before=${first}&limit=0`,`?before=${first}&limit=2001`,`?before=${first}&after=2`])
    expect((await call(`/api/sessions/${session.id}/history${query}`)).status,query).toBe(400);
  expect((await call(`/api/sessions/missing/history?before=5`)).status).toBe(404);
  expect(rows()).toBe(before);
});

test('API cursor pages stay stable while live events append',async()=>{
  const first=(await api(`/api/sessions/${session.id}`)).events[0].seq;
  const page=await api(`/api/sessions/${session.id}/history?before=${first}`);
  await api(`/api/sessions/${session.id}/resume`,{});await idle();
  expect(await api(`/api/sessions/${session.id}/prompt`,{text:'hello',requestId:'live-api-1'})).toEqual({accepted:true});
  await idle();
  const after=await api(`/api/sessions/${session.id}`);
  // The window slid forward; the old cursor still names exactly the same older page.
  expect(after.events[0].seq).toBeGreaterThan(first);
  expect(await api(`/api/sessions/${session.id}/history?before=${first}`)).toEqual(page);
  const next=await api(`/api/sessions/${session.id}/history?before=${after.events[0].seq}`);
  expect(next.events.at(-1).seq).toBe(after.events[0].seq-1);
});

async function open(page){
  await page.goto(base+'/');await page.evaluate(id=>localStorage.setItem('agentworks-session',id),session.id);
  await page.goto(base+'/');
  await expect(page.locator('#prompt')).toBeEnabled({timeout:15000});
  await expect(page.locator('.history-more')).toBeVisible();
}
// Transcript facts read in one pass: user prompts in order, reply texts, tool outputs.
const facts=page=>page.evaluate(()=>({
  prompts:[...document.querySelectorAll('#messages .message.user .message-body')].map(n=>n.textContent),
  replies:[...document.querySelectorAll('#messages .message.assistant .message-body')].map(n=>n.textContent.trim()),
  tools:[...document.querySelectorAll('#messages .tool-card')].map(n=>({title:n.querySelector('.tool-title')?.textContent,text:n.querySelector('pre').textContent})),
  first:document.querySelector('#messages').firstElementChild?.className,
}));
function checkSeeded(f){
  const seededPrompts=f.prompts.filter(p=>/^Prompt \d+$/.test(p)).map(p=>+p.slice(7));
  // Unique and consecutive up to the last seeded turn: no duplicates, no gaps.
  expect(seededPrompts).toEqual(seededPrompts.map((_,i)=>seededPrompts[0]+i));
  expect(seededPrompts.at(-1)).toBe(TURNS);
  const replies=f.replies.filter(r=>/^Reply \d+/.test(r));
  expect(new Set(replies).size).toBe(replies.length);
  for(const reply of replies)expect(reply).toMatch(/^Reply \d+ done$/);
  const tools=f.tools.filter(t=>/^Seed tool/.test(t.title));
  expect(new Set(tools.map(t=>t.title)).size).toBe(tools.length);
  return {prompts:seededPrompts,replies,tools};
}

test('browser loads multiple older pages, keeps the reading position and ends at the beginning marker',async({page})=>{
  await open(page);
  const messages=page.locator('#messages');
  const initial=checkSeeded(await facts(page));
  expect(initial.prompts[0]).toBeGreaterThan(1);
  await messages.evaluate(n=>n.scrollTo({top:0,behavior:'instant'}));
  // Anchor on a fully rendered prompt near the top and remember where the reader sees it.
  const anchorText=`Prompt ${initial.prompts[1]}`;
  const anchor=page.locator('#messages .message.user .message-body',{hasText:new RegExp(`^${anchorText}$`)});
  const offset=async()=>{const [a,m]=await Promise.all([anchor.boundingBox(),messages.boundingBox()]);return a.y-m.y;};
  const seen=await offset();
  await page.locator('.history-more').click();
  await expect.poll(async()=>(await facts(page)).prompts.length).toBeGreaterThan(initial.prompts.length);
  expect(Math.abs(await offset()-seen)).toBeLessThanOrEqual(2);
  const second=checkSeeded(await facts(page));
  expect(second.prompts[0]).toBeLessThan(initial.prompts[0]);
  await expect(page.locator('.history-more')).toBeVisible();
  let clicks=1;
  while(await page.locator('.history-more').count()){
    await page.locator('.history-more').click();clicks++;
    await expect(page.locator('.history-more').or(page.locator('.history-start'))).toBeVisible();
    await expect(page.locator('.history-more:disabled')).toHaveCount(0);
  }
  expect(clicks).toBe(3);
  await expect(page.locator('.history-start')).toHaveText('Beginning of conversation');
  const done=await facts(page);expect(done.first).toBe('history-boundary');
  const full=checkSeeded(done);
  expect(full.prompts[0]).toBe(1);expect(full.replies.length).toBe(TURNS);expect(full.tools.length).toBe(TURNS);
  // Tool streams that straddled a page boundary are whole after the re-fold.
  for(const [i,tool] of full.tools.entries())expect(tool.text).toContain(`line ${i+1}.1\nline ${i+1}.2\nline ${i+1}.3\nline ${i+1}.4\nline ${i+1}.5\nline ${i+1}.6\n`);
});

test('browser merges older pages with live events and keeps them across a reconnect',async({page})=>{
  const sockets=[];
  await page.routeWebSocket(/\/api\/sessions\/[^/]+\/events/,ws=>{ws.connectToServer();sockets.push(ws);});
  let release;const held=new Promise(r=>release=r);
  await page.route(/\/history\?/,async route=>{await held;await route.continue();});
  await open(page);
  const initial=checkSeeded(await facts(page));
  // A live turn completes while the older page request is in flight.
  await page.locator('.history-more').click();
  await expect(page.locator('.history-more')).toBeDisabled();
  const otherReplies=async()=>(await facts(page)).replies.filter(r=>!/^Reply \d+/.test(r)).length;
  const repliesBefore=await otherReplies();
  await page.fill('#prompt','hello');await page.click('#send');
  await expect(page.locator('#messages .message.user .message-body').last()).toHaveText('hello');
  await expect.poll(otherReplies).toBeGreaterThan(repliesBefore);
  await idle();await expect(page.locator('#session-state')).toHaveText('Ready');
  const liveReplies=await otherReplies();
  release();
  await expect.poll(async()=>checkSeeded(await facts(page)).prompts[0]).toBeLessThan(initial.prompts[0]);
  const merged=await facts(page);
  const seededMerged=checkSeeded(merged);
  expect(seededMerged.prompts[0]).toBeLessThan(initial.prompts[0]);
  expect(merged.prompts.at(-1)).toBe('hello');
  expect(merged.prompts.filter(p=>p==='hello').length).toBe(2);// one from the API test, one live
  expect(merged.replies.filter(r=>!/^Reply \d+/.test(r)).length).toBe(liveReplies);
  // Drop the socket: the reconnect snapshot overlaps what is loaded and must not duplicate or lose it.
  await page.unroute(/\/history\?/);
  expect(sockets.length).toBe(1);
  await sockets[0].close();
  await expect.poll(()=>sockets.length).toBe(2);
  await expect(page.locator('#connection')).toHaveText('Connected');
  await expect.poll(async()=>JSON.stringify(await facts(page))).toBe(JSON.stringify(merged));
  // Paging continues from the merged history's earliest event.
  await page.locator('.history-more').click();
  await expect.poll(async()=>checkSeeded(await facts(page)).prompts[0]).toBeLessThan(seededMerged.prompts[0]);
  const more=checkSeeded(await facts(page));
  expect(more.prompts[0]).toBeLessThan(seededMerged.prompts[0]);
});
