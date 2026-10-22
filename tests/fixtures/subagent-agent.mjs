// Deterministic native child-session traffic, including both ACP draft versions.
import readline from 'node:readline';
import {appendFileSync} from 'node:fs';
const write=m=>process.stdout.write(JSON.stringify({jsonrpc:'2.0',...m})+'\n');
const update=(sessionId,update)=>write({method:'session/update',params:{sessionId,update}});
const reply=(id,result)=>write({id,result});
const chunk=(id,text)=>update(id,{sessionUpdate:'agent_message_chunk',messageId:'shared-message',content:{type:'text',text}});
let root,promptId,permissionId=1000;
readline.createInterface({input:process.stdin}).on('line',line=>{
  const m=JSON.parse(line),{id,method,params={}}=m;
  if(process.env.AGENTWORKS_SUBAGENT_LOG)appendFileSync(process.env.AGENTWORKS_SUBAGENT_LOG,JSON.stringify(m)+'\n');
  if(!method){
    if(id===permissionId){
      chunk('legacy','Legacy result');chunk('modern','Modern result');
      update(root,{sessionUpdate:'subagent_state_update',subagentSessionId:'legacy',state:'completed'});
      update('legacy',{sessionUpdate:'subagent_update',sessionId:'modern',state:{state:'idle',stopReason:'end_turn'}});
      chunk(root,'Root result');reply(promptId,{stopReason:'end_turn'});promptId=null;
    }
    return;
  }
  if(method==='initialize')return reply(id,{protocolVersion:1,agentCapabilities:{loadSession:true,sessionCapabilities:{resume:{},subagents:{}}},authMethods:[]});
  if(method==='session/new'){root='root-'+process.pid;return reply(id,{sessionId:root});}
  if(method==='session/resume'||method==='session/load'){root=params.sessionId;return reply(id,{});}
  if(method==='session/prompt'){
    promptId=id;
    const text=params.prompt?.[0]?.text||'';
    if(text==='fallback'){chunk(root,'Ordinary tool fallback');return reply(id,{stopReason:'end_turn'});}
    update(root,{sessionUpdate:'usage_update',used:10,size:1000});
    update(root,{sessionUpdate:'subagent_spawned',subagentSessionId:'legacy',name:'Researcher',task:'Inspect the code',capabilities:{}});
    update('legacy',{sessionUpdate:'subagent_update',sessionId:'modern',title:'Reviewer',description:'Check the findings',capabilities:{},state:{state:'running'}});
    update('modern',{sessionUpdate:'session_message',messageId:'instruction',senderSessionId:'legacy',recipientSessionId:'modern',content:[{type:'text',text:'Verify the code carefully'}]});
    chunk(root,'Root working');chunk('legacy','Legacy working');chunk('modern','Modern working');
    for(const sid of [root,'legacy','modern']){
      update(sid,{sessionUpdate:'tool_call',toolCallId:'shared-tool',title:sid===root?'Root tool':sid+' tool',kind:'read',status:'in_progress'});
      update(sid,{sessionUpdate:'tool_call_update',toolCallId:'shared-tool',status:'completed',content:[{type:'content',content:{type:'text',text:sid+' output'}}]});
    }
    update('legacy',{sessionUpdate:'usage_update',used:999,size:1000});
    update('modern',{sessionUpdate:'available_commands_update',availableCommands:[{name:'child-only',description:'Do not leak'}]});
    update('modern',{sessionUpdate:'config_option_update',configOptions:[{id:'model',name:'Model',category:'model',type:'select',currentValue:'child-model',options:[{value:'child-model',name:'Child model'}]}]});
    if(text==='slow')return;
    write({id:permissionId,method:'session/request_permission',params:{sessionId:'modern',toolCall:{toolCallId:'shared-tool',title:'Review permission'},options:[{optionId:'allow',name:'Allow once',kind:'allow_once'},{optionId:'reject',name:'Reject',kind:'reject_once'}]}});
    return;
  }
  if(method==='session/cancel'){
    update(root,{sessionUpdate:'subagent_state_update',subagentSessionId:'legacy',state:'cancelled'});
    update('legacy',{sessionUpdate:'subagent_update',sessionId:'modern',state:{state:'idle',stopReason:'cancelled'}});
    if(promptId!==null)reply(promptId,{stopReason:'cancelled'});promptId=null;return;
  }
  if(id!==undefined)reply(id,{});
});
