import {test,expect} from '@playwright/test';
import {spawn} from 'node:child_process';
import {mkdtemp,mkdir,readFile,writeFile} from 'node:fs/promises';
import {resolve,join} from 'node:path';
import {tmpdir} from 'node:os';
import WebSocket from 'ws';
import {serverReady,stopChild} from './support/cleanup.mjs';

const base='http://127.0.0.1:4456';
let fixture,server,local,remote,host;
const archivedSessions=[];
async function api(path,body,method){
  const response=await fetch(base+path,{method:method||(body===undefined?'GET':'POST'),headers:{Origin:base,'Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body)});
  if(!response.ok)throw Error(await response.text());return response.json();
}
async function waitFor(check){for(let n=0;n<150;n++){const value=await check();if(value)return value;await new Promise(r=>setTimeout(r,100));}throw Error('Timed out');}
const exists=pid=>{try{process.kill(Number(pid),0);return true;}catch{return false;}};

async function startServer(){
  server=spawn(resolve('target/debug/agentworks'),[],{detached:true,env:{...process.env,HOME:join(fixture,'home'),SHELL:'/bin/bash',NO_BROWSER:'1',NO_COLOR:'1',CODEX_CI:'1',PAGER:'cat',GIT_PAGER:'cat',AGENTWORKS_BIND:'127.0.0.1',AGENTWORKS_PORT:'4456',AGENTWORKS_ROOTS:fixture,AGENTWORKS_DB_PATH:join(fixture,'sessions.sqlite3'),AGENTWORKS_CATALOG_PATH:join(fixture,'catalog.json'),AGENTWORKS_DEMO_STATE_DIR:join(fixture,'demo'),AGENTWORKS_SSH:resolve('scripts/fake-ssh.sh')},stdio:['ignore','pipe','pipe']});
  let log='';server.stderr.on('data',data=>log+=data);server.stdout.on('data',data=>log+=data);
  await waitFor(async()=>{if(server.exitCode!==null)throw Error(log);return serverReady(server,base);});
}

test.beforeAll(async()=>{
  fixture=await mkdtemp(join(tmpdir(),'agentworks-terminal-'));await mkdir(join(fixture,'home'));
  await writeFile(join(fixture,'home','.bashrc'),"PS1='terminal-fixture> '\n");
  await writeFile(join(fixture,'query.py'),`import os,tty,termios,select\nold=termios.tcgetattr(0)\ntry:\n tty.setraw(0)\n os.write(1,b'\\x1b[6n')\n reply=os.read(0,100) if select.select([0],[],[],3)[0] else b'NONE'\nfinally:\n termios.tcsetattr(0,termios.TCSANOW,old)\nprint('query-reply:'+repr(reply))\n`);
  await startServer();
  local=await api('/api/sessions',{provider:'demo',cwd:fixture,title:'Terminal local'});
  host=await api('/api/hosts',{name:'Terminal host',destination:'terminal-box',command:resolve('target/debug/agentworks')});
  await waitFor(async()=>((await api('/api/catalog')).hosts.find(h=>h.id===host.id)?.status==='connected'));
  const roots=await fetch(base+`/api/hosts/${host.id}/roots`,{method:'PUT',headers:{Origin:base,'Content-Type':'application/json'},body:JSON.stringify({roots:[fixture]})});expect(roots.ok).toBe(true);
  remote=await api('/api/sessions',{provider:'demo',hostId:host.id,cwd:fixture,title:'Terminal remote'});
});
test.afterAll(async()=>{await stopChild(server,{signal:'SIGINT'}); /* Keep fixture until all PTYs have stopped. */
  const {rm}=await import('node:fs/promises');await rm(fixture,{recursive:true,force:true});
});

for(const kind of ['local','remote'])test(`${kind}: one persistent shell, reconnect, resize, queries, real exit and restart`,async({page})=>{
  const session=kind==='local'?local:remote;
  const frames=[],errors=[];page.on('pageerror',error=>errors.push(error.message));
  page.on('websocket',ws=>{if(ws.url().includes('/terminal/events'))ws.on('framereceived',event=>{try{frames.push(JSON.parse(event.payload));}catch{}});});
  await page.goto(base);
  await page.evaluate(id=>localStorage.setItem('agentworks-session',id),session.id);await page.reload();
  await expect(page.locator('#conversation')).toBeVisible();
  await expect(page.locator('#terminal-surface .xterm')).toHaveCount(0);
  await page.getByRole('tab',{name:'Terminal',exact:true}).click();
  await expect(page.locator('#terminal-status')).toHaveText('Shell running');
  const input=async command=>{await page.locator('#terminal-surface .xterm-helper-textarea').focus();await page.keyboard.insertText(command);await page.keyboard.press('Enter');};
  const output=async text=>expect.poll(()=>frames.some(frame=>frame.data?.includes(text))).toBe(true);
  await input(`export AW_TERMINAL_PERSIST=kept; printf '%s' $$ > ${kind}.pid; printf 'opened-%s\\n' '${kind}'`);
  await output(`opened-${kind}`);
  const pid=await readFile(join(fixture,kind+'.pid'),'utf8');expect(exists(pid)).toBe(true);
  await input("printf 'browser:%s ci:%s pager:%s term:%s\\n' \"${NO_BROWSER-unset}\" \"${CODEX_CI-unset}\" \"${PAGER-unset}\" \"$TERM\"");
  await output('browser:1 ci:unset pager:unset term:xterm-256color');
  await input("printf '\\033[38;2;220;30;40mRGB-CHECK\\033[0m \\033[48;2;10;80;170mBG-CHECK\\033[0m\\n'");
  await expect(page.locator('.xterm-rows span').filter({hasText:'RGB-CHECK'}).last()).toHaveCSS('color','rgb(220, 30, 40)');
  await expect(page.locator('.xterm-rows span').filter({hasText:'BG-CHECK'}).last()).toHaveCSS('background-color','rgb(10, 80, 170)');
  await input("mkdir -p ls-color-check; LS_COLORS='di=38;5;196' command ls --color=always -d ls-color-check");
  await expect(page.locator('.xterm-rows span').filter({hasText:/^ls-color-check$/}).last()).toHaveCSS('color','rgb(255, 0, 0)');
  await input('python3 query.py');await output('query-reply:');
  expect(frames.some(frame=>frame.data?.includes("query-reply:b'\\x1b["))).toBe(true);
  await input(`sleep 1000 & printf '%s' $! > ${kind}.job`);
  await waitFor(()=>readFile(join(fixture,kind+'.job'),'utf8').catch(()=>false));
  await page.getByRole('tab',{name:'Chat',exact:true}).click();await expect(page.locator('#messages')).toBeVisible();expect(exists(pid)).toBe(true);
  // Browser reload drops the socket; reopening attaches to the same shell.
  await page.reload();await page.getByRole('tab',{name:'Terminal',exact:true}).click();await expect(page.locator('#terminal-status')).toHaveText('Shell running');
  await input(`printf 'retained-%s\\n' "$AW_TERMINAL_PERSIST"; printf '%s' $$ > ${kind}.reconnected`);await output('retained-kept');
  expect(await readFile(join(fixture,kind+'.reconnected'),'utf8')).toBe(pid);
  const oldSize=frames.at(-1).cols;await page.setViewportSize({width:900,height:720});
  await expect.poll(()=>frames.at(-1)?.cols).not.toBe(oldSize);
  // Escape reaches shell applications and doesn't switch away from the view.
  await page.keyboard.press('Escape');await expect(page.locator('#session-terminal')).toBeVisible();
  const beforeCancel=frames.at(-1)?.seq||0;await page.keyboard.press('Control+c');
  // Wait for the shell's cancel/redraw before pasting: SIGINT flushes queued
  // PTY input, including a command pasted before readline handles the signal.
  await expect.poll(()=>frames.filter(frame=>frame.seq>beforeCancel).map(frame=>frame.data||'').join('')).toMatch(/\^C[\s\S]*terminal-fixture> /);
  await input("printf 'final-%s\\n' output; exit 7");
  await expect(page.locator('#terminal-status')).toHaveText('Shell exited (7)');await output('final-output');
  const job=await readFile(join(fixture,kind+'.job'),'utf8');await expect.poll(()=>exists(job)).toBe(false);
  await page.getByRole('button',{name:'Start new terminal'}).click();await expect(page.locator('#terminal-status')).toHaveText('Shell running');
  await input(`printf '%s' $$ > ${kind}.new`);await waitFor(()=>readFile(join(fixture,kind+'.new'),'utf8').catch(()=>false));expect(await readFile(join(fixture,kind+'.new'),'utf8')).not.toBe(pid);
  expect(errors).toEqual([]);
});

async function shellInput(id,data,until,scope='sessions'){
  await new Promise((resolve,reject)=>{
    const ws=new WebSocket(base.replace('http','ws')+`/api/${scope}/${id}/terminal/events`,{origin:base});
    const timer=setTimeout(()=>{ws.terminate();reject(Error('Terminal output timed out: '+until));},10000);
    ws.on('open',()=>ws.send(JSON.stringify({type:'input',data})));
    ws.on('message',bytes=>{const frame=JSON.parse(bytes);if(frame.data?.includes(until)){clearTimeout(timer);ws.close();resolve();}});
    ws.on('error',error=>{clearTimeout(timer);reject(error);});
  });
}
for(const kind of ['local','remote'])test(`${kind}: scrollback survives reconnect and stays put during live output`,async({page})=>{
  const session=kind==='local'?local:remote;
  const errors=[];page.on('pageerror',error=>errors.push(error.message));
  await page.route('**/terminal.js',async route=>{const response=await route.fetch();const source=await response.text();await route.fulfill({response,body:source.replace("terminal.open($('terminal-surface'));","terminal.open($('terminal-surface'));window.__terminal=terminal;")});});
  await page.goto(base);await page.evaluate(id=>localStorage.setItem('agentworks-session',id),session.id);await page.reload();
  await page.getByRole('tab',{name:'Terminal',exact:true}).click();await expect(page.locator('#terminal-status')).toHaveText('Shell running');
  const screen=page.locator('#terminal-surface .xterm-rows');
  await shellInput(session.id,"for i in $(seq 1 150); do printf '\\033[31mscroll-%03d 界\\033[0m\\n' $i; done\n",'scroll-150');
  await expect(screen).toContainText('scroll-150');
  const top=async()=>{await page.locator('#terminal-surface').hover();await page.mouse.wheel(0,-800);await page.locator('#terminal-surface .xterm-helper-textarea').focus();for(let i=0;i<10;i++)await page.keyboard.press('Shift+PageUp');};
  await top();await expect(screen).toContainText('scroll-001');
  const copied=await page.evaluate(()=>{const terminal=window.__terminal,buffer=terminal.buffer.normal;let row=0;while(row<buffer.length&&!buffer.getLine(row).translateToString(true).startsWith('scroll-001'))row++;terminal.select(0,row,13);const copy=terminal.getSelection();terminal.clearSelection();return copy;});
  expect(copied).toBe('scroll-001 界');
  const old=await screen.innerText();
  await shellInput(session.id,"for i in $(seq 1 5); do printf 'live-%03d\\n' $i; done\n",'live-005');
  await expect.poll(()=>screen.innerText()).toBe(old);
  await page.reload();await page.getByRole('tab',{name:'Terminal',exact:true}).click();
  await expect(screen).toContainText('live-005');await top();await expect(screen).toContainText('scroll-001');
  await shellInput(session.id,"printf '\\033[?1049hALT-%s\\n' active\n",'ALT-active');
  await expect(screen).toContainText('ALT-active');await expect(screen).not.toContainText('scroll-001');
  expect(await page.evaluate(()=>window.__terminal.buffer.active.type)).toBe('alternate');
  await shellInput(session.id,"printf '\\033[?1049lRETURN-%s\\n' normal\n",'RETURN-normal');
  await expect(screen).toContainText('RETURN-normal');await top();await expect(screen).toContainText('scroll-001');
  expect(errors).toEqual([]);
});

async function socketStatus(path,origin=base){
  return new Promise((resolve,reject)=>{const ws=new WebSocket(base.replace('http','ws')+path,{origin});ws.on('unexpected-response',(_,res)=>{resolve(res.statusCode);res.resume();ws.terminate();});ws.on('error',()=>{});ws.on('open',()=>{ws.close();reject(Error('Expected socket rejection'));});});
}
for(const kind of ['local','remote'])test(`${kind}: host terminal is lazy, independent, persistent, and stoppable`,async({page})=>{
  const id=kind==='local'?'local':host.id,name=kind==='local'?'Local':'Terminal host';
  expect(await socketStatus(`/api/hosts/${id}/terminal/events`)).toBe(404);
  const errors=[];page.on('pageerror',error=>errors.push(error.message));
  if(kind==='remote')await page.setViewportSize({width:390,height:844});
  await page.goto(base);
  if(kind==='remote'){await page.locator('#menu').click();await expect(page.locator('#new-session')).toBeInViewport();}
  await page.getByRole('tab',{name:'Hosts',exact:true}).click();
  expect(await socketStatus(`/api/hosts/${id}/terminal/events`)).toBe(404);
  await page.getByRole('button',{name:'Open terminal on '+name,exact:true}).click();
  await expect(page.locator('#host-terminal-status')).toHaveText('Shell running');
  await expect(page.locator('#host-terminal-title')).toHaveText(name+' · Terminal');
  await expect(page.locator('#conversation')).toBeHidden();
  const input=async command=>{await page.locator('#host-terminal-surface .xterm-helper-textarea').focus();await page.keyboard.insertText(command);await page.keyboard.press('Enter');};
  await input(`pwd > ${fixture}/host-${kind}.cwd; printf '%s' $$ > ${fixture}/host-${kind}.pid; export AW_HOST_PERSIST=kept; sleep 1000 & printf '%s' $! > ${fixture}/host-${kind}.job; for i in $(seq 1 120); do echo host-history-$i; done; printf 'host-%s\\n' ready`);
  await expect(page.locator('#host-terminal-surface .xterm-rows')).toContainText('host-ready');
  expect((await readFile(join(fixture,`host-${kind}.cwd`),'utf8')).trim()).toBe(join(fixture,'home'));
  const pid=await readFile(join(fixture,`host-${kind}.pid`),'utf8'),job=await readFile(join(fixture,`host-${kind}.job`),'utf8');
  expect(exists(pid)).toBe(true);
  // Reopening is idempotent, while replacing a running shell is rejected.
  await api(`/api/hosts/${id}/terminal`,{});
  const conflict=await fetch(base+`/api/hosts/${id}/terminal`,{method:'POST',headers:{Origin:base,'Content-Type':'application/json'},body:'{"restart":true}'});expect(conflict.status).toBe(409);
  await page.getByRole('button',{name:'Back to sessions',exact:true}).click();expect(exists(pid)).toBe(true);
  // The session shell keeps its own state and is not the host shell.
  const session=kind==='local'?local:remote;
  await api(`/api/sessions/${session.id}/terminal`,{});
  await shellInput(session.id,`printf '%s' $$ > ${fixture}/host-${kind}.session; printf 'session-%s\\n' independent\n`,'session-independent');
  expect(await readFile(join(fixture,`host-${kind}.session`),'utf8')).not.toBe(pid);
  await page.getByRole('button',{name:'Open terminal on '+name,exact:true}).click();
  await page.reload();await expect(page.locator('#host-terminal-status')).toHaveText('Shell running');
  await input(`printf '%s' $$ > ${fixture}/host-${kind}.again; printf 'host-retained-%s\\n' "$AW_HOST_PERSIST"`);
  await expect(page.locator('#host-terminal-surface .xterm-rows')).toContainText('host-retained-kept');
  expect(await readFile(join(fixture,`host-${kind}.again`),'utf8')).toBe(pid);
  await page.locator('#host-terminal-surface .xterm-helper-textarea').focus();for(let i=0;i<10;i++)await page.keyboard.press('Shift+PageUp');
  await expect(page.locator('#host-terminal-surface .xterm-rows')).toContainText('host-history-1');
  expect(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBe(true);
  await page.getByRole('button',{name:'Stop terminal',exact:true}).click();
  await expect(page.locator('#host-terminal-status')).toHaveText('Terminal stopped');
  await expect.poll(()=>exists(pid)||exists(job)).toBe(false);
  await expect(page.locator('#host-terminal-surface .xterm-rows')).toContainText('host-history-1');
  expect(exists(await readFile(join(fixture,`host-${kind}.session`),'utf8'))).toBe(true);
  await page.getByRole('button',{name:'Start new terminal',exact:true}).click();await expect(page.locator('#host-terminal-status')).toHaveText('Shell running');
  await input(`printf '%s' $$ > ${fixture}/host-${kind}.new; printf 'host-new-%s\\n' ready`);
  await expect(page.locator('#host-terminal-surface .xterm-rows')).toContainText('host-new-ready');
  expect(await readFile(join(fixture,`host-${kind}.new`),'utf8')).not.toBe(pid);
  expect(await socketStatus(`/api/hosts/${id}/terminal/events`,'https://untrusted.example')).toBe(403);
  expect(errors).toEqual([]);
});

test('host terminals need no sessions or roots and stop when their host is removed',async()=>{
  const h=await api('/api/hosts',{name:'Standalone host',destination:'standalone-box',command:resolve('target/debug/agentworks')});
  await waitFor(async()=>((await api('/api/catalog')).hosts.find(host=>host.id===h.id)?.status==='connected'));
  await api(`/api/hosts/${h.id}/terminal`,{});
  await shellInput(h.id,`printf '%s' $$ > ${fixture}/standalone.pid; printf 'standalone-%s\\n' ready\n`,'standalone-ready','hosts');
  const pid=await readFile(join(fixture,'standalone.pid'),'utf8');expect(exists(pid)).toBe(true);
  await api(`/api/hosts/${h.id}`,undefined,'DELETE');await expect.poll(()=>exists(pid)).toBe(false);
  expect(await socketStatus(`/api/hosts/${h.id}/terminal/events`)).toBe(404);
});

for(const kind of ['local','remote'])test(`${kind}: archive releases shells, saves history, and deletion cleans up`,async({page})=>{
  const session=await api('/api/sessions',{provider:'demo',cwd:fixture,title:'Archive '+kind,...(kind==='remote'?{hostId:host.id}:{})});
  await api(`/api/sessions/${session.id}/terminal`,{});
  await shellInput(session.id,`printf '%s' $$ > archive-${kind}.pid; sleep 1000 & printf '%s' $! > archive-${kind}.job; for i in $(seq 1 100); do echo saved-$i; done; printf 'archive-%s\\n' ready\n`,'archive-ready');
  const pid=await readFile(join(fixture,`archive-${kind}.pid`),'utf8'),job=await readFile(join(fixture,`archive-${kind}.job`),'utf8');
  await api(`/api/sessions/${session.id}`,{archived:true},'PATCH');
  await expect.poll(()=>exists(pid)||exists(job)).toBe(false);
  await page.goto(base);await page.evaluate(id=>localStorage.setItem('agentworks-session',id),session.id);await page.reload();
  await page.getByRole('tab',{name:'Terminal',exact:true}).click();
  await expect(page.locator('#terminal-status')).toHaveText('Archived · saved terminal history');
  await expect(page.locator('.xterm-rows')).toContainText('archive-ready');
  await expect(page.getByRole('button',{name:'Start new terminal'})).toBeHidden();
  const denied=await fetch(base+`/api/sessions/${session.id}/terminal`,{method:'POST',headers:{Origin:base,'Content-Type':'application/json'},body:'{"restart":true}'});expect(denied.status).toBe(409);
  archivedSessions.push(session);
  // A different session exercises unarchive/restart and deleting a live PTY.
  const deleted=await api('/api/sessions',{provider:'demo',cwd:fixture,title:'Delete '+kind,...(kind==='remote'?{hostId:host.id}:{})});
  await api(`/api/sessions/${deleted.id}/terminal`,{});
  await shellInput(deleted.id,`printf '%s' $$ > delete-${kind}.pid; printf 'delete-%s\\n' ready\n`,'delete-ready');
  const dead=await readFile(join(fixture,`delete-${kind}.pid`),'utf8');
  await api(`/api/sessions/${deleted.id}`,{archived:true},'PATCH');
  await api(`/api/sessions/${deleted.id}`,{archived:false},'PATCH');
  await api(`/api/sessions/${deleted.id}/terminal`,{});expect(exists(dead)).toBe(false);
  await api(`/api/sessions/${deleted.id}/terminal`,{restart:true});
  await shellInput(deleted.id,`printf '%s' $$ > delete-${kind}.new; printf 'delete-%s\\n' restarted\n`,'delete-restarted');
  const fresh=await readFile(join(fixture,`delete-${kind}.new`),'utf8');expect(fresh).not.toBe(dead);
  await api(`/api/sessions/${deleted.id}`,undefined,'DELETE');await expect.poll(()=>exists(fresh)).toBe(false);
  expect((await fetch(base+`/api/sessions/${deleted.id}/terminal`,{method:'POST',headers:{Origin:base,'Content-Type':'application/json'},body:'{}'})).status).toBe(404);
});

test('archived local and remote history remains viewable after service restart',async({page})=>{
  const hostPids=[];for(const [kind,id] of [['local','local'],['remote',host.id]]){await api(`/api/hosts/${id}/terminal`,{});await shellInput(id,`printf '%s' $$ > ${fixture}/host-${kind}.shutdown; printf 'host-shutdown-%s\\n' ready\n`,'host-shutdown-ready','hosts');hostPids.push(await readFile(join(fixture,`host-${kind}.shutdown`),'utf8'));}
  await stopChild(server,{signal:'SIGINT'});for(const pid of hostPids)await expect.poll(()=>exists(pid)).toBe(false);await startServer();
  await waitFor(async()=>((await api('/api/catalog')).hosts.find(h=>h.id===host.id)?.status==='connected'));
  for(const session of archivedSessions){
    await page.goto(base);await page.evaluate(id=>localStorage.setItem('agentworks-session',id),session.id);await page.reload();
    await page.getByRole('tab',{name:'Terminal',exact:true}).click();
    await expect(page.locator('#terminal-status')).toHaveText('Archived · saved terminal history');
    await expect(page.locator('.xterm-rows')).toContainText('archive-ready');
    await page.locator('#terminal-surface .xterm-helper-textarea').focus();for(let i=0;i<10;i++)await page.keyboard.press('Shift+PageUp');
    await expect(page.locator('.xterm-rows')).toContainText('saved-1');
  }
});

test('terminal WebSocket rejects cross-origin clients',async()=>{
  await api(`/api/sessions/${local.id}/terminal`,{});
  const status=await new Promise((resolve,reject)=>{const ws=new WebSocket(base.replace('http','ws')+`/api/sessions/${local.id}/terminal/events`,{origin:'https://untrusted.example'});ws.on('unexpected-response',(_,res)=>{resolve(res.statusCode);res.resume();ws.terminate();});ws.on('error',()=>{});ws.on('open',()=>{ws.close();reject(Error('Cross-origin socket accepted'));});});
  expect(status).toBe(403);
});

test('service shutdown ends its local and remote shells',async()=>{
  const pids=[];for(const [kind,session] of [['local',local],['remote',remote]]){await api(`/api/sessions/${session.id}/terminal`,{});await shellInput(session.id,`printf '%s' $$ > ${kind}.shutdown; printf 'shutdown-%s\\n' ready\n`,'shutdown-ready');pids.push(await readFile(join(fixture,kind+'.shutdown'),'utf8'));}
  await stopChild(server,{signal:'SIGINT'});
  expect(server.exitCode).toBe(0);
  for(const pid of pids)await expect.poll(()=>exists(pid)).toBe(false);
});
