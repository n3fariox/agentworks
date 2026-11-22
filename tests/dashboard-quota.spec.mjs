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
  await expect(local).toContainText('Agentworks received');
  await expect(local).toContainText('Adapter: available');
  await expect(cards.filter({hasText:'Goose · Work ChatGPT · Rock'}).getByRole('meter')).toHaveJSProperty('value', 40);
  await expect(cards.filter({has:page.getByRole('heading',{name:'Codex',exact:true})}).locator('.quota-state')).toHaveText('No quota reported');
  // A report from any session reaches the open dashboard without a refresh.
  socket.send(JSON.stringify({kind:'quota', entry:claude(0.6, now + 1000)}));
  await expect(local.getByRole('meter')).toHaveJSProperty('value', 60);
});
