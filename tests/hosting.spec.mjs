import {test, expect} from '@playwright/test';
import {spawn, execFileSync} from 'node:child_process';
import {mkdtemp, readFile, rm, writeFile} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join, resolve} from 'node:path';
import {createServer} from 'node:https';
import {request} from 'node:http';

// Isolated executable/data plus a real TLS + StripPrefix proxy. No live service.
const upstream = 'http://127.0.0.1:4423';
const origin = 'https://localhost:4424';
const mounts = ['/a1b918dd-2b91-4fc2-8822-e6a559503040/', '/second-instance/'];
let fixture, server, proxy, session, workerRevision = 0, log = '';
const sockets = new Set();
// Chromium's service-worker fetch does not honor context ignoreHTTPSErrors.
// This dedicated fixture browser visits only the isolated loopback proxy.
test.use({ignoreHTTPSErrors: true, launchOptions: {args: ['--ignore-certificate-errors']}});
test.describe.configure({mode: 'serial'});

async function api(path, body, method) {
  const response = await fetch(upstream + path, {method: method || (body ? 'POST' : 'GET'),
    headers: {Origin: upstream, 'Content-Type': 'application/json'}, body: body && JSON.stringify(body)});
  expect(response.ok, await response.clone().text()).toBe(true);
  return response.json();
}
async function start(enabled) {
  const env = {...process.env, AGENTWORKS_BIND: '127.0.0.1', AGENTWORKS_PORT: '4423',
    AGENTWORKS_ROOTS: fixture,
    AGENTWORKS_CATALOG_PATH: join(fixture, 'catalog.json'), AGENTWORKS_DB_PATH: join(fixture, 'sessions.sqlite3'),
    AGENTWORKS_DEMO_STATE_DIR: join(fixture, 'demo')};
  // Exercise the actual default, regardless of the parent process setting.
  if (enabled) delete env.AGENTWORKS_PWA;
  else env.AGENTWORKS_PWA = '0';
  for (const key of ['AGENTWORKS_SERVICE_DIR','NOTIFY_SOCKET','AGENTWORKS_RELEASE_DIR','INVOCATION_ID']) delete env[key];
  server = spawn(resolve('target/debug/agentworks'), [], {env, stdio: ['ignore', 'pipe', 'pipe']});
  let exited = false;
  server.on('exit', () => exited = true);
  server.on('error', error => {log += error.message; exited = true;});
  for (const stream of [server.stdout, server.stderr]) stream.on('data', data => log += data);
  for (let i = 0; i < 100; i++) {
    if (exited) throw new Error(log);
    try {if ((await fetch(upstream + '/api/status')).ok) return;} catch {}
    await new Promise(resolve => setTimeout(resolve, 100));
  }
  throw new Error('Fixture not ready: ' + log);
}
async function stop() {
  if (!server || server.exitCode !== null) return;
  const exited = new Promise(resolve => server.once('exit', resolve));
  server.kill('SIGTERM'); await exited;
}
test.beforeAll(async () => {
  fixture = await mkdtemp(join(tmpdir(), 'agentworks-hosting-'));
  await writeFile(join(fixture, 'hello #%.md'), 'Hello from the mounted workspace.\n\n```mermaid\nsequenceDiagram\nAlice->>Bob: Hello\n```\n');
  await start(true);
  session = await api('/api/sessions', {provider: 'demo', cwd: fixture, title: 'Mounted session'});
  execFileSync('openssl', ['req','-x509','-newkey','rsa:2048','-nodes','-keyout',join(fixture,'key.pem'),
    '-out',join(fixture,'cert.pem'),'-days','1','-subj','/CN=localhost'], {stdio: 'ignore'});
  const forward = req => {
    const mount = mounts.find(mount => req.url.startsWith(mount));
    return request({hostname: '127.0.0.1', port: 4423, method: req.method,
      path: mount ? req.url.slice(mount.length - 1) : req.url,
      headers: {...req.headers, 'x-forwarded-proto': 'https', 'x-forwarded-host': req.headers.host,
        'x-forwarded-prefix': mount?.slice(0, -1) || ''}});
  };
  proxy = createServer({key: await readFile(join(fixture,'key.pem')), cert: await readFile(join(fixture,'cert.pem'))}, (req,res) => {
    const url = new URL(req.url, origin);
    if (mounts.some(mount => url.pathname === mount.slice(0, -1))) {
      res.writeHead(308, {Location: url.pathname + '/' + url.search}); res.end(); return;
    }
    if (workerRevision && url.pathname.endsWith('/service-worker.js')) {
      res.writeHead(200, {'Content-Type':'text/javascript','Cache-Control':'no-store'});
      readFile('web/service-worker.js','utf8').then(source => res.end(source + `\n// fixture revision ${workerRevision}\n`));
      return;
    }
    const backend = forward(req);
    backend.on('response', response => {res.writeHead(response.statusCode, response.headers); response.pipe(res);});
    backend.on('error', () => {res.writeHead(502); res.end();}); req.pipe(backend);
  });
  proxy.on('connection', socket => {sockets.add(socket); socket.on('close', () => sockets.delete(socket));});
  proxy.on('upgrade', (req, socket, head) => {
    const backend = forward(req);
    backend.on('upgrade', (response, stream, backendHead) => {
      socket.write('HTTP/1.1 101 Switching Protocols\r\n' + Object.entries(response.headers).map(([key,value]) => `${key}: ${value}\r\n`).join('') + '\r\n');
      if (head.length) stream.write(head); if (backendHead.length) socket.write(backendHead);
      stream.on('error', () => socket.destroy()); socket.on('error', () => stream.destroy());
      stream.pipe(socket); socket.pipe(stream);
    });
    backend.on('response', response => {socket.end(`HTTP/1.1 ${response.statusCode} Rejected\r\nConnection: close\r\n\r\n`); response.resume();});
    backend.on('error', () => socket.destroy()); backend.end();
  });
  await new Promise(resolve => proxy.listen(4424, '127.0.0.1', resolve));
});
test.afterAll(async () => {
  for (const socket of sockets) socket.destroy();
  if (proxy) await new Promise(resolve => proxy.close(resolve));
  await stop();
  if (fixture) await rm(fixture, {recursive: true, force: true});
});

