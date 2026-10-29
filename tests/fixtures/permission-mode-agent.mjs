// Offline ACP fixture: records the actual mode at prompt dispatch, and the login
// environment marker each process opened the session with, and the Agentworks
// variables it inherited. No model or tools.
import readline from 'node:readline';
import {randomUUID} from 'node:crypto';
import {readFileSync,writeFileSync} from 'node:fs';
import {join} from 'node:path';
const dir=process.env.AGENTWORKS_PERMISSION_FIXTURE;
const controls=()=>JSON.parse(readFileSync(join(dir,'controls.json'),'utf8'));
const file=id=>join(dir,id+'.json');
const login=process.env.AGENTWORKS_LOGIN_FIXTURE_VALUE??null;
const inherited=['AGENTWORKS_SESSION_ID','AGENTWORKS_SESSION_URI','AGENTWORKS_DB_PATH','AGENTWORKS_PORT','AGENTWORKS_SUPERVISOR_TOKEN','NOTIFY_SOCKET'];
const environment=Object.fromEntries(inherited.map(name=>[name,process.env[name]??null]));
const modes=['read-only','workspace-write','agent','agent-full-access'];
const config=state=>[{id:'mode',category:'mode',name:'Mode',type:'select',currentValue:state.mode,options:modes.filter(value=>!controls().unsupported||value!=='agent-full-access').map(value=>({value,name:value}))}];
readline.createInterface({input:process.stdin}).on('line',line=>{
  const {id,method,params={}}=JSON.parse(line);
  if(id===undefined||!method)return;
  const reply=result=>process.stdout.write(JSON.stringify({jsonrpc:'2.0',id,result})+'\n');
  const save=(sid,state)=>writeFileSync(file(sid),JSON.stringify(state));
  if(method==='initialize')return reply({protocolVersion:1,agentInfo:{name:'permission-fixture',version:'1'},agentCapabilities:{loadSession:true,sessionCapabilities:{resume:{}}},authMethods:[]});
  if(method==='session/new'){
    const sessionId=randomUUID(),state={mode:'agent',selections:[],prompts:[],logins:[login],environment};save(sessionId,state);
    return reply({sessionId,configOptions:config(state)});
  }
  const state=readFileSync(file(params.sessionId),'utf8');
  const session=JSON.parse(state);
  if(method==='session/load'||method==='session/resume'){
    session.logins=[...(session.logins||[]),login];session.environment=environment;save(params.sessionId,session);
    return reply({configOptions:config(session)});
  }
  if(method==='session/set_config_option'){
    session.selections.push(params.value);
    if(!controls().unconfirmed)session.mode=params.value;
    save(params.sessionId,session);
    return reply({configOptions:config(session)});
  }
  if(method==='session/prompt'){
    session.prompts.push(session.mode);save(params.sessionId,session);
    return reply({stopReason:'end_turn'});
  }
  reply({});
});
