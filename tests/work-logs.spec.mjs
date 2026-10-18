import {test,expect} from '@playwright/test';
import {readFile} from 'node:fs/promises';
import {buildWorkLogs,summaryText} from '../web/work-logs.js';

// Deterministic transcript fixtures; no server, network or model inference.
const user=id=>({id,type:'message',role:'user',text:id});
const say=id=>({id,type:'message',role:'assistant',text:id});
const tool=(id,status,extra={})=>({id,type:'tool',title:id,status,...extra});

test('groups a completed turn from first to last tool, keeping the answer outside',()=>{
  const items=[user('u1'),say('intro'),tool('t1','completed'),say('mid'),tool('t2','failed'),say('answer'),user('u2')];
  const [log]=buildWorkLogs(items);
  expect(log.members).toEqual(['t1','mid','t2']);
  expect(log).toMatchObject({calls:2,failed:1,running:0,complete:true,duration:null});
  expect(summaryText(log)).toBe('2 tool calls · 1 failed');
});
test('counts unique calls, reports interruption and still-running work after completion',()=>{
  const shared=tool('t1','in_progress');
  const items=[user('u1'),shared,shared,tool('t2','completed',{observedStart:0,observedEnd:5000}),{id:'n',type:'notice',interrupted:true},user('u2')];
  const [log]=buildWorkLogs(items);
  expect(log.calls).toBe(2);
  expect(summaryText(log)).toBe('2 tool calls · interrupted · 1 still running');
});
test('imported history without later user message stays open until session is idle',()=>{
  const items=[user('u1'),tool('t1','completed')];
  expect(buildWorkLogs(items)[0].complete).toBe(false);
  expect(buildWorkLogs(items,{sessionIdle:true})[0].complete).toBe(true);
  expect(buildWorkLogs([user('u1'),say('plain')])).toEqual([]);
});
test('live-observed duration is shown only when every call has timing',()=>{
  const [log]=buildWorkLogs([user('u'),tool('a','completed',{observedStart:1000,observedEnd:3000}),tool('b','completed',{observedStart:2000,observedEnd:135000})],{sessionIdle:true});
  expect(summaryText(log)).toBe('2 tool calls · 2m 14s');
});

test('DOM fold collapses on completion, respects user choice and replays stably',async({page})=>{
  const source=await readFile(new URL('../web/work-logs.js',import.meta.url),'utf8');
  // A real origin so localStorage-backed choices behave as in the app.
  await page.route('http://work-logs.test/',r=>r.fulfill({contentType:'text/html',body:'<main id="messages"></main>'}));
  await page.goto('http://work-logs.test/');
  await page.addScriptTag({type:'module',content:source.replace(/^export /gm,'')+`
    const el=(t,c,x)=>{const n=document.createElement(t);if(c)n.className=c;if(x!==undefined)n.textContent=x;return n;};
    window.run=(items,idle,fresh)=>{
      if(fresh||!window.wl){window.wl=setupWorkLogs({el});window.nodes=new Map();document.getElementById('messages').replaceChildren();}
      const area=document.getElementById('messages');
      for(const i of items)if(!nodes.has(i.id)){const n=el(i.type==='tool'?'details':'div',i.type,i.id);nodes.set(i.id,n);area.append(n);}
      wl.apply(area,items,nodes,{sessionIdle:idle});
      return [...area.children].map(n=>n.dataset.workLog?{log:n.open,summary:n.querySelector('.work-log-counts').textContent,body:[...n.querySelector('.work-log-body').children].map(c=>c.textContent)}:n.textContent);
    };`});
  await page.waitForFunction(()=>window.run);
  const items=[user('u1'),tool('t1','in_progress'),say('mid'),tool('t2','completed'),say('answer')];
  let view=await page.evaluate(i=>run(i,false),items);
  expect(view).toEqual(['u1',{log:true,summary:'2 tool calls · 1 running',body:['t1','mid','t2']},'answer']);
  items[1].status='completed';
  view=await page.evaluate(i=>run(i,true),items);
  expect(view[1]).toMatchObject({log:false,summary:'2 tool calls'});
  // User expands; the choice persists across a full replay with fresh nodes.
  await page.click('.work-log-summary');await page.waitForTimeout(50);
  view=await page.evaluate(i=>run(i,true,true),items);
  expect(view).toEqual(['u1',{log:true,summary:'2 tool calls',body:['t1','mid','t2']},'answer']);
  await expect(page.locator('.work-log-summary')).toBeVisible();
  await page.setViewportSize({width:375,height:700});
  const box=await page.locator('.work-log-summary').boundingBox();
  expect(box.width).toBeLessThanOrEqual(375);
  // The expanded preference keeps other completed logs open without overriding explicit choices.
  await page.evaluate(()=>{wl.mount(document.body);});
  await page.check('#work-log-expanded');
  view=await page.evaluate(i=>run(i,true,true),[user('u9'),tool('z','completed')].map(x=>x));
  expect(view[1].log).toBe(true);
});
