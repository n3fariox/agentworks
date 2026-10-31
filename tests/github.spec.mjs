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
// Presentation fixtures control GitHub data; forward the live session stream
// while keeping real background refreshes from overwriting those fixtures.
async function trackingSocket(page){
  let socket;
  await page.routeWebSocket('**/api/sessions/*/events',ws=>{
    socket=ws;const server=ws.connectToServer();
    server.onMessage(message=>{const event=JSON.parse(String(message));if(!['github_context','github_event'].includes(event.kind))ws.send(message);});
  });
  return ()=>socket;
}
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
  await trackingSocket(page);
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
  await trackingSocket(page);
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

test('pushed PR status refreshes the open panel and badge and emits a change event without reload',async({page})=>{
  const socket=await trackingSocket(page),session=await create(linked);
  await page.addInitScript(id=>localStorage.setItem('agentworks-session',id),session.id);
  const data={status:'ready',repository:{host:'ghe-a.example.com',owner:'team',name:'repo'},connection:{host:'ghe-a.example.com',login:'work-a'},branch:'feature/topic',commit:'abc',accounts:[],links:[],tracking:{observedAt:1},badgeRevision:1,badgesAt:Date.now(),badges:[{id:'github.pr.12',source:'@github',label:'PR#12',tone:'warning',tooltip:'Awaiting review',url:'https://ghe-a.example.com/team/repo/pull/12'}],snapshot:{fetchedAt:Date.now(),complete:true,resources:[{kind:'pullRequest',number:12,title:'Tracked PR',url:'https://ghe-a.example.com/team/repo/pull/12',state:'OPEN',draft:false,headSha:'abc',headMatches:true,reviewSummary:{state:'REVIEW_REQUIRED',complete:true},checks:{state:'PENDING',complete:true,items:[]}}]}};
  await page.route(`**/api/sessions/${session.id}/github`,route=>route.fulfill({json:data}));
  await page.goto(base);await expect(page.locator('#connection')).toHaveText('Connected');await page.locator('#session-github').click();await expect(page.locator('#github-resources')).toContainText('Awaiting review');
  await page.evaluate(()=>document.addEventListener('github-event',event=>window.__prEvent=event.detail));
  const next=structuredClone(data);next.tracking.observedAt=2;next.badgeRevision=2;next.badges[0].tone='success';next.badges[0].tooltip='Approved';next.snapshot.resources[0].reviewSummary.state='APPROVED';next.snapshot.resources[0].checks.state='SUCCESS';
  socket().send(JSON.stringify({kind:'github_context',data:next}));
  await expect(page.locator('#github-resources')).toContainText('Approved');await expect(page.locator('#github-resources')).toContainText('SUCCESS');
  const row=page.locator(`.session-item[data-session-id="${session.id}"]`);await expect(row.locator('.session-badge.success')).toHaveText('PR#12');
  socket().send(JSON.stringify({kind:'github_event',data:{id:99,kind:'pr.status_changed',initial:false,before:{review:'REVIEW_REQUIRED'},after:{review:'APPROVED'},changed:['review']}}));
  await expect.poll(()=>page.evaluate(()=>window.__prEvent?.id)).toBe(99);expect(await page.evaluate(()=>window.__prEvent.sessionId)).toBe(session.id);
  const unavailable=structuredClone(next);unavailable.status='unavailable';unavailable.message='GitHub access is unavailable';unavailable.tracking.observedAt=3;unavailable.badgeRevision=3;unavailable.badges[0].tone='neutral';delete unavailable.snapshot;
  socket().send(JSON.stringify({kind:'github_context',data:unavailable}));await expect(page.locator('#github-status')).toHaveText('GitHub access is unavailable');await expect(row.locator('.session-badge.success')).toHaveCount(0);
  socket().send(JSON.stringify({kind:'github_context',data}));await expect(page.locator('#github-status')).toHaveText('GitHub access is unavailable');await expect(row.locator('.session-badge.success')).toHaveCount(0);
});

