// Isolated executable, empty data store, synthetic read responses, and real TLS
// StripPrefix proxy. Build first with CARGO_TARGET_DIR=.cache/b009-u10-target.
import assert from 'node:assert/strict';
import {chromium} from 'playwright';
import {spawn, execFileSync} from 'node:child_process';
import {mkdtemp, readFile, rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join, resolve} from 'node:path';
import {createServer} from 'node:https';
import {request} from 'node:http';

const upstream = 'http://127.0.0.1:4531', origin = 'https://localhost:4532', mount = '/proxy/workspace/';
const fixture = await mkdtemp(join(tmpdir(), 'agentworks-u10-'));
const env = {...process.env, AGENTWORKS_BIND:'127.0.0.1', AGENTWORKS_PORT:'4531',
  AGENTWORKS_ROOTS:fixture, AGENTWORKS_CATALOG_PATH:join(fixture,'catalog.json'),
  AGENTWORKS_DB_PATH:join(fixture,'sessions.sqlite3'), AGENTWORKS_DEMO_STATE_DIR:join(fixture,'demo')};
for (const key of Object.keys(env)) if (/^AGENTWORKS_(SUPERVISOR|RELEASE)/.test(key)) delete env[key];
const server = spawn(resolve(process.env.AGENTWORKS_DASHBOARD_TEST_BINARY || '.cache/b009-u10-target/debug/agentworks'), [], {env, stdio:['ignore','pipe','pipe']});
let log = '', proxy, browser;
for (const stream of [server.stdout,server.stderr]) stream.on('data', chunk => log += chunk);
const sockets = new Set();
try {
  let ready = false;
  for (let i = 0; i < 100; i++) {
    try {if ((await fetch(upstream+'/api/status')).ok) {ready = true; break;}} catch {}
    await new Promise(resolve => setTimeout(resolve,100));
  }
  assert.ok(ready,log);
  assert.deepEqual(await (await fetch(upstream+'/api/sessions')).json(), []);
  for (const [path, mime] of [['/dashboard','text/html'],['/dashboard.js','text/javascript'],['/dashboard.css','text/css']]) {
    const response = await fetch(upstream+path); assert.equal(response.status,200); assert.ok(response.headers.get('content-type').startsWith(mime));
    assert.equal(response.headers.get('cache-control'),'no-store');
  }
  execFileSync('openssl',['req','-x509','-newkey','rsa:2048','-nodes','-keyout',join(fixture,'key.pem'),'-out',join(fixture,'cert.pem'),'-days','1','-subj','/CN=localhost'],{stdio:'ignore'});
  proxy = createServer({key:await readFile(join(fixture,'key.pem')),cert:await readFile(join(fixture,'cert.pem'))}, (req,res) => {
    const backend = request({hostname:'127.0.0.1',port:4531,method:req.method,
      path:req.url.startsWith(mount)?req.url.slice(mount.length-1):req.url,
      headers:{...req.headers,'x-forwarded-proto':'https','x-forwarded-host':req.headers.host}});
    backend.on('response', response => {res.writeHead(response.statusCode,response.headers);response.pipe(res);});
    backend.on('error', () => {res.writeHead(502);res.end();}); req.pipe(backend);
  });
  proxy.on('connection',socket => {sockets.add(socket);socket.on('close',()=>sockets.delete(socket));});
  await new Promise(resolve => proxy.listen(4532,'127.0.0.1',resolve));
  browser = await chromium.launch({headless:true});
  const context = await browser.newContext({ignoreHTTPSErrors:true});
  const page = await context.newPage();
  const errors = []; page.on('pageerror',error=>errors.push(error.message));
  await page.goto(upstream+'/dashboard');
  await page.getByRole('button',{name:'Refresh dashboard'}).waitFor();
  await page.waitForFunction(()=>!document.querySelector('#refresh').disabled);
  assert.match(await page.locator('#hosts').innerText(),/Local/);
  assert.match(await page.locator('#hosts').innerText(),/0 active turns/);
  assert.match(await page.locator('#accounts').innerText(),/Quota unavailable|No cap reported/);
  console.log('PASS real Rust hosting and empty read APIs');
  await page.goto(origin+mount);
  await page.locator('#nav-dashboard').waitFor();
  assert.equal(await page.locator('#nav-dashboard').getAttribute('href'),'./dashboard');
  assert.equal(await page.locator('#workspace-nav .workspace-nav-item').first().getAttribute('id'),'nav-dashboard');
  errors.length = 0;
  const sessions = [
    {id:'cx-one',title:'Codex source one',provider:'codex',status:'running',turnRunning:true,projectId:'p1',hostId:'local',created:20},
    {id:'cx-two',title:'Codex source two',provider:'codex',status:'waiting',projectId:'p1',hostId:'local'},
    {id:'cl-one',title:'Claude <script> source',provider:'claude',status:'idle',projectId:'p1',hostId:'local'},
    {id:'wait-input',title:'Waiting input',provider:'demo',status:'waiting',projectId:'p1',hostId:'local'},
    {id:'suspended',title:'Archived session',provider:'demo',status:'suspended',archived:true,projectId:'p2',hostId:'remote'},
    {id:'closed',title:'Closed session',provider:'demo',status:'closed',projectId:null,hostId:'local'},
    {id:'error',title:'Failed session',provider:'demo',status:'error',projectId:'p2',hostId:'remote'},
    {id:'unknown',title:'Future state',provider:'demo',status:'future-state',projectId:'missing',hostId:'unreported'},
    {id:'no-usage',title:'No usage reported',provider:'claude',status:'idle',projectId:null,hostId:'local'},
    {id:'bad-snapshot',title:'Unavailable snapshot',provider:'codex',status:'idle',projectId:null,hostId:'local'},
  ];
  const now = Date.now(), secret = 'FIXTURE-CREDENTIAL-MUST-NOT-RENDER';
  const usage = data => ({kind:'acp',seq:4,data:{sessionUpdate:'usage_update',...data}});
  const codex = percent => usage({_meta:{'_codex/subscriptionLimits':{version:1,limits:[{limitId:'codex',primary:{usedPercent:{value:percent,observedAtMs:now-700000},windowDurationMins:{value:300,observedAtMs:now},resetsAt:{value:now/1000+7200,observedAtMs:now}},secondary:{usedPercent:{value:80}}}]}}});
  const snapshots = {
    'cx-one':{events:[{kind:'acp',data:{sessionUpdate:'user_message_chunk',content:{text:secret}}},codex(31)],truncated:true},
    'cx-two':{events:[codex(70)]},
    'cl-one':{events:[usage({used:33,size:100,cost:{amount:1.5,currency:'USD'},_meta:{'_claude/rateLimit':{unifiedWindows:{five_hour:{utilization:.25,resetsAt:now/1000+3600}}}}})]},
    'no-usage':{events:[]},
  };
  let failed = new Set(), empty = false;
  const requests = [], websockets = [];
  page.on('request',req=>requests.push({url:req.url(),method:req.method()}));
  page.on('websocket',ws=>websockets.push(ws.url()));
  await page.route('**/api/**',async route => {
    const url = new URL(route.request().url()); assert.ok(url.pathname.startsWith(mount+'api/'),url.href);
    const path = url.pathname.slice(mount.length);
    if (failed.has(path)) return route.fulfill({status:503,body:'fixture failure'});
    let body;
    if (path === 'api/catalog') body = {hosts:[{id:'local',name:'Local',status:'connected'},{id:'remote',name:'Remote',status:'disconnected'}],projects:[{id:'p1',name:'Active project'},{id:'p2',name:'Archived project',archived:true},{id:'p0',name:'Empty project'}],locations:[]};
    else if (path === 'api/sessions') {assert.equal(url.searchParams.has('archived'),false);body = empty?[]:sessions;}
    else if (path === 'api/status') body = {providers:[{id:'claude',installed:true,authMode:'existing',keyPresent:false},{id:'codex',installed:true,authMode:'api_key',keyPresent:true}],credentials:secret};
    else if (snapshots[decodeURIComponent(path.split('/').at(-1))]) body = snapshots[decodeURIComponent(path.split('/').at(-1))];
    else return route.fulfill({status:404,body:'not found'});
    await route.fulfill({contentType:'application/json',body:JSON.stringify(body)});
  });
  await page.locator('#nav-dashboard').click();
  await page.waitForURL(origin+mount+'dashboard');
  assert.equal(await page.locator('#workspace-nav').isVisible(),true);
  await page.waitForFunction(()=>!document.querySelector('#refresh').disabled);
  assert.equal(await page.title(),'Operations dashboard · Agentworks');
  const hostText = await page.locator('#hosts').innerText();
  assert.match(hostText,/connected/); assert.match(hostText,/disconnected/); assert.match(hostText,/1 active turn/);
  const active = page.locator('details.project').filter({has:page.locator('summary',{hasText:'Active project'})});
  assert.match(await active.locator('summary').innerText(),/1 active turn/);
  await active.locator('summary').click();
  assert.match(await active.innerText(),/Taking a turn/);
  assert.doesNotMatch(await active.innerText(),/Waiting input|Archived session|Closed session|Failed session/);
  const archived = page.locator('details.project').filter({has:page.locator('summary',{hasText:'Archived project'})});
  assert.match(await archived.locator('summary').innerText(),/0 active turns/);
  assert.doesNotMatch(await page.locator('#projects').innerText(),/Archived session/);
  assert.match(await page.locator('main').innerText(),/Account identity and authentication are not verified/);
  assert.match(await page.locator('#accounts').innerText(),/configured \(unverified\)/);
  assert.match(await page.locator('#accounts').innerText(),/Freshness unknown/);
  assert.match(await page.locator('#accounts').innerText(),/stale/);
  assert.equal(await page.locator('#accounts h4.quota-state').count(),2);
  assert.equal(await page.locator('meter').count(),3);
  assert.equal(await page.getByLabel(/primary.*quota used: 31.0%/).getAttribute('value'),'31');
  assert.equal(await page.getByLabel(/primary.*quota used/).count(),1);
  const primaryDetail = page.locator('.usage-meter').filter({has:page.locator('label',{hasText:/primary/})}).locator('.usage-detail');
  assert.match(await primaryDetail.innerText(),/Resets in 2h/);
  assert.match(await primaryDetail.innerText(),/Reset freshness: Adapter observed/);
  assert.match(await page.locator('.usage-meter').filter({has:page.locator('label',{hasText:/secondary/})}).innerText(),/Reset unavailable/);
  assert.match(await page.locator('.usage-meter').filter({has:page.locator('label',{hasText:/Reported 5h/})}).innerText(),/Resets in 1h/);
  await page.getByRole('button',{name:'Refresh dashboard',exact:true}).click();
  await page.waitForFunction(()=>!document.querySelector('#refresh').disabled);
  assert.match(await primaryDetail.innerText(),/Resets in 2h/);

  assert.ok(!(await page.locator('body').innerText()).includes(secret));
  assert.equal(await page.locator('#accounts script').count(),0);
  assert.equal(await page.locator('meter').evaluateAll(nodes=>nodes.every(n=>n.labels.length>0&&n.getAttribute('aria-describedby'))),true);
  assert.equal(await page.getByRole('heading',{level:1}).count(),1);
  // Begin a fresh keyboard/read-only pass after the main app has navigated away.
  // Its notification stream can start while the dashboard link is being clicked.
  requests.length = 0; websockets.length = 0;
  await page.reload();
  await page.waitForFunction(()=>!document.querySelector('#refresh').disabled);
  // A manual quota refresh must leave the host/project DOM (including open
  // details) intact, reread only GET snapshots, and preserve source freshness.
  const project = page.locator('details.project').first();
  await project.locator('summary').click();
  await page.evaluate(() => {window.originalHosts = document.querySelector('#hosts').firstChild; window.originalProjects = document.querySelector('#projects').firstChild;});
  requests.length = 0;
  snapshots['cx-one'].events.push(codex(45));
  await page.getByRole('button',{name:'Refresh quota observations',exact:true}).click();
  await page.waitForFunction(()=>!document.querySelector('#refresh-quota').disabled);
  assert.equal(await page.evaluate(() => window.originalHosts === document.querySelector('#hosts').firstChild && window.originalProjects === document.querySelector('#projects').firstChild),true);
  assert.equal(await project.getAttribute('open'),'');
  assert.equal(await page.getByLabel(/primary.*quota used: 45.0%/).getAttribute('value'),'45');
  assert.match(await page.locator('#quota-refresh-status').innerText(),/not a live account refresh/);
  assert.match(await primaryDetail.innerText(),/Resets in 2h/);
  assert.match(await page.locator('#accounts').innerText(),/stale/);
  assert.match(await page.locator('#accounts').innerText(),/1 unavailable · 1 truncated/);
  assert.ok(!requests.some(req=>req.url.includes('api/catalog')));
  assert.ok(requests.some(req=>req.url.endsWith('api/sessions/cx-one')));
  // A failed quota refresh clears quota rather than presenting stale cache as a
  // successful refresh, while the workspace cards remain untouched.
  failed = new Set(['api/sessions']);
  await page.getByRole('button',{name:'Refresh quota observations',exact:true}).click();
  await page.waitForFunction(()=>!document.querySelector('#refresh-quota').disabled);
  assert.match(await page.locator('#accounts').innerText(),/Quota unavailable/);
  assert.equal(await page.locator('meter').count(),0);
  assert.match(await page.locator('#quota-refresh-status').innerText(),/reads failed/);
  assert.equal(await page.evaluate(() => window.originalHosts === document.querySelector('#hosts').firstChild && window.originalProjects === document.querySelector('#projects').firstChild),true);
  failed = new Set();
  snapshots['cl-one'] = {events:[usage({cost:{amount:42,currency:'USD'},_meta:{'_claude/rateLimit':{plan:'enterprise'}}})]};
  await page.getByRole('button',{name:'Refresh quota observations',exact:true}).click();
  await page.waitForFunction(()=>!document.querySelector('#refresh-quota').disabled);
  assert.match(await page.locator('#accounts').innerText(),/No cap reported/);
  assert.ok(!(await page.locator('#accounts').innerText()).includes('42.00'));
  console.log('PASS isolated quota refresh, failure/recovery, unchanged workspace DOM, coverage and no unlimited/spend inference');
  await page.reload();
  await page.waitForFunction(()=>!document.querySelector('#refresh').disabled);
  await page.keyboard.press('Tab');
  assert.equal(await page.evaluate(()=>document.activeElement.textContent),'Skip to dashboard');
  await page.keyboard.press('Enter');
  assert.equal(await page.evaluate(()=>document.activeElement.id),'content');
  await page.setViewportSize({width:1280,height:900});
  await page.emulateMedia({colorScheme:'dark'});
  await page.screenshot({path:'.cache/u10-dashboard-desktop.png',fullPage:true});
  for (const width of [1280,768,390,320]) {
    await page.setViewportSize({width,height:850});
    assert.ok(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),`overflow at ${width}`);
  }
  await page.emulateMedia({colorScheme:'light',reducedMotion:'reduce'});
  await page.screenshot({path:'.cache/u10-dashboard-mobile.png',fullPage:true});
  console.log('PASS HTTPS proxy mount, active turns, top-level quota, freshness, keyboard, responsive 320–1280px');
  failed = new Set(['api/catalog','api/sessions','api/status']);
  await page.getByRole('button',{name:'Refresh dashboard'}).click();
  await page.waitForFunction(()=>!document.querySelector('#refresh').disabled);
  assert.equal(await page.locator('#errors .error').count(),3);
  assert.match(await page.locator('#hosts').innerText(),/unavailable/);
  assert.match(await page.locator('#projects').innerText(),/unavailable/);
  assert.equal(await page.locator('meter').count(),0);
  assert.ok(!(await page.locator('#hosts').innerText()).includes('0 active turns'));
  failed = new Set(); empty = true;
  await page.getByRole('button',{name:'Refresh dashboard'}).click();
  await page.waitForFunction(()=>!document.querySelector('#refresh').disabled);
  assert.equal(await page.locator('#errors .error').count(),0);
  assert.match(await page.locator('#accounts').innerText(),/Quota unavailable|No cap reported/);
  assert.equal(await page.locator('meter').count(),0);
  assert.deepEqual(errors,[]);
  assert.deepEqual(websockets,[]);
  assert.ok(requests.every(req=>req.method==='GET'),JSON.stringify(requests));
  assert.ok(requests.every(req=>req.url.startsWith(origin+mount)),JSON.stringify(requests));
  assert.ok(!requests.some(req=>/models|external-sessions|prompt|events|usage\//.test(req.url)));
  console.log('PASS unavailable states, refresh recovery, empty data, no writes/streams/account-service requests or browser errors');
} finally {
  if (browser) await browser.close();
  for (const socket of sockets) socket.destroy();
  if (proxy) await new Promise(resolve=>proxy.close(resolve));
  if (server.exitCode === null) {const stopped = new Promise(resolve=>server.once('exit',resolve));server.kill('SIGTERM');await stopped;}
  await rm(fixture,{recursive:true,force:true});
}
