import {test,expect} from '@playwright/test';
import {readFile} from 'node:fs/promises';
import {resolve} from 'node:path';

// Exercise the real terminal UI with presentation frames, without starting a PTY.
async function fixture(page){
  await page.route('https://terminal.test/**',async route=>{
    const path=new URL(route.request().url()).pathname;
    if(path==='/')return route.fulfill({contentType:'text/html',body:`
      <link rel="stylesheet" href="/vendor/xterm.css">
      <link rel="stylesheet" href="/workspace.css">
      <section id="session-terminal" style="width:620px;height:300px">
        <div id="terminal-status"></div><button id="terminal-restart" hidden></button>
        <span id="terminal-cell-measure">W</span><div id="terminal-surface"></div>
      </section>
      <script type="module">
        import {setupTerminal} from '/terminal.js';
        setupTerminal({$:id=>document.getElementById(id),api:async()=>({}),getActive:()=>({id:'fixture'}),socketUrl:()=> 'wss://terminal.test/events',tabs:false}).open();
      </script>`});
    if(!/^\/(?:vendor\/)?[\w.-]+$/.test(path))return route.abort();
    let body=await readFile(resolve('web','.'+path),'utf8');
    if(path==='/terminal.js')body=body.replace("terminal.open($('terminal-surface'));","terminal.open($('terminal-surface'));window.__terminal=terminal;");
    return route.fulfill({contentType:path.endsWith('.css')?'text/css':'text/javascript',body});
  });
  let socket;
  await page.routeWebSocket('wss://terminal.test/events',ws=>socket=ws);
  await page.goto('https://terminal.test/');
  await expect(page.locator('#terminal-status')).toHaveText('Shell running');
  return async data=>{
    const size=await page.evaluate(()=>({cols:__terminal.cols,rows:__terminal.rows}));
    socket.send(JSON.stringify({type:'frame',seq:1,...size,data}));
    await expect(page.locator('.xterm-rows')).toContainText(/auth.example|accounts.google.com/);
  };
}

test('auth URLs open intact in a separate tab and remain selectable',async({page})=>{
  const send=await fixture(page),url='https://auth.example/activate?user_code=ABCD-1234&state=a%2Fb';
  await page.evaluate(()=>{window.__opened=[];window.open=(...args)=>{__opened.push(args);return null;};});
  await send('\x1b[2J\x1b[H'+url+'\r\nCode: ABCD-1234');
  const point=await page.evaluate(()=>{
    const screen=document.querySelector('.xterm-screen').getBoundingClientRect();
    return {x:screen.x+screen.width/__terminal.cols*10.5,y:screen.y+screen.height/__terminal.rows/2};
  });
  await page.mouse.move(point.x,point.y);
  await expect(page.locator('.xterm-cursor-pointer')).toHaveCount(1);
  await page.mouse.click(point.x,point.y);
  expect(await page.evaluate(()=>__opened)).toEqual([[url,'_blank','noopener,noreferrer']]);
  await page.mouse.move(point.x,point.y);
  await page.mouse.down();
  await page.mouse.move(point.x+50,point.y,{steps:5});
  await page.mouse.up();
  expect(await page.evaluate(()=>__opened.length)).toBe(1);
  expect(await page.evaluate(()=>__terminal.getSelection())).not.toBe('');
  expect(await page.evaluate(length=>{__terminal.select(0,0,length);return __terminal.getSelection();},url.length)).toBe(url);
  expect(await page.evaluate(()=>{__terminal.select(6,1,9);return __terminal.getSelection();})).toBe('ABCD-1234');
});

const googleUrl='https://accounts.google.com/o/oauth2/auth?access_type=offline&client_id=fixture.apps.googleusercontent.com&code_challenge=ABCDEFGHIJKLMNOPQRSTUVWXYZ_0123456789&code_challenge_method=S256&prompt=consent&redirect_uri=https%3A%2F%2Fantigravity.google%2Foauth-callback&response_type=code&scope=https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fcloud-platform+https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fuserinfo.email+https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fuserinfo.profile+https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fcclog+https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fexperimentsandconfigs+https%3A%2F%2Fwww.googleapis.com%2Fauth%2Faicode+openid&state=fixture_state';

async function copySelection(page){
  await page.context().grantPermissions(['clipboard-read','clipboard-write'],{origin:'https://terminal.test'});
  await page.locator('.xterm-helper-textarea').focus();
  expect(await page.evaluate(()=>document.execCommand('copy'))).toBe(true);
  return page.evaluate(()=>navigator.clipboard.readText());
}

for(const inset of [0,1,2])test(`long auth URL copies and opens intact across repainted rows with ${inset}-cell insets`,async({page})=>{
  const send=await fixture(page);
  const {cols,rows}=await page.evaluate(()=>({cols:__terminal.cols,rows:__terminal.rows}));
  const width=cols-inset*2,chunks=googleUrl.match(new RegExp(`.{1,${width}}`,'g')).map(chunk=>' '.repeat(inset)+chunk+' '.repeat(inset));
  expect(chunks.length).toBeLessThan(rows);
  // Match the service's physical-row painting: absolute cursor positions,
  // disabled auto-wrap, and full-width padding. This loses xterm's isWrapped.
  const frame='\x1b[?7l\x1b[2J'+chunks.map((chunk,row)=>`\x1b[${row+1};1H${chunk.padEnd(cols)}`).join('');
  await send(frame);
  await page.evaluate(length=>__terminal.select(0,0,length),(chunks.length-1)*cols+chunks.at(-1).length);
  expect(await copySelection(page)).toBe(googleUrl);
  await page.evaluate(()=>{__terminal.clearSelection();window.__opened=[];window.open=(...args)=>{__opened.push(args);return null;};});
  const point=await page.evaluate(({inset,lastRow})=>{
    const screen=document.querySelector('.xterm-screen').getBoundingClientRect();
    return {x:screen.x+screen.width/__terminal.cols*(inset+3.5),y:screen.y+screen.height/__terminal.rows*(lastRow+0.5)};
  },{inset,lastRow:chunks.length-1});
  await page.mouse.move(point.x,point.y);
  await expect(page.locator('.xterm-cursor-pointer')).toHaveCount(1);
  await page.mouse.click(point.x,point.y);
  expect(await page.evaluate(()=>__opened)).toEqual([[googleUrl,'_blank','noopener,noreferrer']]);
});

test('copy keeps real newlines, indentation, and spaces in ordinary output',async({page})=>{
  const send=await fixture(page);
  await send('\x1b[2J\x1b[Hhttps://auth.example/short\r\n  next line of output\r\n    indented code');
  await page.evaluate(()=>__terminal.select(0,0,__terminal.cols*2+17));
  expect(await copySelection(page)).toBe('https://auth.example/short\n  next line of output\n    indented code');
});
