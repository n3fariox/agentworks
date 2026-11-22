import {test,expect} from '@playwright/test';
import {spawn} from 'node:child_process';
import {mkdtemp,mkdir,rm} from 'node:fs/promises';
import {resolve,join} from 'node:path';
import WebSocket from 'ws';
import {createServer as createHttpsServer} from 'node:https';
import {request as httpRequest} from 'node:http';
import {execFileSync} from 'node:child_process';
import {readFile} from 'node:fs/promises';

// Own server, catalog, database and demo state on a dedicated port; never the live service.
const base='http://127.0.0.1:4413';
test.use({baseURL:base});
let fixture,server,log='',exited=false,watched,background;
let proxy;
const secureBase='https://localhost:4414';

async function api(path,body,method){
  const response=await fetch(base+path,{method:method||(body?'POST':'GET'),headers:{Origin:base,'Content-Type':'application/json'},body:body?JSON.stringify(body):undefined});
  expect(response.ok,await response.clone().text()).toBe(true);return response.json();
}
// Records every Notification the page creates instead of showing it.
async function captureNotifications(page,permission='granted'){
  await page.addInitScript(permission=>{
    window.__permissionReads=[];window.__permissionRequests=[];window.__permission=permission;window.__handles=[];
    const NativeSocket=window.WebSocket;window.__notificationConnected=false;
    window.WebSocket=class extends NativeSocket{constructor(url,...args){super(url,...args);if(String(url).endsWith('/api/notifications/events')){this.addEventListener('open',()=>window.__notificationConnected=true);}}};
    window.__notifications=[];
    class Fake{
      constructor(title,options){window.__notifications.push({title,body:options?.body||'',tag:options?.tag});window.__handles.push(this);}
      close(){}
    }
    Object.defineProperty(Fake,'permission',{get:()=>{window.__permissionReads.push(window.__permission);return window.__permission;}});
    Fake.requestPermission=async()=>{window.__permissionRequests.push(navigator.userActivation.isActive);window.__permission='granted';return 'granted';};
    window.Notification=Fake;globalThis.Notification=Fake;
  },permission);
}
const shown=page=>page.evaluate(()=>window.__notifications);

async function startServer(){
  exited=false;log='';
  const env={...process.env,
    AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:'4413',AGENTWORKS_ROOTS:fixture,
    AGENTWORKS_CATALOG_PATH:join(fixture,'catalog.json'),AGENTWORKS_DB_PATH:join(fixture,'sessions.sqlite3'),AGENTWORKS_DEMO_STATE_DIR:join(fixture,'demo')};
  for(const key of ['AGENTWORKS_SERVICE_DIR','NOTIFY_SOCKET','AGENTWORKS_RELEASE_DIR','INVOCATION_ID'])delete env[key];
  server=spawn(resolve('target/debug/agentworks'),[],{env,stdio:['ignore','pipe','pipe']});
  server.on('error',e=>{log+=e.message;exited=true;});server.on('exit',()=>exited=true);
  for(const stream of [server.stdout,server.stderr])stream.on('data',s=>{log+=String(s);});
  for(let i=0;i<100;i++){
    if(exited)throw new Error(log);
    try{if((await fetch(base+'/api/status')).ok)break;}catch{}
    await new Promise(r=>setTimeout(r,100));
    if(i===99)throw new Error('Isolated server readiness timed out: '+log);
  }
}
test.beforeAll(async()=>{
  await mkdir('.cache',{recursive:true});fixture=await mkdtemp(resolve('.cache/notifications-'));
  await startServer();
  // Preserve the browser's HTTPS Origin and public Host, as a root TLS proxy does.
  execFileSync('openssl',['req','-x509','-newkey','rsa:2048','-nodes','-keyout',join(fixture,'key.pem'),'-out',join(fixture,'cert.pem'),'-days','1','-subj','/CN=localhost'],{stdio:'ignore'});
  const upstream=req=>httpRequest({hostname:'127.0.0.1',port:4413,path:req.url,method:req.method,headers:{...req.headers,'x-forwarded-proto':'https','x-forwarded-host':req.headers.host}});
  proxy=createHttpsServer({key:await readFile(join(fixture,'key.pem')),cert:await readFile(join(fixture,'cert.pem'))},(req,res)=>{
    const request=upstream(req);request.on('response',response=>{res.writeHead(response.statusCode,response.headers);response.pipe(res);});
    request.on('error',()=>{res.writeHead(502);res.end();});req.pipe(request);
  });
  proxy.on('upgrade',(req,socket,head)=>{
    const request=upstream(req);
    request.on('upgrade',(response,backend,backendHead)=>{
      socket.write('HTTP/1.1 101 Switching Protocols\r\n'+Object.entries(response.headers).map(([key,value])=>`${key}: ${value}\r\n`).join('')+'\r\n');
      if(head.length)backend.write(head);if(backendHead.length)socket.write(backendHead);
      backend.on('error',()=>socket.destroy());socket.on('error',()=>backend.destroy());
      backend.pipe(socket);socket.pipe(backend);
    });
    request.on('response',response=>{socket.end(`HTTP/1.1 ${response.statusCode} Rejected\r\nConnection: close\r\n\r\n`);response.resume();});
    request.on('error',()=>socket.destroy());request.end();
  });
  await new Promise(resolve=>proxy.listen(4414,'127.0.0.1',resolve));
});
// The browser watches `watched`; events are produced by a different `background`
// session so a notice proves the cross-session stream, not the session socket.
test.beforeEach(async()=>{
  watched=await api('/api/sessions',{provider:'demo',cwd:fixture,title:'Watched session'});
  background=await api('/api/sessions',{provider:'demo',cwd:fixture,title:'Background session'});
});
test.afterEach(async()=>{
  await api('/api/notifications',{enabled:false,events:['turnEnd','approval','error'],projects:{},mutedSessions:[]},'PUT');
  for(const id of [background?.id,watched?.id])if(id)await api(`/api/sessions/${id}`,undefined,'DELETE').catch(()=>{});
});
test.afterAll(async()=>{
  if(server&&!exited){const stopped=new Promise(r=>server.once('exit',r));server.kill('SIGTERM');await stopped;}
  if(proxy){proxy.closeAllConnections();await new Promise(resolve=>proxy.close(resolve));}
  if(fixture)await rm(fixture,{recursive:true,force:true});
});
// Drive the background session until it is idle again.
async function run(id,text='approval'){
  await expect.poll(async()=> (await api(`/api/sessions/${id}`)).session.status).toBe('idle');
  await api(`/api/sessions/${id}/prompt`,{text,requestId:crypto.randomUUID()});
  for(let i=0;i<100;i++){
    const state=await api(`/api/sessions/${id}`);
    if(state.session.status==='idle'||state.session.status==='error')return state;
    if(state.approvals.length)await api(`/api/sessions/${id}/approvals/${state.approvals[0].id}`,{optionId:'allow'});
    await new Promise(r=>setTimeout(r,100));
  }
  throw new Error('Background session did not become idle');
}

