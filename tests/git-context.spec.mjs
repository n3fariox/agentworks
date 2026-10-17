import {test,expect} from '@playwright/test';
import {spawn,execFileSync} from 'node:child_process';
import {mkdtemp,mkdir,writeFile,rm,readFile,symlink} from 'node:fs/promises';
import {resolve,join} from 'node:path';

// Own server and repositories: never use the live service or mutate the source checkout.
const base='http://127.0.0.1:4331';
const gitBinary=execFileSync('which',['git'],{encoding:'utf8'}).trim();
let fixture,repo,linked,nested,plain,unborn,server,log='',exited=false;
const git=(cwd,...args)=>execFileSync(gitBinary,['-c','user.name=Workspace Test','-c','user.email=workspace@localhost',...args],{cwd,encoding:'utf8',env:{...process.env,GIT_CONFIG_NOSYSTEM:'1',GIT_CONFIG_GLOBAL:'/dev/null'}}).trim();
async function api(path,body,method){
  const response=await fetch(base+path,{method:method||(body?'POST':'GET'),headers:{Origin:base,'Content-Type':'application/json'},body:body?JSON.stringify(body):undefined});
  expect(response.ok,await response.clone().text()).toBe(true);return response.json();
}
const create=cwd=>api('/api/sessions',{provider:'demo',cwd,title:'Workspace context test'});
const context=s=>api(`/api/sessions/${s.id}/workspace`);
const remove=s=>api(`/api/sessions/${s.id}`,undefined,'DELETE');

