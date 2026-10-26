// The browser can disconnect freely; the service owns the shell and emulator.
export function setupTerminal({$,api,getActive,socketUrl,tabs=true,onSelect=()=>{},endpoint=id=>`/api/sessions/${id}/terminal`,eventsEndpoint=id=>endpoint(id)+'/events',runningStatus='Shell running',exitStatus=frame=>frame.code==null?'Terminal stopped':`Shell exited (${frame.code})`}) {
  let current=null, terminal=null, socket=null, timer=null, observer=null, generation=0, ready=false, exited=false;
  const selected=new Map();
  let renderQueue=[],writing=false,alternate=false,historyStart=0,historyEpoch=null;
  function send(value){if(socket?.readyState===WebSocket.OPEN)socket.send(JSON.stringify(value));}
  function status(text){$('terminal-status').textContent=text;}
  function dispose(){generation++;clearTimeout(timer);timer=null;observer?.disconnect();observer=null;if(socket){socket.onclose=null;socket.close();socket=null;}terminal?.dispose();terminal=null;ready=false;exited=false;if($('terminal-stop'))$('terminal-stop').disabled=true;renderQueue=[];writing=false;alternate=false;historyStart=0;historyEpoch=null;}
  function fit(){
    if(!terminal||exited||$('session-terminal').hidden||!$('terminal-surface').clientWidth)return;
    // Measure rendered cells so browser font metrics and line height fit the panel.
    const surface=$('terminal-surface'),probe=$('terminal-cell-measure'),rect=probe.getBoundingClientRect();
    const screen=surface.querySelector('.xterm-screen'),row=surface.querySelector('.xterm-rows>div');
    const cellWidth=screen?.clientWidth/terminal.cols||rect.width,cellHeight=row?.getBoundingClientRect().height||rect.height;
    const cols=Math.max(20,Math.min(300,Math.floor((surface.clientWidth-18)/cellWidth)));
    const rows=Math.max(5,Math.min(120,Math.floor((surface.clientHeight-4)/cellHeight)));
    if(terminal.cols!==cols||terminal.rows!==rows){terminal.resize(cols,rows);send({type:'resize',cols,rows});}
  }
  function renderNext(gen){
    if(writing||!terminal||gen!==generation||!renderQueue.length)return;
    const frame=renderQueue.shift(),view=terminal,history=frame.history;
    const buffer=view.buffer.active,atBottom=buffer.viewportY>=buffer.baseY;
    const anchor=buffer.viewportY+historyStart,oldEpoch=historyEpoch;
    let data='';
    // Keep xterm's 10,000-row capacity fixed. Shrinking it after a resize can
    // invalidate its reflowed screen rows; authoritative history resets use 3J.
    if(view.cols!==frame.cols||view.rows!==frame.rows)view.resize(frame.cols,frame.rows);
    if(history&&(history.reset||history.rows.length)){
      // Append authoritative rows to xterm's normal scroll buffer. Each LF
      // moves exactly one supplied row into history, then the live screen is
      // redrawn. No raw application output or query reaches the browser.
      if(alternate){data+='\x1b[?1047l';alternate=false;}
      data+='\x1b[?2026h\x1b[?25l\x1b[?7l\x1b[?6l\x1b[r';
      if(history.reset)data+='\x1b[3J\x1b[H\x1b[0m\x1b[2J';
      for(const row of history.rows)data+='\x1b[1;1H\x1b[0m\x1b[2K'+row+`\x1b[${frame.rows};1H\n`;
    }
    if(history){historyStart=history.start;historyEpoch=history.epoch;}
    if(Boolean(frame.alternate)!==alternate){alternate=Boolean(frame.alternate);data+=alternate?'\x1b[?1047h':'\x1b[?1047l';}
    data+=frame.data;
    writing=true;
    view.write(data,()=>{
      if(gen!==generation||terminal!==view)return;
      writing=false;
      if(!alternate){
        if(atBottom||oldEpoch===null||(oldEpoch!==historyEpoch&&!history?.rows.length))view.scrollToBottom();
        else view.scrollToLine(Math.max(0,anchor-historyStart));
      }
      if(exited&&atBottom)$('terminal-surface').scrollTop=$('terminal-surface').scrollHeight;
      send({type:'ack',seq:frame.seq});renderNext(gen);
    });
  }
  function connect(gen){
    if(gen!==generation||!terminal)return;
    status('Connecting…');
    const ws=socket=new WebSocket(socketUrl(eventsEndpoint(current)));
    ws.onopen=()=>{if(gen!==generation)return;ready=true;if($('terminal-stop'))$('terminal-stop').disabled=false;$('terminal-restart').hidden=true;send({type:'flowControl'});send({type:'resize',cols:terminal.cols,rows:terminal.rows});status(runningStatus);};
    ws.onmessage=event=>{
      if(gen!==generation)return;
      let frame;try{frame=JSON.parse(event.data);}catch{return;}
      if(frame.type!=='frame')return;
      if(frame.exited){exited=true;$('terminal-surface').classList.add('terminal-readonly');}
      renderQueue.push(frame);renderNext(gen);
      if(frame.exited){exited=true;ready=false;if($('terminal-stop'))$('terminal-stop').disabled=true;status(frame.error||(frame.archived?'Archived · saved terminal history':exitStatus(frame)));$('terminal-restart').hidden=Boolean(frame.archived);ws.onclose=null;ws.close();}
    };
    ws.onclose=()=>{if(gen!==generation)return;ready=false;status('Disconnected · reconnecting…');$('terminal-restart').hidden=false;timer=setTimeout(()=>connect(gen),1500);};
    ws.onerror=()=>ws.close();
  }
  async function start(restart=false){
    dispose();current=getActive()?.id;if(!current)return;
    const gen=generation;status('Starting shell…');$('terminal-restart').hidden=true;
    try{
      const {Terminal}=await import('./vendor/xterm.mjs');
      if(gen!==generation)return;
      const opened=await api(endpoint(current),{method:'POST',body:JSON.stringify({restart})});
      if(opened.available===false){status('No saved terminal for this archived session');return;}
      if(gen!==generation)return;
      $('terminal-surface').replaceChildren();$('terminal-surface').classList.remove('terminal-readonly');
      terminal=new Terminal({fontFamily:'ui-monospace, SFMono-Regular, Menlo, monospace',fontSize:14,lineHeight:1.2,scrollback:10000,cursorBlink:true,theme:{background:'#101218',foreground:'#e2e8f0'}});
      terminal.open($('terminal-surface'));
      terminal.onData(data=>{if(ready)send({type:'input',data});});
      terminal.onBinary(data=>{if(ready)send({type:'binary',data:Array.from(data,c=>c.charCodeAt(0)&255)});});
      observer=new ResizeObserver(fit);observer.observe($('terminal-surface'));fit();connect(gen);terminal.focus();
    }catch(error){if(gen!==generation)return;status(error.message);$('terminal-restart').hidden=Boolean(getActive()?.archived);}
  }
  function select(tab){
    const session=getActive();if(!session)return;
    selected.set(session.id,tab);
    const terminalView=tab==='terminal';
    $('session-chat').hidden=terminalView;$('session-terminal').hidden=!terminalView;
    for(const button of $('session-tabs')?.querySelectorAll('[role="tab"]')||[]){
      const view=button.dataset.sessionView||button.id.replace('session-tab-','');
      const active=view===tab;button.setAttribute('aria-selected',String(active));button.tabIndex=active?0:-1;
      if(active&&!terminalView)$('session-chat').setAttribute('aria-labelledby',button.id);
    }
    onSelect(tab);
    if(tab==='terminal'){if(current!==session.id||!terminal)start();else{requestAnimationFrame(fit);terminal.focus();}}
  }
  if(tabs){
    for(const tab of ['chat','terminal'])$('session-tab-'+tab).onclick=()=>select(tab);
    $('session-tabs').onkeydown=event=>{
      if(!['ArrowLeft','ArrowRight','Home','End'].includes(event.key))return;
      const buttons=[...$('session-tabs').querySelectorAll('[role="tab"]')];
      const current=buttons.indexOf(event.target);if(current<0)return;
      const index=event.key==='Home'?0:event.key==='End'?buttons.length-1:(current+(event.key==='ArrowRight'?1:-1)+buttons.length)%buttons.length;
      const button=buttons[index];select(button.dataset.sessionView||button.id.replace('session-tab-',''));button.focus();button.scrollIntoView({block:'nearest',inline:'nearest'});event.preventDefault();
    };
  }
  $('terminal-restart').onclick=()=>start(true);
  if($('terminal-stop'))$('terminal-stop').onclick=async()=>{const id=current;ready=false;$('terminal-stop').disabled=true;try{await api(endpoint(id),{method:'DELETE'});}catch(error){if(current===id){status(error.message);$('terminal-stop').disabled=false;ready=!exited;}}};
  // Escape belongs to terminal applications (vim, less, shell line editing).
  $('session-terminal').addEventListener('keydown',event=>event.stopPropagation());
  addEventListener('pagehide',dispose);
  return {select,open(){if(current!==getActive()?.id||!terminal)start();else{requestAnimationFrame(fit);terminal.focus();}},sessionUpdated(){if(exited){const archived=Boolean(getActive()?.archived);$('terminal-restart').hidden=archived;if(archived)status('Archived · saved terminal history');else status('Terminal stopped');}},sessionChanged(){const session=getActive();if(session?.id!==current){dispose();current=null;}select(selected.get(session?.id)||'chat');}};
}