for (const mount of ['/', ...mounts]) test(`loads app, APIs, files and scoped install assets at ${mount}`, async ({page, context}) => {
  const requests = [], errors = [], websockets = [];
  page.on('request', req => requests.push(req.url()));
  page.on('pageerror', error => errors.push(error.message));
  page.on('websocket', socket => websockets.push(socket.url()));
  await page.addInitScript(({id, mount}) => {
    if(window!==window.top)return;
    localStorage.setItem('agentworks-session' + (mount === '/' ? '' : '@' + mount), id);
  }, {id: session.id, mount});
  await page.goto(origin + mount);
  await expect(page.locator('#session-state')).toHaveText('Ready');
  await expect.poll(() => page.evaluate(() => navigator.serviceWorker.getRegistrations().then(rs => rs.filter(r => r.active).map(r => r.scope)))).toContain(origin + mount);
  const result = await page.evaluate(async () => {
    const {appBase, appUrl} = await import('./hosting.js');
    const registration = await navigator.serviceWorker.ready;
    const manifestURL = document.querySelector('link[rel=manifest]').href;
    const response = await fetch(manifestURL);
    const manifest = await response.json();
    return {scope: registration.scope, script: registration.active.scriptURL, manifest, manifestURL,
      mime: response.headers.get('content-type'), base: appBase.href, caches: await caches.keys()};
  });
  expect(result.scope).toBe(origin + mount);
  expect(result.script).toBe(origin + mount + 'service-worker.js');
  expect(result.mime).toContain('application/manifest+json');
  expect(new URL(result.manifest.start_url, result.manifestURL).href).toBe(origin + mount);
  expect(new URL(result.manifest.scope, result.manifestURL).href).toBe(origin + mount);
  expect(result.manifest.display).toBe('standalone');
  expect(result.caches).toEqual([]);
  for (const icon of result.manifest.icons) {
    const response = await context.request.get(new URL(icon.src, result.manifestURL).href);
    expect(response.status()).toBe(200);
    const png = await response.body();
    expect(png.readUInt32BE(16)).toBe(parseInt(icon.sizes));
    expect(png.readUInt32BE(20)).toBe(parseInt(icon.sizes));
  }
  const fileURL = origin + mount + `api/sessions/${session.id}/file?path=${encodeURIComponent(join(fixture,'hello #%.md'))}&download=true`;
  const file = await context.request.get(fileURL);
  expect(file.status()).toBe(200);
  expect(await file.text()).toContain('mounted workspace');
  const status = await page.evaluate(async () => {
    const {appUrl} = await import('./hosting.js');
    const response = await fetch(appUrl('/api/notifications'), {method: 'PUT', headers: {'Content-Type':'application/json'},
      body: JSON.stringify({enabled:false,events:['turnEnd','approval','error'],projects:{},mutedSessions:[]})});
    return response.status;
  });
  expect(status).toBe(200);
  await expect.poll(() => websockets.some(url => url.endsWith(`${mount}api/sessions/${session.id}/events`))).toBe(true);
  await page.locator('#nav-files').click();
  await page.locator('.file-entry').filter({hasText:'hello #%.md'}).click();
  const diagram=page.locator('#workspace-file-detail .mermaid-block');
  await expect(diagram.locator('.mermaid-status')).toBeHidden();
  await expect(diagram.frameLocator('iframe').locator('svg')).toContainText('Alice');
  await expect(diagram.locator('iframe')).toHaveAttribute('src',origin+mount+'mermaid-frame.html');

  await page.reload();
  await expect(page.locator('#session-state')).toHaveText('Ready');
  await expect(page.locator('#connection')).toHaveText('Connected');
  const cdp = await context.newCDPSession(page);
  const appManifest = await cdp.send('Page.getAppManifest');
  expect(appManifest.errors).toEqual([]);
  expect(appManifest.manifest.id).toBe(origin + mount);
  expect(appManifest.manifest.startUrl).toBe(origin + mount);
  expect(appManifest.manifest.scope).toBe(origin + mount);
  await cdp.detach();
  expect(errors).toEqual([]);
  expect(requests.filter(url => new URL(url).origin === origin && !new URL(url).pathname.startsWith(mount))).toEqual([]);
});

