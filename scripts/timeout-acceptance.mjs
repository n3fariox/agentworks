// Isolated L-07 persistence/failure and real runtime acceptance (~65s), no model inference.
import assert from 'node:assert/strict';
import {spawn,execFileSync} from 'node:child_process';
import {once} from 'node:events';
import {mkdtemp} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';
const dir=await mkdtemp(join(tmpdir(),'agentworks-timeout-acceptance-')),db=join(dir,'sessions.sqlite3'),base='http://127.0.0.1:4487';
const env={...process.env,AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:'4487',AGENTWORKS_ROOTS:dir,AGENTWORKS_CATALOG_PATH:join(dir,'catalog.json'),AGENTWORKS_DB_PATH:db,AGENTWORKS_DEMO_STATE_DIR:join(dir,'demo')};
let server,log='';
const sleep=ms=>new Promise(r=>setTimeout(r,ms));
async function api(path,body,method,expected=200){const r=await fetch(base+path,{method:method||(body===undefined?'GET':'POST'),headers:{Origin:base,'Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body)});const t=await r.text();assert.equal(r.status,expected,t);try{return JSON.parse(t);}catch{return t;}}
async function until(fn,ms=10000){const until=Date.now()+ms;while(Date.now()<until){const x=await fn();if(x)return x;await sleep(100);}throw Error('Timeout '+log);}
async function start(){server=spawn(resolve(process.env.AGENTWORKS_TEST_BINARY||'target/debug/agentworks'),[],{env,stdio:['ignore','pipe','pipe']});for(const stream of [server.stdout,server.stderr])stream.on('data',s=>log+=s);await until(async()=>{if(server.exitCode!==null)throw Error(log);try{return(await fetch(base+'/api/status')).ok;}catch{return false;}});}
async function stop(){const exited=once(server,'exit');server.kill('SIGINT');const kill=setTimeout(()=>server.kill('SIGKILL'),6000);await exited;clearTimeout(kill);server=null;}
const policy=(total=1200)=>({totalSeconds:total,totalIncludesHumanWait:true,inactivityWarningSeconds:null,inactivityTimeoutSeconds:null,cancelGraceSeconds:5});
const snap=id=>api('/api/sessions/'+id);
const idle=id=>until(async()=>{const s=await snap(id);assert.notEqual(s.session.status,'error',JSON.stringify(s.events.slice(-2)));return s.session.status==='idle'&&s;});
const native=s=>s.events.filter(e=>e.kind==='provider_session').at(-1)?.data.id;
const sql=(source,...args)=>execFileSync('python3',['-c',source,db,...args],{encoding:'utf8'});
try{
 await start();const a=await api('/api/sessions',{provider:'demo',cwd:dir});await idle(a.id);
 await api('/api/timeouts',policy(),'PUT');assert.equal((await api('/api/timeouts')).default.totalSeconds,1200);
 await api('/api/timeouts',policy(1),'PUT',400);
 await api('/api/settings',{roots:[dir],defaultModels:{},timeouts:policy(1)},'PUT',400);
 assert.equal((await api('/api/timeouts')).default.totalSeconds,1200);
 await api('/api/settings',{roots:[dir],defaultModels:{}},'PUT');assert.equal((await api('/api/timeouts')).default.totalSeconds,1200);
 await api(`/api/sessions/${a.id}/timeouts`,{policy:policy(1800)},'PUT');
 const before=await snap(a.id),provider=native(before);
 // Inject a SQLite failure only for the attempted new override, without touching any live data.
 sql("import sqlite3,sys\nc=sqlite3.connect(sys.argv[1]);c.execute(\"CREATE TRIGGER fail_timeout_save BEFORE UPDATE OF state ON sessions WHEN json_extract(NEW.state,'$.timeouts.totalSeconds')=777 BEGIN SELECT RAISE(FAIL,'fixture write failure'); END\");c.commit()");
 await api(`/api/sessions/${a.id}/timeouts`,{policy:policy(777)},'PUT',507);
 const failed=await snap(a.id);assert.equal(failed.session.timeouts.totalSeconds,1800);assert.equal(failed.seq,before.seq);
 assert.equal(Number(sql("import sqlite3,sys\nc=sqlite3.connect(sys.argv[1]);print(c.execute(\"SELECT json_extract(state,'$.timeouts.totalSeconds') FROM sessions WHERE id=?\",(sys.argv[2],)).fetchone()[0])",a.id)),1800);
 sql("import sqlite3,sys\nc=sqlite3.connect(sys.argv[1]);c.execute('DROP TRIGGER fail_timeout_save');c.commit()");
 await stop();await start();assert.equal((await api('/api/timeouts')).default.totalSeconds,1200);let restored=await snap(a.id);assert.equal(restored.session.timeouts.totalSeconds,1800);
 await api(`/api/sessions/${a.id}/resume`,{});assert.equal(native(await idle(a.id)),provider);
 await api(`/api/sessions/${a.id}/timeouts`,{policy:null},'PUT');assert.equal((await snap(a.id)).session.effectiveTimeouts.totalSeconds,1200);
 // Two approval-blocked real turns: included waits expire, excluded waits pause total and inactivity clocks.
 const cases=[];
 for(const includeWait of [true,false]){
  const s=await api('/api/sessions',{provider:'demo',cwd:dir,resumePolicy:{enabled:true}});await idle(s.id);
  await api(`/api/sessions/${s.id}/timeouts`,{policy:{...policy(60),totalIncludesHumanWait:includeWait,inactivityWarningSeconds:30,inactivityTimeoutSeconds:60}},'PUT');
  await api(`/api/sessions/${s.id}/prompt`,{text:'quota active-approval',requestId:'timeout-wait-'+includeWait});
  await until(async()=>(await snap(s.id)).approvals.length===1);cases.push(s);
 }
 const timed=await until(async()=>{const s=await snap(cases[0].id);return s.session.status==='idle'&&s;},75000);
 assert.equal(timed.events.find(e=>e.kind==='timeout')?.data.kind,'total');assert.equal(timed.events.find(e=>e.kind==='turn_end')?.data.timedOut,'total');assert.equal(timed.approvals.length,0);assert.equal(timed.session.continuation.dueAt,null);assert.equal(timed.session.continuation.limited,false);assert.equal(timed.session.continuation.attempts,0);assert.equal(timed.session.continuation.phase,null);assert.equal(timed.events.filter(e=>e.kind==='user').length,1);
 const paused=await snap(cases[1].id);assert.equal(paused.session.status,'waiting');assert.equal(paused.events.filter(e=>e.kind==='timeout'||e.kind==='timeout_warning').length,0);
 const approval=paused.approvals[0];const allow=approval.request.options.find(o=>o.kind==='allow_once');assert(allow);
 await api(`/api/sessions/${cases[1].id}/approvals/${approval.id}`,{optionId:allow.optionId});const done=await idle(cases[1].id);assert(!done.events.some(e=>e.kind==='timeout'));assert.equal(done.session.continuation.dueAt,null);
 console.log('PASS timeout validation/settings consistency, failed-write rollback, restart/native preservation, override clearing, real total expiry, paused approval clocks and no quota replay');
}finally{if(server)await stop();}
