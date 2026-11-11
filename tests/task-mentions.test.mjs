import assert from 'node:assert/strict';
import {test} from 'node:test';
import {setupTaskMentions} from '../web/task-mentions.js';

const A='11111111-1111-4111-8111-111111111111',B='22222222-2222-4222-8222-222222222222';
const task=(key='APP-1',project=A)=>({key,uri:`agentworks://projects/${project}/tasks/${key.split('-')[1]}`,title:'A task',status:'open'});
class Node {
  children=[];attributes={};hidden=true;value='';selectionStart=0;selectionEnd=0;
  replaceChildren(){this.children=[];}
  append(node){this.children.push(node);}
  setAttribute(k,v){this.attributes[k]=v;}
  removeAttribute(k){delete this.attributes[k];}
  querySelector(){return this.children.find(c=>c.className==='active');}
  scrollIntoView(){}
  addEventListener(){}
  dispatchEvent(){}
  focus(){}
  setSelectionRange(start,end){this.selectionStart=start;this.selectionEnd=end;}
}
globalThis.document={createElement:()=>new Node()};
function fixture(enabled=true){
  let active={projectId:A},tasksEnabled=enabled;const requests=[],prompt=new Node(),list=new Node();
  const mentions=setupTaskMentions({prompt,list,getActive:()=>active,getProject:id=>id?{id,tasksEnabled}:null,api:path=>new Promise((resolve,reject)=>requests.push({path,resolve,reject}))});
  const draft=value=>{prompt.value=value;prompt.setSelectionRange(value.length,value.length);};
  return {mentions,prompt,list,requests,draft,enable:value=>{tasksEnabled=value;},scope:projectId=>{active={projectId};}};
}
async function loaded(){const f=fixture(),pending=f.mentions.refresh();f.requests[0].resolve({tasks:[task()]});await pending;return f;}
const link=(key='APP-1',project=A)=>`[#${key}](agentworks://projects/${project}/tasks/${key.split('-')[1]})`;
function key(key){return {key,prevented:false,preventDefault(){this.prevented=true;},stopPropagation(){}};}

test('same-project refresh preserves cache and picker until success, including failures',async()=>{
  const f=await loaded();f.draft('#APP');f.mentions.update();const button=f.list.children[0];
  const pending=f.mentions.refresh();
  assert.equal(f.list.hidden,false);assert.equal(f.list.children[0],button);
  assert.equal(f.mentions.expand('#APP-1'),link());
  f.requests[1].reject(new Error('offline'));await pending;
  assert.equal(f.list.children[0],button);assert.equal(f.mentions.expand('#APP-1'),link());
  const next=f.mentions.refresh();f.requests[2].resolve({tasks:[task('APP-2')]});await next;
  assert.equal(f.list.children[0].textContent,'#APP-2');
  assert.equal(f.mentions.expand('#APP-1 #APP-2'),`#APP-1 ${link('APP-2')}`);
});

test('scope changes clear immediately and stale refresh generations cannot restore old tasks',async()=>{
  const f=await loaded();f.draft('#APP');f.mentions.update();const old=f.mentions.refresh();
  f.scope(B);const next=f.mentions.refresh();
  assert.equal(f.list.hidden,true);assert.equal(f.mentions.expand('#APP-1'),'#APP-1');
  f.requests[2].resolve({tasks:[task('APP-2',B)]});await next;
  f.requests[1].resolve({tasks:[task('APP-1')]});await old;
  assert.equal(f.mentions.expand('#APP-1 #APP-2'),`#APP-1 ${link('APP-2',B)}`);
});

test('newest same-project request wins even when requests resolve out of order',async()=>{
  const f=await loaded(),old=f.mentions.refresh(),next=f.mentions.refresh();
  f.requests[2].resolve({tasks:[task('APP-2')]});await next;
  f.requests[1].resolve({tasks:[task('APP-3')]});await old;
  assert.equal(f.mentions.expand('#APP-1 #APP-2 #APP-3'),`#APP-1 ${link('APP-2')} #APP-3`);
});

