// Real browser controller/display, served entirely by Playwright route fixtures.
// No Agentworks server, provider process, account query or session is started.
import {test,expect} from '@playwright/test';
import {readFileSync} from 'node:fs';
import {observeSubscriptionLimits,subscriptionLimitsMeta} from './fixtures/subscription-limits.mjs';
const script=readFileSync(new URL('../web/usage-status.js',import.meta.url),'utf8');
const fixture=JSON.parse(readFileSync(new URL('./fixtures/codex-subscription.json',import.meta.url)));
test.use({viewport:{width:390,height:844}});

test('Codex subscription display preserves window identity, sparse freshness, replay and session isolation',async({page})=>{
  const time=fixture.observedAtMs;
  await page.clock.install({time});
  await page.route('http://usage.test/**',route=>{
    if(route.request().url().endsWith('/usage-status.js'))return route.fulfill({contentType:'text/javascript',body:script});
    return route.fulfill({contentType:'text/html',body:`<div id="usage"></div><button id="chip"></button>
      <script type="module">
        import {setupUsageStatus} from '/usage-status.js';
        window.ui=setupUsageStatus({root:document.querySelector('#usage'),chip:document.querySelector('#chip'),getProvider:()=> 'codex'});
        window.consume=(data,live)=>{ui.consume({seq:1,kind:'acp',data},{live});ui.render();};
      </script>`});
  });
  await page.goto('http://usage.test/');
  await page.waitForFunction(()=>window.ui);
  const cache={};
  for(const snapshot of fixture.notifications.slice(0,2))observeSubscriptionLimits(cache,snapshot,time);
  const first={sessionUpdate:'usage_update',used:1234,size:200000,...subscriptionLimitsMeta(cache)};
  const consume=(data,live=true)=>page.evaluate(({data,live})=>window.consume(data,live),{data,live});
  const label=page.locator('.usage-status-toggle');
  await consume(first);
  await expect(label).toContainText('codex primary (5h) 85% left');
  await expect(label).toContainText('codex secondary (7d) 33% left');
  await expect(label).toContainText('Research (research) primary (90m) 100% left');
  await expect(label).toContainText('Research (research) secondary (2d) 0% left');
  await label.click();
  await expect(page.locator('#usage-status-details')).toContainText('15% used / 85% remaining');
  await expect(page.locator('#usage-status-details')).toContainText('reset unknown');
  const reset=await page.evaluate(()=>Object.values(ui.state.codexQuota)[0].resetsAt.value);
  expect(reset).toBe(1792385436000);

  // Session B resets state; session A is later restored through replay.
  await page.evaluate(()=>ui.reset());
  await expect(label).toHaveText('Context unknown');
  await consume({sessionUpdate:'usage_update',used:20,size:100});
  await expect(label).toHaveText('20/100 · 20%');
  await page.evaluate(()=>ui.reset());
  await consume(first,false);
  await label.click();
  await expect(page.locator('#usage-status-details')).toContainText('(replayed)');
  await page.clock.fastForward(600001);
  await consume({sessionUpdate:'usage_update',used:2000,size:200000});
  await expect(label).toContainText('2.0k/200k · 1%');
  await expect(label).toContainText('85% left ⚠');

  // A new reset observation cannot make an older percentage fresh.
  observeSubscriptionLimits(cache,fixture.notifications[2],time+600001);
  await consume({sessionUpdate:'usage_update',used:2000,size:200000,...subscriptionLimitsMeta(cache)});
  await expect(label).toContainText('85% left ⚠');
  const fieldTimes=await page.evaluate(()=>{
    const w=Object.values(ui.state.codexQuota)[0];return [w.usedPercent.at,w.resetsAt.at];
  });
  expect(fieldTimes).toEqual([time,time+600001]);
  observeSubscriptionLimits(cache,fixture.notifications[4],time+600001);
  observeSubscriptionLimits(cache,fixture.notifications[5],time+600001);
  await consume({sessionUpdate:'usage_update',used:2000,size:200000,...subscriptionLimitsMeta(cache)});
  await expect(label).toContainText('codex primary (5h) usage unknown');
  await expect(label).not.toContainText('Research (research) secondary');
});
