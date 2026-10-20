import {test,expect} from '@playwright/test';
import {spawn} from 'node:child_process';
import {mkdtemp,mkdir} from 'node:fs/promises';
import {resolve,join} from 'node:path';

// L-02: isolated server/database on assigned port 4531; demo agent only, no paid inference.
const base='http://127.0.0.1:4531';
test.use({baseURL:base});
let fixture,server,session;

function launch(port,dir){
  const proc={log:'',exited:false};
  proc.child=spawn(resolve('target/debug/agentworks'),[],{env:{...process.env,
    AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:String(port),AGENTWORKS_ROOTS:dir,
    AGENTWORKS_CATALOG_PATH:join(dir,'catalog.json'),AGENTWORKS_DB_PATH:join(dir,'sessions.sqlite3'),AGENTWORKS_DEMO_STATE_DIR:join(dir,'demo')},stdio:['ignore','pipe','pipe']});
  proc.child.on('error',e=>{proc.log+=e.message;proc.exited=true;});proc.child.on('exit',()=>proc.exited=true);
  for(const stream of [proc.child.stdout,proc.child.stderr])stream.on('data',s=>{proc.log+=s.toString();});
  return proc;
}
async function ready(proc,url){
  for(let i=0;i<100;i++){
    if(proc.exited)throw new Error(proc.log);
    try{if((await fetch(url+'/api/status')).ok)return;}catch{}
    await new Promise(r=>setTimeout(r,100));
  }
  throw new Error('Isolated server readiness timed out: '+proc.log);
}
async function call(path,body,method){
  const response=await fetch(base+path,{method:method||(body?'POST':'GET'),headers:{Origin:base,'Content-Type':'application/json'},body:body?JSON.stringify(body):undefined});
  return {status:response.status,body:await response.text().then(t=>{try{return JSON.parse(t);}catch{return t;}})};
}
async function api(path,body,method){const r=await call(path,body,method);expect(r.status,JSON.stringify(r.body)).toBeLessThan(300);return r.body;}
async function until(fn,ms=20000){const end=Date.now()+ms;while(Date.now()<end){const v=await fn();if(v)return v;await new Promise(r=>setTimeout(r,100));}throw new Error('Condition timed out');}
const snapshot=()=>api(`/api/sessions/${session.id}`);
const state=async()=>(await snapshot()).session.status;
const pendingQuestion=()=>until(async()=>(await snapshot()).approvals.find(a=>a.kind==='question'));
const answerPath=id=>`/api/sessions/${session.id}/questions/${id}`;
async function openSession(page){
  await page.addInitScript(id=>{if(!sessionStorage.getItem('q-init')){localStorage.setItem('agentworks-session',id);sessionStorage.setItem('q-init','1');}},session.id);
  await page.goto('/');
  await expect(page.locator('#session-state')).toHaveText('Ready');
}
async function type(page,text){await page.locator('#prompt').fill(text);await page.locator('#prompt').press('Enter');}
const card=page=>page.locator('form.question');
const transcriptText=page=>page.locator('#messages').innerText();

test.beforeAll(async()=>{
  await mkdir('.cache',{recursive:true});fixture=await mkdtemp(resolve('.cache/questions-'));
  server=launch(4531,fixture);await ready(server,base);
});
test.afterAll(()=>server?.child.kill());
test.beforeEach(async()=>{session=await api('/api/sessions',{provider:'demo',cwd:fixture,title:'Question test'});await until(async()=>await state()==='idle');});
test.afterEach(async()=>{if(session)await api(`/api/sessions/${session.id}`,undefined,'DELETE');});

