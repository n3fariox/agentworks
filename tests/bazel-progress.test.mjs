import assert from 'node:assert/strict';
import {test} from 'node:test';
import {applyToolUpdate,toolActivityView as view,toolRawText as raw,events,build,delta,snapshot} from './fixtures/bazel-helpers.mjs';

test('fixture: output before start, ANSI, CR, changing totals, snapshots and authoritative failure',()=>{
  const tool={};let before;
  for(const event of events){
    if(event.kind!=='acp')continue;
    applyToolUpdate(tool,event.data,event.seq);
    if(event.seq===4){assert.match(view(tool).text,/Bazel · Testing · 2 \/ 6 actions reported · 1 \/ 3 tests reported/);before=raw(tool);}
    if(event.seq===5){assert.match(view(tool).text,/3 \/ 8 actions reported · 2 \/ 5 tests reported/);assert.ok(!raw(tool).includes('Compiling fixture.cc'));}
  }
  assert.ok(before.includes('\x1b[32mLoading:'));
  assert.ok(before.includes('\r\x1b[KAnalyzing:'));
  assert.equal(tool.status,'failed');assert.equal(view(tool).running,false);
  assert.match(view(tool).text,/4 \/ 9 actions reported · 3 \/ 5 tests reported · Exit 1$/);
  assert.ok(raw(tool).includes('Build completed successfully'));
  assert.ok(raw(tool).includes('failure after progress'));
  assert.ok(!view(tool).text.includes('%'));
});
test('all chunk boundaries, including split ANSI/OSC, preserve the same progress and exact raw bytes',()=>{
  const text='\x1b]0;[999 / 999] fake\x1b\\\x1b[32mLoading: 1 packages loaded\x1b[0m\r\x1b[K[1,234 / 2,000] 2 / 8 tests; test\n';
  for(let at=0;at<=text.length;at++){
    const tool=build();applyToolUpdate(tool,delta(text.slice(0,at)),1);applyToolUpdate(tool,delta(text.slice(at)),2);
    assert.match(view(tool).text,/Bazel · Testing · 1,234 \/ 2,000 actions reported · 2 \/ 8 tests reported/);
    assert.equal(tool.activityOutput.streams.get('terminal:term-build').raw,text);
  }
  const tool=build();[...text].forEach((c,i)=>applyToolUpdate(tool,delta(c),i));
  assert.match(view(tool).text,/1,234 \/ 2,000 actions reported/);
});
test('partial counts do not invent a denominator; later digits and changing totals are accepted',()=>{
  const tool=build();applyToolUpdate(tool,delta('[1 / 2'),1);assert.equal(view(tool).progress,false);
  applyToolUpdate(tool,delta('0] Compiling\r[2 / 30] Compiling\r'),2);assert.match(view(tool).text,/2 \/ 30 actions reported/);
  applyToolUpdate(tool,delta('[1 / 3] restarted\r'),3);assert.match(view(tool).text,/1 \/ 3 actions reported/);
});
test('only deltas append; snapshots replace progress and an empty snapshot clears it',()=>{
  const tool=build();applyToolUpdate(tool,delta('[1 / 3] compile\r'),1);
  applyToolUpdate(tool,snapshot('Analyzing: fixture\r'),2);
  assert.equal(view(tool).text,'Bazel · Analyzing');assert.ok(!raw(tool).includes('compile'));
  applyToolUpdate(tool,snapshot(''),3);assert.equal(view(tool).text,'In progress');
});
test('event sequence deduplicates, whereas identical new deltas remain raw output',()=>{
  const tool=build(),update=delta('[1 / 3] compile\n');
  applyToolUpdate(tool,update,4);assert.equal(applyToolUpdate(tool,update,4),false);
  assert.equal(applyToolUpdate(tool,{status:'failed',...delta('bad')},3),false);
  applyToolUpdate(tool,update,5);
  assert.equal(tool.activityOutput.streams.get('terminal:term-build').raw,'[1 / 3] compile\n'.repeat(2));
  assert.equal(tool.status,'in_progress');
});
test('content and rawOutput are snapshots, support mixed content, and never parse input',()=>{
  const tool=build();tool.rawInput.command+=' # [5 / 5]';assert.equal(view(tool).progress,false);
  const content=[{type:'content',content:{type:'text',text:'[2 / 7] compile\r'}},{type:'content',content:{type:'image',data:'fixture'}},{type:'unknown',fixture:true}];
  applyToolUpdate(tool,{content},1);assert.match(view(tool).text,/2 \/ 7 actions reported/);
  assert.ok(raw(tool).includes('"fixture": true'));assert.ok(raw(tool).includes('"type": "image"'));
  applyToolUpdate(tool,{content:[{type:'content',content:{type:'text',text:'Loading: new snapshot'}}]},2);
  assert.equal(view(tool).text,'Bazel · Loading');assert.ok(!raw(tool).includes('compile'));
  applyToolUpdate(tool,{content:null,rawOutput:{stderr:'[3 / 8] compile\n',exit_code:1}},3);
  assert.match(view(tool).text,/3 \/ 8 actions reported/);assert.equal(tool.status,'in_progress');
});
test('ordinary activity with elapsed time for no stream, unusable output or unknown command',()=>{
  const tool=build();tool.observedStart=1000;
  assert.equal(view(tool,62000).text,'In progress · 1m 1s');
  applyToolUpdate(tool,delta('quiet fixture, no counts'),1);assert.equal(view(tool,62000).progress,false);
  tool.rawInput.command='make all';applyToolUpdate(tool,snapshot('[1 / 2] arbitrary text'),2);
  assert.equal(view(tool,62000).progress,false);
  assert.equal(view(tool,62000,false).text,'No final status · 1m 1s');
});
test('zero, invalid and unsafe counts are not converted into estimates or success',()=>{
  for(const text of ['[0 / 0] prep','[4 / 2] invalid','[9007199254740992 / 9007199254740993] too large','INFO: Build completed successfully']){
    const tool=build();applyToolUpdate(tool,delta(text),1);
    assert.equal(tool.status,'in_progress');assert.equal(view(tool).running,true);
    assert.ok(!view(tool).text.includes('%'));
    if(text.startsWith('[0'))assert.match(view(tool).text,/0 \/ 0 actions reported/);else assert.equal(view(tool).progress,false);
  }
});
test('each tool and terminal has separate state; replay produces identical derived progress',()=>{
  const a=build(),b=build();applyToolUpdate(a,delta('[1 / 5] a'),1);applyToolUpdate(b,delta('[3 / 8] b'),1);
  assert.match(view(a).text,/1 \/ 5/);assert.match(view(b).text,/3 \/ 8/);
  applyToolUpdate(a,snapshot('Loading: other','other'),2);assert.equal(view(a).text,'Bazel · Loading');
  const replay=()=>{const t={};for(const e of events)if(e.kind==='acp')applyToolUpdate(t,e.data,e.seq);return t;};
  assert.deepEqual(view(replay()),view(replay()));assert.equal(raw(replay()),raw(replay()));
});
test('exit/signal or failed result stops live indicator; progress never changes tool status',()=>{
  for(const status of ['failed','completed']){
    const tool=build();applyToolUpdate(tool,{status,...delta('[1 / 1] done\nINFO: Build completed successfully')},1);
    assert.equal(view(tool).running,false);assert.equal(tool.status,status);
  }
  const tool=build();applyToolUpdate(tool,{_meta:{terminal_exit:{terminal_id:'term-build',exit_code:null,signal:'SIGTERM'}}},1);
  assert.equal(view(tool).running,false);assert.match(view(tool).text,/Signal SIGTERM/);assert.equal(tool.status,'in_progress');
});
test('oversized lines bound parser state and recover at the next line; raw output remains exact',()=>{
  const tool=build(),text='x'.repeat(20000)+'[9 / 9]\n[2 / 4] compile';
  applyToolUpdate(tool,delta(text),1);
  assert.match(view(tool).text,/2 \/ 4 actions reported/);
  assert.equal(tool.activityOutput.streams.get('terminal:term-build').raw,text);
  assert.ok(tool.activityOutput.streams.get('terminal:term-build').line.length<=16384);
});
test('exit and turn-end observations freeze elapsed time without inventing a final status',()=>{
  const tool=build();tool.observedStart=1000;
  applyToolUpdate(tool,{_meta:{terminal_exit:{terminal_id:'term-build',exit_code:1,signal:null}}},1,4000);
  assert.equal(view(tool,9000).text,'3s · Exit 1');
  const unresolved=build();unresolved.observedStart=1000;applyToolUpdate(unresolved,delta('[1 / 3] fixture'),1);
  unresolved.activityOutput.turnEnded=true;unresolved.activityOutput.turnEndedAt=4000;
  assert.equal(view(unresolved,9000,true).running,false);
  assert.match(view(unresolved,9000,true).text,/No final status · 3s$/);
  assert.equal(unresolved.status,'in_progress');
});
