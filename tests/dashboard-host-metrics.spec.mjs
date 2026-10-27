import {test, expect} from '@playwright/test';
import {readFile} from 'node:fs/promises';
import {resolve} from 'node:path';

test('host bars show pressure, retain mount expansion, and fit a phone viewport', async ({page}) => {
  let socket;
  const sample = () => ({hosts:{local:{sampledAt:Date.now(),cpu:{percent:12.3},memory:{used:95,total:100},swap:{used:0,total:0},disks:[
    {path:'/',used:85,total:100}, {path:'/build',used:95,total:100}, {path:'/boot',used:1,total:100},
    {path:'/home',used:85,total:100}, {path:`/container/${'a'.repeat(150)}`,reason:'Permission denied'}
  ]},old:{reason:'Update this host’s worker to enable host metrics'}}});
  await page.route('http://metrics.test/**', async route => {
    const path = new URL(route.request().url()).pathname;
    if(path.startsWith('/api/')) {
      const json = path === '/api/catalog' ? {hosts:[{id:'local',name:'Local',kind:'local',status:'connected'},{id:'old',name:'Older worker',kind:'remote',status:'connected'}],projects:[]}
        : path === '/api/sessions' ? [] : {providers:[]};
      return route.fulfill({json});
    }
    const file = path === '/dashboard' ? 'dashboard.html' : path.slice(1);
    if(!/^[\w.-]+$/.test(file)) return route.fulfill({status:404});
    try {
      await route.fulfill({body:await readFile(resolve('web',file)),contentType:file.endsWith('.js') ? 'text/javascript' : file.endsWith('.css') ? 'text/css' : 'text/html'});
    } catch { await route.fulfill({status:404}); }
  });
  await page.routeWebSocket('**/api/dashboard/host-usage/events', ws => { socket=ws; ws.send(JSON.stringify(sample())); });
  await page.goto('http://metrics.test/dashboard');
  const local=page.locator('[data-host-metrics="local"]');
  await expect(local.getByRole('meter',{name:'CPU usage',exact:true})).toHaveAttribute('aria-valuenow','12.3');
  await expect(local.getByRole('meter',{name:'Swap usage',exact:true})).toHaveCount(0);
  await expect(local.getByText('Not configured',{exact:true})).toBeVisible();
  await expect(local.locator('.metric-tile').nth(1).locator('.metric-track > span')).toHaveCSS('background-color','rgb(238, 130, 140)');
  await expect(page.locator('[data-host-metrics="old"]')).toContainText('Update this host');
  const summary=local.locator('.mount-details > summary');
  await summary.click();
  await summary.focus();
  const next=sample(); next.hosts.local.sampledAt-=21000;
  socket.send(JSON.stringify(next));
  await expect(local.locator('.metrics-freshness')).toHaveText('Stale');
  await expect(local.locator('.mount-details')).toHaveAttribute('open','');
  await expect(summary).toBeFocused();
  await expect(local.getByText('Permission denied',{exact:true})).toBeVisible();
  await page.setViewportSize({width:390,height:844});
  expect(await page.evaluate(()=>document.documentElement.scrollWidth <= innerWidth)).toBe(true);
});
