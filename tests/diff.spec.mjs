import {test,expect} from '@playwright/test';
import {spawn,execFileSync} from 'node:child_process';
import {mkdtemp,mkdir,writeFile,rm,readFile,symlink} from 'node:fs/promises';
import {resolve,join} from 'node:path';
import {tmpdir} from 'node:os';

const base='http://127.0.0.1:4421';
const gitBin=execFileSync('which',['git'],{encoding:'utf8'}).trim();
const git=(cwd,...args)=>execFileSync(gitBin,['--no-optional-locks','-c','user.name=Diff Fixture','-c','user.email=diff@localhost',...args],{cwd,encoding:'utf8',env:{...process.env,GIT_CONFIG_NOSYSTEM:'1',GIT_CONFIG_GLOBAL:'/dev/null'}}).trim();
let fixture,repo,linked,nested,initial,head,session,server,exited=false,log='';
async function api(path,body,method){
  const r=await fetch(base+path,{method:method||(body?'POST':'GET'),headers:{Origin:base,'Content-Type':'application/json'},body:body?JSON.stringify(body):undefined});
  expect(r.ok,await r.clone().text()).toBe(true);return r.json();
}
const create=cwd=>api('/api/sessions',{provider:'demo',cwd,title:'Diff fixture'});
const diff=(params={},id=session.id)=>api(`/api/sessions/${id}/diff?${new URLSearchParams(params)}`);

test.beforeAll(async()=>{
  fixture=await mkdtemp(join(tmpdir(),'agentworks-diff-'));repo=join(fixture,'main');linked=join(fixture,'linked');nested=join(linked,'nested');
  await mkdir(repo);git(repo,'init','-b','main');
  for(const [name,content] of [['sample.js','const old = 1;\n// keep\n'],['both.txt','initial\n'],['staged.txt','initial\n'],['large.txt','small\n'],['binary.dat',Buffer.from([0,1,2])],['literal[1].txt','old\n'],['literal1.txt','old\n'],['with space.txt','old\n'],['line\nbreak.txt','old\n'],[':(glob)*','old\n']])await writeFile(join(repo,name),content);
  git(repo,'add','.');git(repo,'commit','-m','Initial');initial=git(repo,'rev-parse','HEAD');
  git(repo,'worktree','add','-b','feature',linked);await mkdir(nested);
  await writeFile(join(linked,'branch.txt'),'feature only\n');git(linked,'add','.');git(linked,'commit','-m','Feature');head=git(linked,'rev-parse','HEAD');
  await writeFile(join(repo,'base-only.txt'),'base only\n');git(repo,'add','.');git(repo,'commit','-m','Base advanced');git(repo,'branch','selectable');
  await writeFile(join(linked,'both.txt'),'staged version\n');await writeFile(join(linked,'staged.txt'),'staged only\n');git(linked,'add','both.txt','staged.txt');
  await writeFile(join(linked,'both.txt'),'working version\n');await writeFile(join(linked,'untracked.txt'),'untracked secret\n');
  await writeFile(join(linked,'binary.dat'),Buffer.from([0,3,4]));await writeFile(join(linked,'large.txt'),'long line '.repeat(40000)+'\n');
  await writeFile(join(linked,'literal[1].txt'),'literal change\n');await writeFile(join(linked,'literal1.txt'),'other change\n');
  for(const name of ['with space.txt','line\nbreak.txt',':(glob)*'])await writeFile(join(linked,name),'special '+name+'\n');
  await writeFile(join(linked,'sample.js'),'const next = 42;  \t\n// keep\n');
  // Repo-configured helper scripts must never run, even when attributes request them.
  const helper=join(fixture,'helper');
  await writeFile(helper,`#!/bin/sh\ntouch '${join(fixture,'HELPER_RAN')}'\nexit 1\n`,{mode:0o755});
  git(linked,'config','diff.external',helper);git(linked,'config','diff.unsafe.command',helper);git(linked,'config','diff.unsafe.textconv',helper);
  await writeFile(join(git(linked,'rev-parse','--git-common-dir'),'info/attributes'),'*.txt diff=unsafe\n');
  await mkdir(join(fixture,'bin'));
  const quote=s=>"'"+s.replaceAll("'","'\\''")+"'";
  await writeFile(join(fixture,'bin/git'),`#!/bin/sh\ncase "$PWD" in */slow) for arg in "$@"; do if [ "$arg" = diff ]; then exec sleep 20; fi; done;; esac\nexec ${quote(gitBin)} "$@"\n`,{mode:0o755});
  server=spawn(resolve('target/debug/agentworks'),[],{env:{...process.env,PATH:join(fixture,'bin')+':'+process.env.PATH,
    AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:'4421',AGENTWORKS_ROOTS:fixture,
    AGENTWORKS_CATALOG_PATH:join(fixture,'catalog.json'),AGENTWORKS_DB_PATH:join(fixture,'sessions.sqlite3'),AGENTWORKS_DEMO_STATE_DIR:join(fixture,'demo'),
    GIT_DIR:join(repo,'.git'),GIT_WORK_TREE:repo},stdio:['ignore','pipe','pipe']});
  server.on('error',e=>{log+=e.message;exited=true;});server.on('exit',()=>exited=true);
  for(const s of [server.stdout,server.stderr])s.on('data',s=>log+=s.toString());
  for(let i=0;i<100;i++){if(exited)throw Error(log);try{if((await fetch(base+'/api/status')).ok)break;}catch{}await new Promise(r=>setTimeout(r,100));}
  session=await create(nested);
});
test.afterAll(async()=>{
  if(server&&!exited){const stopped=new Promise(r=>server.once('exit',r));server.kill('SIGTERM');await stopped;}
  if(fixture)await rm(fixture,{recursive:true,force:true});
});