test('slash redirect preserves query; mount storage and app identity stay separate', async ({page, context}) => {
  const response = await context.request.get(origin + mounts[0].slice(0,-1) + '?source=install', {maxRedirects:0});
  expect(response.status()).toBe(308);
  expect(response.headers().location).toBe(mounts[0] + '?source=install');
  for (const [i,mount] of mounts.entries()) {
    await page.goto(origin + mount);
    await page.evaluate(async i => {
      const {localState, sessionState, deploymentKey} = await import('./hosting.js');
      if (i) {
        if (localState.getItem('agentworks-project-locations') !== null) throw new Error('Sibling state leaked');
        if (sessionState.getItem('agentworks-pending-sends') !== null) throw new Error('Sibling requests leaked');
      }
      localState.setItem('agentworks-project-locations', String(i));
      sessionState.setItem('agentworks-pending-sends', String(i));
      window.claimKey = deploymentKey('agentworks-notifications');
    }, i);
    expect(await page.evaluate(() => window.claimKey)).toBe('agentworks-notifications@' + mount);
  }
  await page.goto(origin + mounts[0]);
  expect(await page.evaluate(async () => (await import('./hosting.js')).localState.getItem('agentworks-project-locations'))).toBe('0');
});

test('mounted notification stream navigates to an approval and mounted controls complete the turn', async ({page}) => {
  const background = await api('/api/sessions', {provider:'demo',cwd:fixture,title:'Mounted background'});
  await api('/api/notifications', {enabled:true,events:['turnEnd','approval','error'],projects:{},mutedSessions:[]}, 'PUT');
  await page.addInitScript(id => {
    localStorage.setItem('agentworks-session@/a1b918dd-2b91-4fc2-8822-e6a559503040/', id);
    window.__notices = [];
    window.Notification = class {
      static permission = 'granted';
      constructor(title, options) {this.title=title; this.options=options; window.__notices.push(this);}
      close() {}
    };
    const NativeSocket = window.WebSocket;
    window.WebSocket = class extends NativeSocket {
      constructor(url, ...args) {
        super(url, ...args);
        if (String(url).endsWith('/api/notifications/events')) this.addEventListener('open', () => window.__notifying=true);
      }
    };
  }, session.id);
  try {
    await page.goto(origin + mounts[0]);
    await page.waitForFunction(() => window.__notifying);
    await expect.poll(async () => (await api(`/api/sessions/${background.id}`)).session.status).toBe('idle');
    const response = await page.evaluate(async id => {
      const {appUrl} = await import('./hosting.js');
      return (await fetch(appUrl(`/api/sessions/${id}/prompt`), {method:'POST',headers:{'Content-Type':'application/json'},
        body:JSON.stringify({text:'approval',requestId:crypto.randomUUID()})})).status;
    }, background.id);
    expect(response).toBe(200);
    await expect.poll(() => page.evaluate(() => window.__notices.some(n => n.options.body === 'Waiting for your approval.'))).toBe(true);
    await page.evaluate(() => window.__notices.at(-1).onclick());
    await expect(page.locator('#header-title')).toHaveText('Mounted background');
    await page.getByRole('button',{name:'Allow once',exact:true}).click();
    await expect(page.locator('#session-state')).toHaveText('Ready');
    await expect(page.locator('.message.assistant').last()).toContainText('demo agent');
  } finally {
    await api('/api/notifications', {enabled:false,events:['turnEnd','approval','error'],projects:{},mutedSessions:[]}, 'PUT');
    await api(`/api/sessions/${background.id}`, undefined, 'DELETE');
  }
});

