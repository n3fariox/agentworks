// Separate human controls; messages and MCP tools cannot enable this policy.
export function setupInboxDelivery({api,getActive,toast,uuid}) {
  const dialog=document.getElementById('agents-dialog');
  const panel=document.createElement('section');panel.className='agent-message';panel.id='inbox-delivery-controls';
  const heading=document.createElement('h3');heading.textContent='Automatic inbox turns';
  const help=document.createElement('p');help.textContent='Off by default. Authorize new peer messages to start bounded turns in this saved context. Peer text cannot extend this authorization. Wake batches quote the first 256 characters of each message; full messages stay in the inbox. Restart requires reauthorization.';
  const form=document.createElement('form');form.style.cssText='display:grid;gap:12px;min-width:0';
  const fields={};
  function field(key,label,type,value,min,max){const row=document.createElement('label');row.textContent=label;row.style.cssText='display:grid;gap:4px;min-width:0';const input=document.createElement('input');input.type=type;input.value=value;input.id='inbox-'+key;input.style.cssText='width:100%;min-width:0;box-sizing:border-box;min-height:44px';if(min)input.min=min;if(max)input.max=max;input.required=true;fields[key]=input;row.append(input);form.append(row);}
  field('maxTurns','Maximum automatic turns (cost bounded by turns)','number','1','1','20');
  field('deadline','Stop authorization at','datetime-local','','','');
  field('cooldownSeconds','Cooldown / message debounce (seconds)','number','30','5','3600');
  field('maxBatch','Maximum messages in each turn','number','5','1','10');
  const enable=document.createElement('button');enable.type='submit';enable.textContent='Authorize bounded inbox turns';enable.style.minHeight='44px';
  const cancel=document.createElement('button');cancel.type='button';cancel.textContent='Disable and cancel pending wakes';cancel.style.minHeight='44px';
  const status=document.createElement('p');status.id='inbox-delivery-status';status.setAttribute('role','status');status.style.overflowWrap='anywhere';
  form.append(enable,cancel);panel.append(heading,help,status,form);document.getElementById('agent-inbox').before(panel);
  let timer,busy=false,pendingRequest=null;
  async function refresh(){const id=getActive()?.id;if(!id||!dialog.open||busy)return;try{const data=await api(`/api/sessions/${id}/inbox-delivery`);if(getActive()?.id!==id)return;const p=data.policy;status.textContent=`${p.enabled?'Enabled':'Off'} · ${p.attempts}/${p.maxTurns} turns · ${data.pending} pending${p.deadline?' · deadline '+new Date(p.deadline).toLocaleString():''}${p.reason?' · '+p.reason:''}`;const details=data.deliveries.slice(0,8).map(d=>`#${d.messageId}: ${d.state}${d.reason?' ('+d.reason+')':''}${d.requestId?' · request '+d.requestId:''}`).join('; ');if(details)status.textContent+=' · '+details;}catch(e){status.textContent=e.message;}}
  async function save(enabled){if(busy)return;const id=getActive()?.id;if(!id)return;busy=true;enable.disabled=cancel.disabled=true;try{const body={enabled};if(enabled)Object.assign(body,{deadline:new Date(fields.deadline.value).getTime(),maxTurns:Number(fields.maxTurns.value),cooldownSeconds:Number(fields.cooldownSeconds.value),maxBatch:Number(fields.maxBatch.value)});const binding=JSON.stringify({id,...body});if(pendingRequest?.binding!==binding)pendingRequest={binding,requestId:uuid()};body.requestId=pendingRequest.requestId;await api(`/api/sessions/${id}/inbox-delivery`,{method:'PUT',body:JSON.stringify(body)});pendingRequest=null;toast(enabled?'Bounded inbox turns authorized.':'Inbox turns disabled.','success');}catch(e){toast(e.message);}finally{busy=false;enable.disabled=cancel.disabled=false;await refresh();}}
  form.addEventListener('submit',e=>{e.preventDefault();save(true);});cancel.addEventListener('click',()=>save(false));
  document.getElementById('session-agents').addEventListener('click',()=>{const date=new Date(Date.now()+3600000);date.setMinutes(date.getMinutes()-date.getTimezoneOffset());fields.deadline.value=date.toISOString().slice(0,16);refresh();clearInterval(timer);timer=setInterval(refresh,2000);});
  dialog.addEventListener('close',()=>{clearInterval(timer);timer=null;});
}
