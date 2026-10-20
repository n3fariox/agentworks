import {createServer} from 'node:http';
import test from 'node:test';
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {once} from 'node:events';
import {mkdtemp,writeFile,access,rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {stopChild,removeFixture,serverReady} from './support/cleanup.mjs';

async function child(t,source){
  const proc=spawn(process.execPath,['-e',source],{stdio:['ignore','pipe','pipe']});
  t.after(()=>stopChild(proc,{timeout:100}));
  await once(proc.stdout,'data');return proc;
}
test('failed fixture body still stops the server before deleting its store',{timeout:5000},async t=>{
  const dir=await mkdtemp(join(tmpdir(),'agentworks-cleanup-'));
  t.after(()=>rm(dir,{recursive:true,force:true}));
  const proc=await child(t,"console.log('ready');setInterval(()=>{},1000)");
  await assert.rejects(async()=>{
    try{await writeFile(join(dir,'sessions.sqlite3'),'fixture');throw Error('injected assertion');}
    finally{await stopChild(proc);await rm(dir,{recursive:true,force:true});}
  },/injected assertion/);
  assert.notEqual(proc.signalCode,null);
  await assert.rejects(access(dir),{code:'ENOENT'});
  await stopChild(proc); // Repeated teardown after exit is harmless.
});
test('hung child is killed and reaped within bounded teardown',{timeout:5000},async t=>{
  const proc=await child(t,"process.on('SIGTERM',()=>{});console.log('ready');setInterval(()=>{},1000)");
  await stopChild(proc,{timeout:100});
  assert.equal(proc.signalCode,'SIGKILL');
});
test('smoke fixture retention is explicit and cleanup is the default',{timeout:5000},async t=>{
  const original=process.env.AGENTWORKS_KEEP_TEST_FIXTURES;
  const dir=await mkdtemp(join(tmpdir(),'agentworks-retain-'));
  t.after(async()=>{if(original===undefined)delete process.env.AGENTWORKS_KEEP_TEST_FIXTURES;else process.env.AGENTWORKS_KEEP_TEST_FIXTURES=original;await rm(dir,{recursive:true,force:true});});
  process.env.AGENTWORKS_KEEP_TEST_FIXTURES='1';await removeFixture(dir);await access(dir);
  delete process.env.AGENTWORKS_KEEP_TEST_FIXTURES;await removeFixture(dir);
  await assert.rejects(access(dir),{code:'ENOENT'});
});

test('failed spawn does not wait forever for an exit event', {timeout:5000},async()=>{
  const proc=spawn('/nonexistent/agentworks-fixture-executable',[],{stdio:'ignore'});
  const [error]=await once(proc,'error');assert.equal(error.code,'ENOENT');
  await stopChild(proc);
});

test('readiness rejects a different process on the test port before fixture mutations',{timeout:5000},async t=>{
  const server=createServer((request,response)=>{assert.equal(request.method,'GET');response.end(JSON.stringify({pid:process.pid}));});
  await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
  t.after(()=>new Promise(resolve=>server.close(resolve)));
  const base=`http://127.0.0.1:${server.address().port}`;
  assert.equal(await serverReady({pid:process.pid},base),true);
  await assert.rejects(serverReady({pid:process.pid+1},base),/belongs to PID/);
});