test('unstaged is index-to-worktree, staged/untracked separate; literal selection and no writes',async()=>{
  const index=join(linked,'.git');const indexPath=git(linked,'rev-parse','--absolute-git-dir')+'/index';
  const before=await readFile(indexPath);const gitFile=await readFile(index);const status=git(linked,'status','--porcelain');
  const d=await diff({file:'both.txt'});
  expect(d.context).toMatchObject({cwd:nested,worktreeRoot:linked,branch:'feature'});
  expect(d.files).toContain('both.txt');expect(d.files).not.toContain('staged.txt');expect(d.files).not.toContain('untracked.txt');
  expect(d.staged.files).toEqual(['both.txt','staged.txt']);expect(d.untracked.files).toEqual(['untracked.txt']);
  const expected=execFileSync(gitBin,['--no-optional-locks','--literal-pathspecs','diff','--no-color','--no-ext-diff','--no-textconv','--no-renames','--ignore-submodules=all','--patch','--unified=3','--','both.txt'],{cwd:linked,encoding:'utf8'});
  expect(d.patch).toBe(expected);
  expect(d.patch).toContain('-staged version');expect(d.patch).toContain('+working version');expect(d.patch).not.toContain('-initial');
  const literal=await diff({file:'literal[1].txt'});expect(literal.patch).toContain('+literal change');expect(literal.patch).not.toContain('+other change');
  for(const file of ['with space.txt','line\nbreak.txt',':(glob)*']){const special=await diff({file});expect(special.files).toContain(file);expect(special.patch).toContain('+special ');expect(special.patch).not.toContain('+working version');}
  expect((await diff({file:'binary.dat'})).patch).toContain('Binary files');
  const large=await diff({file:'large.txt'});expect(large.patch).toBeNull();expect(large.patchUnavailable).toContain('limit');
  expect(await readFile(join(fixture,'HELPER_RAN')).catch(()=>null)).toBeNull();
  expect(await readFile(indexPath)).toEqual(before);expect(await readFile(index)).toEqual(gitFile);expect(git(linked,'status','--porcelain')).toBe(status);
});

test('branch uses merge base of selectable base; commit/root/merge/detached comparisons',async()=>{
  const d=await diff({mode:'branch',base:'selectable',file:'branch.txt'});
  expect(d.mergeBase).toBe(initial);expect(d.to).toBe(head);expect(d.files).toEqual(['branch.txt']);expect(d.patch).toContain('+feature only');expect(d.bases).toContain('selectable');
  const commit=await diff({mode:'commit',commit:head,file:'branch.txt'});expect(commit.from).toBe(initial);expect(commit.files).toEqual(['branch.txt']);
  const root=await diff({mode:'commit',commit:initial,file:'both.txt'});expect(root.rootCommit).toBe(true);expect(root.patch).toContain('+initial');
  git(repo,'merge','--no-ff','feature','-m','Merge feature');const merge=git(repo,'rev-parse','HEAD');
  expect((await diff({mode:'commit',commit:merge})).files).toEqual(['branch.txt']);
  git(linked,'checkout','--detach');expect((await diff({mode:'branch',base:'main'})).context.head).toBe('detached');
});

