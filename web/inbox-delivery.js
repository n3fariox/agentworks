// Separate human controls; messages and MCP tools cannot enable this policy.
export function setupInboxDelivery({api,getActive,toast,uuid}) {
  const dialog=document.getElementById('agents-dialog');
  const panel=document.createElement('section');panel.className='agent-message';panel.id='inbox-delivery-controls';
  const heading=document.createElement('h3');heading.textContent='Automatic inbox turns';
  const help=document.createElement('p');help.textContent='Off by default. Authorize new peer messages to start turns in this saved context. The authorization stands until you disable it — it has no automatic expiry, and it survives a normal service restart while this session, its saved context, project, directory and grants stay the same. A turn interrupted by a restart is never repeated: it stays paused in the ledger for you to inspect. Stop, Suspend, archive, a completion report, a grant or project change, or a replaced native context ends it and asks you to authorize again. Peer text is untrusted context and cannot extend any of this.';
  const form=document.createElement('form');form.style.cssText='display:grid;gap:12px;min-width:0';
  const fields={};
  function field(key,label,type,value,min,max,required=true){const row=document.createElement('label');row.textContent=label;row.style.cssText='display:grid;gap:4px;min-width:0';const input=document.createElement('input');input.type=type;input.value=value;input.id='inbox-'+key;input.style.cssText='width:100%;min-width:0;box-sizing:border-box;min-height:44px';if(min)input.min=min;if(max)input.max=max;input.required=required;fields[key]=input;row.append(input);form.append(row);return row;}
  field('maxTurns','Maximum total automatic turns (submission attempts, 1–20)','number','1','1','20');
  const unlimited=document.createElement('label');unlimited.style.cssText='display:flex;gap:8px;align-items:center;min-height:44px';
  const unlimitedBox=document.createElement('input');unlimitedBox.type='checkbox';unlimitedBox.id='inbox-unlimitedTurns';unlimitedBox.style.cssText='width:22px;height:22px;min-height:22px;margin:0';
  unlimited.append(unlimitedBox,document.createTextNode('No total-turn cap — keep waking while authorized'));form.append(unlimited);
  field('deadline','Optional: stop automatic turns at (leave empty for standing authorization)','datetime-local','','','',false);
  field('cooldownSeconds','Cooldown / message debounce (seconds)','number','30','5','3600');
  field('maxBatch','Maximum messages in each turn','number','5','1','10');
  const note=document.createElement('p');note.style.cssText='margin:0;overflow-wrap:anywhere';note.id='inbox-delivery-note';note.textContent='Wake batches quote the first 256 characters of each message; full messages stay in the inbox.';
  const enable=document.createElement('button');enable.type='submit';enable.textContent='Authorize standing inbox turns';enable.style.minHeight='44px';
  const cancel=document.createElement('button');cancel.type='button';cancel.textContent='Disable and cancel pending wakes';cancel.style.minHeight='44px';
  const status=document.createElement('p');status.id='inbox-delivery-status';status.setAttribute('role','status');status.style.overflowWrap='anywhere';
  form.append(enable,cancel);panel.append(heading,help,status,form,note);document.getElementById('agent-inbox').before(panel);
  unlimitedBox.addEventListener('change',()=>{fields.maxTurns.disabled=unlimitedBox.checked;});
  let timer,busy=false,pendingRequest=null;
  async function refresh(){const id=getActive()?.id;if(!id||!dialog.open||busy)return;try{const data=await api(`/api/sessions/${id}/inbox-delivery`);if(getActive()?.id!==id)return;const p=data.policy;const turns=p.maxTurns==null?`${p.attempts} turns, no total-turn cap`:`${p.attempts}/${p.maxTurns} turns`;const until=p.enabled?(p.deadline?` · stops ${new Date(p.deadline).toLocaleString()}`:' · standing'):'';status.textContent=`${p.enabled?'Enabled':'Off'} · ${turns}${until} · ${data.pending} pending${p.reason?' · '+p.reason:''}`;const paused=data.deliveries.filter(d=>d.state==='paused').length;if(paused)status.textContent+=` · ${paused} paused for inspection`;const details=data.deliveries.slice(0,8).map(d=>`#${d.messageId}: ${d.state}${d.reason?' ('+d.reason+')':''}${d.requestId?' · request '+d.requestId:''}`).join('; ');if(details)status.textContent+=' · '+details;}catch(e){status.textContent=e.message;}}
  async function save(enabled){if(busy)return;const id=getActive()?.id;if(!id)return;busy=true;enable.disabled=cancel.disabled=true;try{const body={enabled};if(enabled){const uncapped=unlimitedBox.checked;Object.assign(body,{unlimitedTurns:uncapped,cooldownSeconds:Number(fields.cooldownSeconds.value),maxBatch:Number(fields.maxBatch.value)});if(uncapped)body.maxTurns=undefined;else body.maxTurns=Number(fields.maxTurns.value);const stop=fields.deadline.value;if(stop)body.deadline=new Date(stop).getTime();else body.deadline=null;}const binding=JSON.stringify({id,...body});if(pendingRequest?.binding!==binding)pendingRequest={binding,requestId:uuid()};body.requestId=pendingRequest.requestId;await api(`/api/sessions/${id}/inbox-delivery`,{method:'PUT',body:JSON.stringify(body)});pendingRequest=null;toast(enabled?'Inbox turns authorized.':'Inbox turns disabled.','success');}catch(e){toast(e.message);}finally{busy=false;enable.disabled=cancel.disabled=false;await refresh();}}
  form.addEventListener('submit',e=>{e.preventDefault();save(true);});cancel.addEventListener('click',()=>save(false));
  document.getElementById('session-agents').addEventListener('click',()=>{refresh();clearInterval(timer);timer=setInterval(refresh,2000);});
  dialog.addEventListener('close',()=>{clearInterval(timer);timer=null;});
}
