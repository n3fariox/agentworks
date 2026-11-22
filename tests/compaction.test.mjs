import assert from 'node:assert/strict';
import {test} from 'node:test';
import {applyCompaction,compactionLabel} from '../web/compaction.js';
import {buildWorkLogs} from '../web/work-logs.js';

const item=()=>({id:'compaction-c1',type:'compaction',meta:{}});

test('Claude compaction: progress, then boundary metadata, then summary',()=>{
  const c=item();
  applyCompaction(c,{sessionUpdate:'compaction_update',compactionId:'c1',status:'in_progress'});
  assert.equal(compactionLabel(c),'Compacting context…');
  applyCompaction(c,{sessionUpdate:'compaction_update',compactionId:'c1',status:'completed',_meta:{'_claude/compaction':{trigger:'auto',preTokens:243295,postTokens:9575}}});
  assert.equal(compactionLabel(c),'Context compacted · automatic · 243k → 9.6k tokens');
  applyCompaction(c,{sessionUpdate:'compaction_update',compactionId:'c1',status:'completed',summary:[{type:'text',text:'1. Primary Request'}]});
  assert.equal(c.summary,'1. Primary Request');
  assert.equal(c.meta['_claude/compaction'].trigger,'auto','an update without _meta keeps it');
});
test('Goose compaction metadata labels the same way',()=>{
  const c=item();
  applyCompaction(c,{sessionUpdate:'compaction_update',compactionId:'c1',status:'completed',_meta:{'_goose/compaction':{trigger:'manual',preTokens:140,postTokens:206}}});
  assert.equal(compactionLabel(c),'Context compacted · manual · 140 → 206 tokens');
});
test('status-only compactions, failures and summary chunks',()=>{
  const c=item();
  applyCompaction(c,{sessionUpdate:'compaction_update',compactionId:'c1',status:'completed'});
  assert.equal(compactionLabel(c),'Context compacted');
  applyCompaction(c,{sessionUpdate:'compaction_update',compactionId:'c1',status:'failed',error:'Not enough messages'});
  assert.equal(compactionLabel(c),'Context compaction failed: Not enough messages');
  const chunked=item();
  applyCompaction(chunked,{sessionUpdate:'compaction_summary_chunk',compactionId:'c1',content:{type:'text',text:'a'}});
  applyCompaction(chunked,{sessionUpdate:'compaction_summary_chunk',compactionId:'c1',content:{type:'text',text:'b'}});
  assert.equal(chunked.summary,'ab');assert.equal(chunked.status,'in_progress');
  applyCompaction(chunked,{sessionUpdate:'compaction_update',compactionId:'c1',status:'completed',summary:null});
  assert.equal(chunked.summary,null);
});
test('a compaction is never folded into a work log',()=>{
  const tool=id=>({id,type:'tool',status:'completed'});
  const logs=buildWorkLogs([{id:'u',type:'message',role:'user'},tool('t1'),{...item(),status:'completed'},tool('t2')],{sessionIdle:true});
  assert.equal(logs.length,2);
  assert.ok(logs.every(l=>!l.members.includes('compaction-c1')));
});