async function watch(page){
  await captureNotifications(page);
  await page.addInitScript(id=>localStorage.setItem('agentworks-session',id),watched.id);
  await page.goto('/');
  await expect(page.locator('#session-state')).toHaveText('Ready');
  await page.waitForFunction(()=>window.__notificationConnected);
}

test('off by default: an event produces no notice until the policy allows it',async({page})=>{
  await watch(page);
  await run(background.id);
  expect(await shown(page)).toEqual([]);
});

test('an enabled policy notifies approvals and turn completion from another session',async({page})=>{
  await api('/api/notifications',{enabled:true,events:['turnEnd','approval','error'],projects:{},mutedSessions:[]},'PUT');
  await watch(page);
  await run(background.id,'approval');

  await expect.poll(async()=> (await shown(page)).some(n=>n.body==='Turn finished.')).toBe(true);
  const notices=await shown(page);
  expect(notices.length).toBeGreaterThan(0);
  // Every notice names its session and carries a stable per-event identifier.
  for(const notice of notices){
    expect(notice.tag).toMatch(/^[\w-]+:\d+$/);
    expect(notice.body).not.toContain('undefined');
    expect(notice.title).toBe('Background session');
  }
  expect(notices.some(n=>n.body==='Waiting for your approval.')).toBe(true);
  expect(notices.some(n=>n.body==='Turn finished.')).toBe(true);
});

test('the session you are watching does not notify you',async({page})=>{
  await api('/api/notifications',{enabled:true,events:['turnEnd','approval','error'],projects:{},mutedSessions:[]},'PUT');
  await watch(page);
  await run(watched.id,'approval');
  expect(await shown(page)).toEqual([]);
});

test('muting a session suppresses only that session',async({page})=>{
  await api('/api/notifications',{enabled:true,events:['turnEnd','approval','error'],projects:{},mutedSessions:[background.id]},'PUT');
  await watch(page);
  await run(background.id,'approval');
  expect(await shown(page)).toEqual([]);
});

test('a project rule overrides the global setting',async({page})=>{
  const project=await api('/api/projects',{name:'Quiet project'});
  await api(`/api/sessions/${background.id}/project`,{projectId:project.id});
  await api('/api/notifications',{enabled:true,events:['turnEnd','approval','error'],
    projects:{[project.id]:{enabled:false}},mutedSessions:[]},'PUT');
  await watch(page);
  await run(background.id,'approval');
  expect(await shown(page)).toEqual([]);
  await api(`/api/projects/${project.id}`,undefined,'DELETE');
});

test('a project rule can narrow categories without disabling them',async({page})=>{
  const project=await api('/api/projects',{name:'Completion only'});
  await api(`/api/sessions/${background.id}/project`,{projectId:project.id});
  await api('/api/notifications',{enabled:true,events:['turnEnd','approval','error'],
    projects:{[project.id]:{events:['turnEnd']}},mutedSessions:[]},'PUT');
  await watch(page);
  await run(background.id,'approval');
  await expect.poll(async()=> (await shown(page)).some(n=>n.body==='Turn finished.')).toBe(true);
  const notices=await shown(page);
  expect(notices.length).toBeGreaterThan(0);
  for(const notice of notices)expect(notice.body).toBe('Turn finished.');
  await api(`/api/projects/${project.id}`,undefined,'DELETE');
});

test('the same event is never shown twice, and a second tab does not repeat it',async({page,context})=>{
  await api('/api/notifications',{enabled:true,events:['turnEnd','approval','error'],projects:{},mutedSessions:[]},'PUT');
  await watch(page);
  await run(background.id,'approval');
  await expect.poll(async()=> (await shown(page)).some(n=>n.body==='Turn finished.')).toBe(true);
  const first=await shown(page);
  expect(first.length).toBeGreaterThan(0);

  // A second tab on the same origin must not repeat an already-shown notice.
  const second=await context.newPage();
  await captureNotifications(second);
  await second.addInitScript(id=>localStorage.setItem('agentworks-session',id),watched.id);
  await second.goto('/');
  await expect(second.locator('#session-state')).toHaveText('Ready');
  await new Promise(r=>setTimeout(r,500));
  expect(await shown(second)).toEqual([]);

  // A reload reconnects to a stream with no backlog, so nothing is replayed.
  await second.close();
  await page.reload();
  await expect(page.locator('#session-state')).toHaveText('Ready');
  await new Promise(r=>setTimeout(r,700));
  expect(await shown(page)).toEqual([]);

  // A new event after the reload is still shown exactly once.
  await run(background.id,'approval');
  await expect.poll(async()=> (await shown(page)).some(n=>n.body==='Turn finished.')).toBe(true);
  const after=await shown(page);
  expect(after.length).toBeGreaterThan(0);
  expect(new Set(after.map(n=>n.tag)).size).toBe(after.length);
});

