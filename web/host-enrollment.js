// Lazy-load the terminal so ordinary sessions do not pay its startup cost.
export function hostEnrollment({$, api, refresh, directories}) {
  let socket=null, terminal=null, resize=null, active=false, finished=false, enrolledHost=null;
  const dialog=$('host-dialog');
  function send(value){if(socket?.readyState===WebSocket.OPEN)socket.send(JSON.stringify(value));}
  function dispose(){if(socket){socket.onclose=null;socket.onmessage=null;socket.onerror=null;}socket?.close();socket=null;resize?.disconnect();resize=null;terminal?.dispose();terminal=null;active=false;}
  function controls(busy){
    active=busy;
    for(const id of ['host-name','host-destination','host-mode','host-command','save-host'])$(id).disabled=busy;
    $('dismiss-host').setAttribute('aria-label',busy?'Cancel enrollment':'Close host dialog');
    $('dismiss-host').title=busy?'Cancel enrollment':'Close';
  }
  function mode(){
    const automatic=$('host-mode').value==='enroll';
    $('host-manual').hidden=automatic;$('host-enrollment-note').hidden=!automatic;
    $('save-host').textContent=automatic?'Connect and install':'Add host';
  }
  $('host-mode').onchange=mode;
  dialog.addEventListener('close',()=>{dispose();controls(false);refresh();});
  $('dismiss-host').onclick=()=>dialog.close();
  async function open(){
    dispose();finished=false;enrolledHost=null;controls(false);$('host-form').reset();mode();
    $('host-terminal-wrap').hidden=true;$('host-terminal').replaceChildren();
    $('host-error').textContent='';$('host-stage').textContent='';
    $('host-done').hidden=true;$('save-host').hidden=false;
    dialog.showModal();$('host-name').focus();
  }
  $('host-done').onclick=()=>{const host=enrolledHost;dialog.close();if(host)directories(host);};
  $('host-form').onsubmit=async event=>{
    event.preventDefault();if(active)return;
    dispose();$('host-error').textContent='';finished=false;controls(true);
    const name=$('host-name').value,destination=$('host-destination').value;
    if($('host-mode').value==='manual'){
      try{await api('/api/hosts',{method:'POST',body:JSON.stringify({name,destination,command:$('host-command').value})});await refresh();dialog.close();}
      catch(error){$('host-error').textContent=error.message;}finally{controls(false);}return;
    }
    try{
      const {Terminal}=await import('./vendor/xterm.mjs');
      if(!dialog.open){controls(false);return;}
      terminal?.dispose();$('host-terminal').replaceChildren();
      $('host-terminal-wrap').hidden=false;$('host-done').hidden=true;
      terminal=new Terminal({fontSize:13,fontFamily:'ui-monospace, SFMono-Regular, Menlo, monospace',rows:18,cols:80,scrollback:1500,theme:{background:'#101218',foreground:'#e2e8f0'},allowProposedApi:false});
      terminal.open($('host-terminal'));
      const fit=()=>{if(!terminal)return;const cols=Math.max(20,Math.min(120,Math.floor(($('host-terminal').clientWidth-20)/7.85)));terminal.resize(cols,18);send({type:'resize',cols,rows:18});};
      resize?.disconnect();resize=new ResizeObserver(fit);resize.observe($('host-terminal'));fit();
      const url=new URL('./api/hosts/enroll/events',location.href);url.protocol=location.protocol==='https:'?'wss:':'ws:';
      const ws=socket=new WebSocket(url);ws.binaryType='arraybuffer';
      terminal.onData(data=>{if(active)send({type:'input',data});});
      ws.onopen=()=>{send({name,destination});fit();terminal.focus();};
      ws.onmessage=event=>{
        if(event.data instanceof ArrayBuffer){terminal?.write(new Uint8Array(event.data));return;}
        let value;try{value=JSON.parse(event.data);}catch{return;}
        if(value.type==='stage')$('host-stage').textContent=value.message;
        if(value.type==='error'){$('host-error').textContent=value.message;$('host-stage').textContent='Enrollment needs attention';controls(false);ws.close();refresh();}
        if(value.type==='complete'){
          finished=true;enrolledHost=value.host;controls(false);$('save-host').hidden=true;$('host-done').hidden=false;$('host-done').textContent='Choose working directories';
          $('host-stage').textContent='Connected — '+(value.host.worker?.providers?.join(', ')||'worker ready');
          ws.close();refresh();
        }
      };
      ws.onerror=()=>{$('host-error').textContent='Could not connect to the enrollment terminal.';};
      ws.onclose=()=>{if(active&&!finished){controls(false);$('host-stage').textContent='Enrollment stopped';if(!$('host-error').textContent)$('host-error').textContent='Connection closed. You can retry enrollment.';}};
    }catch(error){$('host-error').textContent=error.message;controls(false);}
  };
  return {open};
}
