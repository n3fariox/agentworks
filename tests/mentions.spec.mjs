import {serverReady,stopChild} from './support/cleanup.mjs';
import {test,expect} from '@playwright/test';
import {spawn} from 'node:child_process';
import {mkdtemp,mkdir,rm} from 'node:fs/promises';
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
  for(let i=0;i<100;i++){if(await serverReady(server,base))return;await new Promise(r=>setTimeout(r,100));}
  throw new Error('server did not start');
});
test.afterAll(async()=>{await stopChild(server);if(fixture)await rm(fixture,{recursive:true,force:true});});

test('user references preserve literal prompt text and only link valid sessions',async({page})=>{
  await page.goto('/');
  const id='28d9887c-e7c3-4096-870c-8ccf6d4eb4ae';
  const prefix='**literal** <img src=x onerror=alert(1)>\n';
  const invalid='[invalid](agentworks://sessions/missing) [external](https://example.com)';
  const result=await page.evaluate(async({id,prefix,invalid})=>{
    const {renderUserReferences}=await import('/chat-references.js');
    const body=document.createElement('div');
    renderUserReferences(body,`${prefix}[@one](agentworks://sessions/${id}) and [@two](agentworks://sessions/${id}).\n${invalid}`);
    return {text:body.textContent,links:[...body.querySelectorAll('a')].map(a=>a.getAttribute('href')),images:body.querySelectorAll('img').length};
  },{id,prefix,invalid});
  expect(result).toEqual({text:`${prefix}@one and @two.\n${invalid}`,links:[`agentworks://sessions/${id}`,`agentworks://sessions/${id}`],images:0});
});

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
  const message=page.locator('.message.user .message-body').last();
  await expect(message).toHaveText('Ping @frontend-reviewer.');
  const reference=message.getByRole('link',{name:'@frontend-reviewer',exact:true});
  await expect(reference).toHaveAttribute('href',`agentworks://sessions/${reviewer.id}`);
  await page.reload();
  await expect(page.locator('.message.user .message-body').last()).toHaveText('Ping @frontend-reviewer.');
  await page.locator('.message.user .message-body').last().getByRole('link',{name:'@frontend-reviewer',exact:true}).click();
  await expect.poll(()=>page.evaluate(()=>localStorage.getItem('agentworks-session'))).toBe(reviewer.id);
});