test('clicking a notice opens the session that produced it',async({page})=>{
  await api('/api/notifications',{enabled:true,events:['turnEnd','approval','error'],projects:{},mutedSessions:[]},'PUT');
  await captureNotifications(page);
  await page.addInitScript(id=>localStorage.setItem('agentworks-session',id),watched.id);
  await page.goto('/');
  await expect(page.locator('#session-state')).toHaveText('Ready');
  await expect(page.locator('#header-title')).toHaveText('Watched session');
  await page.waitForFunction(()=>window.__notificationConnected);

  await run(background.id,'approval');
  await expect.poll(async()=> (await shown(page)).length).toBeGreaterThan(0);

  // Invoke the attached handler the way a notification click does.
  await page.evaluate(()=>window.__handles.at(-1).onclick());
  await expect(page.locator('#header-title')).toHaveText('Background session');
});

test('the stream and its settings are unavailable cross-origin',async()=>{
  const post=await fetch(base+'/api/notifications',{method:'PUT',headers:{Origin:'http://evil.example','Content-Type':'application/json'},body:'{}'});
  expect(post.status).toBe(403);
  const dismissal=await fetch(base+'/api/notifications/inbox/dismiss',{method:'POST',headers:{Origin:'http://evil.example','Content-Type':'application/json'},body:JSON.stringify({ids:[],dismissed:true})});
  expect(dismissal.status).toBe(403);
  // A cross-origin WebSocket upgrade is refused by the same guard.
  const socket=new WebSocket(base.replace('http','ws')+'/api/notifications/events',{headers:{Origin:'http://evil.example'}});
  const outcome=await new Promise(resolve=>{
    socket.on('open',()=>resolve('open'));
    socket.on('error',()=>resolve('error'));
    socket.on('close',()=>resolve('close'));
    setTimeout(()=>resolve('timeout'),3000);
  });
  expect(outcome).not.toBe('open');
});

test('settings report categories, projects and the saved policy',async({page})=>{
  await page.goto('/');
  await page.locator('#nav-settings').click();await page.locator('#settings-tab-notifications').click();
  await expect(page.locator('#notification-controls legend')).toHaveText('Notifications');
  await expect(page.locator('#notification-controls')).toContainText('Notify me about agent activity');
  // Permission is reported, never requested without a user action.
  await expect(page.locator('#notification-controls')).toContainText(/Allowed|Blocked|secure|requested/i);
  const payload=await api('/api/notifications');
  expect(payload.settings.enabled).toBe(false);
  expect(payload.categories.map(c=>c.id).sort()).toEqual(['approval','error','turnEnd']);
});

test('saved notification settings survive a restart and leave other settings alone',async()=>{
  await api('/api/notifications',{enabled:true,events:['approval'],projects:{},mutedSessions:[background.id]},'PUT');
  const stopped=new Promise(r=>server.once('exit',r));server.kill('SIGTERM');await stopped;
  await startServer();
  const saved=await api('/api/notifications');
  expect(saved.settings.enabled).toBe(true);
  expect(saved.settings.events).toEqual(['approval']);
  expect(saved.settings.mutedSessions).toEqual([background.id]);
  expect(saved.settings.projects).toEqual({});

  const {readFile}=await import('node:fs/promises');
  const catalog=JSON.parse(await readFile(join(fixture,'catalog.json'),'utf8'));
  expect(catalog.settings.notifications.enabled).toBe(true);
  // The saved timeouts default is untouched by a notification-only save.
  expect(catalog.settings.timeouts??null).toBeNull();
  expect(catalog.settings.roots??null).toBeNull();
});
// Real browser storage and WebSockets with deterministic event injection. The
// same event is delivered concurrently to already-connected tabs, then replayed.
async function harness(page,{permission='granted',insecure=false,unsupported=false,noBus=false}={}){
  await captureNotifications(page,permission);
  await page.addInitScript(({insecure,unsupported,noBus})=>{
    if(insecure)Object.defineProperty(globalThis,'isSecureContext',{value:false});
    if(unsupported)delete window.Notification;
    if(noBus){delete window.BroadcastChannel;Object.defineProperty(navigator,'locks',{value:undefined});}
  },{insecure,unsupported,noBus});
  const sockets=[];
  await page.routeWebSocket('**/api/notifications/events',ws=>sockets.push(ws));
  await page.route('**/',route=>route.fulfill({contentType:'text/html',body:'<fieldset id="controls"></fieldset>'}));
  await page.goto('/');
  await page.evaluate(async()=>{
    const {setupNotifications}=await import('/notifications.js');
    window.__client=setupNotifications({getSessions:()=>[],getActive:()=>null,selectSession:s=>window.__selected=s.id});
    await window.__client.start();window.__client.mount(document.querySelector('#controls'));
  });
  if(!insecure&&!unsupported)await expect.poll(()=>sockets.length).toBe(1);
  return sockets;
}
const event=(id,kind='turnEnd')=>JSON.stringify({id,sessionId:'source-session',title:'Source',event:kind,body:kind});

