import {test,expect} from '@playwright/test';
import {spawn} from 'node:child_process';
import {mkdtemp,mkdir,writeFile,readFile,rm} from 'node:fs/promises';
import {resolve,join} from 'node:path';
import {serverReady,stopChild} from './support/cleanup.mjs';
const base='http://127.0.0.1:4462';
test.use({viewport:{width:390,height:844}});
const binary=resolve(process.env.AGENTWORKS_TEST_BINARY||'target/debug/agentworks');
let dir,server,remote,localId;
async function api(path,body){
 const response=await fetch(base+path,{method:body===undefined?'GET':'POST',headers:{Origin:base,'Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body)});
 if(!response.ok)throw Error(await response.text());return response.json();
}
async function start(){
 server=spawn(binary,[],{env:{...process.env,AGENTWORKS_LOGIN_ENV:'0',AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:'4462',AGENTWORKS_ROOTS:dir,AGENTWORKS_CATALOG_PATH:join(dir,'catalog.json'),AGENTWORKS_DB_PATH:join(dir,'sessions.sqlite3'),AGENTWORKS_ANTIGRAVITY_PATH:join(dir,'agy'),AGENTWORKS_SSH:resolve('scripts/fake-ssh.sh')},stdio:['ignore','pipe','pipe']});
 let log='';server.stdout.on('data',b=>log+=b);server.stderr.on('data',b=>log+=b);
 await expect.poll(async()=>{if(server.exitCode!==null)throw Error(log);return serverReady(server,base);}).toBe(true);
}
async function ready(id){await expect.poll(async()=>{const s=await api('/api/sessions/'+id);if(s.session.status==='error')throw Error(JSON.stringify(s.events.slice(-4)));return s.session.status;}).toBe('idle');}
test.beforeAll(async()=>{
 await mkdir('.cache',{recursive:true});dir=await mkdtemp(resolve('.cache/antigravity-fixture-'));
 await writeFile(join(dir,'agy'),`#!/usr/bin/env python3
import sys,json,os,uuid
args=sys.argv[1:]
assert not os.environ.get('GEMINI_API_KEY') and not os.environ.get('GOOGLE_API_KEY')
if args==['--print','/usage','--output-format','json']:
 with open(${JSON.stringify(join(dir,'queries'))},'a') as log: log.write('q')
 print(json.dumps({'status':'SUCCESS','num_turns':0,'command':{'name':'usage','data':{'groups':[{'name':'Gemini Models','buckets':[{'id':'gemini-5h','window':'5h','remaining_fraction':0.75,'reset_time':'2026-11-18T00:00:00Z'}]},{'name':'Argon','buckets':[{'id':'gemini-argon-wtu-weekly','window':'weekly','remaining_fraction':0.2}]},{'name':'Claude and GPT','buckets':[{'id':'3p-weekly','window':'weekly','remaining_fraction':0.1}]}]}}}));sys.exit(0)
assert args==['acp']
model='gemini-4-argon';mode='default'
def config():
 return {'configOptions':[{'id':'model','name':'Model','category':'model','type':'select','currentValue':model,'options':[{'value':'gemini-4-argon','name':'Gemini 4 Argon'},{'value':'gemini-3.8-flash','name':'Gemini 3.8 Flash'},{'value':'claude-opus-5-5','name':'Claude Opus'}]},{'id':'mode','name':'Mode','category':'mode','type':'select','currentValue':mode,'options':[{'value':v,'name':v} for v in ['default','accept-edits','auto','plan']]}]}
for line in sys.stdin:
 r=json.loads(line);m=r.get('method');p=r.get('params',{})
 if m=='initialize':out={'protocolVersion':1,'agentCapabilities':{'loadSession':True,'sessionCapabilities':{'resume':{},'close':{},'list':{}},'_meta':{'antigravity':{'systemPrompt':{'append':True}}}},'authMethods':[]}
 elif m in ('session/new','session/resume','session/load'):
  assert 'systemPrompt' not in p
  assert 'Review only' in p['_meta']['antigravity']['systemPrompt']['append']
  out={'sessionId':p.get('sessionId',str(uuid.uuid4())),**config()}
 elif m=='session/set_config_option':
  if p['configId']=='model': model=p['value']
  elif p['configId']=='mode': mode=p['value']
  else:raise Exception('Unexpected config')
  out=config()
 elif m=='session/close':out={}
 else:raise Exception(m)
 print(json.dumps({'jsonrpc':'2.0','id':r['id'],'result':out}),flush=True)
`,{mode:0o700});
 await start();
 remote=await api('/api/hosts',{name:'Quota remote',destination:'antigravity-fixture',command:binary});
 await expect.poll(async()=>(await api('/api/catalog')).hosts.find(h=>h.id===remote.id)?.status).toBe('connected');
 await fetch(base+`/api/hosts/${remote.id}/roots`,{method:'PUT',headers:{Origin:base,'Content-Type':'application/json'},body:JSON.stringify({roots:[dir]})});
});
test.afterAll(async()=>{await stopChild(server);await rm(dir,{recursive:true,force:true});});
for(const host of ['local','remote'])test(`${host}: role metadata, native quota, refresh coalescing and resume`,async({page})=>{
 const role=await api('/api/roles',{name:'Quota reviewer '+host,instructions:'Review only'});
 const s=await api('/api/sessions',{provider:'antigravity',hostId:host==='local'?'local':remote.id,cwd:dir,title:'Antigravity '+host,role:role.id});
 if(host==='local')localId=s.id;
 await ready(s.id);
 await expect.poll(async()=>(await api('/api/sessions/'+s.id)).events.some(e=>e.kind==='antigravity_quota')).toBe(true);
 const before=(await readFile(join(dir,'queries'),'utf8')).length;
 const results=await Promise.all([api(`/api/sessions/${s.id}/quota/refresh`,{}),api(`/api/sessions/${s.id}/quota/refresh`,{})]);
 expect(results[0]).toEqual(results[1]);
 expect((await readFile(join(dir,'queries'),'utf8')).length).toBe(before);
 await api(`/api/sessions/${s.id}/permission-mode`,{mode:'auto'});await ready(s.id);
 await api(`/api/sessions/${s.id}/suspend`,{});await api(`/api/sessions/${s.id}/resume`,{});await ready(s.id);
 await page.goto(base);await page.evaluate(id=>localStorage.setItem('agentworks-session',id),s.id);await page.reload();
 await expect(page.locator('.usage-status-toggle')).toContainText('20% left');
 await expect(page.locator('.usage-status-toggle')).not.toContainText('10% left');
 await page.locator('#session-more').click();
 await expect(page.locator('#permission-mode-picker')).toHaveValue('auto');
 await page.locator('#permission-mode-picker').selectOption('plan');
 await expect.poll(async()=>(await api(`/api/sessions/${s.id}/permission-mode`)).currentValue).toBe('plan');
 await ready(s.id);await page.locator('#session-more').click();
 await page.locator('#model-picker').selectOption('gemini-3.8-flash');
 await expect(page.locator('.usage-status-toggle')).toContainText('75% left');
 await expect(page.locator('.usage-status-toggle')).not.toContainText('20% left');
 await ready(s.id);await page.locator('#model-picker').selectOption('claude-opus-5-5');
 await expect(page.locator('.usage-status-toggle')).toContainText('10% left');
 await expect(page.locator('.usage-status-toggle')).not.toContainText('75% left');
 if((await page.locator('#composer-more').getAttribute('aria-expanded'))!=='true')await page.locator('#composer-more').click();await page.locator('#show-usage').click();
 expect((await api('/api/sessions/'+s.id)).events.filter(e=>e.kind==='user')).toHaveLength(0);
 await page.goto(base+'/dashboard');
 const card=page.locator('#accounts .card').filter({has:page.getByRole('heading',{name:host==='local'?'Antigravity · Local':'Antigravity · Quota remote',exact:true})});
 await expect(card.getByRole('meter')).toHaveCount(3);
 await expect(card.getByRole('meter').first()).toHaveJSProperty('value',25);
});
test('quota survives server restart with its original observation time',async()=>{
 const before=(await api('/api/sessions/'+localId)).events.findLast(e=>e.kind==='antigravity_quota').data.observedAtMs;
 await stopChild(server);await start();
 const after=(await api('/api/sessions/'+localId)).events.findLast(e=>e.kind==='antigravity_quota').data.observedAtMs;
 expect(after).toBe(before);
});
