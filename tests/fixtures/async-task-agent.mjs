// Deterministic asyncTasks extension fixture; no provider CLI or inference.
import readline from 'node:readline';
import {appendFileSync} from 'node:fs';
const write=m=>process.stdout.write(JSON.stringify({jsonrpc:'2.0',...m})+'\n');
const reply=(id,result)=>write({id,result});
const update=(sessionId,update)=>write({method:'session/update',params:{sessionId,update}});
let root,promptId=null;
const owners=new Map();
function state(task,state){update(owners.get(task)||root,{sessionUpdate:'async_task_state_update',asyncTaskId:task,state,summary:state==='failed'?'Build failed':state==='completed'?'Checks passed':undefined});}
function job(owner,id,name){
  owners.set(id,owner);
  update(owner,{sessionUpdate:'tool_call',toolCallId:'launch-'+id,title:name,kind:'execute',status:'in_progress',rawInput:{command:'fixture build',run_in_background:true}});
  update(owner,{sessionUpdate:'tool_call_update',toolCallId:'launch-'+id,status:'completed',_meta:{jetbrains:{air:{version:1,asyncTasks:{backgrounded:true}}}}});
  update(owner,{sessionUpdate:'async_task_spawned',asyncTaskId:id,name,taskType:'shell',canStop:true,showInTranscript:false,toolCallId:'launch-'+id,outputFilePath:'/tmp/fixture-job.output'});
  update(owner,{sessionUpdate:'async_task_progress',asyncTaskId:id,summary:'Compiling fixture',lastToolName:'Bash',usage:{totalTokens:5}});
}
readline.createInterface({input:process.stdin}).on('line',line=>{
  const m=JSON.parse(line),{id,method,params={}}=m;
  if(process.env.AGENTWORKS_ASYNC_LOG)appendFileSync(process.env.AGENTWORKS_ASYNC_LOG,JSON.stringify(m)+'\n');
  if(method==='initialize')return reply(id,{protocolVersion:1,agentCapabilities:{loadSession:true,sessionCapabilities:{resume:{}}},authMethods:[],_meta:{jetbrains:{air:{version:1,capabilities:['asyncTasks']}}}});
  if(method==='session/new'){root='root-'+process.pid;return reply(id,{sessionId:root});}
  if(method==='session/resume'||method==='session/load'){root=params.sessionId;return reply(id,{});}
  if(method==='session/prompt'){
    promptId=id;
    const text=params.prompt.map(p=>p.text||'').join('\n');
    if(text.endsWith('complete')){for(const task of ['root-job','child-job'])state(task,'completed');reply(id,{stopReason:'end_turn'});promptId=null;return;}
    job(root,'root-job','Root build');
    if(text.endsWith('busy'))return;
    if(text.endsWith('late')){reply(id,{stopReason:'end_turn'});promptId=null;setTimeout(()=>state('root-job','completed'),300);return;}
    if(text.endsWith('trim')){
      for(let i=0;i<3100;i++)update(root,{sessionUpdate:'agent_thought_chunk',messageId:'trim',content:{type:'text',text:'.'}});
      reply(id,{stopReason:'end_turn'});promptId=null;return;
    }
    update(root,{sessionUpdate:'subagent_spawned',subagentSessionId:'child',name:'Worker',task:'Build the child',capabilities:{}});
    job('child','child-job','Child build');job(root,'fail-job','Failed build');state('fail-job','failed');
    // A duplicate spawn must not resurrect a finished job.
    update(root,{sessionUpdate:'async_task_spawned',asyncTaskId:'fail-job',name:'Failed build',canStop:true});
    // Foreign sessions must never inject job state.
    update('foreign',{sessionUpdate:'async_task_spawned',asyncTaskId:'foreign',name:'Foreign job',canStop:true});
    job(root,'deny-job','Protected build');
    reply(id,{stopReason:'end_turn'});promptId=null;return;
  }
  if(method==='_session/async_task/stop'){
    if(params.asyncTaskId==='deny-job')return write({id,error:{code:-32603,message:'Fixture stop rejected'}});
    if(!owners.has(params.asyncTaskId))return reply(id,{stopped:false});
    state(params.asyncTaskId,'stopped');return reply(id,{stopped:true});
  }
  if(method==='session/cancel'){if(promptId!==null)reply(promptId,{stopReason:'cancelled'});promptId=null;return;}
  if(id!==undefined)reply(id,{});
});
