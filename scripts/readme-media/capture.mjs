// Regenerate README screenshots and GIFs: npm run build && npm run readme:media
// Uses an isolated server, throwaway sample repos and the scripted demo agent. No model is called.
import {chromium} from '@playwright/test';
import {spawn,execFileSync} from 'node:child_process';
import {existsSync,mkdirSync,rmSync,writeFileSync,readdirSync} from 'node:fs';
import {dirname,join,resolve} from 'node:path';
import {randomUUID} from 'node:crypto';
import {stopChild,serverReady} from '../../tests/support/cleanup.mjs';
import {script,HERO_PROMPT,MESSAGE_PROMPT,NOTES_TITLE,backgroundSessions} from './conversation.mjs';
import {repos,heroEdits} from './sample-repos.mjs';

const port=process.env.AGENTWORKS_MEDIA_PORT||'4391',base=`http://127.0.0.1:${port}`;
const out=resolve('docs/media'),root='/tmp/agentworks-readme',marker=join(root,'.agentworks-readme');
const only=process.argv.slice(2);
const want=name=>!only.length||only.includes(name);
// The sample repo paths appear in screenshots, so use a fixed short root this script owns.
if(existsSync(root)){if(!existsSync(marker))throw new Error(`${root} exists and was not created by this script`);rmSync(root,{recursive:true,force:true});}
mkdirSync(root,{recursive:true});writeFileSync(marker,'');mkdirSync(out,{recursive:true});

const git=(cwd,...args)=>execFileSync('git',args,{cwd,env:{...process.env,GIT_AUTHOR_NAME:'Acme Dev',GIT_AUTHOR_EMAIL:'dev@acme.test',GIT_COMMITTER_NAME:'Acme Dev',GIT_COMMITTER_EMAIL:'dev@acme.test',GIT_AUTHOR_DATE:'2026-10-20T10:00:00Z',GIT_COMMITTER_DATE:'2026-10-20T10:00:00Z'}});
const write=(dir,files)=>{for(const [path,text] of Object.entries(files)){mkdirSync(dirname(join(dir,path)),{recursive:true});writeFileSync(join(dir,path),text);}};
for(const [name,files] of Object.entries(repos)){
  const dir=join(root,name);mkdirSync(dir);write(dir,files);
  git(dir,'init','-q','-b','main');git(dir,'add','.');git(dir,'commit','-qm','Initial import');
}
writeFileSync(join(root,'script.json'),JSON.stringify(script));

const env={...process.env,AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:port,AGENTWORKS_URL:base,
  AGENTWORKS_ROOTS:Object.keys(repos).map(n=>join(root,n)).join(':'),AGENTWORKS_DB_PATH:join(root,'sessions.sqlite3'),
  AGENTWORKS_DEMO_STATE_DIR:join(root,'demo'),AGENTWORKS_CATALOG_PATH:join(root,'catalog.json'),AGENTWORKS_DEMO_SCRIPT:join(root,'script.json')};
const server=spawn(resolve('target/debug/agentworks'),[],{env,stdio:'ignore'});
const delay=ms=>new Promise(r=>setTimeout(r,ms));
const api=async(path,method='GET',body)=>{
  const r=await fetch(base+path,{method,headers:{'Content-Type':'application/json',Origin:base},body:body&&JSON.stringify(body)});
  if(!r.ok)throw new Error(`${method} ${path}: ${r.status} ${await r.text()}`);return r.status===204?null:r.json();
};
const turns=async id=>(await api(`/api/sessions/${id}`)).events.filter(e=>e.kind==='user').length;
// Record a browser context from `started`, trimming everything before `trim` seconds.
function toGif(videoDir,trim,name){
  if(!want(name))return;
  const webm=join(videoDir,readdirSync(videoDir).find(f=>f.endsWith('.webm')));
  execFileSync('ffmpeg',['-y','-loglevel','error','-ss',String(trim),'-i',webm,'-vf',
    'fps=10,split[a][b];[a]palettegen=stats_mode=diff:max_colors=64[p];[b][p]paletteuse=dither=bayer:bayer_scale=5:diff_mode=rectangle',
    join(out,name+'.gif')]);
}
async function idle(id){for(let i=0;i<300;i++){const s=(await api('/api/sessions')).find(s=>s.id===id);if(s?.status==='idle'&&!s.turnRunning)return;await delay(100);}throw new Error('turn did not finish');}

