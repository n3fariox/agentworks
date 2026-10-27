import {test} from 'node:test';
import assert from 'node:assert/strict';
import {hostUsageLines, startHostUsage} from '../host-usage.js';

test('host usage labels freshness, zero swap, scope, warmup and failures', () => {
  const sample = {sampledAt:1000,cpu:{reason:'Warming up'},memory:{used:1024,total:2048},swap:{used:0,total:0},disk:{reason:'Denied'}};
  assert.match(hostUsageLines(sample, 17000).join('\n'), /stale/);
  assert.match(hostUsageLines(sample, 2000).join('\n'), /fresh/);
  assert.match(hostUsageLines(sample, 0).join('\n'), /source clock ahead/);
  assert.match(hostUsageLines(sample).join('\n'), /CPU unavailable: Warming up/);
  assert.match(hostUsageLines(sample).join('\n'), /no swap configured/);
  assert.match(hostUsageLines(sample).join('\n'), /Disk \/ unavailable: Denied/);
  sample.cpu = {percent:12.345}; sample.disk = {path:'/',used:1,total:2};
  assert.match(hostUsageLines(sample).join('\n'), /12.3%/);
  assert.match(hostUsageLines(sample).join('\n'), /root filesystem only/);
  assert.deepEqual(hostUsageLines({reason:'Linux only'}), ['Linux only']);
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
