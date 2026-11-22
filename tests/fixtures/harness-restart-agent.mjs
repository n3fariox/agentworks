#!/usr/bin/env node
import readline from 'node:readline';
import {randomUUID} from 'node:crypto';
import {readFileSync,writeFileSync} from 'node:fs';
import {join} from 'node:path';

const dir=process.env.AGENTWORKS_PERMISSION_FIXTURE;
const inherited=['AGENTWORKS_SESSION_ID','AGENTWORKS_SESSION_URI','AGENTWORKS_DB_PATH','AGENTWORKS_PORT','AGENTWORKS_SUPERVISOR_TOKEN','AGENTWORKS_OPENCODE_PATH','AGENTWORKS_MCP_TOKEN','AGENTWORKS_MCP_TOKEN_FILE','AGENTWORKS_MCP_ENDPOINT','AGENTWORKS_MCP_ENDPOINT_FILE','NOTIFY_SOCKET'];
const environment=Object.fromEntries(inherited.map(name=>[name,process.env[name]??null]));
const login=process.env.AGENTWORKS_LOGIN_FIXTURE_VALUE??null;
const file=id=>join(dir,id+'.json');
const options=[{id:'model',category:'model',name:'Model',type:'select',currentValue:'opencode/big-pickle',options:[{value:'opencode/big-pickle',name:'Big Pickle'}]}];
const save=(id,state)=>writeFileSync(file(id),JSON.stringify(state));
readline.createInterface({input:process.stdin}).on('line',line=>{
  const {id,method,params={}}=JSON.parse(line);
  if(id===undefined||!method)return;
  const reply=result=>process.stdout.write(JSON.stringify({jsonrpc:'2.0',id,result})+'\n');
  if(method==='initialize')return reply({protocolVersion:1,agentInfo:{name:'harness-restart-fixture',version:'1'},agentCapabilities:{loadSession:true,sessionCapabilities:{resume:{}}},authMethods:[]});
  if(method==='session/new'){
    const sessionId=randomUUID();save(sessionId,{logins:[login],environment});
    return reply({sessionId,configOptions:options});
  }
  if(method==='session/load'||method==='session/resume'){
    const state=JSON.parse(readFileSync(file(params.sessionId),'utf8'));
    state.logins.push(login);state.environment=environment;save(params.sessionId,state);
    return reply({configOptions:options});
  }
  if(method==='session/set_config_option')return reply({configOptions:options});
  reply({});
});
