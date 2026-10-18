// A deliberately local demo, speaking the same ACP v1 stdio protocol as the real adapters.
// It never calls a model or edits project files. Harness state is saved for resume tests. Protocol output goes only to stdout.
import readline from 'node:readline';
import {Client as McpClient} from '@modelcontextprotocol/sdk/client/index.js';
import {StdioClientTransport} from '@modelcontextprotocol/sdk/client/stdio.js';
import { randomUUID } from 'node:crypto';
import {mkdirSync,writeFileSync,renameSync,readFileSync,readdirSync} from 'node:fs';
import {resolve,join} from 'node:path';
const stateDir=process.env.AGENTWORKS_DEMO_STATE_DIR||resolve('.agentworks/demo');
mkdirSync(stateDir,{recursive:true});
function save(id){const file=join(stateDir,id+'.json');writeFileSync(file+'.tmp',JSON.stringify(sessions.get(id)),{mode:0o600});renameSync(file+'.tmp',file);}
function commands(id){update(id,{sessionUpdate:'available_commands_update',availableCommands:[{name:'help',description:'Show demo help'},{name:'$demo-skill',description:'Example skill entry; no model is called'}]});}
const sessions = new Map(), approvals = new Map(), mcpConfigs = new Map();
const modelOptions=[{value:'demo/steady',name:'Steady (local demo)'},{value:'demo/quick',name:'Quick (local demo)'}];
const modelConfig=(model,mode='default')=>[{id:'model',name:'Model',category:'model',type:'select',currentValue:model,options:modelOptions},...(process.env.AGENTWORKS_DEMO_MODES==='1'?[{id:'mode',name:'Mode',category:'mode',type:'select',currentValue:mode,options:[{value:'default',name:'Manual'},{value:'auto',name:'Auto'}]}]:[])];
const send = value => process.stdout.write(JSON.stringify({jsonrpc:'2.0',...value})+'\n');
const reply = (id,result) => send({id,result});
const update = (sessionId,update) => {
  const session=sessions.get(sessionId);
  if(session&&!['available_commands_update','config_option_update'].includes(update.sessionUpdate))(session.history??=[]).push(update);
  send({method:'session/update',params:{sessionId,update}});
};
const delay = ms => new Promise(r=>setTimeout(r,ms));
async function prompt(id, params) {
  const session = sessions.get(params.sessionId);
  if(!session) return send({id,error:{code:-32602,message:'Unknown session'}});
  session.cancelled=false;
  const turn=++session.turn;save(params.sessionId);
  const text=params.prompt.filter(p=>p.type==='text').map(p=>p.text).join('\n');
  (session.history??=[]).push({sessionUpdate:'user_message_chunk',content:{type:'text',text}});session.title??=text.slice(0,80);session.updatedAt=new Date().toISOString();save(params.sessionId);
  const chunk=text=>update(params.sessionId,{sessionUpdate:'agent_message_chunk',content:{type:'text',text}});
  chunk(`### Local session, turn ${turn}\n\n`);
  if(text==='quota native-error'){send({id,error:{code:-32603,message:'Trusted native quota',data:{codexErrorInfo:'usageLimitExceeded'}}});return;}
  if(text==='quota active-approval'){
    update(params.sessionId,{sessionUpdate:'usage_update',used:100,size:1000,_meta:{'_claude/rateLimit':{status:'rejected',resetsAt:Math.ceil(Date.now()/1000)+2}}});
    const requestId=randomUUID();const outcome=new Promise(resolve=>approvals.set(requestId,{resolve,sessionId:params.sessionId}));
    send({id:requestId,method:'session/request_permission',params:{sessionId:params.sessionId,toolCall:{toolCallId:randomUUID(),title:'Quota approval boundary',kind:'other',status:'pending'},options:[{optionId:'allow',name:'Allow once',kind:'allow_once'}]}});
    await outcome;reply(id,{stopReason:session.cancelled?'cancelled':'end_turn'});return;
  }
  if(text==='quota repeat')session.repeatQuota=2;
  if(/^quota (known|unknown|multi|repeat)/.test(text)||(text.startsWith('[Agentworks usage-limit continuation;')&&session.repeatQuota>0)) {
    if(session.repeatQuota>0)session.repeatQuota--;
    const reset=Math.ceil(Date.now()/1000)+4;
    update(params.sessionId,{sessionUpdate:'usage_update',used:100,size:1000,_meta:{'_claude/rateLimit':{status:'rejected',...(text.includes('unknown')?{}:{resetsAt:reset}),...(text.includes('multi')?{overageStatus:'rejected',overageResetsAt:reset+3}:{})}}});
    save(params.sessionId);if(text.includes('recovered'))reply(id,{stopReason:'end_turn'});else send({id,error:{code:-32603,message:'Demo structured quota rejection',data:{codexErrorInfo:'usageLimitExceeded'}}});return;
  }
  if(text==='quota generic') {send({id,error:{code:-32603,message:'rate limit quota resetsAt 9999999999 (injected prose)'}});return;}
  if(text.startsWith('mcp ')) {
    const toolCallId=randomUUID();let result;
    const spec=mcpConfigs.get(params.sessionId)?.find(s=>s.name==='agentworks');
    const match=/^mcp (\S+)(?: (.*))?$/s.exec(text);
    update(params.sessionId,{sessionUpdate:'tool_call',toolCallId,title:'Agentworks MCP',kind:'other',status:'in_progress'});
    let client,transport;
    try{
      if(!spec)throw new Error('No Agentworks MCP configuration');
      transport=new StdioClientTransport({command:spec.command,args:spec.args,env:Object.fromEntries(spec.env.map(e=>[e.name,e.value])),stderr:'pipe'});
      client=new McpClient({name:'agentworks-demo',version:'1.0.0'});
      await client.connect(transport);
      result=match[1]==='tools/list'?await client.listTools():await client.callTool({name:match[1],arguments:JSON.parse(match[2]||'{}')});
    }catch(e){result={isError:true,message:e.message};}
    finally{await client?.close().catch(()=>{});}
    update(params.sessionId,{sessionUpdate:'tool_call_update',toolCallId,status:result.isError?'failed':'completed',rawOutput:result,content:[{type:'content',content:{type:'text',text:JSON.stringify(result)}}]});
    chunk('```json\n'+JSON.stringify(result)+'\n```\n');save(params.sessionId);reply(id,{stopReason:'end_turn'});return;
  }
  if(/approval/i.test(text)) {
    const toolCallId=randomUUID();
    update(params.sessionId,{sessionUpdate:'tool_call',toolCallId,title:'Show a demo tool result',kind:'other',status:'pending',rawInput:{demo:true}});
    const requestId=randomUUID();
    const outcome=new Promise(resolve=>approvals.set(requestId,{resolve,sessionId:params.sessionId}));
    send({id:requestId,method:'session/request_permission',params:{sessionId:params.sessionId,
      toolCall:{toolCallId,title:'Show a demo tool result',kind:'other',status:'pending'},
      options:[{optionId:'allow',name:'Allow once',kind:'allow_once'},{optionId:'deny',name:'Deny',kind:'reject_once'}]}});
    const decision=await outcome;
    if(!session.cancelled) {
      const allowed=decision?.outcome==='selected'&&decision.optionId==='allow';
      update(params.sessionId,{sessionUpdate:'tool_call_update',toolCallId,status:allowed?'completed':'failed',content:[{type:'content',content:{type:'text',text:allowed?'Demo output approved. No command was executed.':'Demo operation denied.'}}]});
      chunk(allowed?'You **approved** the demo operation.\n\n':'You **denied** the demo operation.\n\n');
    }
  }
  const answer='This is the **demo agent**, connected through the Rust ACP client. No model API was called.\n\n'+
    'Your project is `'+session.cwd+'`.\n\n'+
    'Try another message to check session continuity, **approval** to test a permission request, or **slow** to test cancellation and browser reconnects.\n\n'+
    'Open [README.md](README.md) to preview a local Markdown file.\n\n'+
    '> '+text.replaceAll('\n','\n> ')+'\n';
  for(const word of answer.match(/\S+\s*/g)||[]) {
    if(session.cancelled) break;
    chunk(word);
    await delay(/slow/i.test(text)?140:12);
  }
  if(!session.cancelled && /image/i.test(text)) {
    update(params.sessionId,{sessionUpdate:'agent_message_chunk',content:{type:'image',mimeType:'image/png',data:'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII='}});
  }
  save(params.sessionId);reply(id,{stopReason:session.cancelled?'cancelled':'end_turn'});
}
readline.createInterface({input:process.stdin}).on('line',line=>{
  let message;try{message=JSON.parse(line);}catch{return;}
  if(!message.method){
    const waiting=approvals.get(message.id);
    if(waiting){approvals.delete(message.id);waiting.resolve(message.result?.outcome);}
    return;
  }
  const {id,method,params={}}=message;
  switch(method){
    case 'initialize': reply(id,{protocolVersion:1,agentInfo:{name:'agentworks-demo',version:'0.1.0'},agentCapabilities:{loadSession:true,sessionCapabilities:{resume:{},list:{}},promptCapabilities:{image:true}},authMethods:[]});break;
    case 'session/new':{
      const sessionId=randomUUID();mcpConfigs.set(sessionId,params.mcpServers||[]);sessions.set(sessionId,{cwd:params.cwd,turn:0,cancelled:false,model:'demo/steady'});
      save(sessionId);reply(id,{sessionId,configOptions:modelConfig('demo/steady')});
      commands(sessionId);break;
    }
    case 'session/list':{
      try{
        const all=readdirSync(stateDir).filter(file=>/^[a-f0-9-]{36}\.json$/.test(file)).flatMap(file=>{try{const s=JSON.parse(readFileSync(join(stateDir,file),'utf8'));return [{sessionId:file.slice(0,-5),cwd:s.cwd,title:s.title||'Demo conversation',updatedAt:s.updatedAt||'2026-01-01T00:00:00Z'}];}catch{return [];}}).filter(s=>!params.cwd||s.cwd===params.cwd).sort((a,b)=>b.updatedAt.localeCompare(a.updatedAt));
        const offset=params.cursor?Number(params.cursor):0;if(!Number.isInteger(offset)||offset<0)throw new Error('Invalid cursor');
        reply(id,{sessions:all.slice(offset,offset+10),...(offset+10<all.length?{nextCursor:String(offset+10)}:{})});
      }catch{send({id,error:{code:-32602,message:'Could not list demo sessions'}});}
      break;
    }
    case 'session/load':
    case 'session/resume':{
      try{
        if(!/^[a-f0-9-]{36}$/.test(params.sessionId))throw new Error('Invalid session ID');
        const session=JSON.parse(readFileSync(join(stateDir,params.sessionId+'.json'),'utf8'));
        if(session.cwd!==params.cwd)throw new Error('Directory mismatch');
        session.cancelled=false;sessions.set(params.sessionId,session);mcpConfigs.set(params.sessionId,params.mcpServers||[]);
        if(method==='session/load')for(const update of session.history||[])send({method:'session/update',params:{sessionId:params.sessionId,update}});
        if(method==='session/load'&&session.failLoad){send({id,error:{code:-32603,message:'Demo history load failed'}});break;}
        // Deterministic native-opening race fixture; never delays ordinary demo sessions.
        const ready=()=>{reply(id,{configOptions:modelConfig(session.model,session.mode)});commands(params.sessionId);};
        if(Number.isInteger(session.resumeDelayMs)&&session.resumeDelayMs>0&&session.resumeDelayMs<=2000)setTimeout(ready,session.resumeDelayMs);else ready();
      }catch{send({id,error:{code:-32002,message:'Saved demo session is unavailable'}});}
      break;
    }
    case 'session/set_config_option':{
      const session=sessions.get(params.sessionId);
      if(session&&params.configId==='mode'&&process.env.AGENTWORKS_DEMO_MODES==='1'&&['default','auto'].includes(params.value)){session.mode=params.value;save(params.sessionId);reply(id,{configOptions:modelConfig(session.model,session.mode)});break;}
      if(!session||params.configId!=='model'||!modelOptions.some(o=>o.value===params.value)) {send({id,error:{code:-32602,message:'Unknown model'}});break;}
      session.model=params.value;save(params.sessionId);reply(id,{configOptions:modelConfig(session.model,session.mode)});break;
    }
    case 'session/prompt':prompt(id,params).catch(error=>send({id,error:{code:-32603,message:error.message}}));break;
    case 'session/cancel':{
      const session=sessions.get(params.sessionId);if(session)session.cancelled=true;
      for(const [key,a] of approvals){if(a.sessionId===params.sessionId){a.resolve({outcome:'cancelled'});approvals.delete(key);}}
      break;
    }
    default: if(id!==undefined)send({id,error:{code:-32601,message:'Method not supported by demo'}});
  }
});