test('atomic claims prevent concurrent duplicates and reconnect/reload replays without locks or broadcast',async({page,context})=>{
  const first=await harness(page,{noBus:true});
  const secondPage=await context.newPage(),second=await harness(secondPage,{noBus:true});
  const total=async()=> (await shown(page)).length+(await shown(secondPage)).length;
  for(const ws of [first[0],second[0]]){ws.send(event('race:1'));ws.send(event('race:1'));}
  await expect.poll(total).toBe(1);
  // Distinct approvals must survive even when received milliseconds apart.
  for(const ws of [first[0],second[0]]){ws.send(event('race:2','approval'));ws.send(event('race:3','approval'));}
  await expect.poll(total).toBe(3);
  first[0].close();
  await expect.poll(()=>first.length).toBe(2);
  first[1].send(event('race:1'));first[1].send(event('race:4','error'));
  await expect.poll(total).toBe(4);
  await secondPage.close();
  await page.reload();
  await page.evaluate(async()=>{
    const {setupNotifications}=await import('/notifications.js');
    window.__client=setupNotifications({getActive:()=>null});await window.__client.start();
  });
  await expect.poll(()=>first.length).toBe(3);
  first[2].send(event('race:1'));first[2].send(event('race:5'));
  await expect.poll(async()=> (await shown(page)).length).toBe(1);
  expect((await shown(page))[0].tag).toBe('race:5');
});

test('permission is requested only by the explicit allow button; denied permission is respected',async({page})=>{
  const sockets=await harness(page,{permission:'default'});
  sockets[0].send(event('permission:1'));
  expect(await page.evaluate(()=>window.__permissionRequests)).toEqual([]);
  await page.locator('#notifications-enabled').check();
  expect(await page.evaluate(()=>window.__permissionRequests)).toEqual([]);
  await page.getByRole('button',{name:'Allow notifications in this browser'}).click();
  expect(await page.evaluate(()=>window.__permissionRequests)).toEqual([true]);
  sockets[0].send(event('permission:2'));
  await expect.poll(async()=> (await shown(page)).length).toBe(1);
  await page.evaluate(()=>window.__permission='denied');
  sockets[0].send(event('permission:3'));
  await page.waitForFunction(()=>window.__permissionReads.includes('denied'));
  await page.evaluate(()=>window.__permission='granted');
  sockets[0].send(event('permission:4'));
  await expect.poll(async()=> (await shown(page)).length).toBe(2);
});

for(const mode of ['insecure','unsupported'])test(`${mode} origins/browsers explain support and never request permission`,async({page})=>{
  const sockets=await harness(page,{[mode]:true,permission:'default'});
  await expect(page.locator('#controls')).toContainText(mode==='insecure'?'secure':'does not show notifications');
  await expect(page.locator('#notifications-enabled')).toHaveCount(0);
  await expect(page.locator('#notifications-auto-dismiss')).toBeVisible();
  expect(sockets).toEqual([]);
  expect(await page.evaluate(()=>window.__permissionRequests)).toEqual([]);
});

test('server failures notify and general settings saves preserve the policy',async({page})=>{
  const settings={enabled:true,events:['error'],projects:{},mutedSessions:[watched.id]};
  await api('/api/notifications',settings,'PUT');
  const general=await api('/api/settings');
  await api('/api/settings',general,'PUT');
  expect((await api('/api/notifications')).settings).toEqual(settings);
  await watch(page);
  await run(background.id,'quota generic');
  await expect.poll(async()=> (await shown(page)).length).toBe(1);
  expect((await shown(page))[0].body).toContain('rate limit quota');
});

test('repeated settings toggles retain project and session preferences',async({page})=>{
  const settings={enabled:false,events:['approval'],projects:{p:{enabled:false,events:null}},mutedSessions:[watched.id]};
  await api('/api/notifications',settings,'PUT');
  await harness(page);
  const toggle=page.locator('#notifications-enabled');
  await toggle.check();await expect(toggle).toBeEnabled();
  await toggle.uncheck();await expect(toggle).toBeEnabled();
  expect((await api('/api/notifications')).settings).toEqual(settings);
});

test('a click fetches a new session missing from the last sidebar refresh',async({page})=>{
  const sockets=await harness(page);
  await page.route('**/api/sessions/source-session',route=>route.fulfill({json:{session:{id:'source-session',title:'New session'}}}));
  sockets[0].send(event('click:1'));
  await expect.poll(async()=> (await shown(page)).length).toBe(1);
  await page.evaluate(()=>window.__handles[0].onclick());
  await page.waitForFunction(()=>window.__selected==='source-session');
});

test('the app Settings flow asks only on Allow and can test actual display',async({page})=>{
  await captureNotifications(page,'default');
  await page.goto('/');
  await page.locator('#nav-settings').click();await page.locator('#settings-tab-notifications').click();
  await expect(page.locator('#notification-controls legend')).toHaveText('Notifications');
  expect(await page.evaluate(()=>window.__permissionRequests)).toEqual([]);
  await page.locator('#notifications-enabled').check();
  await expect(page.locator('#notifications-enabled')).toBeEnabled();
  expect(await page.evaluate(()=>window.__permissionRequests)).toEqual([]);
  await page.getByRole('button',{name:'Allow notifications in this browser'}).click();
  expect(await page.evaluate(()=>window.__permissionRequests)).toEqual([true]);
  await expect(page.getByRole('button',{name:'Allow notifications in this browser'})).toHaveCount(0);
  await page.getByRole('button',{name:'Send a test notification'}).click();
  expect((await shown(page)).map(n=>n.body)).toEqual(['Notifications are working.']);
  // Both synchronous constructor and asynchronous browser failures are visible.
  await page.evaluate(()=>window.__handles[0].onerror());
  await expect(page.locator('#toast')).toContainText('could not display');
  await page.evaluate(()=>{window.Notification=class{static permission='granted';constructor(){throw new TypeError('Use a service worker');}};});
  await page.getByRole('button',{name:'Send a test notification'}).click();
  await expect(page.locator('#toast')).toContainText('could not display');
});