test('GitHub PR hook starter installs and its registration is displayed',async({page})=>{
  await page.goto(base);await page.locator('#nav-settings').evaluate(node=>node.click());await page.locator('#settings-tab-parsers').click();
  const panel=page.locator('#parser-script-controls');await panel.locator('[data-new]').click();await panel.locator('[data-starter]').selectOption('github');
  await panel.locator('[data-name]').fill('pr_status.star');await panel.locator('[data-save]').click();
  await expect(panel.locator('[data-script-entry="pr_status.star"]')).toContainText('GitHub · pr.tracked, pr.snapshot, pr.status_changed, pr.untracked, pr.unavailable');
  const scripts=await api('/api/scripts');expect(scripts.scripts.find(s=>s.name==='pr_status.star').hooks).toContainEqual({hook:'github',events:['pr.tracked','pr.snapshot','pr.status_changed','pr.untracked','pr.unavailable']});
  await api('/api/scripts/pr_status.star',undefined,'DELETE');
});

function githubBrowserData(){
  const sha=git(repo,'rev-parse','HEAD');
  const hosts=['github.com','ghe-a.example.com','ghe-b.example.com'].map((host,i)=>({host,accounts:[i?'work':'personal'],intervalMs:300000,nextPollAt:Date.now()+300000,messages:[],rows:[{repository:{host,owner:'team',name:'repo'},pr:{number:100+i,title:i===1?'<img src=x onerror=alert(1)>':'A pull request',author:'me',headBranch:'feature/topic',headSha:sha,state:'OPEN',draft:false,reviewSummary:{state:i===0?'APPROVED':'REVIEW_REQUIRED'},checks:{state:i===1?'FAILURE':'PENDING'},url:`https://${host}/team/repo/pull/${100+i}`},authored:i===0,assigned:i===1,reviewRequested:i===2,stale:false,badge:{label:`PR#${100+i}`,tone:['success','danger','warning'][i],tooltip:'PR status',url:`https://${host}/team/repo/pull/${100+i}`}}]}));
  return {hosts,defaultIntervalMs:300000};
}
for(const mobile of [false,true])test(`GitHub sidebar tables and PR-to-session prefill ${mobile?'mobile':'desktop'}`,async({page})=>{
  if(mobile)await page.setViewportSize({width:390,height:844});
  const dataset=githubBrowserData();let reads=0;
  await page.route('**/api/github/pulls',route=>{reads++;return route.fulfill({json:dataset});});
  await page.goto(base);expect(reads).toBe(0);await page.locator('#nav-github').click();
  await expect(page.locator('.github-pr-table')).toHaveCount(3);await expect(page.locator('.github-browser-row')).toHaveCount(3);
  await expect(page.locator('#github-browser .session-badge.success')).toHaveText('PR#100');await expect(page.locator('#github-browser .session-badge.danger')).toHaveText('PR#101');await expect(page.locator('#github-browser .session-badge.warning')).toHaveText('PR#102');
  await expect(page.locator('#github-browser img')).toHaveCount(0);await expect(page.locator('#github-browser')).toContainText('Assigned to me');await expect(page.locator('#github-browser')).toContainText('Review requested');
  expect(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBe(true);
  const host=page.locator('#github-browser>.github-browser-group').first();await host.locator(':scope>summary').click();await expect(host).not.toHaveAttribute('open','');await host.locator(':scope>summary').click();
  await page.screenshot({path:`.cache/github-browser-${mobile?'mobile':'desktop'}-20261030.png`});
  const beforeIds=new Set((await api('/api/sessions')).map(s=>s.id));
  const row=page.locator('.github-browser-row[data-number="101"]');
  await row.scrollIntoViewIfNeeded();await page.evaluate(()=>new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve))));
  if(mobile)await row.getByRole('button',{name:'Open in session…'}).click();else{await row.click({button:'right'});await page.getByRole('menuitem',{name:'Open in session…'}).click();}
  await expect(page.locator('#create-dialog')).toBeVisible();await expect(page.locator('#workspace-mode')).toHaveValue('newWorktree');await expect(page.locator('#worktree-branch')).toHaveValue(/pr-101-feature\/topic-/);await expect(page.locator('#worktree-base')).toHaveValue(dataset.hosts[1].rows[0].pr.headSha);await expect(page.locator('#create-github-note')).toContainText('ghe-a.example.com/team/repo');
  await expect(page.locator('#session-title')).toHaveValue('PR #101: <img src=x onerror=alert(1)>');expect((await api('/api/sessions')).every(s=>beforeIds.has(s.id))).toBe(true);
  await page.locator('#provider').selectOption('demo');await page.locator('#directory').fill(repo);
  const submitted=page.waitForRequest(request=>request.url()===base+'/api/sessions'&&request.method()==='POST');await page.locator('#create-submit').click();const payload=(await submitted).postDataJSON();expect(payload.workspace.pullRequest.number).toBe(101);expect(payload).not.toHaveProperty('prompt');
  await expect(page.locator('#create-dialog')).toBeHidden();await expect(page.locator('#session-state')).toHaveText('Ready');const created=(await api('/api/sessions')).find(s=>!beforeIds.has(s.id));expect(created.cwd).toContain('.worktrees');expect(git(created.cwd,'rev-parse','HEAD')).toBe(dataset.hosts[1].rows[0].pr.headSha);
  const snapshot=await api(`/api/sessions/${created.id}`);expect(snapshot.events.filter(e=>e.kind==='user')).toEqual([]);
});

