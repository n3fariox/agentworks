import {removeFixture} from '../tests/support/cleanup.mjs';
// Project archive/delete: sessions follow the project by default, or move to Ungrouped.
// Isolated demo server; no credentials, real providers, or shared fixtures are used.
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {once} from 'node:events';
import {mkdtemp} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';
import {chromium,expect} from '@playwright/test';
const dir=await mkdtemp(join(tmpdir(),'agentworks-projects-'));
const port=Number(process.env.AGENTWORKS_PROJECTS_PORT||4411);
const base=`http://127.0.0.1:${port}`,env={...process.env,AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:String(port),AGENTWORKS_ROOTS:dir,AGENTWORKS_CATALOG_PATH:join(dir,'catalog.json'),AGENTWORKS_DB_PATH:join(dir,'sessions.sqlite3'),AGENTWORKS_DEMO_STATE_DIR:join(dir,'demo')};
let server,browser;
const delay=ms=>new Promise(r=>setTimeout(r,ms));
async function api(path,body,method=body===undefined?'GET':'POST',status=200){const r=await fetch(base+path,{method,headers:{Origin:base,'Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body),signal:AbortSignal.timeout(10000)});const text=await r.text();assert.equal(r.status,status,text);return status===200?JSON.parse(text):text;}
async function until(fn){for(let i=0;i<180;i++){const v=await fn();if(v)return v;await delay(80);}throw Error('Project lifecycle wait timed out');}
async function start(){server=spawn(resolve('target/debug/agentworks'),[],{env,stdio:['ignore','inherit','inherit']});await until(async()=>{if(server.exitCode!==null)throw Error('Server exited');try{return(await fetch(base+'/api/status')).ok;}catch{return false;}});}
async function stop(){const done=once(server,'exit');server.kill('SIGINT');const timer=setTimeout(()=>server.kill('SIGKILL'),6000);timer.unref();await done;clearTimeout(timer);server=null;}
const all=()=>api('/api/sessions?archived=all'),find=async id=>(await all()).find(s=>s.id===id);
const project=async name=>{const p=await api('/api/projects',{name});const ids=[];for(const title of ['one','two'])ids.push((await api('/api/sessions',{provider:'demo',cwd:dir,title:`${name} ${title}`,projectId:p.id})).id);return {p,ids};};
const catalogProject=async id=>(await api('/api/catalog')).projects.find(p=>p.id===id);
try{
 await start();
 // Archive cascades by default; unarchive restores the project and its sessions.
 let {p,ids}=await project('Archive cascade');
 await api(`/api/projects/${p.id}/archive`,{archived:true});
 assert.equal((await catalogProject(p.id)).archived,true);
 for(const id of ids){const s=await find(id);assert.equal(s.archived,true);assert.equal(s.projectId,p.id);}
 await api(`/api/projects/${p.id}/archive`,{archived:false});
 assert.equal((await catalogProject(p.id)).archived,false);
 for(const id of ids)assert.equal((await find(id)).archived,false);
 // Archive with ungroup leaves sessions active in Ungrouped.
 ({p,ids}=await project('Archive ungroup'));
 await api(`/api/projects/${p.id}/archive`,{archived:true,sessions:'ungroup'});
 for(const id of ids){const s=await find(id);assert.equal(s.archived,false);assert.equal(s.projectId,null);}
 // Delete cascades by default.
 ({p,ids}=await project('Delete cascade'));
 await api(`/api/projects/${p.id}`,undefined,'DELETE');
 assert.equal(await catalogProject(p.id),undefined);
 for(const id of ids)assert.equal(await find(id),undefined);
 // Delete with ungroup keeps the previous behavior.
 ({p,ids}=await project('Delete ungroup'));
 await api(`/api/projects/${p.id}?sessions=ungroup`,undefined,'DELETE');
 for(const id of ids)assert.equal((await find(id)).projectId,null);
 await api(`/api/projects/${p.id}`,undefined,'DELETE',400);
 // State survives restart.
 const archived=(await project('Archived restart')).p;await api(`/api/projects/${archived.id}/archive`,{archived:true});
 await stop();await start();
 assert.equal((await catalogProject(archived.id)).archived,true);
 for(const s of (await all()).filter(s=>s.projectId===archived.id))assert.equal(s.archived,true);
 // Browser: archive from the project dialog, then find it under archived sessions.
 ({p,ids}=await project('Browser archive'));
 browser=await chromium.launch({headless:true});const page=await browser.newPage();page.setDefaultTimeout(10000);const errors=[];page.on('pageerror',e=>errors.push(e.message));
 await page.goto(base);await page.locator('#view-projects').click();await page.locator('#session-filter').selectOption('active');
 const group=name=>page.locator('#sessions > details').filter({has:page.locator(':scope > summary .tree-label',{hasText:name})});
 await expect(group('Browser archive')).toBeVisible();await expect(group('Archived restart')).toHaveCount(0);
 await page.getByRole('button',{name:'Edit project Browser archive',exact:true}).click();
 await page.locator('#archive-project').click();
 await expect(page.locator('#project-remove-description')).toContainText('archives its 2 sessions');
 await page.locator('#confirm-project-remove').click();
 await expect(page.locator('#project-remove-dialog')).not.toBeVisible();await expect(group('Browser archive')).toHaveCount(0);
 for(const id of ids)assert.equal((await find(id)).archived,true);
 await page.locator('#session-filter').selectOption('archived');await expect(group('Browser archive')).toContainText('Archived');
 await page.getByRole('button',{name:'Edit project Browser archive',exact:true}).click();
 await expect(page.locator('#archive-project')).toHaveText('Unarchive project');await page.locator('#archive-project').click();
 await expect(page.locator('#project-dialog')).not.toBeVisible();
 for(const id of ids)assert.equal((await find(id)).archived,false);
 // Browser: delete while keeping sessions.
 await page.locator('#session-filter').selectOption('active');
 await page.getByRole('button',{name:'Edit project Browser archive',exact:true}).click();
 await page.locator('#delete-project').click();await page.locator('#project-remove-ungroup').check();await page.locator('#confirm-project-remove').click();
 await expect(group('Browser archive')).toHaveCount(0);
 for(const id of ids)assert.equal((await find(id)).projectId,null);
 assert.deepEqual(errors,[]);
 console.log('Project lifecycle smoke passed');
}finally{await browser?.close();if(server)await stop();await removeFixture(dir);}
