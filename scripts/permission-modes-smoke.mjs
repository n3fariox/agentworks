import {serverReady,stopChild,removeFixture} from '../tests/support/cleanup.mjs';
// Isolated permission-mode selection and native reconnect; no model inference.
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {mkdtemp,readFile,writeFile} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';
import {chromium} from '@playwright/test';
const dir=await mkdtemp(join(tmpdir(),'agentworks-modes-')),base='http://127.0.0.1:4481';
const env={...process.env,AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:'4481',AGENTWORKS_ROOTS:dir,AGENTWORKS_CATALOG_PATH:join(dir,'catalog.json'),AGENTWORKS_DB_PATH:join(dir,'sessions.sqlite3'),AGENTWORKS_DEMO_STATE_DIR:join(dir,'demo'),AGENTWORKS_DEMO_MODES:'1'};
let server,browser,spawnError;let output='';
async function api(path,body,expected=200){const r=await fetch(base+path,{method:body===undefined?'GET':'POST',headers:{Origin:base,'Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body)});assert.equal(r.status,expected,await r.clone().text());return r.headers.get('content-type')?.includes('application/json')?r.json():r.text();}
async function until(fn){for(let i=0;i<200;i++){const x=await fn();if(x)return x;await new Promise(r=>setTimeout(r,50));}throw Error('Timed out '+output);}
async function start(){spawnError=null;server=spawn(resolve(process.env.AGENTWORKS_TEST_BINARY||'target/debug/agentworks'),[],{env,stdio:['ignore','pipe','pipe']});server.on('error',error=>spawnError=error);server.stdout.on('data',b=>output+=b);server.stderr.on('data',b=>output+=b);await until(async()=>{if(spawnError)throw spawnError;if(server.exitCode!==null||server.signalCode!==null)throw Error('Server exited: '+output);return serverReady(server,base);});}
async function stop(){await stopChild(server,{signal:'SIGINT'});server=null;}
try{
 await start();const s=await api('/api/sessions',{provider:'demo',cwd:dir}),path='/api/sessions/'+s.id;
 await until(async()=>(await api(path)).session.status==='idle');
 assert.equal((await api(path+'/permission-mode')).currentValue,'default');
 await api(path+'/permission-mode',{mode:'invented'},400);
 browser=await chromium.launch({headless:true});const page=await browser.newPage();const errors=[];page.on('pageerror',e=>errors.push(e.message));await page.goto(base);await page.evaluate(id=>localStorage.setItem('agentworks-session',id),s.id);await page.reload();
 const picker=page.locator('#permission-mode-picker');await picker.waitFor({state:'visible'});await picker.selectOption('auto');
 await until(async()=>(await api(path+'/permission-mode')).currentValue==='auto');
 const before=await api(path),native=before.events.filter(e=>e.kind==='provider_session').at(-1).data.id;
 await api(path+'/prompt',{text:'slow',requestId:'mode-busy-test'});await api(path+'/permission-mode',{mode:'default'},409);await api(path+'/stop',{});await until(async()=>(await api(path)).session.status==='idle');
 await stop();const file=join(dir,'demo',native+'.json');const state=JSON.parse(await readFile(file,'utf8'));state.mode='default';await writeFile(file,JSON.stringify(state));
 await start();await api(path+'/resume',{});await until(async()=>(await api(path)).session.status==='idle');
 assert.equal((await api(path+'/permission-mode')).currentValue,'auto');assert.equal(JSON.parse(await readFile(file,'utf8')).mode,'auto');
 const after=await api(path);assert.equal(after.events.filter(e=>e.kind==='provider_session').at(-1).data.id,native);assert.equal(after.events.filter(e=>e.kind==='user').length,1);assert.deepEqual(errors,[]);
 console.log('PASS mode UI/API, invalid/busy rejection, saved selection restored into same native context without replay');
}finally{await browser?.close();if(server)await stop();await removeFixture(dir);}
