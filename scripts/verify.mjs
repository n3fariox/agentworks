import {serverReady,stopChild,removeFixture} from '../tests/support/cleanup.mjs';
import {mkdir,mkdtemp} from 'node:fs/promises';
// Verify a built prototype with its own server and deterministic teardown.
import {spawn} from 'node:child_process';
import {once} from 'node:events';
import {resolve,join} from 'node:path';
import assert from 'node:assert/strict';
import WebSocket from 'ws';
const port=process.env.AGENTWORKS_VERIFY_PORT||'4318';
const base=`http://127.0.0.1:${port}`;
await mkdir('.cache',{recursive:true});
const fixture=await mkdtemp(resolve('.cache/verify-'));
const env={...process.env,AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_ROOTS:resolve('.'),AGENTWORKS_DB_PATH:join(fixture,'sessions.sqlite3'),AGENTWORKS_DEMO_STATE_DIR:join(fixture,'demo'),AGENTWORKS_PORT:port,AGENTWORKS_URL:base,AGENTWORKS_CATALOG_PATH:join(fixture,'catalog.json')};
const server=spawn(resolve('target/debug/agentworks'),[],{env,stdio:['ignore','inherit','inherit']});
let exited=false;server.on('exit',()=>{exited=true;});
const delay=ms=>new Promise(r=>setTimeout(r,ms));
async function run(command,args,overrides={}){const child=spawn(command,args,{env:{...env,...overrides},stdio:'inherit'});const [code]=await once(child,'exit');assert.equal(code,0,`${command} ${args.join(' ')} failed`);}
let socket;
try{
  let ready=false;
  for(let i=0;i<50;i++){
    if(exited)throw new Error('Prototype server exited before becoming ready');
    ready=await serverReady(server,base);
    if(ready)break;await delay(100);
  }
  assert(ready,'Server readiness timed out');
  // U-05 progress: embedded Starlark crate, browser view logic and the running service.
  await run('cargo',['test','--locked','-p','agentworks-starlark']);
  await run('node',['--test','tests/bazel-progress.test.mjs']);
  await run('node',['--test','tests/fixture-cleanup.test.mjs']);
  await run('node',['tests/codex-subscription.test.mjs']);
  await run('node',['scripts/mcp-wire-smoke.mjs']);
  await run('node',['scripts/remote-host-smoke.mjs']);
  await run('node',['scripts/remote-plumbing-smoke.mjs']);
  await run('node',['scripts/remote-partition-smoke.mjs']);
  await run('node',['scripts/smoke.mjs']);
  await run('node',['scripts/progress-smoke.mjs']);
  await run('node',['scripts/projects-smoke.mjs']);
  await run('node',['scripts/settings-smoke.mjs']);
  await run('node',['scripts/import-smoke.mjs']);
  await run('node',['scripts/workspace-smoke.mjs']);
  await run('node',['scripts/completion-smoke.mjs']);
  await run('node',['scripts/review-queue-smoke.mjs']);
  await run('node',['scripts/worktree-policy-smoke.mjs']);
  await run('node',['scripts/worktree-cleanup-smoke.mjs']);
  await run('node',['scripts/worktree-cleanup-smoke.mjs'],{AGENTWORKS_CLEANUP_REMOTE:'1'});
  await run('node',['scripts/limit-resume-smoke.mjs']);
  await run('node',['scripts/lifecycle-smoke.mjs']);
  await run('node',['scripts/project-lifecycle-smoke.mjs']);
  await run('python3',['scripts/service/supervisor-test.py']);
  await run('node',['scripts/service-restart-smoke.mjs']);
  await run('node',['node_modules/@playwright/test/cli.js','test']);
  const config=await (await fetch(base+'/api/status')).json();
  const s=await (await fetch(base+'/api/sessions',{method:'POST',headers:{'Content-Type':'application/json',Origin:base},body:JSON.stringify({provider:'demo',cwd:config.roots[0],title:'Shutdown test'})})).json();
  socket=new WebSocket(base.replace('http','ws')+`/api/sessions/${s.id}/events`,{headers:{Origin:base}});
  await once(socket,'open');
  const closed=once(socket,'close');
  const exit=once(server,'exit');
  server.kill('SIGINT');
  const timer=setTimeout(()=>server.kill('SIGKILL'),8000);timer.unref();
  const [code]=await exit;clearTimeout(timer);
  await closed;
  assert.equal(code,0,'Server must shut down cleanly with a browser connection open');
  console.log('PASS: graceful shutdown with an open browser WebSocket');
}finally{socket?.terminate();await stopChild(server);await removeFixture(fixture);}
