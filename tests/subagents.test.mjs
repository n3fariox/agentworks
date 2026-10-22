import {test} from 'node:test';
import assert from 'node:assert/strict';
import {createSubagents,activityLabel} from '../web/subagents.js';
import {applyUsageEvent,emptyUsage} from '../web/usage-status.js';

test('patches preserve omitted fields and clear nulls; tools and streams are scoped',()=>{
  const transcript=[];let seq=0,id=0;
  const view=createSubagents({append:c=>transcript.push(c),uuid:()=>String(++id),applyToolUpdate:(tool,d)=>Object.assign(tool,d)});
  const emit=(source,update,parent)=>view.consume({kind:'acp',seq:++seq,data:{...update,acpSessionId:source,...(parent?{parentSessionId:parent}:{})}});
  emit('root',{sessionUpdate:'subagent_update',sessionId:'a',title:'Research',description:'Inspect',state:{state:'running'}});
  emit('root',{sessionUpdate:'subagent_update',sessionId:'b',title:'Review'});
  for(const child of ['a','b']){
    emit(child,{sessionUpdate:'agent_message_chunk',messageId:'m',content:{type:'text',text:child}},'root');
    emit(child,{sessionUpdate:'tool_call',toolCallId:'same',title:child},'root');
  }
  assert.equal(transcript.length,2);assert.equal(transcript[0].items[0].text,'a');assert.equal(transcript[1].items[0].text,'b');
  assert.notEqual(transcript[0].tools.get('same'),transcript[1].tools.get('same'));
  emit('root',{sessionUpdate:'subagent_update',sessionId:'a',state:{state:'idle',stopReason:'end_turn'}});
  assert.equal(transcript[0].title,'Research');assert.equal(transcript[0].description,'Inspect');
  emit('root',{sessionUpdate:'subagent_update',sessionId:'a',title:null,description:null,state:null});
  assert.equal(transcript[0].title,null);assert.equal(transcript[0].state,null);
  assert.equal(activityLabel(null,false),'Activity unconfirmed');
  assert.equal(activityLabel({state:'running'},false),'Activity unconfirmed');
  assert.equal(activityLabel('completed',false),'Completed');
  assert.equal(activityLabel({state:'idle'},true),'Idle');
  view.reset();assert.equal(view.name('a'),undefined);
});

test('child usage does not replace root context or quota',()=>{
  const root=applyUsageEvent(emptyUsage(),{kind:'acp',data:{sessionUpdate:'usage_update',used:10,size:1000}});
  const child=applyUsageEvent(root,{kind:'acp',data:{sessionUpdate:'usage_update',parentSessionId:'root',used:999,size:1000,_meta:{'_claude/rateLimit':{status:'rejected'}}}});
  assert.equal(child.context.used,10);assert.equal(child.events,1);assert.equal(child.ignored,1);assert.deepEqual(child.quota,{});
});


test('inter-session instructions upsert, stream, and clear without duplicating messages',()=>{
  const items=[];let id=0,seq=0;
  const view=createSubagents({append:item=>items.push(item),uuid:()=>String(++id)});
  const emit=d=>view.consume({kind:'acp',seq:++seq,data:d});
  emit({sessionUpdate:'subagent_update',acpSessionId:'root',sessionId:'a'});
  const base={acpSessionId:'a',parentSessionId:'root',messageId:'m'};
  emit({...base,sessionUpdate:'session_message',recipientSessionId:'a',content:[{type:'text',text:'Inspect'}]});
  emit({...base,sessionUpdate:'session_message_chunk',content:{type:'text',text:' the code'}});
  assert.equal(items[0].items[0].text,'Inspect the code');
  emit({...base,sessionUpdate:'session_message'});
  assert.equal(items[0].items[0].text,'Inspect the code');
  emit({...base,sessionUpdate:'session_message',content:null});
  assert.equal(items[0].items.length,1);assert.equal(items[0].items[0].text,'');
  const e={kind:'acp',seq:++seq,data:{...base,sessionUpdate:'session_message_chunk',content:{type:'text',text:'same'}}};
  view.consume(e);view.consume(e);
  assert.equal(items[0].items[0].text,'same');
  assert.equal(items[0].items[0].role,'Instruction');
});