test.beforeAll(async()=>{
  await mkdir('.cache',{recursive:true});fixture=await mkdtemp(resolve('.cache/git-context-'));
  repo=join(fixture,'main');linked=join(fixture,'linked worktree');nested=join(linked,'nested');plain=join(fixture,'plain');unborn=join(fixture,'unborn');
  for(const p of [repo,plain,unborn,join(fixture,'bin')])await mkdir(p,{recursive:true});
  git(repo,'init','-b','main');await writeFile(join(repo,'fixture.txt'),'Read-only Git context fixture\n');git(repo,'add','fixture.txt');git(repo,'commit','-m','Fixture');git(repo,'worktree','add','-b','feature/workspace',linked);await mkdir(nested);
  git(unborn,'init','-b','new-branch');
  // Test-only Git wrapper lets real HTTP tests exercise bounded output and timeout.
  const quote=s=>"'"+s.replaceAll("'","'\\''")+"'";
  await writeFile(join(fixture,'bin/git'),`#!/bin/sh\ncase "$PWD" in\n */git-slow) exec sleep 20;;\n */git-large) head -c 20000 /dev/zero; exit 0;;\n */git-fail) exit 2;;\nesac\nexec ${quote(gitBinary)} "$@"\n`,{mode:0o755});
  server=spawn(resolve('target/debug/agentworks'),[],{env:{...process.env,
    PATH:join(fixture,'bin')+':'+process.env.PATH,GIT_DIR:join(repo,'.git'),GIT_WORK_TREE:repo,GIT_CONFIG_COUNT:'1',GIT_CONFIG_KEY_0:'core.bare',GIT_CONFIG_VALUE_0:'true',
    AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:'4331',AGENTWORKS_ROOTS:fixture,
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

test('real repositories: nested linked worktree, fresh branches, detached HEAD and unborn branch',async()=>{
  const s=await create(nested);
  const expectedCommit=git(repo,'rev-parse','HEAD');
  const before=await readFile(join(repo,'.git/index')).catch(()=>null);
  expect(await context(s)).toMatchObject({cwd:nested,status:'git',repositoryRoot:repo,worktreeRoot:linked,branch:'feature/workspace',head:'branch',commit:expectedCommit,linkedWorktree:true});
  git(linked,'switch','-c','changed-branch');
  expect((await context(s)).branch).toBe('changed-branch');
  git(linked,'checkout','--detach');
  expect(await context(s)).toMatchObject({head:'detached',branch:null,commit:expectedCommit,linkedWorktree:true});
  expect(await readFile(join(repo,'.git/index')).catch(()=>null)).toEqual(before);
  await remove(s);
  const main=await create(repo);expect(await context(main)).toMatchObject({branch:'main',linkedWorktree:false,worktreeRoot:repo});await remove(main);
  const fresh=await create(unborn);expect(await context(fresh)).toMatchObject({status:'git',branch:'new-branch',commit:null,message:'No commits yet'});await remove(fresh);
});

test('non-Git, removed directories, Git failures, output bound and timeout stay usable',async()=>{
  const s=await create(plain);expect(await context(s)).toMatchObject({status:'non_git',cwd:plain,branch:null});await remove(s);
  const missing=join(fixture,'removed');await mkdir(missing);const removed=await create(missing);await rm(missing,{recursive:true});
  expect(await context(removed)).toMatchObject({status:'unavailable',cwd:missing});await remove(removed);
  for(const name of ['git-fail','git-large','git-slow']){
    const dir=join(fixture,name);await mkdir(dir);const session=await create(dir);const started=Date.now();
    const result=await context(session);expect(result.status).toBe('unavailable');
    if(name==='git-large')expect(result.message).toContain('limit');
    if(name==='git-slow'){expect(result.message).toContain('timed out');expect(Date.now()-started).toBeLessThan(5000);}
    await remove(session);
  }
  expect((await fetch(base+'/api/sessions/unknown/workspace')).status).toBe(404);
});

test('current roots and symlinks remain bounded',async()=>{
  const dir=join(fixture,'moved');await mkdir(dir);const s=await create(dir);await rm(dir,{recursive:true});await symlink('/tmp',dir);
  expect((await fetch(base+`/api/sessions/${s.id}/workspace`)).status).toBe(403);await remove(s);
  const bounded=await create(nested);
  await api('/api/settings',{roots:[nested],defaultModels:{}},'PUT');
  expect((await context(bounded)).status).toBe('non_git');
  await api('/api/settings',{roots:[fixture],defaultModels:{}},'PUT');await remove(bounded);
});

for(const mobile of [false,true])test(`Workspace dialog ${mobile?'mobile':'desktop'} preserves draft and stream`,async({page})=>{
  git(linked,'checkout','--detach');
  const s=await create(nested);
  if(mobile)await page.setViewportSize({width:390,height:844});
  await page.addInitScript(id=>localStorage.setItem('agentworks-session',id),s.id);
  const errors=[];page.on('pageerror',e=>errors.push(e.message));let sockets=0;page.on('websocket',()=>sockets++);
  await page.goto(base);await expect(page.locator('#session-state')).toHaveText('Ready');
  await page.locator('#prompt').fill('hello workspace');await page.locator('#send').click();
  await expect(page.locator('#messages')).toContainText('Local session, turn 1');
  await page.locator('#prompt').fill('Keep this draft');const before=sockets;
  await page.getByRole('button',{name:'Workspace info',exact:true}).click();
  await expect(page.getByRole('dialog',{name:'Workspace info'})).toBeVisible();
  await expect(page.locator('#workspace-info-status')).toHaveText('Detached HEAD');
  await expect(page.locator('#workspace-info-fields')).toContainText(nested);
  await expect(page.locator('#workspace-info-fields')).toContainText(git(repo,'rev-parse','HEAD'));
  expect(await page.locator('#workspace-info-dialog').evaluate(n=>{const r=n.getBoundingClientRect();return r.left>=0&&r.right<=innerWidth&&r.height<=innerHeight;})).toBe(true);
  await page.keyboard.press('Escape');await expect(page.locator('#workspace-info-dialog')).not.toBeVisible();
  await expect(page.locator('#prompt')).toHaveValue('Keep this draft');expect(sockets).toBe(before);
  await expect(page.locator('#session-state')).toHaveText('Ready');
  await expect(page.locator('#messages')).toContainText('hello workspace');
  // Safe text and error states, using only a mocked response for hostile display strings.
  await page.route('**/api/sessions/*/workspace',route=>route.fulfill({json:{cwd:'<img src=x onerror=alert(1)>',status:'non_git',message:'Not a repository'}}));
  await page.locator('#session-workspace').click();await expect(page.locator('#workspace-info-status')).toHaveText('Not a Git worktree');
  await expect(page.locator('#workspace-info-fields')).toContainText('<img src=x onerror=alert(1)>');expect(await page.locator('#workspace-info-fields img').count()).toBe(0);
  await page.locator('#dismiss-workspace-info').click();
  await page.route('**/api/sessions/*/workspace',route=>route.fulfill({status:503,body:'Git unavailable'}));
  await page.locator('#session-workspace').click();await expect(page.locator('#workspace-info-status')).toHaveText('Workspace context unavailable');
  await page.locator('#dismiss-workspace-info').click();expect(errors).toEqual([]);
  expect(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBe(true);await remove(s);
});