test('renders the form, validates in the browser and submits by keyboard exactly once',async({page})=>{
  await openSession(page);
  await type(page,'question');
  const form=card(page);
  await expect(form).toBeVisible();
  await expect(form.getByRole('heading',{name:'Question from Demo agent'})).toBeVisible();
  await expect(page.locator('#session-state')).toHaveText('Needs your answer');
  await expect(form.getByText('How should the demo project be set up?')).toBeVisible();
  // Accessible names come from legends/labels; descriptions are attached to the controls.
  await expect(form.getByRole('group',{name:/Database/})).toBeVisible();
  await expect(form.getByRole('radio',{name:/Postgres/})).toBeVisible();
  await expect(form.getByRole('checkbox',{name:'Authentication'})).toBeVisible();
  await expect(form.getByRole('spinbutton',{name:/Replicas/})).toHaveAttribute('aria-required','true');
  await expect(form.getByRole('checkbox',{name:'Create sample data'})).toBeChecked();

  // Empty submit: nothing is sent, errors are announced and focus moves to the first invalid field.
  let posts=0;page.on('request',r=>{if(r.method()==='POST'&&r.url().includes('/questions/'))posts++;});
  await form.getByRole('button',{name:'Submit answer'}).click();
  await expect(form.locator('.question-error')).toHaveText('Please fix 2 fields before submitting.');
  await expect(form.getByText('Database is required')).toBeVisible();
  await expect(form.getByText('Replicas is required')).toBeVisible();
  await expect(form.getByRole('radio',{name:/Postgres/})).toBeFocused();
  await expect(form.getByRole('radio',{name:/Postgres/})).toHaveAttribute('aria-invalid','true');
  expect(posts).toBe(0);

  // Keyboard only: arrow within the radio group, type values, toggle checkboxes, Enter submits.
  await page.keyboard.press('ArrowDown');
  await expect(form.getByRole('radio',{name:/SQLite/})).toBeChecked();
  await expect(form.getByText('Database is required')).toBeHidden();
  await form.getByRole('checkbox',{name:'Search'}).focus();await page.keyboard.press('Space');
  await form.getByRole('checkbox',{name:'Billing'}).focus();await page.keyboard.press('Space');
  await form.getByRole('checkbox',{name:'Authentication'}).focus();await page.keyboard.press('Space');
  const replicas=form.getByRole('spinbutton',{name:/Replicas/});
  await replicas.fill('9');await replicas.press('Enter');
  await expect(form.getByText('Choose at most 2 options')).toBeVisible();
  await expect(form.getByRole('checkbox',{name:'Authentication'})).toBeFocused();
  await expect(form.getByText('Replicas must be at most 5')).toBeVisible();
  await form.getByRole('checkbox',{name:'Authentication'}).uncheck();
  await form.getByRole('textbox',{name:'Contact email'}).fill('not-an-email');
  await replicas.fill('3');await replicas.press('Enter');
  await expect(form.getByText('Contact email must be a valid email')).toBeVisible();
  expect(posts).toBe(0);
  await form.getByRole('textbox',{name:'Contact email'}).fill('me@example.com');
  await form.getByRole('textbox',{name:'Contact email'}).press('Enter');

  await expect(form).toHaveCount(0);
  await until(async()=>await state()==='idle');
  const text=await transcriptText(page);
  expect(text).toContain('You answered: {"confirm":true,"contact":"me@example.com","features":["search","billing"],"question_0":"SQLite","replicas":3}');
  expect(text).toContain('How should the demo project be set up? — answer sent.');
  expect(text.match(/You answered:/g)).toHaveLength(1);
  expect(posts).toBe(1);
});

