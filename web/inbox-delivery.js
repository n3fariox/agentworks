// Separate human controls; messages and MCP tools cannot enable this policy.
export function setupInboxDelivery({api,getActive,toast,uuid}) {
  const dialog=document.getElementById('agents-dialog');
  const panel=document.createElement('section');panel.className='settings-section';panel.id='inbox-delivery-controls';
  const heading=document.createElement('h3');heading.textContent='Automatic inbox turns';
  const help=document.createElement('p');help.textContent='Off by default. Authorize new peer messages to start turns in this saved context. The authorization stands until you disable it — it has no automatic expiry, and it survives a normal service restart while this session, its saved context, project, directory and grants stay the same. A turn interrupted by a restart is never repeated: it stays paused in the ledger for you to inspect. Stop, Suspend, archive, a completion report, a grant or project change, or a replaced native context ends it and asks you to authorize again. Peer text is untrusted context and cannot extend any of this.';
  help.className='field-note';
  const form=document.createElement('form');form.id='inbox-delivery-form';
  const controls=document.createElement('fieldset');controls.disabled=true;form.append(controls);
  const fields={};
  function field(key,label,type,value,min,max,required=true){const row=document.createElement('label');row.textContent=label;const input=document.createElement('input');input.type=type;input.value=value;input.id='inbox-'+key;if(min)input.min=min;if(max)input.max=max;input.required=required;fields[key]=input;row.append(input);controls.append(row);return row;}
  field('maxTurns','Maximum total automatic turns (submission attempts, 1–20)','number','1','1','20');
  const unlimited=document.createElement('label');unlimited.className='workspace-check';
  const unlimitedBox=document.createElement('input');unlimitedBox.type='checkbox';unlimitedBox.id='inbox-unlimitedTurns';
  unlimited.append(unlimitedBox,document.createTextNode('No total-turn cap — keep waking while authorized'));controls.append(unlimited);
  field('deadline','Optional stop time (within 24 hours; blank for standing authorization)','datetime-local','','','',false);
  field('cooldownSeconds','Cooldown / message debounce (seconds)','number','30','5','3600');
  field('maxBatch','Maximum messages in each turn','number','5','1','10');
  const note=document.createElement('p');note.className='field-note';note.id='inbox-delivery-note';note.textContent='Wake batches quote the first 256 characters of each message; full messages stay in the inbox.';
  const enable=document.createElement('button');enable.type='submit';enable.textContent='Authorize standing inbox turns';enable.className='primary';
  const cancel=document.createElement('button');cancel.type='button';cancel.textContent='Disable and cancel pending wakes';cancel.className='text-button';
  const status=document.createElement('p');status.id='inbox-delivery-status';status.setAttribute('role','status');status.className='field-note';
  const actions=document.createElement('div');actions.className='dialog-footer';actions.append(cancel,enable);
  const error=document.createElement('p');error.id='inbox-delivery-error';error.className='error-text';error.setAttribute('role','alert');
  controls.append(actions);form.append(error);panel.append(heading,help,status,form,note);document.getElementById('agent-access-form').after(panel);
  unlimitedBox.addEventListener('change',()=>{fields.maxTurns.disabled=unlimitedBox.checked;});
  let timer,busy=false,pendingRequest=null,dirty=false,loaded=false,epoch=0;
  const current=(id,version)=>dialog.open&&getActive()?.id===id&&epoch===version;
  function fill(p){
    // Unconfigured policies have zero bounds. Show safe defaults until first authorization.
    fields.maxTurns.value=p.maxTurns??1;
    unlimitedBox.checked=p.maxTurns==null&&p.generation>0;
    fields.maxTurns.disabled=unlimitedBox.checked;
    fields.cooldownSeconds.value=p.cooldownSeconds||30;
    fields.maxBatch.value=p.maxBatch||5;
    const local=p.deadline?new Date(p.deadline):null;
    fields.deadline.value=local?new Date(local.getTime()-local.getTimezoneOffset()*60000).toISOString().slice(0,16):'';
  }
  function render(data){
    const p=data.policy;
    const turns=p.maxTurns==null?`${p.attempts} turns, no total-turn cap`:`${p.attempts}/${p.maxTurns} turns`;
    const until=p.enabled?(p.deadline?` · stops ${new Date(p.deadline).toLocaleString()}`:' · standing'):'';
    status.textContent=`${p.enabled?'Enabled':'Off'} · ${turns}${until} · ${data.pending} pending${p.reason?' · '+p.reason:''}`;
    const paused=data.deliveries.filter(d=>d.state==='paused').length;
    if(paused)status.textContent+=` · ${paused} paused for inspection`;
    const details=data.deliveries.slice(0,8).map(d=>`#${d.messageId}: ${d.state}${d.reason?' ('+d.reason+')':''}${d.requestId?' · request '+d.requestId:''}`).join('; ');
    if(details)status.textContent+=' · '+details;
  }
  form.addEventListener('input',()=>{dirty=true;});
  async function refresh(){
    const id=getActive()?.id,version=epoch;
    if(!id||!dialog.open||busy)return;
    try{
      const data=await api(`/api/sessions/${id}/inbox-delivery`);
      if(!current(id,version)||busy)return;
      render(data);
      if(!dirty)fill(data.policy);
      loaded=true;controls.disabled=false;
    }catch(e){if(current(id,version))status.textContent='Cannot load inbox settings: '+e.message;}
  }
  async function save(enabled){
    const id=getActive()?.id;
    if(busy||!id||!loaded)return;
    // Invalidate status reads started before this mutation.
    const version=++epoch;
    busy=true;controls.disabled=true;error.textContent='';
    try{
      const body={enabled};
      if(enabled){
        const uncapped=unlimitedBox.checked;
        Object.assign(body,{unlimitedTurns:uncapped,cooldownSeconds:Number(fields.cooldownSeconds.value),maxBatch:Number(fields.maxBatch.value)});
        if(!uncapped)body.maxTurns=Number(fields.maxTurns.value);
        const stop=fields.deadline.value;
        body.deadline=stop?new Date(stop).getTime():null;
      }
      const binding=JSON.stringify({id,...body});
      if(pendingRequest?.binding!==binding)pendingRequest={binding,requestId:uuid()};
      body.requestId=pendingRequest.requestId;
      const data=await api(`/api/sessions/${id}/inbox-delivery`,{method:'PUT',body:JSON.stringify(body)});
      pendingRequest=null;
      if(current(id,version)){dirty=false;fill(data.policy);render(data);}
      toast(enabled?'Inbox turns authorized.':'Inbox turns disabled.','success');
    }catch(e){
      if(current(id,version))error.textContent='Inbox settings were not saved: '+e.message;
      toast(e.message);
    }finally{
      busy=false;
      if(current(id,version))controls.disabled=!loaded;
      await refresh();
    }
  }
  form.addEventListener('submit',e=>{e.preventDefault();save(true);});cancel.addEventListener('click',()=>save(false));
  new MutationObserver(()=>{
    epoch++;clearInterval(timer);timer=null;
    if(dialog.open){dirty=false;loaded=false;controls.disabled=true;error.textContent='';status.textContent='Loading inbox settings…';refresh();timer=setInterval(refresh,2000);}
  }).observe(dialog,{attributes:true,attributeFilter:['open']});
}
