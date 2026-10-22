import {test,expect} from '@playwright/test';

test('desktop unpinned sessions panel unfolds on rail hover and stays when clicked',async({page})=>{
  await page.setViewportSize({width:1280,height:900});
  await page.addInitScript(()=>localStorage.setItem('agentworks-sidebar-pinned','false'));
  await page.goto('/');
  const sidebar=page.locator('#sidebar');
  // Park the pointer off the rail: a pointer already on the rail opens the panel.
  await page.mouse.move(900,400);
  await expect(sidebar).toBeHidden();
  // Hovering the nav rail unfolds the panel; leaving the rail and panel folds it.
  await page.locator('#nav-sessions').hover();
  await expect(sidebar).toBeVisible();
  await page.mouse.move(900,400);
  await expect(sidebar).toBeHidden();
  // A click keeps the panel open until an outside click closes it.
  await page.locator('#nav-sessions').click();
  await expect(sidebar).toBeVisible();
  await page.mouse.move(900,400);
  await page.waitForTimeout(300);
  await expect(sidebar).toBeVisible();
  await page.mouse.click(900,400);
  await expect(sidebar).toBeHidden();
});