test('network loss recovers without replaying mutations or caching private data', async ({page, context}) => {
  await page.addInitScript(id => localStorage.setItem('agentworks-session@' + '/a1b918dd-2b91-4fc2-8822-e6a559503040/', id), session.id);
  await page.goto(origin + mounts[0]);
  await expect(page.locator('#session-state')).toHaveText('Ready');
  await page.locator('#prompt').fill('Keep this draft across reconnect.');
  const mutations = [];
  page.on('request', req => {if (!['GET','HEAD'].includes(req.method())) mutations.push(req.url());});
  await context.setOffline(true);
  // Playwright offline emulation alone leaves already-open WebSockets alive.
  for (const socket of sockets) socket.destroy();
  await expect(page.locator('#connection')).toHaveText('Reconnecting');
  await context.setOffline(false);
  await expect(page.locator('#connection')).toHaveText('Connected');
  expect(mutations).toEqual([]);
  await expect(page.locator('#prompt')).toHaveValue('Keep this draft across reconnect.');
  expect(await page.evaluate(() => caches.keys())).toEqual([]);
});

test('worker updates wait for explicit refresh and preserve draft and request IDs', async ({page}) => {
  await page.addInitScript(id => localStorage.setItem('agentworks-session@/a1b918dd-2b91-4fc2-8822-e6a559503040/', id), session.id);
  await page.goto(origin + mounts[0]);
  await page.evaluate(() => navigator.serviceWorker.ready);
  await page.reload();
  await expect(page.locator('#session-state')).toHaveText('Ready');
  await page.locator('#prompt').fill('Draft survives worker upgrade');
  await page.evaluate(async () => (await import('./hosting.js')).sessionState.setItem('agentworks-pending-sends', JSON.stringify({sentinel:{requestId:'keep-id'}})));
  let navigations = 0;
  page.on('framenavigated', frame => {if (frame === page.mainFrame()) navigations++;});
  workerRevision = 1;
  try {
    await page.evaluate(async () => (await navigator.serviceWorker.ready).update());
    await expect(page.locator('.pwa-update')).toBeVisible();
    expect(navigations).toBe(0);
    await page.locator('.pwa-update').click();
    await expect.poll(() => navigations).toBe(1);
    await expect(page.locator('#prompt')).toHaveValue('Draft survives worker upgrade');
    expect(await page.evaluate(async () => JSON.parse((await import('./hosting.js')).sessionState.getItem('agentworks-pending-sends')).sentinel.requestId)).toBe('keep-id');
  } finally {workerRevision = 0;}
});

test('disablement removes only this mount worker and preserves state and sibling registrations', async ({page}) => {
  for (const mount of mounts) {
    await page.goto(origin + mount);
    await page.evaluate(async id => {
      await navigator.serviceWorker.ready;
      (await import('./hosting.js')).localState.setItem('agentworks-session', id);
    }, session.id);
  }
  await stop(); await start(false);
  await page.goto(origin + mounts[0]);
  await expect(page.locator('link[rel=manifest]')).toHaveCount(0);
  await expect.poll(() => page.evaluate(() => navigator.serviceWorker.getRegistrations().then(rs => rs.map(r => r.scope)))).toEqual([origin + mounts[1]]);
  expect(await page.evaluate(async () => (await import('./hosting.js')).localState.getItem('agentworks-session'))).toBe(session.id);
  for (const path of ['manifest.webmanifest','service-worker.js','icon-192.png']) {
    expect((await fetch(upstream + '/' + path)).status).toBe(404);
  }
});
