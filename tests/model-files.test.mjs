import {test} from 'node:test';
import assert from 'node:assert/strict';
import {collectModelFiles,companionPath} from '../web/model-files.js';

test('query-based file API preserves model names and resolves nested glTF resources',async()=>{
  const bodies=new Map([
    ['models/scene.gltf',JSON.stringify({buffers:[{uri:'mesh.bin'}],images:[{uri:'textures/a%20b.png'},{uri:'https://example.com/remote.png'},{uri:'data:image/png;base64,AA=='}]})],
    ['models/mesh.bin',new Uint8Array([0,1,2])],['models/textures/a b.png',new Uint8Array([3,4,5])],
  ]),requests=[];
  const result=await collectModelFiles('https://host/mount/api/locations/abc/file?path=models%2Fscene.gltf','models/scene.gltf',async url=>{
    assert.equal(url.pathname,'/mount/api/locations/abc/file');const path=url.searchParams.get('path');requests.push(path);return new Response(bodies.get(path),{status:bodies.has(path)?200:404});
  });
  assert.deepEqual(requests,[...bodies.keys()]);
  assert.deepEqual(result.files.map(file=>file.name),['scene.gltf','mesh.bin','a b.png']);
  assert.deepEqual(result.missing,[]);
});

test('OBJ materials load their textures relative to the material file and report missing companions',async()=>{
  const bodies=new Map([['part.obj','mtllib materials/part.mtl\nv 0 0 0'],['materials/part.mtl','newmtl paint\nmap_Kd -s 1 1 1 paint.png\nmap_bump absent.png'],['materials/paint.png','pixels']]);
  const result=await collectModelFiles('https://host/api/sessions/abc/file','part.obj',async url=>{
    const path=url.searchParams.get('path');return new Response(bodies.get(path)||'Not found',{status:bodies.has(path)?200:404});
  });
  assert.deepEqual(result.files.map(file=>file.name),['part.obj','part.mtl','paint.png']);
  assert.deepEqual(result.missing,['materials/absent.png']);
  assert.equal(companionPath('part.obj','//other-host/file'),null);
  assert.equal(companionPath('part.obj','file:///etc/passwd'),null);
  await assert.rejects(collectModelFiles('https://host/api/locations/abc/file','missing.stl',async()=>new Response('File unavailable',{status:404})),/File unavailable/);
});
