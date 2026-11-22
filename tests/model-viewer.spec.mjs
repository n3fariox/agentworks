import {test,expect} from '@playwright/test';
import {spawn} from 'node:child_process';
import {mkdtemp,mkdir,writeFile} from 'node:fs/promises';
import {resolve,join} from 'node:path';
import {serverReady,stopChild,removeFixture} from './support/cleanup.mjs';

// Tests our integration in one browser; upstream owns format/browser compatibility.
const base='http://127.0.0.1:4669';test.use({baseURL:base});
let fixture,child,log='';
test.beforeAll(async()=>{
  await mkdir('.cache',{recursive:true});fixture=await mkdtemp(resolve('.cache/model-viewer-'));
  await writeFile(join(fixture,'part.obj'),'mtllib paint.mtl\nv 0 0 0\nv 1 0 0\nv 0 1 0\nusemtl paint\nf 1 2 3\n');
  await writeFile(join(fixture,'paint.mtl'),'newmtl paint\nKd 0.8 0.2 0.1\n');
  await writeFile(join(fixture,'broken.stl'),'invalid model');
  await writeFile(join(fixture,'broken.step'),'ISO-10303-21;HEADER;ENDSEC;DATA;ENDSEC;END-ISO-10303-21;');
  child=spawn(process.env.AGENTWORKS_TEST_BINARY||resolve('target/debug/agentworks'),[],{env:{...process.env,AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:'4669',AGENTWORKS_ROOTS:fixture,AGENTWORKS_CATALOG_PATH:join(fixture,'catalog.json'),AGENTWORKS_DB_PATH:join(fixture,'sessions.sqlite3'),AGENTWORKS_DEMO_STATE_DIR:join(fixture,'demo')},stdio:['ignore','pipe','pipe']});
  child.on('error',e=>{log+=e.message;});for(const stream of [child.stdout,child.stderr])stream.on('data',data=>{log+=data;});
  for(let i=0;i<100;i++){if(child.exitCode!==null)throw Error(log);if(await serverReady(child,base))return;await new Promise(r=>setTimeout(r,100));}throw Error('Server did not start: '+log);
});
test.afterAll(async()=>{await stopChild(child);await removeFixture(fixture);});

test('optional viewer assets and file preview integration',async({page,request})=>{
  const status=await (await request.get('/api/status')).json();
  const enabled=status.features.modelViewer;
  const frameResponse=await request.get('/model-viewer-frame.html');
  expect(frameResponse.status()).toBe(enabled?200:404);
  if(enabled){expect(frameResponse.headers()['content-security-policy']).toContain("frame-ancestors 'self'");expect(frameResponse.headers()['content-security-policy']).toContain("'unsafe-eval'");}
  expect((await request.get('/model-viewer-assets/occt-import-js/occt-import-js.wasm')).status()).toBe(enabled?200:404);
  if(enabled){const light=await request.get('/model-viewer-assets/environment/posx.jpg');expect(light.status()).toBe(200);expect(light.headers()['content-type']).toBe('image/jpeg');}
  const requests=[];page.on('request',r=>requests.push(r.url()));
  page.on('console',msg=>{if(msg.type()==='error')console.log('Browser error:',msg.text());});
  page.on('pageerror',error=>console.log('Page error:',error.message));
  await page.goto('/');await page.locator('#nav-files').click();
  await page.locator('#file-entries button[data-path="part.obj"]').click();
  if(!enabled){await expect(page.locator('#workspace-file-detail')).toContainText('mtllib paint.mtl');await expect(page.locator('iframe.model-viewer')).toHaveCount(0);return;}
  const frame=page.frameLocator('iframe.model-viewer');
  await expect(frame.locator('body')).toHaveAttribute('data-model-loaded','true');
  await frame.getByRole('button',{name:'Edges',exact:true}).click();
  await expect(frame.getByRole('button',{name:'Edges',exact:true})).toHaveAttribute('aria-pressed','true');
  await frame.getByRole('button',{name:'Fit model',exact:true}).click();
  const lit=await frame.locator('canvas').screenshot();
  await frame.getByRole('button',{name:'Albedo',exact:true}).click();
  await expect(frame.getByRole('button',{name:'Albedo',exact:true})).toHaveAttribute('aria-pressed','true');
  const unlit=await frame.locator('canvas').screenshot();expect(unlit.equals(lit)).toBe(false);
  await frame.getByRole('button',{name:'Albedo',exact:true}).click();
  expect((await frame.locator('canvas').screenshot()).equals(lit)).toBe(true);
  await frame.getByRole('button',{name:'Albedo',exact:true}).click();
  expect(requests.some(url=>new URL(url).searchParams.get('path')==='paint.mtl')).toBe(true);
  expect(requests.every(url=>url.startsWith(base))).toBe(true);
  await page.locator('#file-entries button[data-path="broken.stl"]').click();
  await expect(page.frameLocator('iframe.model-viewer').locator('#status')).toContainText('Unable to preview');
  await expect(page.frameLocator('iframe.model-viewer').getByRole('button',{name:'Albedo',exact:true})).toHaveAttribute('aria-pressed','true');
  await expect(page.locator('#workspace-file-detail').getByRole('link',{name:'Download'})).toBeVisible();
  // Verify our worker/wasm URLs and CSP, rather than upstream CAD format fidelity.
  const wasm=page.waitForResponse(response=>response.url().endsWith('/occt-import-js/occt-import-js.wasm'));
  await page.locator('#file-entries button[data-path="broken.step"]').click();
  const response=await wasm;expect(response.status()).toBe(200);expect(response.headers()['content-type']).toBe('application/wasm');
  await expect(page.frameLocator('iframe.model-viewer').locator('#status')).toContainText('Unable to preview',{timeout:15000});
  expect(requests.every(url=>url.startsWith(base)||url.startsWith('blob:'+base))).toBe(true);
});
