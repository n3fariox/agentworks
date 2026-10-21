import assert from 'node:assert/strict';
import {test} from 'node:test';
import {applyToolUpdate,applyToolProgress,toolActivityView as view,toolRawText as raw,events,progressEvents,build,delta,snapshot,view as serverView} from './fixtures/bazel-helpers.mjs';

// Parsing runs on the server (crates/starlark, src/tool_progress.rs) and is tested there.
// These tests cover the browser's half: raw output fidelity, server views and timing.
function replay(upto=Infinity){
  const tool={};
  for(const event of events){
    if(event.seq>upto)break;
    if(event.kind==='acp')applyToolUpdate(tool,event.data,event.seq);
    for(const p of progressEvents)if(p.seq===event.seq)applyToolProgress(tool,p.data,p.seq);
  }
  return tool;
}

test('fixture with golden server progress: phases, changing totals, snapshots and authoritative failure',()=>{
  assert.match(view(replay(4)).text,/^Bazel · Testing · 2 \/ 6 actions reported · 1 \/ 3 tests reported$/);
  const before=raw(replay(4));
  assert.ok(before.includes('\x1b[32mLoading:'));assert.ok(before.includes('\r\x1b[KAnalyzing:'));
  assert.match(view(replay(5)).text,/3 \/ 8 actions reported · 2 \/ 5 tests reported/);
  assert.ok(!raw(replay(5)).includes('Compiling fixture.cc'),'terminal_output snapshots replace raw text');
  const tool=replay();
  assert.equal(tool.status,'failed');assert.equal(view(tool).running,false);
  assert.equal(view(tool).text,'Bazel · Testing · 4 / 9 actions reported · 3 / 5 tests reported · Exit 1');
  assert.ok(raw(tool).includes('Build completed successfully'));assert.ok(raw(tool).includes('failure after progress'));
  assert.ok(!view(tool).text.includes('%'));
});
test('the browser does not parse: output without a server view shows ordinary activity',()=>{
  const tool=build();applyToolUpdate(tool,delta('[1 / 3] compile\n2 / 8 tests\n'),1);
  assert.equal(view(tool).text,'In progress');assert.equal(view(tool).progress,false);
});
test('older views never replace newer ones; equal and newer seqs apply',()=>{
  const tool=build();
  assert.ok(applyToolProgress(tool,serverView('Executing',[[2,9,'actions']]),5));
  assert.equal(applyToolProgress(tool,serverView('Loading',[]),4),false);
  assert.match(view(tool).text,/2 \/ 9 actions/);
  assert.ok(applyToolProgress(tool,serverView('Testing',[[3,9,'actions'],[1,2,'tests']]),5));
  assert.equal(view(tool).text,'Bazel · Testing · 3 / 9 actions reported · 1 / 2 tests reported');
  assert.equal(applyToolProgress(undefined,serverView('Testing',[]),6),false);
  assert.equal(applyToolProgress(tool,serverView('Testing',[]),'6'),false);
});
test('a selected parser without progress, and parser failure, leave tool status authoritative',()=>{
  const tool=build();applyToolProgress(tool,serverView(null,[]),1);
  assert.equal(view(tool).text,'In progress');
  applyToolProgress(tool,serverView(null,[],{parserFailure:'bazel_progress.star@abc Runtime: boom'}),2);
  assert.equal(view(tool).text,'In progress · Progress unavailable');
  assert.equal(view(tool).failure,'bazel_progress.star@abc Runtime: boom');
  assert.equal(view(tool).running,true);assert.equal(tool.status,'in_progress');
});
test('any parser name and unit order come from the server view',()=>{
  const tool=build();applyToolProgress(tool,{parser:'Make',progress:{phase:'Building',label:'x',counts:[{done:1234,total:2000,unit:'targets'}]},parserFailure:null},1);
  assert.equal(view(tool).text,'Make · Building · 1,234 / 2,000 targets reported');
});
test('raw output: sequence dedupe, identical new deltas append, snapshots replace',()=>{
  const tool=build(),update=delta('[1 / 3] compile\n');
  applyToolUpdate(tool,update,4);assert.equal(applyToolUpdate(tool,update,4),false);
  assert.equal(applyToolUpdate(tool,{status:'failed',...delta('bad')},3),false);
  applyToolUpdate(tool,update,5);
  assert.equal(tool.activityOutput.streams.get('terminal:term-build').raw,'[1 / 3] compile\n'.repeat(2));
  applyToolUpdate(tool,snapshot('Analyzing: fixture\r'),6);
  assert.equal(tool.activityOutput.streams.get('terminal:term-build').raw,'Analyzing: fixture\r');
  assert.equal(tool.status,'in_progress');
});
test('content and rawOutput are preserved, including mixed and unknown content',()=>{
  const tool=build();
  const content=[{type:'content',content:{type:'text',text:'[2 / 7] compile\r'}},{type:'content',content:{type:'image',data:'fixture'}},{type:'unknown',fixture:true}];
  applyToolUpdate(tool,{content},1);
  assert.ok(raw(tool).includes('"fixture": true'));assert.ok(raw(tool).includes('"type": "image"'));
  applyToolUpdate(tool,{content:null,rawOutput:{stderr:'[3 / 8] compile\n',exit_code:1}},2);
  assert.ok(raw(tool).includes('"exit_code": 1'));assert.equal(tool.status,'in_progress');
});
test('elapsed fallback, exit/signal and turn-end observations',()=>{
  const tool=build();tool.observedStart=1000;
  assert.equal(view(tool,62000).text,'In progress · 1m 1s');
  assert.equal(view(tool,62000,false).text,'No final status · 1m 1s');
  applyToolUpdate(tool,{_meta:{terminal_exit:{terminal_id:'term-build',exit_code:1,signal:null}}},1,4000);
  assert.equal(view(tool,9000).text,'3s · Exit 1');
  const killed=build();applyToolUpdate(killed,{_meta:{terminal_exit:{terminal_id:'term-build',exit_code:null,signal:'SIGTERM'}}},1);
  assert.equal(view(killed).running,false);assert.match(view(killed).text,/Signal SIGTERM/);assert.equal(killed.status,'in_progress');
  const unresolved=build();unresolved.observedStart=1000;applyToolUpdate(unresolved,delta('[1 / 3] fixture'),1);
  applyToolProgress(unresolved,serverView('Executing',[[1,3,'actions']]),1);
  unresolved.activityOutput.turnEnded=true;unresolved.activityOutput.turnEndedAt=4000;
  assert.equal(view(unresolved,9000,true).running,false);
  assert.equal(view(unresolved,9000,true).text,'Bazel · Executing · 1 / 3 actions reported · No final status · 3s');
});
test('replay produces identical views and raw text',()=>{
  assert.deepEqual(view(replay()),view(replay()));assert.equal(raw(replay()),raw(replay()));
});