test('denied permission has recovery instructions and no Allow button',async({page})=>{
  await captureNotifications(page,'denied');
  await page.goto('/');await page.locator('#nav-settings').click();await page.locator('#settings-tab-notifications').click();
  await expect(page.locator('#notification-controls')).toContainText('Blocked for this site');
  await expect(page.getByRole('button',{name:'Allow notifications in this browser'})).toHaveCount(0);
  expect(await page.evaluate(()=>window.__permissionRequests)).toEqual([]);
});

test('settings API failures are visible in the actual Settings dialog',async({page})=>{
  await captureNotifications(page);
  await page.route('**/api/notifications',route=>route.fulfill({status:503,body:'Fixture unavailable'}));
  await page.goto('/');await page.locator('#nav-settings').click();await page.locator('#settings-tab-notifications').click();
  await expect(page.locator('#notification-controls')).toContainText('Could not load notification settings');
  await expect(page.locator('#notifications-enabled')).toBeDisabled();
});

test('HTTPS root proxy preserves Origin and supports permission, policy and event delivery',async({browser})=>{
  const context=await browser.newContext({ignoreHTTPSErrors:true});
  try{
    const page=await context.newPage();
    await captureNotifications(page,'default');
    await page.addInitScript(id=>localStorage.setItem('agentworks-session',id),watched.id);
    await page.goto(secureBase);
    expect(await page.evaluate(()=>isSecureContext)).toBe(true);
    await page.locator('#nav-settings').click();await page.locator('#settings-tab-notifications').click();
    await page.locator('#notifications-enabled').check();
    await expect(page.locator('#notifications-enabled')).toBeEnabled();
    expect((await api('/api/notifications')).settings.enabled).toBe(true);
    await page.getByRole('button',{name:'Allow notifications in this browser'}).click();
    expect(await page.evaluate(()=>window.__permissionRequests)).toEqual([true]);
    await page.waitForFunction(()=>window.__notificationConnected);
    await expect(page.locator('#notification-controls')).toContainText('event stream connected');
    await page.locator('#dismiss-settings').click();
    await run(background.id,'approval');
    await expect.poll(async()=> (await shown(page)).some(n=>n.body==='Turn finished.')).toBe(true);
  }finally{await context.close();}
});

test('forwarded origin is derived from proxy headers and must match browser Origin',async()=>{
  for(const origin of ['https://unconfigured.example','http://localhost:4414']){
    const response=await fetch(base+'/api/notifications',{method:'PUT',headers:{Origin:origin,Host:'localhost:4414','X-Forwarded-Proto':'https','X-Forwarded-Host':'localhost:4414','Content-Type':'application/json'},body:'{}'});
    expect(response.status).toBe(403);
    // Same headers fetch actually sends (it drops Host): a matching Host would be a
    // genuine same-origin loopback request, which tunnels rely on.
    const outcome=await new Promise(resolve=>{
      const ws=new WebSocket(base.replace('http','ws')+'/api/notifications/events',{headers:{Origin:origin,'X-Forwarded-Proto':'https','X-Forwarded-Host':'localhost:4414'}});
      ws.on('open',()=>{ws.close();resolve('open');});ws.on('error',error=>resolve(error.message));
    });
    expect(outcome).toContain('403');
  }
  const response=await fetch(base+'/api/notifications',{method:'PUT',headers:{Origin:'https://proxy.example','Host':'localhost:4413','X-Forwarded-Proto':'https','X-Forwarded-Host':'proxy.example','Content-Type':'application/json'},body:JSON.stringify({enabled:false})});
  expect(response.status).toBe(200);
});

test('real insecure HTTP hostname is explained even when Notification exists',async({page})=>{
  const code=await readFile('web/notifications.js','utf8');
  const hosting=await readFile('web/hosting.js','utf8');
  await page.route('http://insecure.example/**',route=>route.fulfill({contentType:route.request().url().endsWith('.js')?'text/javascript':'text/html',body:route.request().url().endsWith('/hosting.js')?hosting:route.request().url().endsWith('.js')?code:'<fieldset id="controls"></fieldset>'}));
  await page.goto('http://insecure.example/');
  await page.evaluate(async()=>{
    const {setupNotifications}=await import('/notifications.js');setupNotifications().mount(document.querySelector('#controls'));
  });
  expect(await page.evaluate(()=>isSecureContext)).toBe(false);
  await expect(page.locator('#controls')).toContainText('HTTPS or localhost');
  await expect(page.locator('#notifications-auto-dismiss')).toBeVisible();
  await expect(page.getByRole('button')).toHaveCount(0);
});

test('unavailable shared storage suppresses events while test display stays usable',async({page})=>{
  await page.addInitScript(()=>Object.defineProperty(window,'indexedDB',{get(){window.__storageAttempted=true;throw new Error('Storage unavailable');}}));
  const sockets=await harness(page);
  sockets[0].send(event('no-storage:1'));
  // A round trip through the event handler observes the failed storage claim.
  await page.waitForFunction(()=>window.__storageAttempted);
  await page.getByRole('button',{name:'Send a test notification'}).click();
  expect((await shown(page)).map(n=>n.tag)).toEqual(['test']);
});

