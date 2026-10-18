import {test,expect} from '@playwright/test';
import {spawn} from 'node:child_process';
import {mkdir,rm} from 'node:fs/promises';
import {resolve,join} from 'node:path';
import {fuzzyScore,rankItems,sessionItems,sessionRanking,actionItems,commandText,statusLabel} from '../web/palettes.js';

// Own server, SQLite store, catalog, demo state and roots: never the live service or the shared verify port.
const base='http://127.0.0.1:4431';
let fixture,server,log='',exited=false;
const alpha=()=>join(fixture,'alpha'),zeta=()=>join(fixture,'zeta'),omega=()=>join(fixture,'omega');

async function api(path,body,method){
  const response=await fetch(base+path,{method:method||(body?'POST':'GET'),headers:{Origin:base,'Content-Type':'application/json'},body:body?JSON.stringify(body):undefined});
  expect(response.ok,await response.clone().text()).toBe(true);
  return response.status===204?null:response.json();
}

test.beforeAll(async()=>{
  await mkdir('.cache',{recursive:true});
  // A fixed fixture name keeps directory matching deterministic; nothing else uses this server or path.
  fixture=resolve('.cache','palette-fixture');
  await rm(fixture,{recursive:true,force:true});
  await mkdir(fixture,{recursive:true});
  for(const dir of [alpha(),zeta(),omega()])await mkdir(dir,{recursive:true});
  server=spawn(resolve('target/debug/agentworks'),[],{env:{...process.env,
    AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:'4431',AGENTWORKS_ROOTS:fixture,
    AGENTWORKS_CATALOG_PATH:join(fixture,'catalog.json'),AGENTWORKS_DB_PATH:join(fixture,'sessions.sqlite3'),
    AGENTWORKS_DEMO_STATE_DIR:join(fixture,'demo')},stdio:['ignore','pipe','pipe']});
  server.on('error',e=>{log+=e.message;exited=true;});
  server.on('exit',()=>exited=true);
  for(const stream of [server.stdout,server.stderr])stream.on('data',chunk=>{log+=chunk.toString();});
  for(let i=0;i<100;i++){
    if(exited)throw new Error(log);
    try{if((await fetch(base+'/api/status')).ok)return;}catch{}
    await new Promise(r=>setTimeout(r,100));
  }
  throw new Error('Isolated palette server readiness timed out: '+log);
});
test.afterAll(async()=>{
  if(server&&!exited){const stopped=new Promise(r=>server.once('exit',r));server.kill('SIGTERM');await stopped;}
  if(fixture)await rm(fixture,{recursive:true,force:true});
});

test('fuzzy scoring prefers word starts, consecutive runs and tight matches',()=>{
  expect(fuzzyScore('', 'anything')).toEqual({score:0,positions:[]});
  expect(fuzzyScore('zz', 'New session')).toBeNull();
  expect(fuzzyScore('abc', 'ab')).toBeNull();
  // Every match reports positions whose characters spell the term, so highlighting cannot drift from scoring.
  for(const [term,text] of [['help','/help'],['help','Show help'],['alp','Alpha chat'],['om','/home/u/omega'],['demo','$demo-skill']]){
    const result=fuzzyScore(term,text);
    expect(result.positions.map(i=>text[i]).join('').toLowerCase(),`${term} in ${text}`).toBe(term);
  }
  const boundary=fuzzyScore('p','Project files').score,midWord=fuzzyScore('p','Stop turn').score;
  expect(boundary).toBeGreaterThan(midWord);
  const tight=fuzzyScore('alp','Alpha chat').score,scattered=fuzzyScore('alp','A lazy penguin').score;
  expect(tight).toBeGreaterThan(scattered);
  expect(fuzzyScore('files','Project files').score).toBeGreaterThan(fuzzyScore('f','Project files').score);
});

test('oversized queries and fields are bounded before allocating the matching table',()=>{
  expect(fuzzyScore('a'.repeat(129),'a'.repeat(200))).toBeNull();
  expect(fuzzyScore('a','a'.repeat(9000))).toBeNull();
  expect(rankItems('a'.repeat(129),[{label:'a'.repeat(200)}])).toEqual([]);
});

