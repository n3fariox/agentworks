import {test, expect} from '@playwright/test';
import {readFile} from 'node:fs/promises';
import {resolve} from 'node:path';

// Account quota cards follow the live shared feed: one card per host and family.
test('dashboard quota cards update live per host and account', async ({page}) => {
  let socket;
  const now = Date.now();
  const claude = (utilization, at) => ({hostId:'local', family:'claude', claude:{windows:{five_hour:{utilization, resetsAt:now/1000 + 3600, status:'allowed', at}}}});
  await page.route('http://quota.test/**', async route => {
    const path = new URL(route.request().url()).pathname;
    if (path.startsWith('/api/')) {
      const json = path === '/api/catalog' ? {hosts:[{id:'local',name:'Local',kind:'local',status:'connected'},{id:'rock',name:'Rock',kind:'remote',status:'connected'}],projects:[]}
        : path === '/api/sessions' ? [] : path === '/api/model-providers' ? {profiles:[{id:'work',name:'Work ChatGPT'}]}
        : {providers:[{id:'claude-rust',installed:true,authMode:'existing'}]};
      return route.fulfill({json});
    }
    const file = path === '/dashboard' ? 'dashboard.html' : path.slice(1);
    if (!/^[\w.-]+$/.test(file)) return route.fulfill({status:404});
    try {
      await route.fulfill({body:await readFile(resolve('web', file)), contentType:file.endsWith('.js') ? 'text/javascript' : file.endsWith('.css') ? 'text/css' : 'text/html'});
    } catch { await route.fulfill({status:404}); }
  });
  await page.routeWebSocket('**/api/dashboard/host-usage/events', () => {});
  await page.routeWebSocket('**/api/account-quota/events', ws => {
    socket = ws;
    ws.send(JSON.stringify({kind:'snapshot', entries:[claude(0.25, now),
      {hostId:'rock', family:'goose/work', codex:{at:now, limits:{codex:{limitId:'codex', primary:{usedPercent:{value:40, observedAtMs:now}, windowDurationMins:{value:300, observedAtMs:now}}}}}}]}));
  });
  await page.goto('http://quota.test/dashboard');
  const cards = page.locator('#accounts .card');
  await expect(page.locator('#quota-status')).toContainText('Live');
  await expect(cards).toHaveCount(3);
  const local = cards.filter({hasText:'Claude · Local'});
  await expect(local.getByRole('meter')).toHaveJSProperty('value', 25);
  await expect(local).toContainText('5h quota used: 25%');
  await expect(local).toContainText(/Resets in (\d+h )?\d+m · Updated /);
  await expect(local.locator('.usage-detail')).not.toContainText('stale');
  await expect(local).toContainText('Adapter: available');
  await expect(cards.filter({hasText:'Goose · Work ChatGPT · Rock'}).getByRole('meter')).toHaveJSProperty('value', 40);
  await expect(cards.filter({has:page.getByRole('heading',{name:'Codex',exact:true})}).locator('.quota-state')).toHaveText('No quota reported');
  // A report from any session reaches the open dashboard without a refresh.
  socket.send(JSON.stringify({kind:'quota', entry:claude(0.6, now + 1000)}));
  await expect(local.getByRole('meter')).toHaveJSProperty('value', 60);

  // Help popovers stay inside narrow mobile viewports and dismiss on Escape or outside click.
  await page.setViewportSize({width:320, height:800});
  const quotaTip = page.locator('.heading-with-help .help-tip');
  await quotaTip.locator('summary').click();
  const body = quotaTip.locator('.help-tip-body');
  await expect(body).toBeVisible();
  const box = await body.boundingBox();
  expect(box.x).toBeGreaterThanOrEqual(0);
  expect(box.x + box.width).toBeLessThanOrEqual(320);
  await page.keyboard.press('Escape');
  await expect(body).toBeHidden();
});

test('dashboard active turns shows flat list, waiting badge, and collapsed awake idle sessions', async ({page}) => {
  await page.route('http://turns.test/**', async route => {
    const path = new URL(route.request().url()).pathname;
    if (path.startsWith('/api/')) {
      const json = path === '/api/catalog' ? {
        hosts:[{id:'local',name:'Local',kind:'local',status:'connected'},{id:'rock',name:'Rock',kind:'remote',status:'connected'}],
        projects:[{id:'p1',name:'Agentworks'},{id:'p2',name:'Docs'},{id:'p3',name:'Empty Project'}],
      } : path === '/api/sessions' ? [
        {id:'s-run',title:'Running turn',projectId:'p1',hostId:'local',provider:'antigravity',status:'running',turnRunning:true},
        {id:'s-wait',title:'Needs approval',projectId:'p2',hostId:'rock',provider:'claude',status:'waiting',turnRunning:true},
        {id:'s-idle',title:'Awake idle',projectId:'p1',hostId:'local',provider:'codex',status:'idle',turnRunning:false,asleep:false},
        {id:'s-sleep',title:'Sleeping harness',projectId:'p2',hostId:'local',provider:'claude',status:'suspended',turnRunning:false,asleep:true},
        {id:'s-arch',title:'Archived active',projectId:'p1',hostId:'local',provider:'claude',status:'running',turnRunning:true,archived:true},
      ] : path === '/api/model-providers' ? {profiles:[]} : {providers:[]};
      return route.fulfill({json});
    }
    const file = path === '/dashboard' ? 'dashboard.html' : path.slice(1);
    if (!/^[\w.-]+$/.test(file)) return route.fulfill({status:404});
    try {
      await route.fulfill({body:await readFile(resolve('web', file)), contentType:file.endsWith('.js') ? 'text/javascript' : file.endsWith('.css') ? 'text/css' : 'text/html'});
    } catch { await route.fulfill({status:404}); }
  });
  await page.routeWebSocket('**/api/dashboard/host-usage/events', () => {});
  await page.routeWebSocket('**/api/account-quota/events', ws => ws.send(JSON.stringify({kind:'snapshot', entries:[]})));
  await page.goto('http://turns.test/dashboard');

  const section = page.locator('#projects');
  await expect(page.locator('#active-turns-summary')).toHaveText('1 waiting · 1 running · 1 idle');
  // Waiting floats to the top with a badge; running omits redundant "Taking a turn".
  const activeRows = section.locator('.turns-card > .session-list > .session-row');
  await expect(activeRows).toHaveCount(2);
  await expect(activeRows.nth(0)).toContainText('Needs approval');
  await expect(activeRows.nth(0)).toContainText('Docs · claude · Rock');
  await expect(activeRows.nth(0).locator('.waiting-badge')).toHaveText('Waiting for input');
  await expect(activeRows.nth(0).locator('a.session-link')).toHaveAttribute('href', /\/\?session=s-wait$/);
  await expect(activeRows.nth(1)).toContainText('Running turn');
  await expect(activeRows.nth(1)).toContainText('Agentworks · antigravity · Local');
  await expect(activeRows.nth(1).locator('.waiting-badge')).toHaveCount(0);
  await expect(section).not.toContainText('Taking a turn');
  await expect(section).not.toContainText('Empty Project');
  await expect(section).not.toContainText('Sleeping harness');
  await expect(section).not.toContainText('Archived active');

  // Awake idle harnesses sit collapsed beneath active turns until expanded.
  const idle = section.locator('.idle-sessions');
  await expect(idle.locator('summary')).toHaveText('1 idle session');
  await expect(idle.locator('.session-row')).toBeHidden();
  await idle.locator('summary').click();
  await expect(idle.locator('.session-row')).toBeVisible();
  await expect(idle.locator('.session-row')).toContainText('Awake idle');
});
