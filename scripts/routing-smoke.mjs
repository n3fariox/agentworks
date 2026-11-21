import {removeFixture} from '../tests/support/cleanup.mjs';
import assert from 'node:assert/strict';
import http from 'node:http';
import {spawn} from 'node:child_process';
import {mkdtemp} from 'node:fs/promises';
import {resolve} from 'node:path';
import {once} from 'node:events';

const dir=await mkdtemp(resolve('.cache/routing-'));
const port=process.env.AGENTWORKS_ROUTING_PORT||'4328',base=`http://127.0.0.1:${port}`;
let server;
const delay=ms=>new Promise(r=>setTimeout(r,ms));

async function api(path,body,method=body===undefined?'GET':'POST',code=200){
  const r=await fetch(base+path,{method,headers:{Origin:base,'Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body)});
  const text=await r.text();
  assert.equal(r.status,code,text);
  return code===200?JSON.parse(text):text;
}
async function start(){
  server=spawn(resolve('target/debug/agentworks'),[],{
    env:{...process.env,AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:port,AGENTWORKS_CATALOG_PATH:dir+'/catalog.json',AGENTWORKS_DB_PATH:dir+'/sessions.sqlite3',AGENTWORKS_DEMO_STATE_DIR:dir+'/demo',AGENTWORKS_MODEL_PROVIDERS_DIR:dir+'/model-providers'},
    stdio:['ignore','inherit','inherit'],
  });
  for(let i=0;i<80;i++){try{if((await fetch(base+'/api/status')).ok)return;}catch{}await delay(100);}
  throw new Error('Startup failed');
}
async function stop(){
  if(!server)return;
  const exit=once(server,'exit');
  server.kill('SIGINT');
  await exit;
  server=null;
}
async function idle(id){
  for(let i=0;i<200;i++){
    const s=await api(`/api/sessions/${id}`);
    if(s.session.status==='idle')return s;
    assert.notEqual(s.session.status,'error',JSON.stringify(s));
    await delay(100);
  }
  throw new Error('Idle timed out '+id);
}
async function mcpCall(id,name,args={}){
  await idle(id);
  const old=await api(`/api/sessions/${id}`);
  await api(`/api/sessions/${id}/prompt`,{text:`mcp ${name} ${JSON.stringify(args)}`,requestId:crypto.randomUUID()});
  const s=await idle(id);
  const result=s.events.filter(e=>e.seq>old.seq&&e.kind==='acp'&&e.data.sessionUpdate==='tool_call_update').at(-1)?.data.rawOutput;
  assert(result,'Missing MCP result '+name);
  return result;
}

let dmServer;
try{
  await start();
  const settings=await api('/api/settings');
  const {roots}=await api('/api/status');
  const cwd=roots[0];

  // 1. Global off overrides per-project routing settings.
  const project=await api('/api/projects',{
    name:'Routing test',
    defaultCommunicationRole:'coordinator',
    sessionRouting:{mode:'always',approach:'quota_round_robin',candidates:[{provider:'demo',model:'demo/steady',enabled:true}]},
  });
  await api('/api/sessions',{provider:'auto',cwd,projectId:project.id},'POST',400);

  // 2. Global default + quota_round_robin balances 10 sessions across candidates.
  await api('/api/settings',{
    ...settings,
    sessionRouting:{
      mode:'default',
      approach:'quota_round_robin',
      candidates:[
        {provider:'demo',model:'demo/steady',enabled:true},
        {provider:'demo',model:'demo/quick',enabled:true},
      ],
    },
  },'PUT');

  // Project currently has mode:'always' which is active now that global is not off.
  // Override explicit selection when mode is always.
  const forced=await api('/api/sessions',{provider:'demo',model:'demo/quick',cwd,projectId:project.id,title:'Always test'});
  assert(forced.routing,'Expected routing metadata in always mode');
  assert.equal(forced.routing.mode,'always');
  assert.equal(forced.model,'demo/steady');
  await idle(forced.id);

  // Set project to inherit ('null') so global 'default' mode applies.
  await api(`/api/projects/${project.id}`,{name:'Routing test',sessionRouting:null},'PATCH');

  // Explicit provider+model is respected in default mode.
  const explicit=await api('/api/sessions',{provider:'demo',model:'demo/quick',cwd,projectId:project.id,title:'Explicit test'});
  assert(!explicit.routing,'Explicit provider should not route in default mode');
  assert.equal(explicit.model,'demo/quick');
  await idle(explicit.id);

  // Omitted provider / 'auto' balances 10 sessions in a row across demo/steady and demo/quick.
  const counts={};
  const createdIds=[];
  for(let i=0;i<10;i++){
    const s=await api('/api/sessions',{...(i%2===0?{provider:'auto'}:{}),cwd,projectId:project.id,title:`Burst ${i}`});
    assert.equal(s.routing.approach,'quota_round_robin');
    assert.equal(s.routing.mode,'default');
    counts[s.model]=(counts[s.model]||0)+1;
    createdIds.push(s.id);
  }
  assert.equal(counts['demo/steady'],5,`Expected balanced demo/steady count, got ${JSON.stringify(counts)}`);
  assert.equal(counts['demo/quick'],5,`Expected balanced demo/quick count, got ${JSON.stringify(counts)}`);
  await Promise.all(createdIds.map(idle));

  // 3. Initial prompt & description on session creation.
  const withPrompt=await api('/api/sessions',{provider:'auto',cwd,projectId:project.id,title:'Initial prompt test',prompt:'Hello from create'});
  const promptSnap=await idle(withPrompt.id);
  assert(promptSnap.events.some(e=>e.kind==='user'&&e.data?.text==='Hello from create'),'Expected queued initial prompt to run');

  const withDesc=await api('/api/sessions',{provider:'auto',cwd,projectId:project.id,title:'Desc only',description:'Do not send as prompt'});
  const descSnap=await idle(withDesc.id);
  assert(!descSnap.events.some(e=>e.kind==='user'),'Description must not be sent as a prompt');

  // 4. Per-project opt-out ('off') works when global is enabled.
  const offProject=await api('/api/projects',{name:'Opt-out project',sessionRouting:{mode:'off'}});
  await api('/api/sessions',{provider:'auto',cwd,projectId:offProject.id},'POST',400);

  // 5. MCP start_session schema & omitted provider routing.
  const coord=await api('/api/sessions',{provider:'demo',cwd,projectId:project.id,title:'Coordinator'});
  await idle(coord.id);
  const tools=await mcpCall(coord.id,'tools/list');
  const startTool=tools.tools.find(t=>t.name==='start_session');
  assert(startTool,'Expected start_session tool');
  assert(!startTool.inputSchema.required.includes('provider'),'provider must be optional in start_session');
  assert(startTool.inputSchema.properties.provider.enum.includes('auto'));
  const mcpChild=JSON.parse((await mcpCall(coord.id,'start_session',{cwd,title:'MCP routed',description:'Route this via MCP',requestId:'mcp-route-1'})).content[0].text);
  assert(mcpChild.routing,'Expected MCP start_session with omitted provider to route');
  await idle(mcpChild.id);

  // 6. Decision model HTTP endpoints (/v1/decisions, /v1beta/decisions, /v1/systemone) + fallback.
  let lastPath=null;
  let lastPayload=null;
  let lastHeaders=null;
  let mode='decisions';
  dmServer=http.createServer((req,res)=>{
    let body='';
    req.on('data',c=>body+=c);
    req.on('end',()=>{
      lastPath=req.url;
      lastHeaders=req.headers;
      lastPayload=JSON.parse(body);
      if(mode==='fail'){res.writeHead(500);res.end('fail');return;}
      if(mode==='auto_systemone'&&req.url==='/v1/decisions'){res.writeHead(404);res.end('not found');return;}
      res.writeHead(200,{'Content-Type':'application/json'});
      if(req.url==='/v1/decisions'){
        res.end(JSON.stringify({
          model:lastPayload.model,
          answers:[{type:'choice',name:'candidate',selected:'demo · demo/quick',confidence:0.93}],
        }));
      }else if(req.url==='/v1beta/decisions'||req.url==='/v1/systemone'){
        res.end(JSON.stringify({
          model:lastPayload.model,
          answers:{candidate:{type:'choice',choice:'demo · demo/quick',probabilities:{'demo · demo/steady':0.08,'demo · demo/quick':0.92},confidence:0.92}},
        }));
      }else{
        res.writeHead(404);res.end('unknown route');
      }
    });
  });
  await new Promise(r=>dmServer.listen(0,'127.0.0.1',r));
  const dmPort=dmServer.address().port;

  // 6a. OpenAI /v1/decisions
  await api('/api/settings',{
    ...settings,
    sessionRouting:{
      mode:'default',
      approach:'decision_model',
      candidates:[
        {provider:'demo',model:'demo/steady',notes:'deep analysis',enabled:true},
        {provider:'demo',model:'demo/quick',notes:'fast edits',enabled:true},
      ],
      decisionModel:{protocol:'decisions',baseUrl:`http://127.0.0.1:${dmPort}`,model:'gpt-6-luna',instructions:'Pick carefully',timeoutMs:2000},
    },
  },'PUT');

  const dmSession=await api('/api/sessions',{provider:'auto',cwd,projectId:project.id,title:'Decisions API routed',description:'Quick typo fix'});
  assert.equal(lastPath,'/v1/decisions');
  assert.equal(lastPayload.model,'gpt-6-luna');
  assert.equal(lastPayload.questions[0].type,'choice');
  assert.equal(lastPayload.questions[0].choices.length,2);
  assert(lastPayload.input.includes('Quick typo fix'));
  assert.equal(dmSession.routing.approach,'decision_model');
  assert.equal(dmSession.routing.fallback,false);
  assert.equal(dmSession.model,'demo/quick');
  await idle(dmSession.id);

  // 6b. Google Gemini /v1beta/decisions (via modelProvider with API key -> x-goog-api-key header)
  const gemProfile=await api('/api/model-providers',{
    operation:'save',
    profile:{name:'Gemini Decisions',kind:'openai-compatible',baseUrl:`http://127.0.0.1:${dmPort}/v1beta`,defaultModel:'gemini-decision-1',apiKey:'gem-secret',hostIds:['local']},
  });
  await api('/api/settings',{
    ...settings,
    sessionRouting:{
      mode:'default',
      approach:'decision_model',
      candidates:[
        {provider:'demo',model:'demo/steady',notes:'deep analysis',enabled:true},
        {provider:'demo',model:'demo/quick',notes:'fast edits',enabled:true},
      ],
      decisionModel:{protocol:'gemini',modelProviderId:gemProfile.id,timeoutMs:2000},
    },
  },'PUT');
  const gemSession=await api('/api/sessions',{provider:'auto',cwd,projectId:project.id,title:'Gemini routed',description:'Gemini typo check'});
  assert.equal(lastPath,'/v1beta/decisions');
  assert.equal(lastHeaders['x-goog-api-key'],'gem-secret');
  assert.equal(lastHeaders['authorization'],undefined);
  assert.equal(lastPayload.model,'gemini-decision-1');
  assert.equal(lastPayload.questions.candidate.type,'choice');
  assert(lastPayload.questions.candidate.criteria['demo · demo/quick']);
  assert(lastPayload.input.includes('Gemini typo check'));
  assert.equal(gemSession.routing.approach,'decision_model');
  assert.equal(gemSession.model,'demo/quick');
  await idle(gemSession.id);

  // 6c. Jev / llama.cpp /v1/systemone
  mode='systemone';
  await api('/api/settings',{
    ...settings,
    sessionRouting:{
      mode:'default',
      approach:'decision_model',
      candidates:[
        {provider:'demo',model:'demo/steady',notes:'deep analysis',enabled:true},
        {provider:'demo',model:'demo/quick',notes:'fast edits',enabled:true},
      ],
      decisionModel:{protocol:'systemone',baseUrl:`http://127.0.0.1:${dmPort}`,model:'kev-4b',timeoutMs:2000},
    },
  },'PUT');
  const sysSession=await api('/api/sessions',{provider:'auto',cwd,projectId:project.id,title:'System One routed',description:'Another quick edit'});
  assert.equal(lastPath,'/v1/systemone');
  assert.equal(lastPayload.model,'kev-4b');
  assert.equal(lastPayload.questions.candidate.type,'choice');
  assert(lastPayload.questions.candidate.criteria['demo · demo/quick']);
  assert(lastPayload.state.includes('Another quick edit'));
  assert.equal(sysSession.routing.approach,'decision_model');
  assert.equal(sysSession.routing.fallback,false);
  assert.equal(sysSession.model,'demo/quick');
  await idle(sysSession.id);

  // 6d. Auto-detect falls through 404 on /v1/decisions to /v1/systemone
  mode='auto_systemone';
  await api('/api/settings',{
    ...settings,
    sessionRouting:{
      mode:'default',
      approach:'decision_model',
      candidates:[
        {provider:'demo',model:'demo/steady',enabled:true},
        {provider:'demo',model:'demo/quick',enabled:true},
      ],
      decisionModel:{protocol:'auto',baseUrl:`http://127.0.0.1:${dmPort}`,model:'custom-local-router',timeoutMs:2000},
    },
  },'PUT');
  const autoSysSession=await api('/api/sessions',{provider:'auto',cwd,projectId:project.id,title:'Auto System One'});
  assert.equal(lastPath,'/v1/systemone');
  assert.equal(autoSysSession.routing.approach,'decision_model');
  assert.equal(autoSysSession.routing.fallback,false);
  await idle(autoSysSession.id);

  // 6e. Fallback to quota round-robin when decision model returns HTTP 500.
  mode='fail';
  const fallbackSession=await api('/api/sessions',{provider:'auto',cwd,projectId:project.id,title:'Fallback routed'});
  assert.equal(fallbackSession.routing.approach,'quota_round_robin');
  assert.equal(fallbackSession.routing.fallback,true);
  await idle(fallbackSession.id);

  // 7. Automation with provider: 'auto' routes when dispatched.
  const autoDef=await api('/api/automations',{name:'Routed auto',provider:'auto',cwd,projectId:project.id,prompt:'Automated routed task',enabled:true});
  mode='auto_systemone';
  const autoRun=await api(`/api/automations/${autoDef.id}/run`,{requestId:'auto-route-1'});
  assert.equal(autoRun.session.routing.approach,'decision_model');
  await idle(autoRun.session.id);

  console.log('PASS: session router smoke tests');
}finally{
  if(dmServer)await new Promise(r=>dmServer.close(r));
  await stop();
  await removeFixture(dir);
}
