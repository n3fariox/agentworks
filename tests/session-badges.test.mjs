import {test} from 'node:test';
import assert from 'node:assert/strict';
import {sessionBadges,setupSessionBadges} from '../web/session-badges.js';
const now=1800000000000;
const badge={id:'release',label:'Ready',tone:'success',tooltip:'Release ready',source:'release.star'};
const view={status:'no_origin',badgesAt:now,badges:[badge]};
test('generic badges work without a GitHub origin',()=>{
  assert.equal(sessionBadges(view,now)[0].tone,'success');
  assert.equal(sessionBadges(view,now)[0].label,'Ready');
});
test('missing and explicitly empty output collapse the badge row',()=>{
  assert.deepEqual(sessionBadges(null,now),[]);
  assert.deepEqual(sessionBadges({...view,badges:[]},now),[]);
});
test('stale or unavailable data cannot retain an approval color',()=>{
  assert.equal(sessionBadges(view,now+601000)[0].tone,'neutral');
  assert.equal(sessionBadges({...view,badgesUnavailable:true},now)[0].tone,'neutral');
});
test('only bounded text and known semantic tones reach the renderer',()=>{
  assert.equal(sessionBadges({...view,badges:[{...badge,tone:'background:url(x)'}]},now)[0].tone,'neutral');
  assert.equal(sessionBadges({...view,badges:[{...badge,label:'x'.repeat(100)}]},now).length,0);
});

test('optional badge links accept only credential-free HTTPS destinations',()=>{
  const linked={...view,badges:[{...badge,url:'https://github.com/org/repo/pull/1234'}]};
  assert.equal(sessionBadges(linked,now)[0].url,'https://github.com/org/repo/pull/1234');
  for(const url of ['javascript:alert(1)','data:text/html,hi','http://example.com','/relative','https://user:secret@example.com','https://example.com/with space']){
    assert.equal(sessionBadges({...view,badges:[{...badge,url}]},now)[0].url,null);
  }
  assert.equal(sessionBadges(view,now)[0].url,null);
});

test('attachment refresh retains badges, isolates sessions and ignores superseded requests',async()=>{
  const previous={document:globalThis.document,IntersectionObserver:globalThis.IntersectionObserver,setInterval:globalThis.setInterval};
  const target=id=>({children:[],closest:()=>({dataset:{sessionId:id}}),replaceChildren(){this.children=[];},append(node){this.children.push(node);}});
  const main=target('main'),other=target('other'),responses=[],calls=[];
  globalThis.document={hidden:false,addEventListener(){},createElement:()=>({setAttribute(){}})};
  globalThis.IntersectionObserver=class{observe(){}disconnect(){}};
  globalThis.setInterval=()=>0;
  const record=label=>({status:'ready',badgesAt:Date.now(),badges:[{...badge,label}]});
  const deferred=()=>{let resolve,reject;const promise=new Promise((ok,fail)=>{resolve=ok;reject=fail;});return {promise,resolve,reject};};
  try{
    const controller=setupSessionBadges({api:path=>{calls.push(path);assert.ok(responses.length,'unexpected API read');return responses.shift();},area:{querySelectorAll:()=>[main,other]}});
    responses.push(Promise.resolve(record('Before')));await controller.read('main');
    responses.push(Promise.resolve(record('Other')));await controller.read('other');
    const old=deferred();controller.invalidate('main');responses.push(old.promise);const oldRead=controller.read('main');
    assert.equal(main.children[0].textContent,'Before');assert.equal(other.children[0].textContent,'Other');
    const next=deferred();controller.invalidate('main');responses.push(next.promise);const nextRead=controller.read('main');
    assert.equal(main.children[0].textContent,'Before');
    next.resolve(record('After'));await nextRead;assert.equal(main.children[0].textContent,'After');
    old.resolve(record('Obsolete'));await oldRead;assert.equal(main.children[0].textContent,'After');
    await controller.read('other');assert.equal(calls.filter(p=>p.includes('/other/')).length,1);
    const failed=deferred();controller.invalidate('main');responses.push(failed.promise);const failureRead=controller.read('main');
    failed.reject(new Error('offline'));await assert.rejects(failureRead,/offline/);
    assert.equal(main.children[0].textContent,'After');assert.equal(main.children[0].className,'session-badge neutral');
    // A transient API failure with an empty badge array is not a confirmed clear.
    controller.invalidate('main');responses.push(Promise.resolve({status:'unavailable',badges:[],badgesAt:Date.now()}));await controller.read('main');
    assert.equal(main.children[0].textContent,'After');assert.equal(main.children[0].className,'session-badge neutral');
    controller.invalidate('main');responses.push(Promise.resolve({status:'ready',badges:[],badgesAt:Date.now()}));await controller.read('main');
    assert.deepEqual(main.children,[]);assert.equal(other.children[0].textContent,'Other');
  }finally{
    for(const [key,value] of Object.entries(previous)){if(value===undefined)delete globalThis[key];else globalThis[key]=value;}
  }
});