test('server validates answers and deduplicates retried submissions',async({page})=>{
  await openSession(page);
  await type(page,'question');
  const q=await pendingQuestion();
  expect(q.unsupported).toBeNull();
  expect(q.request.mode).toBe('form');
  // Malformed answers never reach the agent and leave the question pending.
  let r=await call(answerPath(q.id),{action:'accept',content:{question_0:'MySQL',replicas:0,extra:1},submissionId:'bad'});
  expect(r.status).toBe(400);
  expect(r.body).toContain('Database is not one of the offered choices');
  expect(r.body).toContain('Replicas must be at least 1');
  expect(r.body).toContain('not a field of this question');
  expect((await call(answerPath(q.id),{action:'maybe',submissionId:'x'})).status).toBe(400);
  expect((await call(answerPath(q.id),{action:'accept',content:{question_0:'SQLite',replicas:2}})).status).toBe(422);
  expect((await call(`/api/sessions/${session.id}/approvals/${q.id}`,{optionId:'allow'})).status).toBe(400);
  await expect(card(page)).toBeVisible();

  const answer={action:'accept',content:{question_0:'Postgres',replicas:2},submissionId:'sub-1'};
  expect(await api(answerPath(q.id),answer)).toEqual({ok:true,action:'accept'});
  // A retry after a lost response is acknowledged without a second delivery.
  expect(await api(answerPath(q.id),answer)).toEqual({ok:true,duplicate:true,action:'accept'});
  // A different submission (another tab, a changed answer) is refused clearly.
  r=await call(answerPath(q.id),{...answer,submissionId:'sub-2'});
  expect(r.status).toBe(409);
  expect(r.body).toContain('already answered elsewhere');
  await until(async()=>await state()==='idle');
  await expect(card(page)).toHaveCount(0);
  const text=await transcriptText(page);
  expect(text.match(/You answered:/g)).toHaveLength(1);
  expect(text).toContain('"question_0":"Postgres","replicas":2');
});

test('drafts survive reload and a failed submission; the answer is delivered once after recovery',async({page})=>{
  await openSession(page);
  await type(page,'question');
  let form=card(page);
  await form.getByRole('radio',{name:/Postgres/}).check();
  await form.getByRole('checkbox',{name:'Billing'}).check();
  await form.getByRole('textbox',{name:/Other/}).fill('also Redis');
  await form.getByRole('spinbutton',{name:/Replicas/}).fill('4');
  await form.getByRole('checkbox',{name:'Create sample data'}).uncheck();

  // Browser disconnect: a full reload rebuilds the card from server state plus the saved draft.
  await page.reload();
  form=card(page);
  await expect(form).toBeVisible();
  await expect(form.getByRole('radio',{name:/Postgres/})).toBeChecked();
  await expect(form.getByRole('checkbox',{name:'Billing'})).toBeChecked();
  await expect(form.getByRole('textbox',{name:/Other/})).toHaveValue('also Redis');
  await expect(form.getByRole('spinbutton',{name:/Replicas/})).toHaveValue('4');
  await expect(form.getByRole('checkbox',{name:'Create sample data'})).not.toBeChecked();

  // Live redraws (any session event) must not wipe what is being typed or move focus.
  const email=form.getByRole('textbox',{name:'Contact email'});
  await email.fill('ops@exa');
  await api(`/api/sessions/${session.id}`,{title:'Renamed while answering'},'PATCH');
  await expect(page.locator('#session-heading-title')).toHaveText('Renamed while answering');
  await expect(email).toBeFocused();
  await page.keyboard.type('mple.com');
  await expect(email).toHaveValue('ops@example.com');

  // The network drops the first submission: the answer stays, the user is told, nothing is delivered.
  const bodies=[];
  await page.route('**/questions/**',async(route,request)=>{bodies.push(request.postDataJSON());if(bodies.length===1)await route.abort('connectionreset');else await route.continue();});
  await form.getByRole('button',{name:'Submit answer'}).click();
  await expect(form.locator('.question-error')).toContainText('Not delivered');
  await expect(form.getByRole('button',{name:'Submit answer'})).toBeEnabled();
  await expect(form.getByRole('spinbutton',{name:/Replicas/})).toHaveValue('4');
  expect((await snapshot()).approvals).toHaveLength(1);

  await form.getByRole('button',{name:'Submit answer'}).click();
  await expect(form).toHaveCount(0);
  // The retry reused the same submission ID, so a lost-response retry could be deduplicated.
  expect(bodies).toHaveLength(2);
  expect(bodies[1].submissionId).toBe(bodies[0].submissionId);
  await until(async()=>await state()==='idle');
  const text=await transcriptText(page);
  expect(text.match(/You answered:/g)).toHaveLength(1);
  expect(text).toContain('"question_0":"Postgres","question_0_custom":"also Redis","replicas":4');
  // The draft is gone once the question is resolved.
  expect(await page.evaluate(()=>Object.keys(localStorage).filter(k=>k.startsWith('agentworks-question:')))).toEqual([]);
});

