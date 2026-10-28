import {test,expect} from '@playwright/test';
import {spawn,execFileSync} from 'node:child_process';
import {once} from 'node:events';
import {mkdir,mkdtemp,writeFile,readFile,rm} from 'node:fs/promises';
import {resolve,join} from 'node:path';

const base='http://127.0.0.1:4533';
test.use({baseURL:base});
let dir,repo,server;
async function api(path,method='GET',body){const response=await fetch(base+path,{method,headers:{Origin:base,'Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body)});expect(response.ok,await response.clone().text()).toBe(true);return response.json();}
test.beforeAll(async()=>{
  await mkdir('.cache',{recursive:true});dir=await mkdtemp(resolve('.cache/git-settings-ui-'));repo=join(dir,'repo');await mkdir(repo);
  const env=Object.fromEntries(Object.entries(process.env).filter(([key])=>!key.startsWith('GIT_')));
  execFileSync('git',['init','-b','main'],{cwd:repo,env});
  await writeFile(join(repo,'tracked.txt'),'committed\n');execFileSync('git',['add','.'],{cwd:repo,env});
  execFileSync('git',['-c','core.hooksPath=/dev/null','-c','user.name=Test','-c','user.email=test@localhost','commit','-m','fixture'],{cwd:repo,env});
  server=spawn(resolve('target/debug/agentworks'),[],{env:{...process.env,AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:'4533',AGENTWORKS_ROOTS:dir,AGENTWORKS_CATALOG_PATH:join(dir,'catalog.json'),AGENTWORKS_DB_PATH:join(dir,'sessions.sqlite3')},stdio:['ignore','pipe','pipe']});
  let log='';for(const stream of [server.stdout,server.stderr])stream.on('data',data=>{log+=data;});
  for(let i=0;i<150;i++){if(server.exitCode!==null)throw new Error(log);try{if((await fetch(base+'/api/status')).ok)return;}catch{}await new Promise(ok=>setTimeout(ok,50));}
  throw new Error('Isolated server did not start: '+log);
});
test.afterAll(async()=>{if(server&&server.exitCode===null){const exited=once(server,'exit');server.kill('SIGINT');await exited;}if(dir)await rm(dir,{recursive:true,force:true});});

for(const mobile of [false,true])test(`Git defaults and explicit repository save ${mobile?'mobile':'desktop'}`,async({page})=>{
  const initial=await api('/api/settings');await api('/api/settings','PUT',{...initial,git:{copy:'plain',share:[],runHooks:false}});
  const errors=[];page.on('pageerror',error=>errors.push(error.message));
  if(mobile)await page.setViewportSize({width:390,height:844});
  await page.goto('/');await page.locator('#nav-settings').click();await page.locator('#settings-tab-git').click();
  await expect(page.locator('#settings-panel-git')).toBeVisible();
  await page.locator('#git-copy').selectOption('prefer-cow');await page.locator('#git-run-hooks').check();
  await page.locator('#git-repo-cwd').fill(dir+'/re');
  await expect(page.locator('#git-repo-paths option')).toHaveAttribute('value',repo);
  await page.locator('#git-repo-cwd').fill(dir+'/');await page.locator('#git-repo-browse').click();
  await expect(page.locator('#directory-picker-dialog')).toBeVisible();
  await page.locator('#directory-picker-list').getByRole('button',{name:'repo',exact:true}).click();
  await page.locator('#directory-picker-select').click();
  await expect(page.locator('#directory-picker-dialog')).toBeHidden();
  await expect(page.locator('#git-repo-cwd')).toHaveValue(repo);
  await expect(page.locator('#git-repo-editor')).toBeVisible();
  const text=`version = 1\n[future]\nlabel = "${mobile?'mobile':'desktop'}"\n[git.worktrees]\ncopy = "require-cow"\n`;
  await page.locator('#git-repo-text').fill(text);await page.locator('#git-repo-save').click();await expect(page.locator('#git-repo-status')).toContainText('Saved');
  expect(await readFile(join(repo,'.agentworks/config.toml'),'utf8')).toBe(text);
  expect((await api('/api/settings')).git.runHooks).toBe(false,'Repository save must not also save global defaults');
  await page.locator('#save-settings').click();await expect(page.locator('#settings-dialog')).toBeHidden();
  expect((await api('/api/settings')).git).toMatchObject({copy:'prefer-cow',runHooks:true});
  await page.locator('#nav-settings').click();await page.locator('#settings-tab-git').click();await expect(page.locator('#git-copy')).toHaveValue('prefer-cow');
  await page.locator('#git-repo-cwd').fill(repo);await page.locator('#git-repo-load').click();await expect(page.locator('#git-repo-editor')).toBeVisible();await expect(page.locator('#git-repo-text')).toHaveValue(text);await expect(page.locator('#git-repo-effective')).toContainText('require-cow');
  const external=text+'\n# concurrent edit\n';await writeFile(join(repo,'.agentworks/config.toml'),external);
  await page.locator('#git-repo-text').fill(text+'\n# stale edit\n');await page.locator('#git-repo-save').click();await expect(page.locator('#git-repo-status')).toContainText('changed');
  expect(await readFile(join(repo,'.agentworks/config.toml'),'utf8')).toBe(external);
  expect(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBe(true);expect(errors).toEqual([]);
});

test('repository completion follows the selected host and ignores a late previous-host listing',async({page})=>{
  let releaseLocal,startedLocal;
  const pendingLocal=new Promise(resolve=>{releaseLocal=resolve;}),started=new Promise(resolve=>{startedLocal=resolve;});
  await page.route('**/api/catalog',async route=>{
    const response=await route.fetch(),catalog=await response.json();
    catalog.hosts.push({id:'completion-remote',name:'Completion remote',kind:'remote',status:'connected',roots:['/remote']});
    await route.fulfill({response,json:catalog});
  });
  await page.route('**/api/directories?**',async route=>{
    const url=new URL(route.request().url());
    if(url.searchParams.get('hostId')==='completion-remote')return route.fulfill({json:{path:'/remote',root:'/remote',parent:null,entries:[{name:'repo',path:'/remote/repo'}]}});
    if(url.searchParams.get('path')===dir+'/slow/'){
      startedLocal();await pendingLocal;
      return route.fulfill({json:{path:dir+'/slow',root:dir,parent:dir,entries:[{name:'stale',path:dir+'/slow/stale'}]}});
    }
    return route.continue();
  });
  try{
    await page.goto('/');await page.locator('#nav-settings').click();await page.locator('#settings-tab-git').click();
    await page.locator('#git-repo-cwd').fill(dir+'/slow/re');await started;
    await page.locator('#git-repo-host').selectOption('completion-remote');
    await expect(page.locator('#git-repo-cwd')).toHaveValue('');
    await page.locator('#git-repo-cwd').fill('/remote/re');
    await expect(page.locator('#git-repo-paths option')).toHaveAttribute('value','/remote/repo');
    const late=page.waitForResponse(response=>new URL(response.url()).searchParams.get('path')===dir+'/slow/');
    releaseLocal();await late;
    await expect(page.locator('#git-repo-paths option')).toHaveAttribute('value','/remote/repo');
  }finally{releaseLocal();}
});
