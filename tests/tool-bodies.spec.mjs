import {serverReady,stopChild} from './support/cleanup.mjs';
import {test,expect} from '@playwright/test';
import {spawn} from 'node:child_process';
import {mkdtemp,mkdir,rm} from 'node:fs/promises';
import {resolve,join} from 'node:path';

// Tool cards from compacted snapshots: Starlark header, output loaded on expand,
// and a reconnect that resumes instead of replaying. Isolated server on assigned
// port 4458; the demo agent replays recorded Bazel output, no command or model runs.
const base='http://127.0.0.1:4458';
let fixture,server,log='',exited=false;
async function api(path,body,method){
  const r=await fetch(base+path,{method:method||(body?'POST':'GET'),headers:{Origin:base,'Content-Type':'application/json'},body:body?JSON.stringify(body):undefined});
  expect(r.ok,await r.clone().text()).toBe(true);return r.status===204?null:r.json();
}
test.beforeAll(async()=>{
  await mkdir('.cache',{recursive:true});fixture=await mkdtemp(resolve('.cache/tool-bodies-'));await mkdir(join(fixture,'project'));
  server=spawn(process.env.AGENTWORKS_TEST_BINARY||resolve('target/debug/agentworks'),[],{env:{...process.env,AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:'4458',AGENTWORKS_ROOTS:fixture,
    AGENTWORKS_CATALOG_PATH:join(fixture,'catalog.json'),AGENTWORKS_DB_PATH:join(fixture,'sessions.sqlite3'),AGENTWORKS_DEMO_STATE_DIR:join(fixture,'demo')},stdio:['ignore','pipe','pipe']});
  server.on('exit',()=>exited=true);for(const s of [server.stdout,server.stderr])s.on('data',c=>{log+=c;});
  for(let i=0;i<100;i++){if(exited)throw new Error(log);if(await serverReady(server,base))return;await new Promise(r=>setTimeout(r,100));}
  throw new Error('readiness timed out: '+log);
});
test.afterAll(async()=>{await stopChild(server);if(fixture)await rm(fixture,{recursive:true,force:true});});

test('tool card shows the hook header, loads output on expand and survives a reconnect',async({page})=>{
  await api('/api/scripts/a_header.star',{source:`load("@agentworks//tool.star", "ToolCall", "ToolContext")
def bazel(ctx: ToolContext, tool: ToolCall) -> str | None:
    return "Run Bazel tests"
agent.register_tool_header(bazel, title_pattern = "^bazel ")
`},'PUT');
  const session=await api('/api/sessions',{provider:'demo',cwd:join(fixture,'project'),title:'Tool bodies'});
  const idle=()=>expect.poll(async()=>(await api(`/api/sessions/${session.id}?view=meta`)).status,{timeout:15000}).toBe('idle');
  await idle();
  await api(`/api/sessions/${session.id}/prompt`,{text:'bazel demo',requestId:'tool-bodies-1'});
  await expect.poll(async()=>(await api(`/api/sessions/${session.id}?view=meta`)).turnRunning,{timeout:15000}).toBe(false);
  await idle();

  const sockets=[],bodies=[];
  page.on('request',r=>{if(r.url().includes('/tools/'))bodies.push(r.url());});
  // Proxy the session socket so the test can drop the connection.
  await page.routeWebSocket(/\/api\/sessions\/[^/]+\/events/,ws=>{sockets.push(ws);ws.connectToServer();});
  await page.goto(base+'/');await page.evaluate(id=>localStorage.setItem('agentworks-session',id),session.id);
  await page.goto(base+'/');
  await expect(page.locator('#prompt')).toBeEnabled({timeout:15000});
  await page.locator('.work-log-summary').click();
  const card=page.locator('.tool-card');
  await expect(card.locator('.tool-title')).toHaveText('Run Bazel tests');
  await expect(card.locator('.tool-title')).toHaveAttribute('title','bazel test //fixture:all');
  await expect(card.locator('.tool-state')).toHaveText('failed');
  expect(bodies).toEqual([]);

  await card.locator('summary').click();
  const load=card.getByRole('button',{name:'Load tool output',exact:true});
  await expect(load).toBeVisible();
  expect(bodies).toEqual([]);
  await load.click();
  await expect(card.locator('pre')).toContainText('Build completed successfully');
  await expect(load).toBeHidden();
  await expect(card.locator('pre')).toContainText('"command": "bazel test //fixture:all"');
  expect(bodies).toHaveLength(1);

  // A dropped connection resumes after the last event: the open card keeps its output.
  expect(sockets[0].url()).not.toContain('after=');
  await sockets[0].close();
  await expect.poll(()=>sockets.length,{timeout:10000}).toBe(2);
  expect(sockets[1].url()).toMatch(/\?after=\d+$/);
  await expect(page.locator('#connection')).toHaveText('Connected');
  await expect(card).toHaveAttribute('open','');
  await expect(card.locator('pre')).toContainText('Build completed successfully');
  expect(bodies).toHaveLength(1);
  await api('/api/scripts/a_header.star',undefined,'DELETE');
  await expect(card.locator('.tool-title')).toHaveText('bazel test //fixture:all');
});