let browser;
try{
  for(let i=0;!await serverReady(server,base);i++){if(i>100||server.exitCode!==null)throw new Error('Server did not become ready');await delay(100);}
  const projects={};
  for(const name of Object.keys(repos))projects[name]=(await api('/api/projects','POST',{name})).id;
  const create=(project,title)=>api('/api/sessions','POST',{provider:'demo',cwd:join(root,project),title,projectId:projects[project],hostId:'local',requestId:randomUUID()});
  const ids={};
  for(const s of backgroundSessions){
    const {id}=await create(s.project,s.title);await idle(id);ids[s.title]=id;
    await api(`/api/sessions/${id}/prompt`,'POST',{text:s.prompt,requestId:randomUUID()});await idle(id);
  }
  const hero=await create('acme-api','Rate-limit POST /login');await idle(hero.id);
  browser=await chromium.launch();
  const open=async(page,title)=>{await page.locator('#sessions').getByText(title,{exact:true}).click();await page.locator('#session-state').filter({hasText:'Ready'}).waitFor();};

  // 1. Animated walkthrough of one turn: prompt, plan, tools, approval, test run, summary.
  {
    const videoDir=join(root,'video');
    const context=await browser.newContext({viewport:{width:1280,height:800},recordVideo:{dir:videoDir,size:{width:1280,height:800}}});
    const started=Date.now(),page=await context.newPage();
    await page.goto(base);await open(page,'Rate-limit POST /login');await delay(400);
    const trim=(Date.now()-started)/1000;
    await page.locator('#prompt').pressSequentially(HERO_PROMPT,{delay:28});await delay(500);
    await page.locator('#send').click();
    const allow=page.getByRole('button',{name:'Allow once'});
    await allow.waitFor();await delay(1600);await allow.hover();await delay(400);await allow.click();
    write(join(root,'acme-api'),heroEdits);git(join(root,'acme-api'),'add','-N','.');
    await idle(hero.id);await delay(1200);
    await page.locator('#messages').evaluate(el=>el.scrollTo({top:el.scrollHeight,behavior:'smooth'}));await delay(4000);
    await context.close();toGif(videoDir,trim,'walkthrough');
  }

  // 2. Agent messaging: real Agentworks MCP messages between two sessions, with inbox delivery starting the peer's turn.
  {
    const notes=ids[NOTES_TITLE];
    await api(`/api/sessions/${notes}/inbox-delivery`,'PUT',{enabled:true,requestId:randomUUID(),deadline:null,maxTurns:1,cooldownSeconds:5,maxBatch:5});
    const before=await turns(notes);
    const videoDir=join(root,'video-messaging');
    const context=await browser.newContext({viewport:{width:1280,height:800},recordVideo:{dir:videoDir,size:{width:1280,height:800}}});
    const started=Date.now(),page=await context.newPage();
    await page.goto(base);await open(page,'Rate-limit POST /login');await delay(1200);
    const trim=(Date.now()-started)/1000;
    await page.locator('#prompt').pressSequentially(MESSAGE_PROMPT,{delay:28});await delay(400);
    await page.locator('#send').click();await idle(hero.id);await delay(1800);
    await page.locator('#sessions').getByText(NOTES_TITLE,{exact:true}).click();
    for(let i=0;await turns(notes)===before;i++){if(i>300)throw new Error('Inbox delivery did not start a turn');await delay(100);}
    await idle(notes);await delay(1000);
    await page.locator('#messages').evaluate(el=>el.scrollTo({top:el.scrollHeight,behavior:'smooth'}));await delay(3500);
    // Back in the sender, the reply waits in its inbox without having started a turn.
    await open(page,'Rate-limit POST /login');await delay(800);
    await page.locator('#session-agents').click();await page.locator('#agent-inbox').getByText('Thanks!').first().waitFor();
    await page.locator('#agent-inbox').evaluate(el=>el.scrollIntoView({block:'center',behavior:'smooth'}));await delay(4000);
    await context.close();toGif(videoDir,trim,'messaging');
  }

  // 3. Stills at 2x for crisp README rendering.
  const shot=async(name,options,fn)=>{
    if(!want(name))return;
    const context=await browser.newContext({viewport:{width:1440,height:900},deviceScaleFactor:2,...options});
    const page=await context.newPage();await page.goto(base);await fn(page);await delay(600);
    await page.screenshot({path:join(out,name+'.png')});await context.close();
  };
  await shot('session',{},async page=>{
    await open(page,'Rate-limit POST /login');
    await delay(1500);await page.locator('#messages .work-log-summary').first().click();await delay(300);
    await page.locator('#messages').getByText(HERO_PROMPT).first().evaluate(el=>el.scrollIntoView({block:'start'}));
  });
  await shot('summary',{},async page=>{
    await open(page,'Rate-limit POST /login');await delay(1500);
    await page.locator('#messages h3').filter({hasText:'Done'}).evaluate(el=>el.scrollIntoView({block:'start'}));
  });
  await shot('diff',{},async page=>{
    await open(page,'Rate-limit POST /login');
    await page.getByRole('button',{name:'Diff',exact:true}).click();await delay(800);
    await page.locator('#diff-files').selectOption('src/middleware/rate-limit.ts');await delay(800);
  });
  await shot('palette',{},async page=>{
    await open(page,'Rate-limit POST /login');await page.keyboard.press('Control+k');await delay(300);
  });
  await shot('mobile',{viewport:{width:390,height:844},deviceScaleFactor:3,isMobile:true,hasTouch:true},async page=>{
    await page.goto(base);await page.locator('#menu').click();await delay(300);
    await page.locator('#sessions').getByText('Rate-limit POST /login',{exact:true}).click();await delay(1500);
    await page.locator('#messages h3').filter({hasText:'Done'}).evaluate(el=>el.scrollIntoView({block:'start'}));
  });
  console.log('Wrote README media to',out);
}finally{
  await browser?.close();await stopChild(server);
  // The demo agent can outlive the server briefly and recreate its state directory.
  await delay(1000);
  if(!process.env.AGENTWORKS_KEEP_TEST_FIXTURES)rmSync(root,{recursive:true,force:true});
}
