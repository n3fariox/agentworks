import {test} from 'node:test';
import assert from 'node:assert/strict';
import {sessionBadges} from '../web/session-badges.js';
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
  assert.equal(sessionBadges(view,now+121000)[0].tone,'neutral');
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
