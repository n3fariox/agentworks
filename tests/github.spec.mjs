import {test,expect} from '@playwright/test';
import {spawn,execFileSync} from 'node:child_process';
import {mkdtemp,mkdir,writeFile,readFile,rm} from 'node:fs/promises';
import {resolve,join} from 'node:path';

// Disposable server, repositories and a fake gh. No real account or GitHub API
// is contacted, and no credentials are read from this machine.
const base='http://127.0.0.1:4541';
let fixture,repo,linked,server,log='',exited=false;
const git=(cwd,...args)=>execFileSync('git',['-c','user.name=Fixture','-c','user.email=fixture@localhost',...args],{cwd,encoding:'utf8'}).trim();
async function api(path,body,method){
  const response=await fetch(base+path,{method:method||(body?'POST':'GET'),headers:{Origin:base,'Content-Type':'application/json'},body:body?JSON.stringify(body):undefined});
  expect(response.ok,await response.clone().text()).toBe(true);return response.json();
}
async function start(){
  exited=false;server=spawn(resolve('target/debug/agentworks'),[],{env:{...process.env,PATH:join(fixture,'bin')+':'+process.env.PATH,
    GH_TOKEN:'ENV_TOKEN_MUST_NOT_BE_USED',GH_ENTERPRISE_TOKEN:'ENV_TOKEN_MUST_NOT_BE_USED',GH_DEBUG:'api',
    AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:'4541',AGENTWORKS_ROOTS:fixture,
    AGENTWORKS_CATALOG_PATH:join(fixture,'catalog.json'),AGENTWORKS_DB_PATH:join(fixture,'sessions.sqlite3'),AGENTWORKS_DEMO_STATE_DIR:join(fixture,'demo')},stdio:['ignore','pipe','pipe']});
  server.on('error',e=>{log+=e.message;exited=true;});server.on('exit',()=>exited=true);
  for(const stream of [server.stdout,server.stderr])stream.on('data',s=>log+=s.toString());
  for(let i=0;i<150;i++){if(exited)throw new Error(log);try{if((await fetch(base+'/api/status')).ok)return;}catch{}await new Promise(r=>setTimeout(r,100));}
  throw new Error('Server did not start: '+log);
}
async function stop(){if(server&&!exited){const done=new Promise(r=>server.once('exit',r));server.kill('SIGTERM');await done;}}
const create=cwd=>api('/api/sessions',{provider:'demo',cwd,title:'GitHub context test'});
const connection=(host,login,enabled=true)=>api('/api/github/connections',{host,login,enabled});
const context=s=>api(`/api/sessions/${s.id}/github`);

test.beforeAll(async()=>{
  await mkdir('.cache',{recursive:true});fixture=await mkdtemp(resolve('.cache/github-'));repo=join(fixture,'main');linked=join(fixture,'linked');
  await mkdir(repo);await mkdir(join(fixture,'bin'));
  git(repo,'init','-b','main');await writeFile(join(repo,'file'),'fixture');git(repo,'add','.');git(repo,'commit','-m','fixture');git(repo,'remote','add','origin','git@ghe-a.example.com:team/repo.git');git(repo,'worktree','add','-b','feature/topic',linked);
  const hosts={hosts:{'github.com':[{login:'personal',state:'success',token:'PRIVATE_TOKEN',error:'PRIVATE_ERROR'}],
    'ghe-a.example.com':[{login:'work-a',state:'success',active:true},{login:'alternate',state:'success'}],
    'ghe-b.example.com':[{login:'work-b',state:'error'}]}};
  await writeFile(join(fixture,'accounts.json'),JSON.stringify(hosts));
  await writeFile(join(fixture,'bin/gh'),`#!${process.execPath}\nconst fs=require('node:fs');const root=${JSON.stringify(fixture)};const args=process.argv.slice(2);fs.appendFileSync(root+'/gh-calls',JSON.stringify({args,override:!!(process.env.GH_TOKEN||process.env.GH_ENTERPRISE_TOKEN||process.env.GH_DEBUG)})+'\\n');if(args[0]==='auth'&&args[1]==='status'){process.stdout.write(fs.readFileSync(root+'/accounts.json'));}else{process.stderr.write('PRIVATE_TOKEN');process.exitCode=1;}\n`,{mode:0o755});
  await start();
});
test.afterAll(async()=>{await stop();if(fixture)await rm(fixture,{recursive:true,force:true});});

