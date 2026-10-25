import {test,expect} from '@playwright/test';
import {spawn} from 'node:child_process';
import {createServer} from 'node:http';
import {mkdtemp,readFile} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';
import {serverReady,stopChild,removeFixture} from './support/cleanup.mjs';

const base='http://127.0.0.1:4498';
let dir,server,fixture,providerUrl,remote,offline,output='';
async function api(path,body){const res=await fetch(base+path,{method:body===undefined?'GET':'POST',headers:{Origin:base,'Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body)});expect(res.ok).toBeTruthy();return res.json();}
test.beforeAll(async()=>{
  dir=await mkdtemp(join(tmpdir(),'agentworks-provider-ui-'));
  fixture=createServer((req,res)=>{
    if(req.headers.authorization!=='Bearer ui-fixture-key'){res.writeHead(401);res.end();return;}
    res.writeHead(200,{'Content-Type':'application/json'});res.end(JSON.stringify({data:[{id:'ui-fixture-model'}]}));
  });
  await new Promise(r=>fixture.listen(0,'127.0.0.1',r));providerUrl=`http://127.0.0.1:${fixture.address().port}/v1`;
  server=spawn(resolve(process.env.AGENTWORKS_TEST_BINARY||'target/debug/agentworks'),[],{env:{...process.env,
    AGENTWORKS_SSH:resolve('scripts/fake-ssh.sh'),AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:'4498',AGENTWORKS_ROOTS:dir,
    AGENTWORKS_CATALOG_PATH:join(dir,'catalog.json'),AGENTWORKS_DB_PATH:join(dir,'sessions.sqlite3'),
    AGENTWORKS_MODEL_PROVIDERS_DIR:join(dir,'providers'),GOOSE_DISABLE_SESSION_NAMING:'true',
  },stdio:['ignore','pipe','pipe']});
  server.stdout.on('data',b=>output+=b);server.stderr.on('data',b=>output+=b);
  let error;server.on('error',e=>error=e);
  await expect.poll(async()=>{if(error)throw error;if(server.exitCode!==null)throw Error(output);return serverReady(server,base);},{timeout:10000}).toBe(true);
  remote=await api('/api/hosts',{name:'Build server',destination:'dev@provider-ui',command:resolve(process.env.AGENTWORKS_TEST_BINARY||'target/debug/agentworks')});
  offline=await api('/api/hosts',{name:'Offline workstation',destination:'unreachable-provider-ui'});
  await expect.poll(async()=>(await api('/api/catalog')).hosts.find(h=>h.id===remote.id)?.status).toBe('connected');

});
test.afterAll(async()=>{await stopChild(server);fixture?.closeAllConnections();if(fixture)await new Promise(r=>fixture.close(r));await removeFixture(dir);});

test('configure a provider, discover models, create a bound session, and keep its key private',async({page})=>{
  const errors=[];page.on('pageerror',e=>errors.push(e.message));
  await page.goto(base+'/?settings');
  await page.getByRole('tab',{name:'Model providers',exact:true}).click();
  await page.getByRole('button',{name:'Add provider'}).click();
  await page.locator(`#model-provider-hosts input[value="${remote.id}"]`).check();
  await page.locator('#model-provider-editor').getByLabel('Name',{exact:true}).fill('Local test provider');
  await page.locator('#model-provider-editor').getByLabel('Base URL',{exact:true}).fill(providerUrl);
  await page.locator('#model-provider-editor').getByLabel('API key optional',{exact:true}).fill('ui-fixture-key');
  await page.locator('#load-model-provider-models').click();
  await expect(page.locator('#model-provider-message')).toContainText('1 models found');
  expect((await api('/api/model-providers')).profiles).toHaveLength(0);
  await page.locator('#model-provider-editor').getByLabel('Default model',{exact:true}).fill('ui-fixture-model');
  await page.locator('#save-model-provider').click();
  await expect(page.locator('#model-provider-list')).toContainText('Local test provider');
  const profile=(await api('/api/model-providers')).profiles[0];expect(profile.keyConfigured).toBe(true);expect(profile.hostIds.sort()).toEqual(['local',remote.id].sort());
  expect((await api('/api/model-providers?hostId='+remote.id)).profiles[0].id).toBe(profile.id);
  expect(JSON.stringify(profile)).not.toContain('ui-fixture-key');
  await expect(page.locator('#model-provider-key')).toHaveValue('');
  await page.locator('#model-provider-list').getByRole('button',{name:'Edit',exact:true}).click();
  await expect(page.locator('#model-provider-key')).toHaveAttribute('placeholder',/Configured/);
  await expect(page.locator('#model-provider-key')).toHaveValue('');
  await page.locator('#test-model-provider').click();
  await expect(page.locator('#model-provider-message')).toContainText('Connected from Local.');
  await page.locator('#model-provider-remove-key').check();await page.locator('#test-model-provider').click();
  await expect(page.locator('#model-provider-message')).toContainText('rejected authentication');
  await page.locator('#model-provider-remove-key').uncheck();await page.locator('#save-model-provider').click();
  await expect(page.locator('#model-provider-editor')).toBeHidden();
  await page.locator('#dismiss-settings').click();
  await page.locator('#new-session').click();
  await page.locator('#provider').selectOption('goose');
  await expect(page.locator('#create-model-provider')).toHaveValue(profile.id);
  await expect(page.locator('#create-model')).toHaveValue('ui-fixture-model');
  await page.locator('#directory').fill(dir);await page.locator('#load-models').click();
  await expect(page.locator('#model-note')).toContainText('Model is applied');
  await page.locator('#create-submit').click();
  await expect(page.locator('#create-dialog')).toBeHidden();
  await expect.poll(async()=>{
    const sessions=await api('/api/sessions');return sessions.find(s=>s.provider==='goose')?.status;
  }).toBe('idle');
  const session=(await api('/api/sessions')).find(s=>s.provider==='goose');expect(session.modelProvider).toBe(profile.id);
  await page.reload();await expect(page.locator('#provider-label')).toContainText('Goose');
  expect((await readFile(join(dir,'providers','profiles.json'),'utf8'))).not.toContain('ui-fixture-key');
  await page.goto(base+'/?settings');await page.getByRole('tab',{name:'Model providers',exact:true}).click();
  await page.locator('#model-provider-list').getByRole('button',{name:'Edit',exact:true}).click();
  await page.locator(`#model-provider-hosts input[value="${remote.id}"]`).uncheck();await page.locator('#save-model-provider').click();
  await expect(page.locator('#model-provider-editor')).toBeHidden();
  expect((await api('/api/model-providers')).profiles).toHaveLength(1);
  expect((await api('/api/model-providers?hostId='+remote.id)).profiles).toHaveLength(0);
  expect(errors).toEqual([]);
});

test('provider editor works on a narrow screen',async({page})=>{
  await page.setViewportSize({width:390,height:844});await page.goto(base+'/?settings');
  await page.getByRole('tab',{name:'Model providers',exact:true}).click();
  await page.getByRole('button',{name:'Add provider'}).click();
  await expect(page.locator('#save-model-provider')).toBeVisible();
  expect(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBe(true);
});

test('add one provider and enable it on an offline host',async({page})=>{
  const count=(await api('/api/model-providers')).profiles.length;
  await page.goto(base+'/?settings');await page.getByRole('tab',{name:'Model providers',exact:true}).click();
  await page.getByRole('button',{name:'Add provider'}).click();
  const checkbox=page.locator(`#model-provider-hosts input[value="${offline.id}"]`);
  await expect(checkbox).toBeEnabled();await checkbox.check();
  await page.locator('#model-provider-name').fill('Shared offline provider');await page.locator('#model-provider-url').fill(providerUrl);
  await page.locator('#model-provider-model').fill('ui-fixture-model');await page.locator('#save-model-provider').click();
  await expect(page.locator('#model-provider-editor')).toBeHidden();
  const all=(await api('/api/model-providers')).profiles;expect(all).toHaveLength(count+1);
  const p=all.find(p=>p.name==='Shared offline provider');expect(p.hostIds.sort()).toEqual(['local',offline.id].sort());
  expect((await api('/api/model-providers?hostId='+offline.id)).profiles[0].id).toBe(p.id);
});
