import {test,expect} from '@playwright/test';
import {spawn} from 'node:child_process';
import {mkdtemp,mkdir} from 'node:fs/promises';
import {resolve,join} from 'node:path';

// @-mentions: isolated server on port 4514; demo agent only.
const base='http://127.0.0.1:4514';
test.use({baseURL:base});
let fixture,server;

async function api(path,body,method){
  const response=await fetch(base+path,{method:method||(body?'POST':'GET'),headers:{Origin:base,'Content-Type':'application/json'},body:body?JSON.stringify(body):undefined});
  const text=await response.text();expect(response.status,text).toBeLessThan(300);return JSON.parse(text);
}
test.beforeAll(async()=>{
  await mkdir('.cache',{recursive:true});fixture=await mkdtemp(resolve('.cache/mentions-'));
  server=spawn(resolve('target/debug/agentworks'),[],{env:{...process.env,AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:'4514',AGENTWORKS_ROOTS:fixture,
    AGENTWORKS_CATALOG_PATH:join(fixture,'catalog.json'),AGENTWORKS_DB_PATH:join(fixture,'sessions.sqlite3'),AGENTWORKS_DEMO_STATE_DIR:join(fixture,'demo')},stdio:'ignore'});
  for(let i=0;i<100;i++){try{if((await fetch(base+'/api/status')).ok)return;}catch{}await new Promise(r=>setTimeout(r,100));}
  throw new Error('server did not start');
});
test.afterAll(()=>server?.kill());

test('unanchored @ fuzzy-picks project peers and resolves ids on send',async({page})=>{
  const project=await api('/api/projects',{name:'Mentions'});
  const create=(title,projectId=project.id)=>api('/api/sessions',{provider:'demo',cwd:fixture,title,projectId});
  const me=await create('Coordinator');const reviewer=await create('Frontend reviewer');await create('Backend builder');await create('Outsider',null);
  for(let i=0;i<200&&(await api(`/api/sessions/${me.id}`)).session.status!=='idle';i++)await new Promise(r=>setTimeout(r,100));
  await page.addInitScript(id=>localStorage.setItem('agentworks-session',id),me.id);
  await page.goto('/');await expect(page.locator('#session-state')).toHaveText('Ready');
  const prompt=page.locator('#prompt'),list=page.locator('#mention-list');
  await prompt.pressSequentially('Please send the summary to @');
  await expect(list.getByRole('option')).toHaveCount(2);
  await expect(list).not.toContainText('Outsider');
  await expect(list).not.toContainText('@coordinator');
  await prompt.pressSequentially('frv');
  await expect(list.getByRole('option')).toHaveCount(1);
  await expect(list.getByRole('option').first()).toContainText('@frontend-reviewer');
  await prompt.press('Enter');
  await expect(prompt).toHaveValue('Please send the summary to @frontend-reviewer ');
  await expect(list).toBeHidden();
  // Escape dismisses without submitting; an email-like "a@b" does not trigger.
  await prompt.pressSequentially('and me@x');await expect(list).toBeHidden();
  await prompt.pressSequentially(' @b');await expect(list).toBeVisible();await prompt.press('Escape');await expect(list).toBeHidden();
  await prompt.fill('Ping @frontend-reviewer.');await prompt.press('Enter');
  await expect(prompt).toHaveValue('');
  await expect(page.locator('#messages')).toContainText(`Ping [@frontend-reviewer](agentworks://sessions/${reviewer.id}).`);
});