test('discover three hosts safely, pin accounts, share worktree binding and retain it on restart',async()=>{
  const discovery=await api('/api/github/connections');
  expect(discovery.credentialHost).toBe('local');expect(discovery.connections).toHaveLength(4);
  expect(JSON.stringify(discovery)).not.toContain('PRIVATE_');
  expect(discovery.connections.find(c=>c.host==='ghe-b.example.com').state).toBe('unavailable');
  const session=await create(repo);
  expect((await context(session)).status).toBe('unconfigured');
  await connection('github.com','personal');
  expect((await context(session)).status).toBe('unconfigured'); // never use public credentials for GHE
  await connection('ghe-a.example.com','work-a');
  expect(await context(session)).toMatchObject({status:'unavailable',connection:{host:'ghe-a.example.com',login:'work-a'}});
  await connection('ghe-a.example.com','alternate');
  expect((await context(session)).connection.login).toBe('work-a');
  const worktree=await create(linked);
  expect((await context(worktree)).connection.login).toBe('work-a');
  const metadata=await api(`/api/sessions/${worktree.id}/workspace`);
  expect(metadata).toMatchObject({origin:'ghe-a.example.com:team/repo.git',branch:'feature/topic',linkedWorktree:true});
  await connection('ghe-a.example.com','work-a',false);
  expect((await context(worktree)).status).toBe('connection_unavailable');
  await api(`/api/sessions/${worktree.id}/github`,{login:'alternate'});
  expect((await context(session)).connection.login).toBe('alternate');
  const links=[{host:'ghe-a.example.com',owner:'team',repo:'repo',kind:'issue',number:7}];
  await api(`/api/sessions/${session.id}/github`,{links});
  await stop();await start();
  expect(await context(session)).toMatchObject({connection:{login:'alternate'},links});
  const calls=(await readFile(join(fixture,'gh-calls'),'utf8')).trim().split('\n').map(JSON.parse);
  expect(calls.some(c=>c.args.join(' ')==='auth token --hostname ghe-a.example.com --user alternate')).toBe(true);
  expect(calls.some(c=>c.args.join(' ')==='auth status --hostname ghe-a.example.com --active --json hosts')).toBe(true);
  expect(calls.some(c=>c.args.join(' ')==='auth token --hostname ghe-a.example.com')).toBe(true);
  expect(calls.every(c=>!c.override)).toBe(true);
  expect(log).not.toContain('PRIVATE_TOKEN');
  expect(await readFile(join(fixture,'catalog.json'),'utf8')).not.toContain('PRIVATE_TOKEN');
});

test('same-host ambiguity requires selection and link inputs stay repository-scoped',async()=>{
  await connection('ghe-a.example.com','work-a');
  git(repo,'remote','set-url','origin','https://ghe-a.example.com/team/other.git');
  const session=await create(repo);
  expect((await context(session)).status).toBe('choose_account');
  const bad=await fetch(base+`/api/sessions/${session.id}/github`,{method:'POST',headers:{Origin:base,'Content-Type':'application/json'},body:JSON.stringify({links:[{host:'github.com',owner:'team',repo:'other',kind:'issue',number:7}]})});
  expect(bad.status).toBe(400);
  await api(`/api/sessions/${session.id}/github`,{login:'work-a'});
  expect((await context(session)).connection.login).toBe('work-a');
  git(repo,'remote','set-url','origin','git@ghe-a.example.com:team/repo.git');
});