test('turn cancellation withdraws the question; late answers are refused and drafts dropped',async({page})=>{
  await openSession(page);
  await type(page,'question');
  const q=await pendingQuestion();
  await card(page).getByRole('spinbutton',{name:/Replicas/}).fill('2');
  await expect.poll(()=>page.evaluate(()=>Object.keys(localStorage).filter(k=>k.startsWith('agentworks-question:')).length)).toBe(1);
  await page.locator('#stop').click();
  await expect(card(page)).toHaveCount(0);
  await until(async()=>await state()==='idle');
  const r=await call(answerPath(q.id),{action:'accept',content:{question_0:'SQLite',replicas:2},submissionId:'late'});
  expect(r.status).toBe(409);
  expect(await transcriptText(page)).toContain('— cancelled because the turn ended.');
  expect(await transcriptText(page)).not.toContain('You answered:');
  expect(await page.evaluate(()=>Object.keys(localStorage).filter(k=>k.startsWith('agentworks-question:')))).toEqual([]);
  // A reload replays the history without resurrecting the card.
  await page.reload();
  await expect(page.locator('#session-state')).toHaveText('Ready');
  await expect(card(page)).toHaveCount(0);
});

test('the agent can withdraw a question with $/cancel_request',async({page})=>{
  await openSession(page);
  await type(page,'question withdraw');
  await expect(card(page)).toBeVisible();
  await expect(card(page)).toHaveCount(0,{timeout:10000});
  await until(async()=>await state()==='idle');
  const text=await transcriptText(page);
  expect(text).toContain('— withdrawn by the agent.');
  expect(text).toContain('The question was cancelled.');
});

test('Skip and Cancel buttons send decline and cancel',async({page})=>{
  await openSession(page);
  await type(page,'question');
  await card(page).getByRole('button',{name:'Skip'}).click();
  await until(async()=>await state()==='idle');
  expect(await transcriptText(page)).toContain('You skipped the question.');
  expect(await transcriptText(page)).toContain('— skipped.');
  await type(page,'question');
  await card(page).getByRole('button',{name:'Cancel'}).click();
  await until(async()=>await state()==='idle');
  expect(await transcriptText(page)).toContain('— cancelled by you.');
});

test('unsupported forms are explained and can only be skipped or cancelled',async({page})=>{
  await openSession(page);
  await type(page,'question url');
  const form=card(page);
  await expect(form.getByRole('note')).toContainText('“url” interaction, which Agentworks cannot show');
  await expect(form.getByRole('button',{name:'Submit answer'})).toBeHidden();
  const q=await pendingQuestion();
  const r=await call(answerPath(q.id),{action:'accept',content:{},submissionId:'u'});
  expect(r.status).toBe(400);
  expect(r.body).toContain('not supported');
  await form.getByRole('button',{name:'Skip'}).click();
  await until(async()=>await state()==='idle');
  expect(await transcriptText(page)).toContain('You skipped the question.');
});

test('touch layout keeps controls at least 44px and inside a phone viewport',async({page})=>{
  await page.setViewportSize({width:375,height:740});
  await openSession(page);
  await type(page,'question');
  const form=card(page);
  await expect(form).toBeVisible();
  for(const locator of [form.locator('.question-option').first(),form.getByRole('button',{name:'Submit answer'}),form.getByRole('button',{name:'Skip'}),form.getByRole('spinbutton',{name:/Replicas/})]){
    const box=await locator.boundingBox();
    expect(box.height).toBeGreaterThanOrEqual(44);
    expect(box.x+box.width).toBeLessThanOrEqual(375);
  }
  await form.locator('.question-option',{hasText:'SQLite'}).click();
  await expect(form.getByRole('radio',{name:/SQLite/})).toBeChecked();
});
