import {test,expect} from '@playwright/test';
import {spawn} from 'node:child_process';
import {mkdtemp,mkdir,rm} from 'node:fs/promises';
import {resolve,join} from 'node:path';

// U-05 Settings → Progress parsers against its own server, database and port; never the live service.
const base='http://127.0.0.1:4474';
test.use({baseURL:base});
test.describe.configure({mode:'serial'});
let fixture,server,log='',exited=false;
const BUILTIN='bazel_progress.star';

async function api(path,body,method){
  const response=await fetch(base+path,{method:method||(body?'PUT':'GET'),headers:{Origin:base,'Content-Type':'application/json'},body:body?JSON.stringify(body):undefined});
  expect(response.ok,await response.clone().text()).toBe(true);return response.json();
}
const script=async name=>(await api('/api/scripts')).scripts.find(s=>s.name===name);
test.beforeAll(async()=>{
  await mkdir('.cache',{recursive:true});fixture=await mkdtemp(resolve('.cache/parser-scripts-'));
  server=spawn(resolve('target/debug/agentworks'),[],{env:{...process.env,
    AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:'4474',AGENTWORKS_ROOTS:fixture,
    AGENTWORKS_CATALOG_PATH:join(fixture,'catalog.json'),AGENTWORKS_DB_PATH:join(fixture,'sessions.sqlite3'),AGENTWORKS_DEMO_STATE_DIR:join(fixture,'demo')},stdio:['ignore','pipe','pipe']});
  server.on('error',e=>{log+=e.message;exited=true;});server.on('exit',()=>exited=true);
  for(const stream of [server.stdout,server.stderr])stream.on('data',s=>{log+=s.toString();});
  for(let i=0;i<100;i++){
    if(exited)throw new Error(log);
    try{if((await fetch(base+'/api/status')).ok)return;}catch{}
    await new Promise(r=>setTimeout(r,100));
  }
  throw new Error('Isolated server readiness timed out: '+log);
});
test.afterAll(async()=>{
  if(server&&!exited){const stopped=new Promise(r=>server.once('exit',r));server.kill('SIGTERM');await stopped;}
  if(fixture)await rm(fixture,{recursive:true,force:true});
});
async function openPanel(page){
  await page.goto('/');
  await page.locator('#nav-settings').evaluate(n=>n.click());
  await page.locator('#settings-tab-parsers').click();
  const panel=page.locator('#parser-script-controls');
  await expect(panel.locator(`[data-script="${BUILTIN}"]`)).toBeVisible();
  return panel;
}
const row=(panel,name)=>panel.locator(`.parser-script[data-script="${name}"]`);

test('built-in parser can be disabled and enabled; Settings stays open',async({page})=>{
  const panel=await openPanel(page),builtin=row(panel,BUILTIN);
  await expect(builtin.locator('.parser-script-badge')).toHaveText('Built-in');
  await expect(builtin.locator('.parser-script-more')).toHaveCount(0);
  const box=builtin.getByRole('checkbox',{name:`Enable ${BUILTIN}`});
  await expect(box).toBeChecked();
  await box.uncheck();await expect(panel.locator('[data-note]')).toContainText('disabled');
  expect((await script(BUILTIN)).enabled).toBe(false);
  await row(panel,BUILTIN).getByRole('checkbox').check();await expect(panel.locator('[data-note]')).toContainText('enabled');
  expect((await script(BUILTIN)).enabled).toBe(true);
  await expect(page.locator('#settings-dialog')).toBeVisible();
});

test('new parser from the typed template; type errors are shown and nothing is saved',async({page})=>{
  const panel=await openPanel(page);
  await panel.getByRole('button',{name:'＋ New parser'}).click();
  const source=panel.locator('[data-source]');
  await expect(source).toHaveValue(/def on_line\(ctx: LineContext, line: str\) -> Progress \| None:/);
  await panel.locator('[data-name]').fill('make_progress.star');
  await panel.getByRole('button',{name:'Save and check'}).click();
  await expect(row(panel,'make_progress.star')).toBeVisible();
  await expect(panel.locator('[data-editor]')).toBeHidden();
  const saved=await script('make_progress.star');expect(saved.enabled&&!saved.builtin&&!saved.error).toBe(true);
  // A misspelled attribute read is caught by the static type check and reported in place.
  // (starlark-rust does not check attribute names in assignment targets; see E-01 doc.)
  await row(panel,'make_progress.star').getByRole('button',{name:'Edit make_progress.star'}).click();
  await expect(panel.locator('[data-name]')).toHaveJSProperty('readOnly',true);
  await source.fill((await source.inputValue()).replace('if "total" not in ctx.state:','if "total" not in ctx.stat:'));
  await panel.getByRole('button',{name:'Save and check'}).click();
  await expect(panel.locator('[data-error]')).toContainText('make_progress.star');
  await expect(panel.locator('[data-error]')).toContainText('stat');
  expect((await script('make_progress.star')).source).toBe(saved.source);
  await panel.getByRole('button',{name:'Cancel'}).click();
  // Names are validated before anything is sent.
  await panel.getByRole('button',{name:'＋ New parser'}).click();await panel.locator('[data-name]').fill('make_progress.star');
  await panel.getByRole('button',{name:'Save and check'}).click();
  await expect(panel.locator('[data-error]')).toHaveText('make_progress.star already exists; edit it instead.');
});