test('fresh patch identity follows moved refs, and oversized file lists fail explicitly',async()=>{
  const s=await create(repo);const originalBase=git(repo,'rev-parse','selectable');
  const before=await diff({mode:'branch',base:initial},s.id);
  await writeFile(join(repo,'fresh.txt'),'new committed content\n');git(repo,'add','fresh.txt');git(repo,'commit','-m','Move HEAD');
  const current=git(repo,'rev-parse','HEAD');
  const after=await diff({mode:'branch',base:initial,file:'fresh.txt'},s.id);
  expect(after.to).toBe(current);expect(after.to).not.toBe(before.to);expect(after.resolvedBase).toBe(initial);expect(after.patch).toContain('+new committed content');
  git(repo,'branch','-f','selectable',current);
  const moved=await diff({mode:'branch',base:'selectable'},s.id);expect(moved.resolvedBase).toBe(current);expect(moved.mergeBase).toBe(current);expect(moved.files).toEqual([]);git(repo,'branch','-f','selectable',originalBase);
  const many=join(fixture,'many');await mkdir(many);git(many,'init','-b','many');
  await Promise.all(Array.from({length:2001},(_,i)=>writeFile(join(many,`file-${i}`),'file\n')));git(many,'add','.');git(many,'commit','-m','Many files');
  const m=await create(many);const r=await fetch(base+`/api/sessions/${m.id}/diff?mode=commit`);expect(r.status).toBe(400);expect(await r.text()).toContain('2000-file');
  expect(await readFile(join(fixture,'HELPER_RAN')).catch(()=>null)).toBeNull();
});

test('rejects refs/options, unknown selections; non-Git, unborn, current roots and timeout',async()=>{
  for(const params of [{mode:'branch',base:'--output=bad'},{mode:'commit',commit:'HEAD\nmain'},{mode:'commit',commit:'missing'},{mode:'invalid'},{file:'../main/both.txt'}]){
    const r=await fetch(base+`/api/sessions/${session.id}/diff?${new URLSearchParams(params)}`);expect(r.status).toBe(400);
  }
  const plain=join(fixture,'plain');await mkdir(plain);const p=await create(plain);expect((await diff({},p.id)).unavailable).toContain('Git worktree');
  const unborn=join(fixture,'unborn');await mkdir(unborn);git(unborn,'init','-b','new');const u=await create(unborn);expect((await diff({},u.id)).files).toEqual([]);
  const moved=join(fixture,'moved');await mkdir(moved);const m=await create(moved);await rm(moved,{recursive:true});await symlink('/tmp',moved);
  expect((await fetch(base+`/api/sessions/${m.id}/diff`)).status).toBe(403);
  await api('/api/settings',{roots:[nested],defaultModels:{}},'PUT');expect((await diff()).unavailable).toBeTruthy();await api('/api/settings',{roots:[fixture],defaultModels:{}},'PUT');
  const slow=join(fixture,'slow');await mkdir(slow);git(slow,'init','-b','slow');const s=await create(slow);const started=Date.now();
  const r=await fetch(base+`/api/sessions/${s.id}/diff`);expect(r.status).toBe(408);expect(Date.now()-started).toBeLessThan(12000);
  expect((await fetch(base+'/api/status')).ok).toBe(true);
});