test('only known canonical plain tokens expand; Markdown, escaped text and URLs stay literal',async()=>{
  const {mentions}=await loaded();
  assert.equal(mentions.expand('Use #APP-1 and #APP-1.'),`Use ${link()} and ${link()}.`);
  for(const text of ['#app-1 #APP-01 #APP-0 #UNKNOWN-1 #APP-1x #APP-1-more',
    '    #APP-1','\t#APP-1','# Heading #APP-1','Heading #APP-1\n---',
    '```js\n#APP-1\n```','````js\n```\n#APP-1\n````','~~~~\n~~~\n#APP-1\n~~~~',
    '`#APP-1`','`` a ` #APP-1 ``','``` a `` #APP-1 ```','`` unmatched #APP-1',
    '[label #APP-1](https://example.test/a_(b))','![alt #APP-1](image.png)',
    '[nested [#APP-1] label](url "title #APP-1")','[#APP-1][ref]',
    '\\#APP-1','\\` #APP-1 `','https://example.test/#APP-1','<https://example.test/ #APP-1>']){
    assert.equal(mentions.expand(text),text,text);
  }
  assert.equal(mentions.expand('`` a ` #APP-1 `` then #APP-1'),`\`\` a \` #APP-1 \`\` then ${link()}`);
  assert.equal(mentions.expand('````\n#APP-1\n```\n#APP-1\n````\n#APP-1'),`\`\`\`\`\n#APP-1\n\`\`\`\n#APP-1\n\`\`\`\`\n${link()}`);
});

test('invalid keys and noncanonical or wrong-scope task URIs are never cached',async()=>{
  const f=fixture(),pending=f.mentions.refresh();
  f.requests[0].resolve({tasks:[task('APP-01'),task('APP-2',B),{...task('APP-3'),uri:task('APP-3').uri+'?x=1'},task()]});await pending;
  assert.equal(f.mentions.expand('#APP-01 #APP-2 #APP-3 #APP-1'),`#APP-01 #APP-2 #APP-3 ${link()}`);
});

for(const change of ['caret','selection','text','scope','disabled'])test(`stale ${change} cannot consume keys or choose a saved suggestion`,async()=>{
  for(const action of ['Enter','Tab','ArrowDown','click']){
    const f=await loaded();f.draft('#APP');f.mentions.update();const button=f.list.children[0];
    if(change==='caret')f.prompt.setSelectionRange(0,0);
    if(change==='selection')f.prompt.setSelectionRange(0,4);
    if(change==='text')f.draft('#XYZ');
    if(change==='scope')f.scope(B);
    if(change==='disabled')f.enable(false);
    const before=f.prompt.value;
    if(action==='click')button.onclick();else{const event=key(action);assert.equal(f.mentions.keydown(event),false);assert.equal(event.prevented,false);}
    assert.equal(f.prompt.value,before);assert.equal(f.list.hidden,true);
  }
});

test('valid current picker still accepts Enter and emits a plain canonical token',async()=>{
  const f=await loaded();f.draft('Use #APP');f.mentions.update();const event=key('Enter');
  assert.equal(f.mentions.keydown(event),true);assert.equal(event.prevented,true);assert.equal(f.prompt.value,'Use #APP-1 ');
});


test('disabled projects do not fetch, show suggestions, or expand tokens',async()=>{
  const f=fixture(false);f.draft('#APP');
  await f.mentions.refresh();
  assert.equal(f.requests.length,0);assert.equal(f.mentions.wants(),false);
  assert.equal(f.mentions.update(),false);assert.equal(f.list.hidden,true);
  assert.equal(f.mentions.expand('#APP-1'),'#APP-1');
});

test('disabling clears immediately and an in-flight response cannot restore cached tickets',async()=>{
  const f=await loaded();f.draft('#APP');f.mentions.update();
  const pending=f.mentions.refresh();f.enable(false);
  await f.mentions.refresh();
  assert.equal(f.requests.length,2);assert.equal(f.list.hidden,true);
  assert.equal(f.list.children.length,0);assert.equal(f.mentions.expand('#APP-1'),'#APP-1');
  f.requests[1].resolve({tasks:[task()]});await pending;
  f.enable(true);
  assert.equal(f.mentions.expand('#APP-1'),'#APP-1');
  f.mentions.update();assert.equal(f.list.children[0].textContent,'No matching project tickets.');
});
