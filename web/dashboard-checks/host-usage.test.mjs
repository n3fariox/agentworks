import {test} from 'node:test';
import assert from 'node:assert/strict';
import {hostUsageModel, startHostUsage} from '../host-usage.js';

test('visual metrics preserve freshness, unknown values and real percentages', () => {
  const sample={sampledAt:1000,cpu:{reason:'Warming up'},memory:{used:85,total:100},swap:{used:0,total:0},disks:[]};
  let model=hostUsageModel(sample,2000);
  assert.equal(model.freshness,'live');
  assert.equal(model.tiles[0].percent,null);
  assert.match(model.tiles[0].detail,/Warming up/);
  assert.equal(model.tiles[1].value,'85%');
  assert.equal(model.tiles[1].tone,'high');
  assert.equal(model.tiles[2].value,'—');
  assert.equal(model.tiles[2].detail,'Not configured');
  assert.equal(hostUsageModel(sample,17000).freshness,'stale');
  assert.equal(hostUsageModel(sample,0).freshness,'ahead');
  sample.cpu={percent:12.345}; sample.memory={used:101,total:100};
  model=hostUsageModel(sample,2000);
  assert.equal(model.tiles[0].percent,12.345);
  assert.equal(model.tiles[1].percent,null);
  assert.deepEqual(hostUsageModel({reason:'Unsupported'}),{reason:'Unsupported'});
});
test('storage overview surfaces build mounts and retains every other mount in details', () => {
  const sample={sampledAt:1,cpu:{percent:1},memory:{used:1,total:2},swap:{used:0,total:0},disks:[
    {path:'/',used:85,total:100},{path:'/home',used:85,total:100},{path:'/build',used:95,total:100},
    {path:'/scratch',used:1,total:100},{path:'/container/long/path',reason:'Denied'}
  ]};
  const model=hostUsageModel(sample,2);
  assert.deepEqual(model.disks.map(d=>d.path),['/','/build','/scratch']);
  assert.equal(model.disks[1].tone,'critical');
  assert.deepEqual(model.moreDisks.map(d=>d.path),['/home','/container/long/path']);
  assert.equal(model.moreDisks[1].percent,null);
  assert.equal(model.moreDisks[1].detail,'Denied');
  assert.equal(model.disks.length+model.moreDisks.length,sample.disks.length);
});

function fixture(hidden = false) {
  const doc = new EventTarget(); doc.hidden = hidden;
  const win = new EventTarget(), sockets = [], pending = new Map(), states = [];
  let id = 0;
  const timers = {setInterval(fn){pending.set(++id,fn);return id;},setTimeout(fn){pending.set(++id,fn);return id;},clearInterval(id){pending.delete(id);},clearTimeout(id){pending.delete(id);}};
  const dispose = startHostUsage({doc,win,timers,onState:(sample,status) => states.push({sample,status}),socket(url){
    const ws = {url,readyState:1,sent:[],send(text){this.sent.push(text);},close(){this.closed=true;this.onclose?.();}};
    sockets.push(ws); return ws;
  }});
  return {doc,win,sockets,pending,states,dispose};
}
test('one stream per visible dashboard; hide/pagehide release; restored pages reconnect', () => {
  const f = fixture();
  assert.equal(f.sockets.length,1);
  const ws = f.sockets[0];
  assert.match(ws.url,/\/api\/dashboard\/host-usage\/events$/);
  ws.onopen(); assert.deepEqual(ws.sent,['alive']);
  ws.onmessage({data:JSON.stringify({sampledAt:123})});
  assert.equal(f.states.at(-1).sample.sampledAt,123);
  f.doc.hidden=true; f.doc.dispatchEvent(new Event('visibilitychange'));
  assert.equal(ws.closed,true); assert.equal(f.pending.size,0);
  ws.onmessage({data:'{"sampledAt":456}'});
  assert.equal(f.states.at(-1).sample,null);
  f.doc.hidden=false; f.doc.dispatchEvent(new Event('visibilitychange'));
  assert.equal(f.sockets.length,2);
  f.win.dispatchEvent(new Event('pagehide'));
  assert.equal(f.sockets[1].closed,true);
  f.win.dispatchEvent(new Event('pageshow'));
  assert.equal(f.sockets.length,3);
  f.dispose(); assert.equal(f.sockets[2].closed,true);
});
test('hidden initial page does not connect; errors clear samples and schedule only one reconnect', () => {
  const f=fixture(true);
  assert.equal(f.sockets.length,0);
  f.doc.hidden=false; f.doc.dispatchEvent(new Event('visibilitychange'));
  const ws=f.sockets[0]; ws.onopen(); ws.onmessage({data:'{"sampledAt":123}'}); ws.onerror();
  assert.equal(f.states.at(-1).sample,null);
  assert.match(f.states.at(-1).status,/disconnected/);
  assert.equal(f.pending.size,1);
  f.dispose(); assert.equal(f.pending.size,0);
});