for(const mobile of [false,true])test(`diff browser ${mobile?'mobile':'desktop'}: selection, refs, safe text, draft preserved`,async({page})=>{
  if(mobile)await page.setViewportSize({width:390,height:844});
  const errors=[];page.on('pageerror',e=>errors.push(e.message));let sockets=0;page.on('websocket',()=>sockets++);
  await page.addInitScript(id=>localStorage.setItem('agentworks-session',id),session.id);await page.goto(base);
  await expect(page.locator('#session-state')).toHaveText('Ready');await page.locator('#prompt').fill('Keep diff draft');const before=sockets;
  if(mobile)await page.locator('#session-more').click();
  await page.getByRole('button',{name:'Diff',exact:true}).click();await expect(page.locator('#diff-status')).toContainText('changed file');
  await expect(page.locator('#diff-context')).toContainText(nested);await page.locator('#diff-files').selectOption('both.txt');
  await expect(page.locator('#diff-patch')).toContainText('-staged version');await expect(page.locator('#diff-patch')).toContainText('+working version');
  await page.locator('#diff-mode').selectOption('branch');await page.locator('#diff-base').fill('selectable');await page.getByRole('button',{name:'Refresh diff',exact:true}).click();
  await expect(page.locator('#diff-comparison')).toContainText(initial);await page.locator('#diff-files').selectOption('branch.txt');await expect(page.locator('#diff-patch')).toContainText('+feature only');
  await page.locator('#diff-mode').selectOption('commit');await page.locator('#diff-commit').fill(initial);await page.getByRole('button',{name:'Refresh diff',exact:true}).click();
  await expect(page.locator('#diff-comparison')).toContainText('Empty tree');
  expect(await page.locator('#side-panel').evaluate(n=>{const r=n.getBoundingClientRect();return r.left>=0&&Math.round(r.right)<=innerWidth&&r.height<=innerHeight;})).toBe(true);
  expect(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBe(true);
  await page.keyboard.press('Escape');await expect(page.locator('#side-panel')).not.toBeVisible();await expect(page.locator('#prompt')).toHaveValue('Keep diff draft');expect(sockets).toBe(before);
  await page.route('**/api/sessions/*/diff?**',route=>route.fulfill({json:{context:{cwd:'<img src=x onerror=alert(1)>',status:'non_git'},unavailable:'No Git'}}));
  if(mobile)await page.locator('#session-more').click();
  await page.getByRole('button',{name:'Diff',exact:true}).click();await expect(page.locator('#diff-context')).toContainText('<img src=x onerror=alert(1)>');expect(await page.locator('#diff-context img').count()).toBe(0);
  await page.locator('#side-close').click();await expect(page.locator('#side-panel')).not.toBeVisible();expect(errors).toEqual([]);
});


test('diff syntax, add/remove backgrounds, old/new gutters, raw copy and display preference',async({page})=>{
  await page.addInitScript(id=>localStorage.setItem('agentworks-session',id),session.id);await page.goto(base);
  await page.evaluate(()=>{window.copied=null;Object.defineProperty(navigator,'clipboard',{configurable:true,value:{writeText:async text=>{window.copied=text;}}});});
  await expect(page.locator('#session-state')).toHaveText('Ready');
  await page.getByRole('button',{name:'Diff',exact:true}).click();
  await page.locator('#diff-files').selectOption('sample.js');
  const patch=page.locator('#diff-patch');
  await expect(patch.locator('.diff-add .tok-keyword')).toHaveText('const');
  await expect(patch.locator('.diff-remove .tok-number')).toHaveText('1');
  await expect(patch.locator('.diff-add')).toHaveAttribute('data-line','        1');
  await expect(patch.locator('.diff-remove')).toHaveAttribute('data-line','   1     ');
  const colors=await patch.evaluate(n=>['add','remove'].map(kind=>getComputedStyle(n.querySelector('.diff-'+kind)).backgroundColor));
  expect(colors[0]).not.toBe(colors[1]);expect(colors).not.toContain('rgba(0, 0, 0, 0)');
  const expected=(await diff({file:'sample.js'})).patch;
  expect(await patch.evaluate(n=>n.textContent)).toBe(expected);
  await page.getByRole('button',{name:'Copy js block',exact:true}).click();
  expect(await page.evaluate(()=>window.copied)).toBe(expected);
  await page.evaluate(()=>{navigator.clipboard.writeText=async()=>{throw Error('Unavailable');};document.execCommand=()=>false;});
  await page.getByRole('button',{name:'Copy js block',exact:true}).click();
  await expect(page.getByLabel('Raw js block for copying')).toHaveValue(expected);

  await page.locator('#nav-settings').evaluate(n=>n.click());await page.locator('#settings-tab-display').click();
  await page.getByLabel('Syntax highlighting').selectOption('off');await page.locator('#dismiss-settings').click();
  await expect(patch.locator('[class^=tok-]')).toHaveCount(0);
  await expect(patch.locator('.diff-add')).toHaveCount(1);
  await page.locator('#diff-files').selectOption('binary.dat');
  await expect(patch).toContainText('Binary files');await expect(patch.locator('.source-line, [class^=tok-]')).toHaveCount(0);
  await page.locator('#diff-files').selectOption('large.txt');await expect(page.locator('#diff-patch-note')).toContainText('limit');
});
