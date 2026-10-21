// Hold a worker's transport open while its server stops sending frames. A second
// worker for the same host must wait until the first stops all its adapters.
// Uses only owned stdio pipes, loopback sockets and deterministic demo ACP.
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {createServer} from 'node:http';
import {connect} from 'node:net';
import {mkdir,mkdtemp,writeFile,readdir,readlink} from 'node:fs/promises';
import {resolve,join} from 'node:path';
import {WebSocketServer} from 'ws';
import {once} from 'node:events';
import {stopChild,removeFixture} from '../tests/support/cleanup.mjs';
await mkdir('.cache',{recursive:true});
const dir=await mkdtemp(resolve('.cache/remote-partition-'));
const bin=join(dir,'bin'),work=join(dir,'work');await mkdir(bin);await mkdir(work);
await writeFile(join(bin,'mise'),'#!/bin/sh\nprintf "{}\\n"\n',{mode:0o700});
const env={...process.env};for(const key of Object.keys(env))if(key.startsWith('AGENTWORKS_'))delete env[key];
Object.assign(env,{HOME:join(dir,'home'),PATH:bin+':'+env.PATH,AGENTWORKS_DEMO_STATE_DIR:join(dir,'demo')});
const hostId=crypto.randomUUID(),children=[],sockets=[],peers=[];
const server=createServer(),wss=new WebSocketServer({server});
const pause=ms=>new Promise(resolve=>setTimeout(resolve,ms));
const deadline=(promise,ms,label)=>new Promise((resolve,reject)=>{
  const timer=setTimeout(()=>reject(Error(label)),ms);
  promise.then(v=>{clearTimeout(timer);resolve(v);},e=>{clearTimeout(timer);reject(e);});
});
async function agents(){const result=[];for(const pid of await readdir('/proc')){
  if(!/^\d+$/.test(pid))continue;
  try{if(await readlink(`/proc/${pid}/cwd`)===work)result.push(Number(pid));}catch{}
}return result;}
async function launch(){
  const linked=once(wss,'connection');
  const child=spawn(resolve('target/debug/agentworks'),['agent','--stdio'],{env,stdio:['pipe','pipe','pipe']});
  children.push(child);let log='';child.stderr.on('data',chunk=>log+=chunk);
  child.on('error',()=>{});
  const exit=once(child,'exit');
  const socket=connect(server.address().port,'127.0.0.1');sockets.push(socket);
  socket.on('error',()=>{});child.stdin.on('error',()=>{});
  let prefix=Buffer.alloc(0);
  await deadline(new Promise((resolve,reject)=>{
    const read=chunk=>{
      prefix=Buffer.concat([prefix,chunk]);const end=prefix.indexOf(10);if(end<0)return;
      child.stdout.off('data',read);
      if(prefix.subarray(0,end).toString()!=='agentworks worker ready')return reject(Error('Missing ready line'));
      socket.write(prefix.subarray(end+1));child.stdout.pipe(socket);resolve();
    };child.stdout.on('data',read);
  }),5000,'No worker preamble');
  socket.pipe(child.stdin);
  const [ws]=await deadline(linked,5000,'No worker WebSocket');peers.push(ws);
  let ready=false;
  const accepted=new Promise((resolve,reject)=>{
    ws.on('message',(data,binary)=>{
      if(binary)return;
      const value=JSON.parse(data.toString());
      if(value.type==='hello')ws.send(JSON.stringify({type:'welcome',protocol:value.protocol,hostId}));
      if(value.type==='ready'){ready=true;resolve();}
    });
    ws.once('error',reject);
  });
  return {child,ws,accepted,exit,isReady:()=>ready,log:()=>log};
}
try{
  await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
  const first=await launch();await deadline(first.accepted,5000,'First worker did not acquire ownership');
  first.ws.send(JSON.stringify({type:'spawn',channel:1,provider:'demo',cwd:work}));
  for(let i=0;i<100&&!(await agents()).length;i++)await pause(50);
  assert.equal((await agents()).length,1,'First worker did not start its adapter');
  const replacement=await launch();await pause(500);
  assert.equal(replacement.isReady(),false,'Replacement became usable while old adapter was alive');
  // Neither peer receives any more server frames. The first must detect silence,
  // stop its process group, and release ownership before the second reports ready.
  await deadline(replacement.accepted,55000,'Replacement never acquired ownership after heartbeat expiry');
  assert.deepEqual(await agents(),[],'Old adapter survived exclusive ownership transfer');
  const [code]=await deadline(first.exit,5000,'Silent worker stayed alive');
  assert.equal(code,1);assert.match(first.log(),/server stopped responding/);
  replacement.ws.close();await deadline(replacement.exit,5000,'Replacement did not close');
  console.log('PASS remote partition: worker heartbeat expiry, adapter teardown, exclusive replacement ownership and clean exit');
}finally{
  for(const ws of peers)ws.terminate();for(const socket of sockets)socket.destroy();
  for(const child of children)await stopChild(child,{timeout:5000});
  await new Promise(resolve=>wss.close(resolve));await new Promise(resolve=>server.close(resolve));
  await removeFixture(dir);
}
