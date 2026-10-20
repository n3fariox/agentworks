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
  let queue={items:[],paused:null},editing=null;
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
  function reset(){since=null;exact=false;lastStatus=null;queue={items:[],paused:null};editing=null;renderActivity();render();}
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
  // Apply a delayed response only if the same session is still active.
  async function call(suffix,options){
    const id=sid(),r=await api(path(suffix),options);if(id===sid())set(r.queue);return r;
  }
  // The caller owns the stable per-session request identity; retries reuse it.
  async function enqueue(id,{text,requestId}){
    const r=await api(`/api/sessions/${encodeURIComponent(id)}/queue`,{method:'POST',body:JSON.stringify({text,requestId})});
    if(id===sid())set(r.queue);return r;
  }
  let lastSig=null;
  function render(){
    // Re-render only on change so an in-progress edit is not replaced by unrelated events.
    const sig=JSON.stringify([queue,editing,getStatus()==='idle']);if(sig===lastSig)return;lastSig=sig;
    list.replaceChildren();
    const items=queue.items||[];
    const pending=items.filter(item=>item.state!=='sent'),sent=items.filter(item=>item.state==='sent');
    // Sent history and a paused-but-empty queue have no actionable queued input.
    // Keep the whole panel out of the layout until there is something pending.
    list.hidden=pending.length===0;
    if(!pending.length)return;
    const head=document.createElement('div');head.className='queue-head';
    const title=document.createElement('span');title.textContent=`Queued inputs (${pending.length})`;head.append(title);
    if(queue.paused){
      const note=document.createElement('span');note.className='queue-paused';note.setAttribute('role','status');note.textContent=queue.paused;
      const resume=document.createElement('button');resume.type='button';resume.className='text-button';resume.dataset.queueResume='';
      resume.textContent=getStatus()==='idle'&&pending.some(item=>['queued','paused'].includes(item.state))?'Resume & send next':'Resume queue';
      resume.onclick=()=>call('/resume',{method:'POST'}).catch(e=>note.textContent=e.message);
      head.append(note,resume);
    }else if(getStatus()==='idle'&&items.some(i=>i.state==='queued')){
      const send=document.createElement('button');send.type='button';send.className='text-button';send.dataset.queueResume='';send.textContent='Send next now';
      send.onclick=()=>call('/resume',{method:'POST'}).catch(()=>{});head.append(send);
    }
    const ol=document.createElement('ol');
    for(const item of pending){
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
    list.append(head);
    if(pending.length)list.append(ol);
    if(sent.length){
      const history=document.createElement('details');history.className='queue-history';
      const summary=document.createElement('summary');summary.textContent=`Recently sent (${sent.length})`;history.append(summary);
      const sentList=document.createElement('ol');
      for(const item of sent){
        const li=document.createElement('li');li.className='queue-item';li.dataset.state='sent';li.dataset.id=item.id;
        const badge=document.createElement('span');badge.className='queue-state';badge.textContent=STATE_LABELS[item.state];
        const text=document.createElement('span');text.className='queue-text';text.textContent=item.text;
        li.append(badge,text);if(item.note){const note=document.createElement('span');note.className='queue-note';note.textContent=item.note;li.append(note);}sentList.append(li);
      }
      history.append(sentList);list.append(history);
    }
  }
  return {set,enqueue,observe,reset,render,renderActivity,hasPending:()=>(queue.items||[]).some(i=>['queued','paused','sending','error'].includes(i.state))};
}

// Recovery copies a stopped prompt; it never removes recorded/provider history.
// Exact editor text is kept separately from expanded mentions/wire payloads.
export function setupPromptRecovery({area,storage,getSession,onChange}){
  const read=key=>{try{return JSON.parse(storage.getItem(key))||{};}catch{return {};}};
  const submissions=read('agentworks-submitted-drafts'),handled=read('agentworks-recovered-stops'),pending=read('agentworks-stop-recovery');
  const save=()=>{storage.setItem('agentworks-submitted-drafts',JSON.stringify(submissions));storage.setItem('agentworks-recovered-stops',JSON.stringify(handled));storage.setItem('agentworks-stop-recovery',JSON.stringify(pending));};
  const turns=new Map();
  const panel=document.createElement('div');panel.className='notice-card';panel.setAttribute('role','status');area.closest('form').before(panel);
  function render(){
    panel.replaceChildren();const items=pending[getSession()]||[];panel.hidden=!items.length;
    for(const item of items){
      const row=document.createElement('div'),note=document.createElement('p');
      note.textContent=item.outcome==='cancelled'?'Cancelled. History and tool effects are retained. Edit the prompt and send deliberately to start a new turn.':item.outcome==='error'?'Cancellation ended with an error. Inspect the retained history before retrying.':item.finished?'The turn completed while Stop was being handled. Its prompt is available to edit. History was retained; sending starts a new turn.':'Stop requested. The prompt is available to edit. Cancellation may finish after more output arrives. History and tool effects are retained; sending starts a new turn.';row.append(note);
      if(item.text!==null){
        const button=document.createElement('button');button.type='button';button.className='text-button';button.textContent='Edit recovered prompt (keep current draft)';
        button.onclick=()=>{const previous=area.value;area.value=item.text;item.text=previous||null;save();onChange();area.focus();render();};row.append(button);
      }
      const dismiss=document.createElement('button');dismiss.type='button';dismiss.className='text-button';dismiss.textContent='Dismiss';
      dismiss.onclick=()=>{if(item.text!==null&&!confirm('Dismiss the recovered draft? Your current draft stays in the composer.'))return;pending[getSession()]=items.filter(i=>i!==item);save();render();};row.append(dismiss);panel.append(row);
    }
  }
  return {
    render,
    submitted(sid,submission,draft){submissions[sid+':'+submission.requestId]={draft};save();},
    recovered(sid,requestId){return !!handled[sid+':'+requestId];},
    consume(sid,event){
      if(event.kind==='user'){
        const previous=turns.get(sid);
        if(previous&&event.seq>previous.seq)delete submissions[sid+':'+previous.requestId];
        turns.set(sid,{seq:event.seq,requestId:event.data.requestId});
        pending[sid]=(pending[sid]||[]).filter(item=>!(event.seq>item.userSeq&&item.text===null));save();render();
      }
      if(['turn_end','error','interrupted'].includes(event.kind)){
        const turn=turns.get(sid);if(turn){
          turn.outcome=event.kind==='turn_end'?event.data.stopReason:'error';
          for(const item of pending[sid]||[])if(item.userSeq===turn.seq){item.outcome=turn.outcome;item.finished=true;}
          save();render();
        }
      }
      if(event.kind!=='stop_requested')return;
      const d=event.data,key=sid+':'+(d.requestId||d.userSeq);
      if(handled[key])return;
      handled[key]=true;
      const sent=submissions[sid+':'+d.requestId],text=sent?sent.draft:d.text;
      if(typeof text!=='string'){save();return;}
      const outcome=turns.get(sid)?.seq===d.userSeq?turns.get(sid)?.outcome:null;
      const item={text,userSeq:d.userSeq,outcome,finished:!!d.alreadyFinished||!!outcome};
      (pending[sid]??=[]).push(item);
      if(sid===getSession()&&(!area.value||area.value===text)){area.value=text;item.text=null;onChange();}
      save();render();
    },
  };
}
