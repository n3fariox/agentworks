// L-05 activity indicator and explicit queued inputs. Queue state is server-owned;
// this module only renders it and sends deliberate enqueue/edit/remove/resume actions.
const STATE_LABELS={queued:'Queued',sending:'Sending',sent:'Sent',paused:'Paused',error:'Needs inspection'};
export const BUSY=['running','waiting','cancelling'];

// Pure activity label for tests: status plus whether a tool call is in progress.
export function activityLabel(status,toolActive){
  if(status==='cancelling')return'Cancelling';
  if(status==='waiting')return'Waiting for approval';
  if(status==='running')return toolActive?'Running tool':'Model working';
  return'';
}
export function formatElapsed(ms){
  const s=Math.max(0,Math.floor(ms/1000));const m=Math.floor(s/60);
  return m?`${m}m ${String(s%60).padStart(2,'0')}s`:`${s}s`;
}

export function setupInputQueue({$,api,uuid,getActive,getStatus,toolActive}){
  const indicator=$('activity-indicator'),list=$('input-queue');
  let queue={items:[],paused:null},pendingEnqueue=null,editing=null;
  // Elapsed timing: `exact` only when this page saw the turn start live. After replay or
  // reconnect the start is unknown, so we show time observed since reconnect, labelled so.
  let since=null,exact=false,lastStatus=null,timer=null;
  const sid=()=>getActive()?.id;
  const path=(suffix='')=>`/api/sessions/${encodeURIComponent(sid())}/queue${suffix}`;

  function observe(status,{live}){
    const busy=BUSY.includes(status),wasBusy=BUSY.includes(lastStatus);
    if(busy&&!wasBusy){since=Date.now();exact=!!live&&status==='running';}
    if(!busy){since=null;exact=false;}
    lastStatus=status;renderActivity();
  }
  function reset(){since=null;exact=false;lastStatus=null;queue={items:[],paused:null};editing=null;pendingEnqueue=null;renderActivity();render();}
  function renderActivity(){
    const status=getStatus(),label=activityLabel(status,toolActive());
    indicator.hidden=!label;
    if(!label){clearInterval(timer);timer=null;return;}
    if(since===null){since=Date.now();exact=false;}
    const elapsed=formatElapsed(Date.now()-since);
    indicator.dataset.kind=status==='running'&&toolActive()?'tool':status;
    indicator.querySelector('[data-label]').textContent=label;
    indicator.querySelector('[data-elapsed]').textContent=exact?elapsed:`${elapsed} since reconnect`;
    indicator.querySelector('[data-elapsed]').title=exact?'Elapsed since this turn started':'Turn start time unknown after replay; showing time observed since reconnect';
    if(!timer)timer=setInterval(renderActivity,1000);
  }

  function set(next){queue=next&&Array.isArray(next.items)?next:{items:[],paused:null};render();}
  async function call(suffix,options){
    const r=await api(path(suffix),options);set(r.queue);return r;
  }
  // Retries with the same text reuse one request ID; a changed draft gets a new one.
  async function enqueue(text){
    const id=sid();if(!id)return false;
    if(!pendingEnqueue||pendingEnqueue.text!==text||pendingEnqueue.sid!==id)pendingEnqueue={text,sid:id,requestId:uuid()};
    const submission=pendingEnqueue;
    await call('',{method:'POST',body:JSON.stringify({text:submission.text,requestId:submission.requestId})});
    if(pendingEnqueue===submission)pendingEnqueue=null;
    return true;
  }
  let lastSig=null;
  function render(){
    // Re-render only on change so an in-progress edit is not replaced by unrelated events.
    const sig=JSON.stringify([queue,editing,getStatus()==='idle']);if(sig===lastSig)return;lastSig=sig;
    list.replaceChildren();
    const items=queue.items||[];
    list.hidden=!items.length;
    if(!items.length)return;
    const head=document.createElement('div');head.className='queue-head';
    const title=document.createElement('span');title.textContent=`Queued inputs (${items.filter(i=>i.state!=='sent').length})`;head.append(title);
    if(queue.paused){
      const note=document.createElement('span');note.className='queue-paused';note.setAttribute('role','status');note.textContent=queue.paused;
      const resume=document.createElement('button');resume.type='button';resume.className='text-button';resume.dataset.queueResume='';
      resume.textContent=getStatus()==='idle'?'Resume & send next':'Resume queue';
      resume.onclick=()=>call('/resume',{method:'POST'}).catch(e=>note.textContent=e.message);
      head.append(note,resume);
    }else if(getStatus()==='idle'&&items.some(i=>i.state==='queued')){
      const send=document.createElement('button');send.type='button';send.className='text-button';send.dataset.queueResume='';send.textContent='Send next now';
      send.onclick=()=>call('/resume',{method:'POST'}).catch(()=>{});head.append(send);
    }
    const ol=document.createElement('ol');
    for(const item of items){
      const li=document.createElement('li');li.className='queue-item';li.dataset.state=item.state;li.dataset.id=item.id;
      const badge=document.createElement('span');badge.className='queue-state';badge.textContent=STATE_LABELS[item.state]||item.state;
      li.append(badge);
      const editable=['queued','paused','error'].includes(item.state);
      if(editing===item.id&&editable){
        const area=document.createElement('textarea');area.value=item.text;area.maxLength=32768;area.setAttribute('aria-label','Edit queued input');area.rows=2;
        const save=document.createElement('button');save.type='button';save.className='text-button';save.textContent='Save';
        const cancel=document.createElement('button');cancel.type='button';cancel.className='text-button';cancel.textContent='Cancel';
        const err=document.createElement('span');err.className='error-text';err.setAttribute('role','alert');
        save.onclick=()=>call(`/${encodeURIComponent(item.id)}`,{method:'PATCH',body:JSON.stringify({text:area.value,revision:item.revision})}).then(()=>{editing=null;render();}).catch(e=>err.textContent=e.message);
        cancel.onclick=()=>{editing=null;render();};
        area.onkeydown=e=>{if(e.key==='Escape'){e.preventDefault();cancel.click();}else if(e.key==='Enter'&&(e.ctrlKey||e.metaKey)){e.preventDefault();save.click();}};
        li.append(area,save,cancel,err);ol.append(li);queueMicrotask(()=>area.focus());continue;
      }
      const text=document.createElement('span');text.className='queue-text';text.textContent=item.text;li.append(text);
      if(item.note){const note=document.createElement('span');note.className='queue-note';note.textContent=item.note;li.append(note);}
      if(editable){
        const edit=document.createElement('button');edit.type='button';edit.className='text-button';edit.textContent=item.state==='error'?'Edit to re-queue':'Edit';
        edit.setAttribute('aria-label',`Edit queued input ${item.id}`);edit.onclick=()=>{editing=item.id;render();};
        const remove=document.createElement('button');remove.type='button';remove.className='text-button';remove.textContent='Remove';
        remove.setAttribute('aria-label',`Remove queued input ${item.id}`);remove.onclick=()=>call(`/${encodeURIComponent(item.id)}`,{method:'DELETE'}).catch(()=>{});
        li.append(edit,remove);
      }
      ol.append(li);
    }
    list.append(head,ol);
  }
  return {set,enqueue,observe,reset,render,renderActivity,hasPending:()=>(queue.items||[]).some(i=>['queued','paused','sending'].includes(i.state))};
}
