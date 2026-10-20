import {appUrl, socketUrl, deploymentKey} from './hosting.js';
// U-02 browser notifications.
//
// The server applies policy and only sends noteworthy, allowed events; this
// module owns everything a browser must do well: asking for permission from a
// real user action, suppressing noise, never repeating a notice across tabs or
// reconnects, and opening the session that produced it.
const CATEGORIES=['turnEnd','approval','error'];
const RECONNECT_MS=1000;

// A unique IndexedDB key is an atomic claim shared by all tabs, including when
// Web Locks or BroadcastChannel are unavailable. Commit before displaying, so a
// crash can lose a notice but cannot duplicate it. Keep the newest 1000 claims.
function claimStore(){
  let database;
  return async id=>{
    try{
      database??=new Promise((resolve,reject)=>{
        const request=indexedDB.open(deploymentKey('agentworks-notifications'),1);
        request.onupgradeneeded=()=>request.result.createObjectStore('shown',{autoIncrement:true}).createIndex('id','id',{unique:true});
        request.onsuccess=()=>resolve(request.result);
        request.onerror=()=>reject(request.error);
      });
      const db=await database;
      return await new Promise(resolve=>{
        const transaction=db.transaction('shown','readwrite'),store=transaction.objectStore('shown');
        store.add({id});
        const count=store.count();
        count.onsuccess=()=>{
          let excess=count.result-1000;
          if(excess>0){const cursor=store.openCursor();cursor.onsuccess=()=>{if(cursor.result&&excess-->0){cursor.result.delete();cursor.result.continue();}};}
        };
        transaction.oncomplete=()=>resolve(true);
        transaction.onabort=()=>resolve(false);
        transaction.onerror=()=>{};
      });
    }catch{return false;} // No shared storage: avoid duplicate notices.
  };
}

// Support is an origin property, not a permission one. A browser can support
// notifications and still refuse them, and an insecure origin has neither.
export function support(){
  if(!globalThis.isSecureContext)return {ok:false,reason:'Notifications need a secure (HTTPS or localhost) origin.'};
  if(typeof Notification==='undefined')return {ok:false,reason:'This browser does not show notifications.'};
  return {ok:true,reason:''};
}

export function describePermission(permission){
  const state=support();
  if(!state.ok)return state.reason;
  return {granted:'Allowed in this browser.',denied:'Blocked for this site. Allow notifications in the browser address bar to enable them.',
    default:'Permission is not granted. Use Allow notifications to ask this browser.',unsupported:state.reason}[permission]||'Unknown permission state.';
}

// An event is worth a notice when the person is not already watching that
// session in a focused tab. Watching it live is the whole point of the app.
function shouldNotify(event,{permission,activeId,focused}){
  if(permission!=='granted')return false;
  if(event.sessionId===activeId&&focused)return false;
  return true;
}

