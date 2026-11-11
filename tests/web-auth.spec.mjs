import {test, expect} from '@playwright/test';
import {spawn, execFileSync} from 'node:child_process';
import {chmod, mkdtemp, mkdir, readFile, rm, writeFile} from 'node:fs/promises';
import {existsSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join, resolve} from 'node:path';
import {createServer} from 'node:https';
import {request} from 'node:http';

// PAM sign-in behind a TLS StripPrefix proxy with two mounts on one origin. A private
// PAM directory checks a fixed password through pam_exec, so no host account is used.
const upstream = 'http://127.0.0.1:4425';
const origin = 'https://localhost:4426';
const mounts = ['/first-instance/', '/second-instance/'];
const password = 'correct horse';
let fixture, server, proxy, log = '';
const sockets = new Set();
test.use({ignoreHTTPSErrors: true});
test.describe.configure({mode: 'serial'});
test.skip(!existsSync('/usr/lib/security/pam_exec.so') && !existsSync('/lib/x86_64-linux-gnu/security/pam_exec.so'), 'needs pam_exec.so');

const proxied = {'X-Forwarded-Proto': 'https', 'X-Forwarded-Host': 'localhost:4426'};

test.beforeAll(async () => {
  fixture = await mkdtemp(join(tmpdir(), 'agentworks-web-auth-'));
  const pam = join(fixture, 'pam.d');
  await mkdir(pam);
  const check = join(fixture, 'check-password.sh');
  await writeFile(check, `#!/bin/sh\nread -r pw\n[ "$pw" = "${password}" ]\n`);
  await chmod(check, 0o700);
  await writeFile(join(pam, 'agentworks-test'), `auth required pam_exec.so expose_authtok quiet ${check}\naccount required pam_permit.so\n`);
  const env = {...process.env, AGENTWORKS_BIND: '127.0.0.1', AGENTWORKS_PORT: '4425', AGENTWORKS_ROOTS: fixture,
    AGENTWORKS_CATALOG_PATH: join(fixture, 'catalog.json'), AGENTWORKS_DB_PATH: join(fixture, 'sessions.sqlite3'),
    AGENTWORKS_DEMO_STATE_DIR: join(fixture, 'demo'), AGENTWORKS_PWA: '0',
    AGENTWORKS_TRUSTED_PROXIES: '127.0.0.1', AGENTWORKS_PAM_SERVICE: 'agentworks-test',
    AGENTWORKS_PAM_CONFDIR: pam, AGENTWORKS_PAM_USERS: 'tester'};
  for (const key of ['AGENTWORKS_SERVICE_DIR','NOTIFY_SOCKET','AGENTWORKS_RELEASE_DIR','INVOCATION_ID','AGENTWORKS_TLS_CERT','AGENTWORKS_TLS_KEY','AGENTWORKS_TLS_CLIENT_CA']) delete env[key];
  server = spawn(resolve('target/debug/agentworks'), [], {env, stdio: ['ignore', 'pipe', 'pipe']});
  let exited = false;
  server.on('exit', () => exited = true);
  for (const stream of [server.stdout, server.stderr]) stream.on('data', data => log += data);
  for (let i = 0; ; i++) {
    if (exited || i === 100) throw new Error('Fixture not ready: ' + log);
    try {if ((await fetch(upstream + '/api/status', {headers: proxied})).status === 401) break;} catch {}
    await new Promise(resolve => setTimeout(resolve, 100));
  }
  execFileSync('openssl', ['req','-x509','-newkey','rsa:2048','-nodes','-keyout',join(fixture,'key.pem'),
    '-out',join(fixture,'cert.pem'),'-days','1','-subj','/CN=localhost'], {stdio: 'ignore'});
  const forward = req => {
    const mount = mounts.find(mount => req.url.startsWith(mount));
    return request({hostname: '127.0.0.1', port: 4425, method: req.method,
      path: mount ? req.url.slice(mount.length - 1) : req.url,
      headers: {...req.headers, 'x-forwarded-proto': 'https', 'x-forwarded-host': req.headers.host,
        'x-forwarded-for': req.socket.remoteAddress.replace(/^::ffff:/, '')}});
  };
  proxy = createServer({key: await readFile(join(fixture,'key.pem')), cert: await readFile(join(fixture,'cert.pem'))}, (req, res) => {
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
  await new Promise(resolve => proxy.listen(4426, '127.0.0.1', resolve));
});
test.afterAll(async () => {
  for (const socket of sockets) socket.destroy();
  if (proxy) await new Promise(resolve => proxy.close(resolve));
  if (server && server.exitCode === null) {
    const exited = new Promise(resolve => server.once('exit', resolve));
    server.kill('SIGTERM'); await exited;
  }
  if (fixture) await rm(fixture, {recursive: true, force: true});
});

async function signIn(page, mount, secret = password) {
  await page.goto(origin + mount);
  await expect(page.locator('#login-form')).toBeVisible();
  await page.locator('#login-username').fill('tester');
  await page.locator('#login-password').fill(secret);
  await page.locator('#login-submit').click();
}
const signedIn = page => expect(page.locator('#nav-settings')).toBeVisible();

test('requests that bypass the HTTPS proxy are refused', async () => {
  // Plain HTTP straight to the backend, and a forged HTTPS header from an untrusted peer path.
  expect((await fetch(upstream + '/')).status).toBe(403);
  expect((await fetch(upstream + '/api/auth/login', {method: 'POST', headers: {'Content-Type': 'application/json', 'X-Forwarded-Proto': 'http', Origin: upstream},
    body: JSON.stringify({username: 'tester', password, base: upstream + '/'})})).status).toBe(403);
  expect((await fetch(upstream + '/api/status', {headers: proxied})).status).toBe(401);
});

test('each mount signs in and out separately', async ({page, context}) => {
  await signIn(page, mounts[0], 'wrong');
  await expect(page.locator('#login-error')).toHaveText('Sign-in failed.');
  await expect(page.locator('#login-password')).toHaveValue('');

  await signIn(page, mounts[0]);
  await signedIn(page);
  // The sibling mount shares the origin but not the cookie.
  await page.goto(origin + mounts[1]);
  await expect(page.locator('#login-form')).toBeVisible();
  await signIn(page, mounts[1]);
  await signedIn(page);

  const cookies = (await context.cookies()).filter(c => c.name.startsWith('__Secure-agentworks-'));
  expect(cookies.map(c => c.path).sort()).toEqual([...mounts].sort());
  expect(new Set(cookies.map(c => c.name)).size).toBe(2);
  expect(cookies.every(c => c.secure && c.httpOnly && c.sameSite === 'Lax')).toBe(true);

  await page.goto(origin + mounts[0]);
  await signedIn(page);
  await page.locator('#nav-settings').click();
  await page.locator('#settings-tab-service').click();
  await expect(page.locator('#auth-status')).toContainText('Signed in as tester.');
  await page.locator('#sign-out').click();
  await expect(page.locator('#login-form')).toBeVisible();
  expect((await context.cookies()).map(c => c.path)).toEqual([mounts[1]]);

  await page.goto(origin + mounts[1]);
  await signedIn(page);
  expect(log).toMatch(/Web sign-in failed for "tester" from 127\.0\.0\.1/);
  expect(log).toMatch(/Web sign-in for "tester" from 127\.0\.0\.1/);
});

test('an open app returns to the sign-in page once its session is revoked', async ({page}) => {
  await signIn(page, mounts[0]);
  await signedIn(page);
  // Revoke from another request carrying the same cookie, as another tab would.
  expect((await page.request.post(origin + mounts[0] + 'api/auth/logout', {headers: {Origin: origin}})).ok()).toBe(true);
  await page.evaluate(() => {fetch('./api/status');});
  await expect(page.locator('#login-form')).toBeVisible();
});
