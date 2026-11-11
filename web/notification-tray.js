import {appUrl, socketUrl} from './hosting.js';

const labels={approval:'Approval needed',error:'Turn failed',turnEnd:'Turn finished',taskCompleted:'Task completed',taskBlocked:'Task blocked',automationIssue:'Automation needs attention'};
export function notificationUrl(alert){
  if(alert.automationId)return appUrl('/?'+new URLSearchParams({notificationAutomation:alert.automationId}));
  return appUrl('/?'+new URLSearchParams({notificationSession:alert.sessionId,notificationSeq:String(alert.seq),notificationEvent:alert.event}));
}
const element=(tag,className,text)=>{
  const node=document.createElement(tag);if(className)node.className=className;if(text!==undefined)node.textContent=text;return node;
};
const attention=alert=>!alert.resolved&&['approval','error','taskBlocked','automationIssue'].includes(alert.event);

export function setupNotificationTray({openAlert}={}){
  const nav=document.querySelector('#workspace-nav');if(!nav)return;
  const bell=element('button','workspace-nav-item notification-bell');bell.type='button';bell.id='nav-notifications';
  bell.innerHTML='<span aria-hidden="true"></span>Notifications';
  // An inline bell keeps the control independent of icon fonts.
  bell.firstChild.innerHTML='<svg viewBox="0 0 24 24" width="21" height="21" fill="none" stroke="currentColor" stroke-width="1.7"><path d="M18 8a6 6 0 0 0-12 0c0 7-3 7-3 9h18c0-2-3-2-3-9Z"/><path d="M10 21h4"/></svg>';
  const badge=element('b','notification-count');badge.hidden=true;badge.setAttribute('aria-hidden','true');bell.append(badge);
  bell.setAttribute('aria-haspopup','dialog');bell.setAttribute('aria-controls','notification-tray');bell.setAttribute('aria-expanded','false');
  nav.insertBefore(bell,document.querySelector('#nav-settings')||nav.querySelector('.nav-bottom'));
  const bells=[bell],badges=[badge];
  const sessionHeader=document.querySelector('#conversation .session-bar');
  if(sessionHeader){
    const compact=bell.cloneNode(false);compact.id='session-notifications';compact.className='icon-button notification-bell session-notification-bell';
    const count=badge.cloneNode(true);compact.append(bell.firstChild.cloneNode(true),count);
    sessionHeader.insertBefore(compact,document.querySelector('#session-search'));
    bells.push(compact);badges.push(count);
  }
  let opener=bell;

  const tray=element('dialog','notification-tray');tray.id='notification-tray';tray.setAttribute('aria-labelledby','notification-title');
  const head=element('div','notification-head'),heading=element('h2','', 'Notifications');heading.id='notification-title';
  const close=element('button','notification-close','×');close.type='button';close.setAttribute('aria-label','Close notifications');
  head.append(heading,close);
  const toolbar=element('div','notification-toolbar'),description=element('p','','Alerts stay here until you dismiss them.'),dismissAll=element('button','','Dismiss all');dismissAll.type='button';toolbar.append(description,dismissAll);
  const status=element('p','notification-status');status.setAttribute('role','status');
  const error=element('div','notification-error');error.setAttribute('role','alert');
  const retry=element('button','','Retry');retry.type='button';retry.hidden=true;
  const undoBar=element('div','notification-undo'),undoText=element('span'),undo=element('button','','Undo');undo.type='button';undoBar.hidden=true;undoBar.append(undoText,undo);
  const list=element('div','notification-list');tray.append(head,toolbar,status,error,retry,undoBar,list);document.body.append(tray);
  let alerts=[],loaded=false,busy=false,lastDismissed=[],request=0,socket,reconnect=1000,connection='Connecting…',navigating=false;
  async function read(path,options){
    const response=await fetch(appUrl(path),{cache:'no-store',...options});
    if(!response.ok)throw new Error('Could not '+(options?'save changes.':'load notifications.'));
    return response.json();
  }
  function render(){
    for(const count of badges){count.hidden=!alerts.length;count.textContent=alerts.length>99?'99+':String(alerts.length);}
    for(const trigger of bells)trigger.setAttribute('aria-label',`Notifications${alerts.length?`, ${alerts.length} undismissed`:''}`);
    dismissAll.disabled=busy||!alerts.length;undo.disabled=busy;
    status.textContent=loaded?`${alerts.length} undismissed · ${connection}`:'Loading notifications…';
    if(!tray.open)return;
    // Preserve keyboard focus when a live update replaces the list.
    const focusId=document.activeElement?.dataset?.alertId,focusAction=document.activeElement?.dataset?.action;
    list.replaceChildren();
    if(loaded&&!alerts.length)list.append(element('p','notification-empty','You’re all caught up. New alerts will appear here.'));
    for(const [title,items] of [['Needs attention',alerts.filter(attention)],['Recent activity',alerts.filter(a=>!attention(a))]]){
      if(!items.length)continue;
      const section=element('section','notification-group');section.append(element('h3','',title));
      for(const alert of items){
        const card=element('article','notification-card'+(attention(alert)?' needs-attention':''));card.dataset.id=alert.id;
        const link=element('a','notification-link');link.href=notificationUrl(alert);link.dataset.alertId=alert.id;link.dataset.action='open';
        link.append(element('strong','',alert.resolved?'Request resolved':alert.question?'Answer needed':labels[alert.event]||'Session activity'));
        const time=element('time');time.dateTime=new Date(alert.created).toISOString();time.textContent=new Date(alert.created).toLocaleString();link.append(time);
        link.append(element('span','notification-source',`${alert.title} · ${alert.projectName||'Ungrouped'}`),element('p','notification-body',alert.body),element('span','notification-destination',alert.automationId?'Open automation →':alert.event==='approval'&&!alert.resolved?'Open request →':'Open session →'));
        link.addEventListener('click',async event=>{
          if(!openAlert||event.ctrlKey||event.metaKey||event.shiftKey||event.altKey)return;
          event.preventDefault();
          navigating=true;tray.close();
          try{await openAlert(alert);}catch(err){tray.showModal();for(const trigger of bells)trigger.setAttribute('aria-expanded','true');render();error.textContent=err.message||'Could not open this session.';}
          finally{navigating=false;}
        });
        const dismiss=element('button','notification-dismiss','×');dismiss.type='button';dismiss.disabled=busy;dismiss.dataset.alertId=alert.id;dismiss.dataset.action='dismiss';dismiss.setAttribute('aria-label',`Dismiss ${labels[alert.event]||'alert'} for ${alert.title}`);dismiss.onclick=()=>change([alert.id],true);
        card.append(link,dismiss);section.append(card);
      }
      list.append(section);
    }
    if(focusId){const target=[...list.querySelectorAll('[data-alert-id]')].find(n=>n.dataset.alertId===focusId&&n.dataset.action===focusAction);(target||close).focus();}
  }
  async function refresh(){
    const generation=++request;
    try{const data=await read('/api/notifications/inbox');if(generation!==request)return;alerts=data.alerts;loaded=true;error.textContent='';retry.hidden=true;render();}
    catch(err){if(generation!==request)return;error.textContent=err.message;retry.hidden=false;}
  }
  async function change(ids,dismissed){
    if(busy)return;busy=true;render();
    try{
      for(let start=0;start<ids.length;start+=10000)await read('/api/notifications/inbox/dismiss',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({ids:ids.slice(start,start+10000),dismissed})});
      if(dismissed){lastDismissed=ids;undoText.textContent=`${ids.length} alert${ids.length===1?'':'s'} dismissed.`;undoBar.hidden=false;}else{lastDismissed=[];undoBar.hidden=true;}
      await refresh();
    }catch(err){error.textContent=err.message;}
    finally{busy=false;render();}
  }
  function connect(){
    socket=new WebSocket(socketUrl('/api/notifications/inbox/events'));
    socket.onopen=()=>{reconnect=1000;connection='Live';refresh();};
    socket.onmessage=()=>refresh();
    socket.onclose=()=>{connection='Reconnecting…';render();setTimeout(connect,reconnect);reconnect=Math.min(15000,reconnect*2);};
    socket.onerror=()=>socket.close();
  }
  for(const trigger of bells)trigger.onclick=()=>{opener=trigger;tray.showModal();for(const button of bells)button.setAttribute('aria-expanded','true');render();refresh();};
  close.onclick=()=>tray.close();tray.addEventListener('close',()=>{
    for(const trigger of bells)trigger.setAttribute('aria-expanded','false');
    if(!navigating)(opener.getClientRects().length?opener:bells.find(button=>button.getClientRects().length))?.focus();
  });
  tray.addEventListener('click',event=>{if(event.target===tray&&event.clientX<tray.getBoundingClientRect().left)tray.close();});
  dismissAll.onclick=()=>change(alerts.map(a=>a.id),true);undo.onclick=()=>change(lastDismissed,false);retry.onclick=refresh;
  document.addEventListener('visibilitychange',()=>{if(!document.hidden)refresh();});
  setInterval(()=>{if(!document.hidden)refresh();},30000);
  refresh();connect();render();
  return {refresh};
}