async function clearInbox(){
  const {alerts}=await api('/api/notifications/inbox');
  if(alerts.length)await api('/api/notifications/inbox/dismiss',{ids:alerts.map(a=>a.id),dismissed:true});
}

test('persistent tray works without browser permission; opening keeps alerts until dismissal and Undo restores them',async({page,context})=>{
  await clearInbox();
  await context.addInitScript(()=>localStorage.setItem('agentworks-notification-auto-dismiss','false'));
  await captureNotifications(page,'denied');
  await page.goto('/');
  await run(background.id,'hello');
  const bell=page.locator('#nav-notifications');
  await expect(bell).toHaveAccessibleName('Notifications, 1 undismissed');
  await bell.click();
  const tray=page.locator('#notification-tray');
  await expect(tray.getByText('Turn finished',{exact:true})).toBeVisible();
  await tray.getByRole('link',{name:/Turn finished/}).click();
  await expect(page.locator('#header-title')).toHaveText('Background session');
  await expect(tray).not.toBeVisible();
  await page.reload();
  await expect(bell).toHaveAccessibleName('Notifications, 1 undismissed');
  const second=await context.newPage();await second.goto('/dashboard');
  await expect(second.locator('#nav-notifications')).toHaveAccessibleName('Notifications, 1 undismissed');
  await bell.click();
  await tray.getByRole('button',{name:'Dismiss Turn finished for Background session'}).click();
  await expect(bell).toHaveAccessibleName('Notifications');
  await expect(second.locator('#nav-notifications')).toHaveAccessibleName('Notifications');
  await tray.getByRole('button',{name:'Undo',exact:true}).click();
  await expect(bell).toHaveAccessibleName('Notifications, 1 undismissed');
  await expect(second.locator('#nav-notifications')).toHaveAccessibleName('Notifications, 1 undismissed');
  await tray.getByRole('button',{name:'Dismiss all',exact:true}).click();
  await expect(tray.getByText(/caught up/)).toBeVisible();
  await page.reload();await expect(bell).toHaveAccessibleName('Notifications');
  expect(await shown(page)).toEqual([]);
  await second.close();
});

test('tray groups approvals first and updates resolved requests without removing them',async({page})=>{
  await clearInbox();
  await page.addInitScript(()=>localStorage.setItem('agentworks-notification-auto-dismiss','false'));
  await page.goto('/dashboard');
  await api(`/api/sessions/${background.id}/prompt`,{text:'approval',requestId:crypto.randomUUID()});
  await expect.poll(async()=> (await api(`/api/sessions/${background.id}`)).approvals.length).toBe(1);
  await page.locator('#nav-notifications').click();
  const tray=page.locator('#notification-tray');
  await expect(tray.getByText('Needs attention',{exact:true})).toBeVisible();
  await expect(tray.getByText('Approval needed',{exact:true})).toBeVisible();
  const pending=(await api(`/api/sessions/${background.id}`)).approvals[0];
  await api(`/api/sessions/${background.id}/approvals/${pending.id}`,{optionId:'allow'});
  await expect(tray.getByText('Request resolved',{exact:true})).toBeVisible();
  await expect(tray.getByText('Turn finished',{exact:true})).toBeVisible();
  await page.screenshot({path:'.cache/notification-tray-desktop.png'});
  const count=(await api('/api/notifications/inbox')).alerts.length;
  expect(count).toBe(2);
  await tray.getByRole('link',{name:/Turn finished/}).click();
  await expect(page.locator('#header-title')).toHaveText('Background session');
  await expect(page.locator('#session-state')).toHaveText('Ready');
  await expect(page.locator('.notification-target')).toBeVisible();
  expect((await api('/api/notifications/inbox')).alerts.length).toBe(count);
});

test('subsequent turn ended event dismisses previous ones for the same session',async({page})=>{
  await clearInbox();
  await run(background.id,'hello');
  let {alerts}=await api('/api/notifications/inbox');
  expect(alerts).toHaveLength(1);
  const firstId=alerts[0].id;
  expect(alerts[0].sessionId).toBe(background.id);
  expect(alerts[0].event).toBe('turnEnd');

  // Running another turn in background session should dismiss the previous turnEnd alert
  await run(background.id,'hello again');
  ({alerts}=await api('/api/notifications/inbox'));
  expect(alerts).toHaveLength(1);
  expect(alerts[0].id).not.toBe(firstId);
  expect(alerts[0].sessionId).toBe(background.id);
  expect(alerts[0].event).toBe('turnEnd');

  // A turn in a different session does not dismiss background's turnEnd alert
  await run(watched.id,'hello watched');
  ({alerts}=await api('/api/notifications/inbox'));
  expect(alerts).toHaveLength(2);
  expect(alerts.some(a=>a.sessionId===background.id&&a.event==='turnEnd')).toBe(true);
  expect(alerts.some(a=>a.sessionId===watched.id&&a.event==='turnEnd')).toBe(true);
});

test('tray inbox and dismissal survive a server restart',async({page})=>{
  await clearInbox();await run(background.id,'hello');await run(watched.id,'hello');
  const {alerts}=await api('/api/notifications/inbox');expect(alerts).toHaveLength(2);
  await api('/api/notifications/inbox/dismiss',{ids:[alerts[0].id],dismissed:true});
  await page.goto('/dashboard');await page.locator('#nav-notifications').click();
  await expect(page.locator('.notification-card')).toHaveCount(1);
  const stopped=new Promise(r=>server.once('exit',r));server.kill('SIGTERM');await stopped;await startServer();
  expect((await api('/api/notifications/inbox')).alerts.map(a=>a.id)).toEqual([alerts[1].id]);
  await expect(page.locator('.notification-status')).toHaveText('1 undismissed · Live');
  await expect(page.locator('.notification-card')).toHaveCount(1);
});