export function setupNotifications({getSessions,getActive,selectSession,toast}={}){
  const claim=claimStore();
  let settings=null,socket=null,delay=RECONNECT_MS,generation=0,statusNode=null;
  function connectionStatus(text){if(statusNode)statusNode.textContent=text;}

  function show(event){
    const title=event.title||'Agentworks';
    let notice;
    try{
      notice=new Notification(title,{body:event.body||'',tag:event.id,renotify:false});
    }catch{toast?.('This browser could not display a notification.');return;}
    notice.onerror=()=>toast?.('This browser could not display a notification. Check browser and OS notification settings.');
    notice.onclick=()=>{
      try{globalThis.focus();}catch{}
      notice.close();
      const session=(getSessions?.()||[]).find(s=>s.id===event.sessionId);
      if(session)selectSession(session);
      else fetch(appUrl(`/api/sessions/${encodeURIComponent(event.sessionId)}`)).then(r=>r.ok?r.json():null).then(data=>{if(data?.session)selectSession(data.session);}).catch(()=>toast?.('Could not open the source session.'));
    };
    return notice;
  }

  function connect(){
    if(!support().ok)return;
    const gen=++generation;socket?.close();
    connectionStatus('Connecting to notification events…');
    const url=socketUrl('/api/notifications/events');
    const ws=new WebSocket(url);socket=ws;
    ws.onopen=()=>{if(gen!==generation)return;delay=RECONNECT_MS;connectionStatus('Notification event stream connected.');};
    ws.onmessage=async message=>{
      if(gen!==generation)return;
      let event;try{event=JSON.parse(message.data);}catch{return;}
      if(!CATEGORIES.includes(event.event)||typeof event.id!=='string'||typeof event.sessionId!=='string')return;
      if(!shouldNotify(event,{permission:Notification.permission,activeId:getActive?.()?.id,focused:document.hasFocus()}))return;
      if(!await claim(event.id))return;
      if(gen!==generation||!shouldNotify(event,{permission:Notification.permission,activeId:getActive?.()?.id,focused:document.hasFocus()}))return;
      show(event);
    };
    ws.onclose=()=>{
      if(gen!==generation)return;
      connectionStatus('Notification events disconnected. Retrying…');
      generation++;
      setTimeout(connect,delay);delay=Math.min(15000,delay*2);
    };
    ws.onerror=()=>{try{ws.close();}catch{}};
  }

  async function load(){
    const response=await fetch(appUrl('/api/notifications'),{headers:{'Accept':'application/json'}});
    if(!response.ok)throw new Error(await response.text());
    const data=await response.json();
    settings=data.settings;
    return data;
  }
  async function save(next){
    const response=await fetch(appUrl('/api/notifications'),{method:'PUT',headers:{'Content-Type':'application/json'},body:JSON.stringify(next)});
    if(!response.ok)throw new Error(await response.text());
    settings=(await response.json()).settings;return settings;
  }
  // Permission must follow a user gesture; browsers reject it otherwise.
  async function request(){
    const state=support();
    if(!state.ok)return state.reason;
    if(Notification.permission!=='default')return describePermission(Notification.permission);
    try{return describePermission(await Notification.requestPermission());}
    catch(error){return 'Could not ask for permission: '+error.message;}
  }

  function mount(fieldset,error){
    if(!fieldset)return;
    const state=support();
    fieldset.replaceChildren();
    const legend=document.createElement('legend');legend.textContent='Notifications';fieldset.append(legend);
    if(!state.ok){
      const note=document.createElement('p');note.className='field-note';note.setAttribute('role','status');note.textContent=state.reason;fieldset.append(note);return;
    }
    const note=document.createElement('p');note.className='field-note';note.setAttribute('role','status');note.textContent=error?'Could not load notification settings: '+error.message:'Saved policy and browser permission are separate. Changes here save immediately.';fieldset.append(note);
    statusNode=document.createElement('p');statusNode.className='field-note';statusNode.setAttribute('role','status');fieldset.append(statusNode);
    connectionStatus(socket?.readyState===WebSocket.OPEN?'Notification event stream connected.':socket?'Notification events disconnected. Retrying…':'Notification event stream not connected. Reload the page to retry.');
    const row=document.createElement('label');row.className='workspace-check';
    const toggle=document.createElement('input');toggle.type='checkbox';toggle.id='notifications-enabled';
    toggle.checked=!!settings?.enabled;toggle.disabled=!!error;toggle.onchange=async()=>{
      toggle.disabled=true;
      const enabled=toggle.checked;
      try{await save({...settings,enabled});note.textContent=enabled?'Notifications are on for every project without its own rule.':'Notifications are off.';}
      catch(error){toggle.checked=!enabled;note.textContent='Not saved: '+error.message;}
      finally{toggle.disabled=false;}
    };
    row.append(toggle,document.createTextNode('Notify me about agent activity'));fieldset.append(row);
    const permission=document.createElement('div');fieldset.append(permission);renderPermission(permission);
    if(!settings?.enabled){
      const hint=document.createElement('p');hint.className='field-note';hint.textContent='Turn this on to be told about turn completion, approvals and failures from any session.';fieldset.append(hint);
    }
  }
  function renderPermission(node){
    node.replaceChildren();
    const text=describePermission(Notification.permission);
    const label=document.createElement('span');label.className='field-note';label.textContent=text;node.append(label);
    if(Notification.permission==='default'){
      const allow=document.createElement('button');allow.type='button';allow.className='text-button';allow.textContent='Allow notifications in this browser';
      allow.onclick=async()=>{const pending=request();allow.disabled=true;const result=await pending;renderPermission(node);if(Notification.permission==='default')node.querySelector('span').textContent=result;};
      node.append(allow);
    }
    if(Notification.permission==='granted'){
      const test=document.createElement('button');test.type='button';test.className='text-button';test.textContent='Send a test notification';
      test.onclick=()=>show({id:'test',sessionId:'test',title:'Agentworks',body:'Notifications are working.'});node.append(test);
    }
  }

  async function start(){
    try{
      await load();
      connect();
    }catch{}
  }

  return {start,load,save,mount,request,support,connect,
    settings:()=>({...settings}),connected:()=>socket?.readyState===WebSocket.OPEN};
}