for(const mobile of [false,true])test(`GitHub display, safe text and detached links ${mobile?'mobile':'desktop'}`,async({page})=>{
  const session=await create(linked);
  if(mobile)await page.setViewportSize({width:390,height:844});
  await page.addInitScript(id=>localStorage.setItem('agentworks-session',id),session.id);
  const errors=[];page.on('pageerror',e=>errors.push(e.message));
  const data={status:'ready',repository:{host:'ghe-a.example.com',owner:'team',name:'repo'},branch:'feature/topic',commit:'new-commit',connection:{host:'ghe-a.example.com',login:'work-a'},accounts:[],links:[],snapshot:{fetchedAt:Date.now(),complete:true,resources:[{kind:'pullRequest',number:12,title:'<img src=x onerror=alert(1)>',url:'javascript:alert(1)',state:'OPEN',draft:false,headSha:'old-commit',headMatches:false,explicit:false,reviewDecision:'CHANGES_REQUESTED',checks:{state:'FAILURE',complete:true,items:[{name:'tests',conclusion:'FAILURE',detailsUrl:'https://ghe-a.example.com/checks/1'}]}}]}};
  await page.route(`**/api/sessions/${session.id}/github*`,async route=>{
    if(route.request().method()==='POST'){const body=route.request().postDataJSON();expect(body.links[0]).toMatchObject({kind:'issue',number:7,host:'ghe-a.example.com'});data.links=body.links;await route.fulfill({json:{ok:true}});}
    else await route.fulfill({json:data});
  });
  async function openGithub(){if(mobile)await page.locator('#session-more').click();await page.locator('#session-github').click();}
  await page.goto(base);
  await page.locator('#prompt').fill('Keep my draft');await openGithub();
  await expect(page.locator('#github-resources')).toContainText('PR head differs from checkout');
  await expect(page.locator('#github-resources')).toContainText('Changes requested');
  await expect(page.locator('#github-resources img')).toHaveCount(0);
  await expect(page.locator('#github-resources a').first()).not.toHaveAttribute('href',/javascript/);
  await page.locator('#github-link-kind').selectOption('issue');await page.locator('#github-link-number').fill('7');await page.locator('#github-link-add').click();
  await expect(page.locator('#github-linked')).toContainText('Issue #7');
  expect(await page.locator('#github-dialog').evaluate(n=>{const r=n.getBoundingClientRect();return r.left>=0&&r.right<=innerWidth&&r.height<=innerHeight;})).toBe(true);
  await page.screenshot({path:join('.cache',`github-${mobile?'mobile':'desktop'}.png`)});
  await page.locator('#dismiss-github').click();await expect(page.locator('#prompt')).toHaveValue('Keep my draft');
  await openGithub();await page.locator('#github-settings').click();
  await expect(page.locator('#settings-panel-github')).toBeVisible();
  await expect(page.locator('#github-connections')).toContainText('ghe-b.example.com');
  expect(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBe(true);
  expect(errors).toEqual([]);
});

for(const mobile of [false,true])test(`session browser PR badges ${mobile?'mobile':'desktop'}`,async({page})=>{
  const cases=[
    {title:'Fix account selection',number:1234,reviewDecision:'APPROVED',ci:'SUCCESS',tone:'success'},
    {title:'Simplify worker startup',number:5678,reviewDecision:'REVIEW_REQUIRED',ci:'PENDING',tone:'warning'},
    {title:'Handle expired credentials',number:9012,reviewDecision:'CHANGES_REQUESTED',ci:'FAILURE',tone:'danger'},
  ];
  const examples=new Map(),calls=new Map();
  for(const example of cases){const session=await api('/api/sessions',{provider:'demo',cwd:linked,title:example.title});examples.set(session.id,{...example,session});}
  const first=[...examples.values()][0].session;
  if(mobile)await page.setViewportSize({width:390,height:844});
  await page.addInitScript(id=>localStorage.setItem('agentworks-session',id),first.id);
  await page.route('**/api/sessions/*/github*',route=>{
    const id=new URL(route.request().url()).pathname.split('/')[3],example=examples.get(id);
    calls.set(id,(calls.get(id)||0)+1);
    if(!example)return route.fulfill({json:{status:'no_origin'}});
    return route.fulfill({json:{status:'ready',repository:{host:'ghe-a.example.com',owner:'team',name:'repo'},connection:{host:'ghe-a.example.com',login:'work-a'},links:[],badgesAt:Date.now(),badges:[{id:`github.pr.${example.number}`,label:`PR#${example.number}`,tone:example.tone,tooltip:example.title,url:`https://ghe-a.example.com/team/repo/pull/${example.number}`,source:'@github'}],snapshot:{fetchedAt:Date.now(),complete:true,resources:[{kind:'pullRequest',number:example.number,title:example.title,state:'OPEN',draft:false,headMatches:true,headSha:'1234',reviewDecision:example.reviewDecision,checks:{state:example.ci,complete:true,items:[]}}]}}});
  });
  const errors=[];page.on('pageerror',e=>errors.push(e.message));
  await page.goto(base);await expect(page.locator('#session-state')).toHaveText('Ready');
  if(mobile){await page.locator('#session-home').click();await expect(page.locator('#sidebar')).toHaveCSS('transform','none');}
  for(const [id,example] of examples){
    const row=page.locator(`.session-item[data-session-id="${id}"]`),badge=row.locator('.session-badge');
    await expect(badge).toHaveText(`PR#${example.number}`);await expect(badge).toHaveClass(`session-badge ${example.tone}`);
    expect(calls.get(id)).toBe(1); // selected session and browser share one request
    await expect(badge).toHaveAttribute('href',`https://ghe-a.example.com/team/repo/pull/${example.number}`);
    await expect(badge).toHaveAttribute('target','_blank');await expect(badge).toHaveAttribute('rel','noopener noreferrer');
    expect(await badge.evaluate(n=>n.closest('button'))).toBeNull(); // native link and selection button are siblings
    expect(await badge.evaluate(n=>n.dispatchEvent(new MouseEvent('contextmenu',{bubbles:true,cancelable:true})))).toBe(true);
    const metaBox=await row.locator('small').boundingBox(),badgeBox=await badge.boundingBox();
    expect(badgeBox.y).toBeGreaterThanOrEqual(metaBox.y+metaBox.height);
  }
  await page.screenshot({path:join('.cache',`session-badges-third-line-${mobile?'mobile':'desktop'}-20261029.png`),animations:'disabled'});
  if(!mobile)await page.locator('#sessions').screenshot({path:join('.cache','session-badges-third-line-preview-20261029.png'),animations:'disabled'});
  await page.context().route('https://ghe-a.example.com/**',route=>route.fulfill({contentType:'text/html',body:'<title>PR link test</title>'}));
  const second=[...examples.values()][1],secondBadge=page.locator(`.session-item[data-session-id="${second.session.id}"] .session-badge`);
  const opened=page.waitForEvent('popup');await secondBadge.click();const popup=await opened;await popup.waitForLoadState();
  expect(popup.url()).toBe(`https://ghe-a.example.com/team/repo/pull/${second.number}`);await popup.close();
  expect(await page.evaluate(()=>localStorage.getItem('agentworks-session'))).toBe(first.id);
  const third=[...examples.values()][2],thirdBadge=page.locator(`.session-item[data-session-id="${third.session.id}"] .session-badge`);
  await thirdBadge.focus();const keyed=page.waitForEvent('popup');await page.keyboard.press('Enter');const keyboardPopup=await keyed;await keyboardPopup.waitForLoadState();
  expect(keyboardPopup.url()).toBe(`https://ghe-a.example.com/team/repo/pull/${third.number}`);await keyboardPopup.close();
  expect(await page.evaluate(()=>localStorage.getItem('agentworks-session'))).toBe(first.id);
  await page.locator(`.session-item[data-session-id="${second.session.id}"] .session-select`).click();
  expect(await page.evaluate(()=>localStorage.getItem('agentworks-session'))).toBe(second.session.id);
  expect(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBe(true);expect(errors).toEqual([]);
});

test('existing hooks set/update/clear stored badges, isolate sessions, survive restart and push live updates',async({page})=>{
  const plain=join(fixture,'badge-plain');await mkdir(plain,{recursive:true});
  const source=`load("@agentworks//chat.star", "ChatContext")
def format_line(ctx: ChatContext, line: str) -> str | None:
    if line == "BADGE_SET":
        agent.set_badge("context", "Local", tone = "info", tooltip = "Set by an existing hook")
        agent.set_badge("ready", "Ready", tone = "success", url = "https://github.com/org/repo/pull/1")
    elif line == "BADGE_UPDATE":
        agent.set_badge("ready", "Updated", tone = "warning")
    elif line == "BADGE_FAIL":
        agent.set_badge("ready", "Must not persist")
        return str(1 // 0)
    elif line == "BADGE_INVALID":
        agent.set_badge("ready", "Invalid", url = "javascript:alert(1)")
    elif line == "BADGE_CLEAR":
        agent.clear_badge("context")
        agent.clear_badge("ready")
    return None
agent.register_formatter(format_line, message_type = "chat.user", text_pattern = "^BADGE_")
`;
  const installed=await api('/api/scripts/local_badges.star',{source},'PUT');
  expect(installed.scripts.find(s=>s.name==='local_badges.star').hooks[0].message_type).toBe('chat.user');
  const session=await api('/api/sessions',{provider:'demo',cwd:plain,title:'Stored badges from an existing hook'});
  const other=await api('/api/sessions',{provider:'demo',cwd:plain,title:'Independent badge target'});
  const run=(s,text)=>api(`/api/sessions/${s.id}/user-format`,{text});
  const stored=s=>api(`/api/sessions/${s.id}/badges`);
  expect((await stored(session)).badges).toEqual([]);
  await run(session,'BADGE_SET');
  const response=await stored(session);expect(response.badges.map(b=>b.label)).toEqual(['Local','Ready']);
  expect(response.badges[0].source).toBe('local_badges.star');expect(response.badgesAt).toEqual(expect.any(Number));
  expect((await stored(session)).badgeRevision).toBe(response.badgeRevision); // reading does not invoke hooks
  expect((await stored(other)).badges).toEqual([]);
  await run(session,'BADGE_FAIL');await run(session,'BADGE_INVALID');
  expect((await stored(session)).badges.map(b=>b.label)).toEqual(['Local','Ready']);
  await run(other,'BADGE_SET'); // identical formatter input must not share another session's side effects
  expect((await stored(other)).badges.map(b=>b.label)).toEqual(['Local','Ready']);
  await run(other,'BADGE_CLEAR');expect((await stored(other)).badges).toEqual([]);
  expect((await stored(session)).badges.map(b=>b.label)).toEqual(['Local','Ready']);
  await stop();await start();
  expect((await stored(session)).badges.map(b=>b.label)).toEqual(['Local','Ready']);
  await page.addInitScript(id=>localStorage.setItem('agentworks-session',id),session.id);await page.goto(base);
  await expect(page.locator('#session-state')).toHaveText('Ready');
  const row=page.locator(`.session-item[data-session-id="${session.id}"]`);
  await expect(row.locator('.session-badge')).toHaveText(['Local','Ready']);await expect(row).toBeVisible();await row.scrollIntoViewIfNeeded();
  await expect(page.locator('#connection')).toHaveText('Connected');
  const withBadges=(await row.boundingBox()).height;
  await run(session,'BADGE_UPDATE');await expect(row.locator('.session-badge')).toHaveText(['Local','Updated']); // pushed, no read/refresh
  expect((await stored(session)).badges).toHaveLength(2);
  await run(session,'BADGE_CLEAR');await expect(row.locator('.session-badge')).toHaveCount(0);
  await expect(row.locator('.session-badges')).toHaveCSS('display','none');expect((await row.boundingBox()).height).toBeLessThan(withBadges);
  await run(session,'BADGE_SET');await expect(row.locator('.session-badge')).toHaveText(['Local','Ready']); // setter calls must not be memoized away
  await api('/api/scripts/local_badges.star/enabled',{enabled:false},'PUT');
  await page.evaluate(()=>document.dispatchEvent(new Event('session-badges-changed')));await expect(row.locator('.session-badge')).toHaveCount(0);
  await api('/api/scripts/local_badges.star',undefined,'DELETE');
});