test('tray reports load and dismissal failures and retains alerts',async({page})=>{
  await clearInbox();await run(background.id,'hello');
  await page.route('**/api/notifications/inbox',route=>route.fulfill({status:503,body:'Unavailable'}));
  await page.goto('/');await page.locator('#nav-notifications').click();
  await expect(page.locator('.notification-error')).toHaveText('Could not load notifications.');
  await page.unroute('**/api/notifications/inbox');
  await page.locator('#notification-tray').getByRole('button',{name:'Retry',exact:true}).click();
  await expect(page.locator('.notification-card')).toHaveCount(1);
  await page.route('**/api/notifications/inbox/dismiss',route=>route.fulfill({status:503,body:'Unavailable'}));
  await page.locator('#notification-tray').getByRole('button',{name:'Dismiss all',exact:true}).click();
  await expect(page.locator('.notification-error')).toHaveText('Could not save changes.');
  await expect(page.locator('.notification-card')).toHaveCount(1);
});

test('tray fits mobile and Escape returns focus to its bell',async({page})=>{
  await clearInbox();await run(background.id,'hello');await page.setViewportSize({width:390,height:844});
  await page.goto('/');await expect(page.locator('#nav-notifications')).toBeVisible();
  expect(await page.evaluate(()=>document.documentElement.scrollWidth)).toBeLessThanOrEqual(390);
  await page.locator('#nav-notifications').click();
  const tray=page.locator('#notification-tray');await expect(tray).toBeVisible();
  await page.screenshot({path:'.cache/notification-tray-mobile.png'});
  const bounds=await tray.boundingBox();expect(bounds.x).toBeGreaterThanOrEqual(0);expect(bounds.width).toBeLessThanOrEqual(390);
  await page.keyboard.press('Escape');await expect(tray).not.toBeVisible();await expect(page.locator('#nav-notifications')).toBeFocused();
});


test('tray distinguishes explicit task reports from turn endings and escapes report content',async({page})=>{
  await clearInbox();
  await expect.poll(async()=> (await api(`/api/sessions/${background.id}`)).session.status).toBe('idle');
  await api(`/api/sessions/${background.id}/agent-access`,{communicate:true},'PUT');
  const payload={status:'completed',summary:'<b>Finished the assigned task</b>',requestId:crypto.randomUUID()};
  await run(background.id,'mcp report_completion '+JSON.stringify(payload));
  await page.goto('/dashboard');await page.locator('#nav-notifications').click();
  const tray=page.locator('#notification-tray');
  await expect(tray.getByText('Task completed',{exact:true})).toBeVisible();
  await expect(tray.getByText('Turn finished',{exact:true})).toBeVisible();
  await expect(tray.getByText(payload.summary,{exact:true})).toBeVisible();
  await expect(tray.locator('.notification-body b')).toHaveCount(0);
  await run(background.id,'mcp report_completion '+JSON.stringify({...payload,status:'blocked',summary:'Need your input',requestId:crypto.randomUUID()}));
  await expect(tray.getByText('Task blocked',{exact:true})).toBeVisible();
  await expect(tray.getByText('Needs attention',{exact:true})).toBeVisible();
});

test('full-screen mobile sessions keep the bell and live count accessible in Chat and Terminal',async({page})=>{
  await clearInbox();await page.setViewportSize({width:390,height:844});
  await page.addInitScript(id=>{localStorage.setItem('agentworks-session',id);localStorage.setItem('agentworks-composer-source','true');},watched.id);
  await page.goto('/');await expect(page.locator('#session-state')).toHaveText('Ready');
  await expect(page.locator('#workspace-nav')).not.toBeVisible();
  const bell=page.locator('#session-notifications'),tray=page.locator('#notification-tray');
  await expect(bell).toBeVisible();await expect(bell).toHaveAccessibleName('Notifications');
  await run(background.id,'hello');await expect(bell).toHaveAccessibleName('Notifications, 1 undismissed');
  await page.locator('#prompt').fill('Keep this draft');
  await page.screenshot({path:'.cache/notification-tray-mobile-session.png'});
  await bell.click();await expect(tray).toBeVisible();await expect(tray.locator('.notification-card')).toHaveCount(1);
  await page.screenshot({path:'.cache/notification-tray-mobile-session-open.png'});
  await tray.getByRole('button',{name:'Dismiss all',exact:true}).click();
  await expect(bell).toHaveAccessibleName('Notifications');
  await tray.getByRole('button',{name:'Close notifications',exact:true}).click();
  await expect(bell).toBeFocused();await expect(page.locator('#prompt')).toHaveValue('Keep this draft');
  await expect(page.locator('#workspace-nav')).not.toBeVisible();
  await page.locator('#session-tab-terminal').click();
  await expect(bell).toBeVisible();await bell.click();await expect(tray).toBeVisible();
  await page.keyboard.press('Escape');await expect(bell).toBeFocused();
  await expect(page.locator('#session-terminal')).toBeVisible();
  await page.setViewportSize({width:320,height:700});await expect(bell).toBeInViewport();
  expect(await page.evaluate(()=>document.documentElement.scrollWidth)).toBeLessThanOrEqual(320);
  await page.setViewportSize({width:1280,height:800});
  await expect(bell).not.toBeVisible();await expect(page.locator('#nav-notifications')).toBeVisible();
});