// Independent reference: enumerate every increasing alignment and score it with the documented bonuses, so a
// strong partial prefix can never be mistaken for the best complete match.
const SCORE_RULES={start:12,word:7,consecutive:7,gap:.4};
const BOUNDARY=/[\s/\\\-_.:,;·()[\]{}@#]/;
function exhaustiveScore(term,text){
  const letters=[...term];
  let best=null;
  const walk=(index,previous,value,streak,positions)=>{
    if(index===letters.length){
      const gaps=Math.max(0,positions.at(-1)-positions[0]-(letters.length-1));
      const total=value-SCORE_RULES.gap*gaps;
      if(!best||total>best.score)best={score:total,positions:[...positions]};
      return;
    }
    for(let at=previous+1;at<text.length;at++){
      if(text[at].toLowerCase()!==letters[index].toLowerCase())continue;
      const bonus=at===0?SCORE_RULES.start:(BOUNDARY.test(text[at-1])||(/[a-z0-9]/.test(text[at-1]??'')&&/[A-Z]/.test(text[at]??'')))?SCORE_RULES.word:0;
      const continued=streak>0&&at===previous+1; // one step of the consecutive bonus, matching the module
      positions.push(at);
      walk(index+1,at,value+bonus+(continued?SCORE_RULES.consecutive:0),continued?streak+1:1,positions);
      positions.pop();
    }
  };
  walk(0,-1,0,0,[]);
  return best;
}
function words(lengths,alphabet){
  const all=[''];
  for(const length of lengths)all.push(...expand('',length,alphabet));
  return all;
  function expand(prefix,remaining,alphabet){
    if(!remaining)return [prefix];
    return alphabet.flatMap(character=>expand(prefix+character,remaining-1,alphabet));
  }
}

test('scoring agrees with an exhaustive search of every alignment',()=>{
  const corpora=[
    {alphabet:['a','b',' '],texts:[1,2,3,4],terms:[1,2,3]},
    {alphabet:['a','b','c','/','-'],texts:[1,2,3],terms:[1,2]},
    {alphabet:['a','B','/','-',' '],texts:[1,2,3],terms:[1,2]},
  ];
  const problems=[];
  let checked=0;
  for(const corpus of corpora){
    for(const text of words(corpus.texts,corpus.alphabet))for(const term of words(corpus.terms,corpus.alphabet).slice(1)){
      checked++;
      const mine=fuzzyScore(term,text),reference=exhaustiveScore(term,text);
      const label=`${JSON.stringify(term)} in ${JSON.stringify(text)}`;
      if(!reference){if(mine)problems.push(`${label}: scored ${mine.score} with no alignment`);continue;}
      if(!mine){problems.push(`${label}: no alignment for a complete match`);continue;}
      if(Math.abs(mine.score-reference.score)>1e-6)problems.push(`${label}: ${mine.score} instead of ${reference.score}`);
      // The reported alignment is a real, strictly increasing path that spells the whole term.
      const spelled=mine.positions.map(index=>text[index]).join('').toLowerCase();
      if(spelled!==term.toLowerCase())problems.push(`${label}: positions spell ${JSON.stringify(spelled)}`);
      for(let i=1;i<mine.positions.length;i++)if(mine.positions[i]<=mine.positions[i-1])problems.push(`${label}: positions out of order`);
    }
  }
  expect(checked).toBe(14079); // corpus size, so a silently empty sweep cannot pass
  expect(problems.slice(0,5)).toEqual([]);
});

test('partial prefixes and near misses never pass as complete matches',()=>{
  for(const [term,text] of [['abc','a b c'],['abc','xxaxxbxxc'],['abc','abcabc'],['aa','aaaa'],['ba','ab'],['abc','ab'],['zz','aa'],['abc','acb'],['help','Show help elsewhere'],['alp','Alpha']]){
    const label=`${JSON.stringify(term)} in ${JSON.stringify(text)}`,result=fuzzyScore(term,text);
    if(result){
      expect(result.positions.length,label).toBe(term.length);
      expect(result.positions.map(index=>text[index]).join('').toLowerCase(),label).toBe(term.toLowerCase());
    }
  }
  expect(fuzzyScore('abc','acb')).toBeNull();
  expect(fuzzyScore('abc','ab')).toBeNull();
  expect(fuzzyScore('zz','aa')).toBeNull();
  expect(fuzzyScore('','')).toEqual({score:0,positions:[]});
  // A complete match late in the string still outranks an early partial prefix, with valid positions.
  expect(fuzzyScore('abc','ababc').positions.at(-1)).toBe(4);
  expect(fuzzyScore('abc','ababc').positions.map(index=>'ababc'[index]).join('')).toBe('abc');
  // Tightness counts: contiguous, then word-start, then scattered.
  expect(fuzzyScore('abc','abc').score).toBeGreaterThan(fuzzyScore('abc','a bc').score);
  expect(fuzzyScore('abc','a bc').score).toBeGreaterThan(fuzzyScore('abc','a x x b c').score);
  expect(fuzzyScore('abc','abc').score).toBeGreaterThan(fuzzyScore('abc','zzabc').score);
  // A word start still beats the same letter inside a word, and camelCase starts count as word starts.
  expect(fuzzyScore('p','Project').score).toBeGreaterThan(fuzzyScore('p','Stop').score);
  expect(fuzzyScore('b','aB').score).toBeGreaterThan(fuzzyScore('b','ab').score);
});

test('ranking keeps every term, weights the title and leaves unfiltered order alone',()=>{
  const items=[{id:'a',label:'Alpha chat',search:'alpha demo agentworks'},{id:'b',label:'Beta chat',search:'beta demo'},{id:'c',label:'Alpha notes',search:'alpha demo'}];
  expect(rankItems('',items).map(i=>i.id)).toEqual(['a','b','c']);
  expect(rankItems('beta',items).map(i=>i.id)).toEqual(['b']);
  // Every whitespace-separated term must match somewhere, so two words narrow instead of widening.
  expect(rankItems('alpha demo',items).map(i=>i.id)).toEqual(['a','c']);
  expect(rankItems('alpha beta',items)).toEqual([]);
  expect(rankItems('agentworks',items).map(i=>i.id)).toEqual(['a']);
});

test('session entries carry title, project, host, directory and status context',()=>{
  const sessions=[
    {id:'one',title:'Alpha chat',provider:'demo',status:'idle',cwd:'/srv/projects/alpha',projectId:'p1',hostId:'local'},
    {id:'two',title:'Beta chat',provider:'codex',status:'running',cwd:'/srv/work/zeta',projectId:'p2',hostId:'local'},
    {id:'three',title:'Beta chat',provider:'demo',status:'idle',cwd:'/srv/work/omega',projectId:'p2',hostId:'local'},
  ];
  const context={labels:{demo:'Demo agent',codex:'Codex'},hosts:[{id:'local',name:'Local'}],projects:[{id:'p1',name:'Palette project'},{id:'p2',name:'Second project'}],activeId:'two'};
  const items=sessionItems(sessions,context);
  // Running work is offered first, and the active session is marked rather than hidden.
  expect(items.map(i=>i.id)).toEqual(['two','one','three']);
  expect(items[0]).toMatchObject({current:true,group:'Running turn'});
  expect(items[0].detail).toBe('Current · Codex · Second project · Local · srv/work/zeta');
  // Equal titles keep more of the path so the two Beta rows stay distinguishable.
  expect(items[1].detail).toBe('Demo agent · Palette project · Local · alpha');
  expect(items[2].detail).toBe('Demo agent · Second project · Local · srv/work/omega');
  expect(items[1].group).toBe('Ready');
  expect(items[2].hint).toBe('/srv/work/omega');
  expect(sessionRanking('omega',items).map(i=>i.id)).toEqual(['three']);
  // Project members rank above a session that merely matches the same word inside its path.
  expect(sessionRanking('second',items).map(i=>i.id)).toEqual(['two','three','one']);
  expect(sessionRanking('beta omega',items).map(i=>i.id)).toEqual(['three']);
  expect(sessionRanking('alpha chat',items).map(i=>i.id)).toEqual(['one']);
  // The whole directory stays searchable, at a lower weight than the tail, project or title.
  expect(sessionRanking('srv',items).map(i=>i.id).sort()).toEqual(['one','three','two']);
  expect(sessionRanking('srv work',items).map(i=>i.id)).toEqual(['two','three']);
  expect(sessionRanking('srv omega',items).map(i=>i.id)).toEqual(['three']);
  expect(sessionRanking('projects',items).map(i=>i.id)).toEqual(['one','two','three']);
  expect(sessionRanking('zeta codex',items).map(i=>i.id)).toEqual(['two']);
  expect(sessionRanking('nothing-here',items)).toEqual([]);
  expect(statusLabel('waiting')).toBe('Waiting for approval');
  expect(sessionItems([],context)).toEqual([]);
});

test('actions expose existing UI work and refuse commands until the session is ready',()=>{
  const called=[];
  const handlers={newSession:()=>called.push('new'),insertCommand:t=>called.push(t),stop:()=>called.push('stop')};
  const advertised=[{name:'help',description:'Show demo help'},{name:'$demo-skill',description:'Example skill entry'}];
  const idle=actionItems({active:{id:'a',provider:'demo',title:'Alpha'},status:'idle',connected:true,commands:advertised,views:{files:()=>{}}},handlers);
  const byId=Object.fromEntries(idle.map(item=>[item.id,item]));
  expect(byId['command:/help'].run()).toBe(1);
  expect(called).toEqual(['/help']);
  expect(byId['command:/help']).toMatchObject({group:'Harness commands',disabled:false});
  expect(byId['command:$demo-skill'].hint).toBe('Example skill entry');
  expect(byId['session-stop']).toMatchObject({disabled:true,reason:'No turn is running'});
  expect(byId['session-usage']).toBeUndefined();
  // Actions never appear for a session the browser has not selected.
  const none=actionItems({status:'starting',connected:false,commands:advertised},handlers);
  expect(none.map(i=>i.id)).toEqual(['new-session','import-session','new-project','settings','command:/help','command:$demo-skill']);
  expect(none.find(i=>i.id==='command:/help')).toMatchObject({disabled:true,reason:'No session selected'});
  const running=actionItems({active:{id:'a',provider:'demo'},status:'running',connected:true,commands:advertised},handlers);
  const runningById=Object.fromEntries(running.map(item=>[item.id,item]));
  expect(runningById['session-stop'].disabled).toBe(false);
  runningById['session-stop'].run();expect(called).toContain('stop');
  expect(runningById['command:/help']).toMatchObject({disabled:true,reason:'Available when the session is ready'});
  const cancelling=actionItems({active:{id:'a'},status:'cancelling',connected:true},handlers);
  expect(cancelling.find(i=>i.id==='session-stop')).toMatchObject({disabled:true,reason:'Already cancelling'});
  const offline=actionItems({active:{id:'a'},status:'running',connected:false},handlers);
  expect(offline.find(i=>i.id==='session-stop')).toMatchObject({disabled:true,reason:'Not connected to this session yet'});
  const suspended=actionItems({active:{id:'a'},status:'suspended',connected:true},handlers);
  expect(suspended.map(i=>i.id)).toContain('session-resume');
  expect(actionItems({active:{id:'a'},status:'suspended',connected:false},handlers).find(i=>i.id==='session-resume')).toMatchObject({disabled:true,reason:'Not connected to this session yet'});
  const usage=actionItems({active:{id:'a',provider:'claude'},status:'idle',connected:true,usageAvailable:true},handlers);
  expect(usage.map(i=>i.id)).toContain('session-usage');
  expect(commandText({name:'\$demo-skill'})).toBe('$demo-skill');
  expect(commandText({name:'/usage'})).toBe('/usage');
});

async function seedSessions(){
  const project=await api('/api/projects',{name:'Palette project'});
  const second=await api('/api/projects',{name:'Second project'});
  const create=(title,cwd,projectId)=>api('/api/sessions',{provider:'demo',cwd,title,projectId,hostId:'local'});
  return {project,second,sessions:[await create('Alpha chat',alpha(),project.id),await create('Beta chat',zeta(),second.id),await create('Beta chat',omega(),second.id)]};
}

test('desktop: shortcuts, fuzzy session switching, drafts, running turns and composer-only commands',async({page})=>{
  const {project,second,sessions:[one,two,three]}=await seedSessions();
  const errors=[],prompts=[];
  page.on('pageerror',e=>errors.push(e.message));
  page.on('request',request=>{if(/\/prompt$/.test(new URL(request.url()).pathname))prompts.push(request.method());});
  await page.addInitScript(id=>localStorage.setItem('agentworks-session',id),one.id);
  await page.goto(base);
  await expect(page.locator('#session-state')).toHaveText('Ready');

  // Ctrl+K opens the action palette with a usable listbox and active option.
  await page.keyboard.press('Control+k');
  await expect(page.locator('#palette-dialog')).toBeVisible();
  await expect(page.locator('#palette-title')).toHaveText('Command palette');
  await expect(page.locator('#palette-input')).toBeFocused();
  const options=page.locator('#palette-list [role=option]');
  await expect(options.first()).toBeVisible();
  expect(await options.first().getAttribute('aria-selected')).toBe('true');
  expect(await page.locator('#palette-input').getAttribute('aria-activedescendant')).toBe(await options.first().getAttribute('id'));
  await expect(page.locator('#palette-status')).toContainText('results');
  // Escape closes without running anything and returns focus to the composer it interrupted.
  await page.keyboard.press('Escape');
  await expect(page.locator('#palette-dialog')).toBeHidden();
  await expect(page.locator('#prompt')).toBeFocused();

  // Vertical keys move the active option; horizontal keys stay with the text caret.
  await page.locator('#palette-search').click();
  await page.locator('#palette-input').fill('se');
  const narrowed=page.locator('#palette-list [role=option]');
  expect(await narrowed.count()).toBeGreaterThan(1);
  const secondOption=await narrowed.nth(1).getAttribute('id');
  await page.keyboard.press('ArrowDown');
  expect(await page.locator('#palette-input').getAttribute('aria-activedescendant')).toBe(secondOption);
  const selected=await page.locator('#palette-input').getAttribute('aria-activedescendant');
  await page.keyboard.press('ArrowLeft');
  await page.keyboard.press('ArrowLeft');
  expect(await page.locator('#palette-input').getAttribute('aria-activedescendant')).toBe(selected);
  expect(await page.locator('#palette-input').inputValue()).toBe('se');
  expect(await page.locator('#palette-input').evaluate(node=>node.selectionStart)).toBe(0);
  await page.locator('#palette-input').press('End');
  expect(await page.locator('#palette-input').evaluate(node=>node.selectionStart)).toBe(2);
  expect(await page.locator('#palette-input').getAttribute('aria-activedescendant')).toBe(selected);
  // Tab walks the dialog's own controls instead of switching lists.
  await page.keyboard.press('Tab');
  expect(await page.evaluate(()=>document.getElementById('palette-dialog').contains(document.activeElement))).toBe(true);
  expect(await page.evaluate(()=>document.activeElement.id)).not.toBe('palette-input');
  await expect(page.locator('#palette-mode-actions')).toHaveAttribute('aria-pressed','true');
  await expect(page.locator('#palette-title')).toHaveText('Command palette');
  await expect(page.locator('#palette-input')).toHaveValue('se');
  await page.keyboard.press('Escape');
  await expect(page.locator('#palette-dialog')).toBeHidden();

  // Choosing a command only fills the composer; it must never submit a turn.
  await page.keyboard.press('Control+k');
  await page.locator('#palette-input').fill('demo skill');
  await expect(page.locator('#palette-list [role=option]').first()).toContainText('$demo-skill');
  await page.keyboard.press('Enter');
  await expect(page.locator('#palette-dialog')).toBeHidden();
  await expect(page.locator('#prompt')).toHaveValue('$demo-skill ');
  await expect(page.locator('#prompt')).toBeFocused();
  expect(prompts).toEqual([]);

  // Ctrl+P is a separate session palette that matches title, project, host and directory.
  await page.keyboard.press('Control+p');
  await expect(page.locator('#palette-title')).toHaveText('Switch session');
  await expect(page.locator('#palette-list [role=option]')).toHaveCount(3);
  await expect(page.locator('#palette-list')).toContainText('Palette project');
  await expect(page.locator('#palette-list')).toContainText('Second project');
  await expect(page.locator('#palette-list')).toContainText('Local');
  // The shared directory prefix still matches every session, at the lowest weight.
  await page.locator('#palette-input').fill('palette-fixture');
  await expect(page.locator('#palette-list [role=option]')).toHaveCount(3);
  // The shared prefix still matches every session, so the ranking has to pick the right directory first.
  await page.locator('#palette-input').fill('omega');
  await expect(page.locator('#palette-list [role=option]').first()).toContainText('Beta chat');
  await expect(page.locator('#palette-list [role=option]').first()).toContainText('palette-fixture/omega');
  await page.keyboard.press('Enter');
  await expect(page.locator('#palette-dialog')).toBeHidden();
  await expect(page.locator('#header-title')).toHaveText('Beta chat');
  // A draft survives a round trip through the palette, per session.
  await page.locator('#prompt').fill('draft for omega');
  await page.keyboard.press('Control+p');
  await page.locator('#palette-input').fill('alpha');
  await page.keyboard.press('Enter');
  await expect(page.locator('#header-title')).toHaveText('Alpha chat');
  await expect(page.locator('#prompt')).toHaveValue('$demo-skill ');
  await page.locator('#prompt').fill('');
  await page.keyboard.press('Control+p');
  await page.locator('#palette-input').fill('omega');
  await page.keyboard.press('Enter');
  await expect(page.locator('#header-title')).toHaveText('Beta chat');
  await expect(page.locator('#prompt')).toHaveValue('draft for omega');

  // A turn started before switching keeps running in its own session.
  await page.locator('#prompt').fill('');
  await page.locator('#prompt').fill('slow');
  await page.locator('#send').click();
  await expect(page.locator('#stop')).toBeVisible();
  const streamed=(await page.locator('#messages').textContent()).length;
  await page.keyboard.press('Control+p');
  await expect(page.locator('#palette-list')).toContainText('Running turn');
  await page.locator('#palette-input').fill('alpha');
  await page.keyboard.press('Enter');
  await expect(page.locator('#session-state')).toHaveText('Ready');
  await expect(page.locator('#prompt')).toHaveValue('');
  await page.keyboard.press('Control+p');
  await page.locator('#palette-input').fill('omega');
  await page.keyboard.press('Enter');
  await expect(page.locator('#header-title')).toHaveText('Beta chat');
  // Output from the turn started before the switch keeps arriving in its own session, and the turn finishes.
  await expect.poll(async()=>(await page.locator('#messages').textContent()).length).toBeGreaterThan(streamed);
  await expect(page.locator('#stop')).toBeHidden({timeout:30000});
  await expect(page.locator('#session-state')).toHaveText('Ready');

  // Disabled entries stay readable and refuse to run while the palette is open.
  await page.keyboard.press('Control+k');
  await page.locator('#palette-input').fill('stop turn');
  await expect(page.locator('#palette-list [role=option]').first()).toContainText('Stop turn');
  await expect(page.locator('#palette-list [role=option]').first()).toHaveAttribute('aria-disabled','true');
  await expect(page.locator('#palette-list [role=option]').first()).toContainText('No turn is running');
  await page.keyboard.press('Enter');
  await expect(page.locator('#palette-dialog')).toBeVisible();
  await expect(page.locator('#palette-status')).toContainText('No turn is running');
  await page.keyboard.press('Escape');

  // Other modals keep their own keyboard; the shortcuts never hijack them.
  await page.locator('#new-session').click();
  await expect(page.locator('#create-dialog')).toBeVisible();
  await page.keyboard.press('Control+k');
  await page.keyboard.press('Control+p');
  await expect(page.locator('#palette-dialog')).toBeHidden();
  await expect(page.locator('#create-dialog')).toBeVisible();
  await page.keyboard.press('Escape');

  // A query with no matches says so instead of offering a random action.
  await page.keyboard.press('Control+p');
  await page.locator('#palette-input').fill('qqzzxx');
  await expect(page.locator('#palette-list [role=option]')).toHaveCount(0);
  await expect(page.locator('#palette-list')).toContainText('No matches');
  await page.keyboard.press('Escape');
  await expect(page.locator('#header-title')).toHaveText('Beta chat');
  expect(errors).toEqual([]);
  expect(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBe(true);
  await api(`/api/sessions/${one.id}`,undefined,'DELETE');
  await api(`/api/sessions/${two.id}`,undefined,'DELETE');
  await api(`/api/sessions/${three.id}`,undefined,'DELETE');
  await api(`/api/projects/${project.id}`,undefined,'DELETE');
  await api(`/api/projects/${second.id}`,undefined,'DELETE');
});

test.describe('phone',()=>{
  test.use({viewport:{width:390,height:844},hasTouch:true});
  test('touch entry points open the palettes and tap targets are usable',async({page})=>{
    const project=await api('/api/projects',{name:'Phone project'});
    const first=await api('/api/sessions',{provider:'demo',cwd:alpha(),title:'Phone alpha',projectId:project.id,hostId:'local'});
    const second=await api('/api/sessions',{provider:'demo',cwd:zeta(),title:'Phone zeta',projectId:project.id,hostId:'local'});
    const errors=[];page.on('pageerror',e=>errors.push(e.message));
    await page.goto(base);
    // The sidebar entry is reachable from the mobile drawer.
    await page.locator('#menu').click();
    await page.locator('#palette-open').click();
    await expect(page.locator('#palette-dialog')).toBeVisible();
    // Without a selected session the palette still offers global actions and hides session-only ones.
    await expect(page.locator('#palette-list')).toContainText('New session');
    await expect(page.locator('#palette-list')).toContainText('Settings');
    await expect(page.locator('#palette-list')).not.toContainText('Delete session');
    await page.locator('#palette-input').fill('workspace');
    await expect(page.locator('#palette-list [role=option]')).toHaveCount(0);
    await expect(page.locator('#palette-list')).toContainText('No matches');
    await page.keyboard.press('Escape');
    await page.locator('#palette-open').click();
    expect(await page.locator('#palette-list [role=option]').first().boundingBox()).toMatchObject({height:expect.any(Number)});
    expect(Math.round((await page.locator('#palette-list [role=option]').first().boundingBox()).height)).toBeGreaterThanOrEqual(44);
    expect(await page.locator('#palette-dialog').evaluate(node=>{const r=node.getBoundingClientRect();return r.left>=0&&r.right<=innerWidth&&r.height<=innerHeight;})).toBe(true);
    // Switching from the phone list keeps the layout and the active session marker.
    await page.locator('#palette-mode-sessions').click();
    await expect(page.locator('#palette-title')).toHaveText('Switch session');
    await page.locator('#palette-input').fill('zeta');
    await expect(page.locator('#palette-list [role=option]').first()).toContainText('Phone zeta');
    await page.locator('#palette-list [role=option]').first().click();
    await expect(page.locator('#palette-dialog')).toBeHidden();
    await expect(page.locator('#header-title')).toHaveText('Phone zeta');
    await expect(page.locator('#session-state')).toHaveText('Ready');
    // The header entry is always visible, so no drawer is needed.
    await page.locator('#palette-search').click();
    await expect(page.locator('#palette-dialog')).toBeVisible();
    await expect(page.locator('#palette-mode-actions')).toHaveAttribute('aria-pressed','true');
    await page.locator('#palette-input').fill('new project');
    await page.keyboard.press('Enter');
    await expect(page.locator('#palette-dialog')).toBeHidden();
    await expect(page.locator('#project-dialog')).toBeVisible();
    await page.locator('#dismiss-project').click();
    expect(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBe(true);
    expect(errors).toEqual([]);
    for(const session of [first,second])await api(`/api/sessions/${session.id}`,undefined,'DELETE');
    await api(`/api/projects/${project.id}`,undefined,'DELETE');
  });
});

test('palette entries stay safe with hostile session text',async()=>{
  // Rendered as text nodes, so markup in titles or directories cannot execute.
  const items=sessionItems([{id:'x',title:'<img src=x onerror=alert(1)>',provider:'demo',status:'idle',cwd:'/<script>alert(2)</script>',hostId:'local'}],{labels:{demo:'Demo agent'}});
  expect(items[0].label).toBe('<img src=x onerror=alert(1)>');
  expect(sessionRanking('img',items).map(i=>i.id)).toEqual(['x']);
});