test('modified built-in can be restored; user scripts can be deleted from the ⋯ popover',async({page})=>{
  const panel=await openPanel(page);
  await row(panel,BUILTIN).getByRole('button',{name:`Edit ${BUILTIN}`}).click();
  await expect(panel.locator('[data-editor-note]')).toContainText('stops following Agentworks updates');
  const source=panel.locator('[data-source]');
  await source.fill((await source.inputValue()).replace('DISPLAY_NAME = "Bazel"','DISPLAY_NAME = "Build"'));
  await panel.getByRole('button',{name:'Save and check'}).click();
  await expect(row(panel,BUILTIN).locator('.parser-script-badge')).toHaveText('Modified');
  // Popover: Escape closes it without closing Settings; an outside tap closes it too.
  const more=row(panel,BUILTIN).getByRole('button',{name:`More actions for ${BUILTIN}`});
  await more.click();await expect(panel.getByRole('menuitem',{name:'Restore shipped version'})).toBeVisible();
  await page.keyboard.press('Escape');await expect(panel.locator('[data-popover]')).toBeHidden();
  await expect(page.locator('#settings-dialog')).toBeVisible();await expect(more).toBeFocused();
  await more.click();await panel.locator('legend').click();await expect(panel.locator('[data-popover]')).toBeHidden();
  await more.click();await panel.getByRole('menuitem',{name:'Restore shipped version'}).click();
  await expect(row(panel,BUILTIN).locator('.parser-script-badge')).toHaveText('Built-in');
  expect((await script(BUILTIN)).source).toContain('DISPLAY_NAME = "Bazel"');
  page.once('dialog',d=>d.accept());
  await row(panel,'make_progress.star').getByRole('button',{name:'More actions for make_progress.star'}).click();
  await panel.getByRole('menuitem',{name:'Delete…'}).click();
  await expect(row(panel,'make_progress.star')).toHaveCount(0);
  expect(await script('make_progress.star')).toBeUndefined();
});

test('an enabled script that no longer compiles shows its diagnostic',async({page})=>{
  // Stored directly to simulate a script broken by a later host API change.
  const db=join(fixture,'sessions.sqlite3');
  await api('/api/scripts/legacy.star',{source:(await script(BUILTIN)).source.replace('DISPLAY_NAME = "Bazel"','DISPLAY_NAME = "Legacy"')});
  const {DatabaseSync}=await import('node:sqlite');const sql=new DatabaseSync(db);
  sql.prepare("UPDATE scripts SET source=source||'\nbroken(' WHERE name='legacy.star'").run();sql.close();
  const panel=await openPanel(page);
  await expect(row(panel,'legacy.star').locator('.parser-script-badge.error')).toHaveText('Error');
  await expect(panel.locator('.parser-script-entry .parser-script-error')).toContainText('legacy.star');
  await api('/api/scripts/legacy.star',undefined,'DELETE');
});

test('phone layout: one row per script, 44px targets, no horizontal overflow',async({page})=>{
  await page.setViewportSize({width:390,height:844});
  await api('/api/scripts/make_progress.star',{source:(await script(BUILTIN)).source.replace('DISPLAY_NAME = "Bazel"','DISPLAY_NAME = "Make"')});
  const panel=await openPanel(page);
  for(const name of [BUILTIN,'make_progress.star']){
    const box=await row(panel,name).boundingBox();expect(box.height).toBeLessThan(60);
    for(const control of await row(panel,name).locator('button,label').all())expect((await control.boundingBox()).height).toBeGreaterThanOrEqual(44);
  }
  await row(panel,'make_progress.star').getByRole('button',{name:'More actions for make_progress.star'}).click();
  const popover=await panel.locator('[data-popover]').boundingBox();expect(popover.x+popover.width).toBeLessThanOrEqual(390);
  expect(await panel.evaluate(n=>n.scrollWidth<=n.clientWidth+1)).toBe(true);
  await page.screenshot({path:'.cache/parser-scripts-mobile.png'});
  await page.keyboard.press('Escape');
  await panel.getByRole('button',{name:'＋ New parser'}).click();
  await expect(panel.locator('[data-source]')).toBeVisible();
  expect(await panel.evaluate(n=>n.scrollWidth<=n.clientWidth+1)).toBe(true);
  await page.screenshot({path:'.cache/parser-scripts-mobile-editor.png'});
  await api('/api/scripts/make_progress.star',undefined,'DELETE');
});