test('GitHub listing runs only in the visible view and per-host intervals save through settings',async({page})=>{
  await page.clock.install();let reads=0;await page.route('**/api/github/pulls',route=>{reads++;return route.fulfill({json:githubBrowserData()});});
  await page.goto(base);await page.locator('#nav-github').click();await expect.poll(()=>reads).toBe(1);
  await page.clock.fastForward(299000);expect(reads).toBe(1);await page.clock.fastForward(2000);await expect.poll(()=>reads).toBe(2);
  await page.locator('#nav-files').click();await page.clock.fastForward(600000);expect(reads).toBe(2);
  await page.locator('#nav-settings').evaluate(node=>node.click());await page.locator('#settings-tab-github').click();
  const minutes=page.getByLabel('Polling minutes for github.com');await expect(minutes).toHaveValue('5');await minutes.fill('10');await minutes.locator('..').locator('..').getByRole('button',{name:'Save interval'}).click();await expect(minutes.locator('..').locator('..')).toContainText('Saved');
  const config=await api('/api/github/connections');expect(config.polling.find(p=>p.host==='github.com').intervalSeconds).toBe(600);
  await api('/api/github/polling',{host:'github.com',intervalSeconds:300},'PUT');
});

test('each host keeps one table while its repository groups collapse independently',async({page})=>{
  const data=githubBrowserData();data.hosts=data.hosts.slice(0,1);const extra=structuredClone(data.hosts[0].rows[0]);extra.repository.name='other';extra.pr.number=104;extra.pr.url='https://github.com/team/other/pull/104';extra.badge.label='PR#104';extra.badge.url=extra.pr.url;data.hosts[0].rows.push(extra);
  await page.route('**/api/github/pulls',route=>route.fulfill({json:data}));await page.goto(base+'/?view=github');await expect(page.locator('.github-pr-table')).toHaveCount(1);await expect(page.locator('.github-browser-row')).toHaveCount(2);
  await page.getByRole('button',{name:'Toggle github.com/team/repo',exact:true}).click();await expect(page.locator('.github-browser-row[data-number="100"]')).toBeHidden();await expect(page.locator('.github-browser-row[data-number="104"]')).toBeVisible();
});