test('Alt+N triggers notification tray with keyboard navigation, session browser indicates undismissed alerts, and auto-dismiss clears them on view',async({page})=>{
  await clearInbox();
  await page.goto('/');

  // Create alerts for background and watched sessions
  await run(background.id,'hello background');
  await run(watched.id,'hello watched');

  // Both sessions should be indicated with .has-notifications in the session browser
  const bgItem=page.locator(`#sessions .session-item[data-session-id="${background.id}"]`);
  const watchedItem=page.locator(`#sessions .session-item[data-session-id="${watched.id}"]`);
  await expect(bgItem).toHaveClass(/has-notifications/);
  await expect(watchedItem).toHaveClass(/has-notifications/);

  // Alt+N opens the tray and focuses the top notification link
  await page.keyboard.press('Alt+n');
  const tray=page.locator('#notification-tray');
  await expect(tray).toBeVisible();
  const links=tray.locator('.notification-link');
  await expect(links).toHaveCount(2);
  await expect(links.first()).toBeFocused();

  // ArrowDown focuses the second link
  await page.keyboard.press('ArrowDown');
  await expect(links.nth(1)).toBeFocused();

  // ArrowUp focuses the first link again
  await page.keyboard.press('ArrowUp');
  await expect(links.first()).toBeFocused();

  // Hitting Enter follows the link to the session
  const focusedSessionId=await links.first().getAttribute('data-session-id');
  await page.keyboard.press('Enter');
  await expect(tray).not.toBeVisible();

  // By default, viewing a session auto-dismisses its notifications
  const targetItem=page.locator(`#sessions .session-item[data-session-id="${focusedSessionId}"]`);
  await expect(targetItem).not.toHaveClass(/has-notifications/);

  // The other session still has notifications
  const otherSessionId=focusedSessionId===background.id?watched.id:background.id;
  const otherItem=page.locator(`#sessions .session-item[data-session-id="${otherSessionId}"]`);
  await expect(otherItem).toHaveClass(/has-notifications/);

  // Settings allows turning off auto-dismiss
  await page.locator('#nav-settings').click();
  await page.locator('#settings-tab-notifications').click();
  const autoDismissCheckbox=page.locator('#notifications-auto-dismiss');
  await expect(autoDismissCheckbox).toBeVisible();
  await expect(autoDismissCheckbox).toBeChecked();
  await autoDismissCheckbox.uncheck();
  await page.locator('#dismiss-settings').click();

  // Trigger another alert for the dismissed session
  await run(focusedSessionId,'hello again');
  await expect(targetItem).toHaveClass(/has-notifications/);

  // Switching to the other session and then clicking targetItem with auto-dismiss off does not dismiss its notification
  await otherItem.click();
  await targetItem.click();
  await expect(targetItem).toHaveClass(/has-notifications/);
  const remainingAlerts=(await api('/api/notifications/inbox')).alerts.filter(a=>a.sessionId===focusedSessionId);
  expect(remainingAlerts.length).toBeGreaterThan(0);
});

test('project heading in session browser is bold when any of its non-archived sub-sessions are bold, and un-bolds when dismissed',async({page})=>{
  await clearInbox();
  const project=await api('/api/projects',{name:'Alpha Project'});
  const s1=await api('/api/sessions',{provider:'demo',cwd:fixture,title:'Alpha Session 1',projectId:project.id});
  const s2=await api('/api/sessions',{provider:'demo',cwd:fixture,title:'Alpha Session 2',projectId:project.id});
  const s3=await api('/api/sessions',{provider:'demo',cwd:fixture,title:'Alpha Archived',projectId:project.id});

  // Generate an alert on s3, then archive s3
  await run(s3.id,'alert on s3');
  await api(`/api/sessions/${s3.id}`,{archived:true},'PATCH');

  await page.goto('/');
  const projectHeading=page.locator(`#sessions .tree-heading[data-project-group="${project.id}"]`);
  await expect(projectHeading).toBeVisible();
  // Project heading should NOT be bold because s3 is archived
  await expect(projectHeading).not.toHaveClass(/has-notifications/);

  // Trigger an alert on non-archived s1: project heading should become bold
  await run(s1.id,'alert on s1');
  await expect(projectHeading).toHaveClass(/has-notifications/);
  const s1Item=page.locator(`#sessions .session-item[data-session-id="${s1.id}"]`);
  await expect(s1Item).toHaveClass(/has-notifications/);

  // Trigger an alert on non-archived s2 as well
  await run(s2.id,'alert on s2');
  await expect(projectHeading).toHaveClass(/has-notifications/);

  // View s1: s1 auto-dismisses, but s2 still has an active alert, so project heading remains bold
  await s1Item.click();
  await expect(s1Item).not.toHaveClass(/has-notifications/);
  await expect(projectHeading).toHaveClass(/has-notifications/);

  // View s2: s2 auto-dismisses, leaving no non-archived sessions with alerts
  const s2Item=page.locator(`#sessions .session-item[data-session-id="${s2.id}"]`);
  await s2Item.click();
  await expect(s2Item).not.toHaveClass(/has-notifications/);

  // Project heading should now un-bold
  await expect(projectHeading).not.toHaveClass(/has-notifications/);

  for(const id of [s1.id,s2.id,s3.id])await api(`/api/sessions/${id}`,undefined,'DELETE').catch(()=>{});
  await api(`/api/projects/${project.id}`,undefined,'DELETE').catch(()=>{});
});
