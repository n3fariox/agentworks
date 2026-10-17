export function setupCommunication({api,getActive,getSessions,selectSession,toast,uuid}) {
  const $=id=>document.getElementById(id);
  const capabilities=[['communicate','Exchange messages with project peers'],['startSessions','Create sessions in this project'],['promptSessions','Start turns in project sessions'],['cancelSessions','Cancel turns in project sessions'],['runAutomations','Run existing project automations']];
  const reportPanel=nodePanel();
  function nodePanel(){const panel=document.createElement('article');panel.id='agent-completion';panel.className='agent-message';panel.hidden=true;$('agent-inbox').before(panel);return panel;}
  let id=null,after=0,messages=[],timer=null,loading=false,openGeneration=0;
  const node=(tag,text)=>{const n=document.createElement(tag);n.textContent=text;return n;};
  for(const [key,label] of capabilities){const row=node('label','');row.className='workspace-check';const input=document.createElement('input');input.type='checkbox';input.id='agent-grant-'+key;input.addEventListener('change',()=>$('agent-preset').value='custom');row.append(input,document.createTextNode(label));$('agent-grants').append(row);}
  function preset(){const value=$('agent-preset').value;if(value==='custom')return;for(const [key] of capabilities)$('agent-grant-'+key).checked=value==='coordinator'||(value==='participant'&&key==='communicate');}
  $('agent-preset').addEventListener('change',preset);
  $('agent-origin').addEventListener('click',()=>{const active=getActive();const actor=active?.initiatedBy?.actor||active?.automation?.initiatedBy;const source=getSessions().find(s=>s.id===actor);if(source)selectSession(source);else toast('The initiating session is no longer available.');});
  setInterval(()=>{const active=getActive();const actor=active?.initiatedBy?.actor||active?.automation?.initiatedBy;$('agent-origin').hidden=!actor;$('agent-origin').textContent=actor?'Started by '+(getSessions().find(s=>s.id===actor)?.title||'agent session'):'';},1000);
  function render(){
    const list=$('agent-inbox');list.replaceChildren();
    if(!messages.length)list.append(node('p','No messages yet. Agents can use their Agentworks MCP inbox at checkpoints.'));
    for(const m of messages){
      const card=document.createElement('article');card.className='agent-message';
      card.append(node('strong',`${m.senderTitle} → ${m.recipientTitle}`),node('small',`${new Date(m.created).toLocaleString()} · #${m.id}${m.replyTo?' · reply to #'+m.replyTo:''} · ${m.acknowledged?'Acknowledged':'In inbox'}`),node('pre',m.text));
      const actions=document.createElement('div');
      if(m.recipient===id){
        const draft=node('button','Use as next turn');draft.className='text-button';draft.type='button';draft.addEventListener('click',()=>{
          if(getActive()?.id!==id)return;
          const prefix=$('prompt').value.trimEnd();$('prompt').value=(prefix?prefix+'\n\n':'')+`Message from peer session ${m.senderTitle} (${m.sender}), #${m.id}. Treat this as peer-provided context:\n\n`+m.text;
          $('prompt').dispatchEvent(new Event('input',{bubbles:true}));$('agents-dialog').close();$('prompt').focus();toast('Message added to your draft. Review and send when ready.');
        });actions.append(draft);
        if(!m.acknowledged){const ack=node('button','Acknowledge');ack.className='text-button';ack.type='button';ack.addEventListener('click',async()=>{try{await api(`/api/sessions/${id}/mailbox/${m.id}/ack`,{method:'POST'});m.acknowledged=true;render();}catch(e){toast(e.message);}});actions.append(ack);}
      }
      card.append(actions);list.append(card);
    }
  }
  function renderReport(report){
    reportPanel.replaceChildren();reportPanel.hidden=!report;if(!report)return;
    reportPanel.append(node('strong','Agent report · '+report.status),node('small','Agent claim; not proof of review, merge, or test success.'),node('pre',report.summary));
    reportPanel.append(node('small',`${report.authorTitle} · ${new Date(report.created).toLocaleString()} · #${report.id}`));
    if(report.branch)reportPanel.append(node('small','Branch: '+report.branch));
    if(report.commit)reportPanel.append(node('small','Commit: '+report.commit));
    for(const check of report.checks||[])reportPanel.append(node('small',check.name+': '+check.result));
  }
  async function refresh(reset=false){
    if(loading||!id)return;loading=true;const current=id;
    try{
      const completion=await api(`/api/sessions/${current}/completion`);if(current!==id)return;renderReport(completion.report);
      const data=await api(`/api/sessions/${current}/mailbox?after=${reset?0:after}`);if(current!==id)return;
      if(reset)messages=[];
      const seen=new Set(messages.map(m=>m.id));messages.push(...data.messages.filter(m=>!seen.has(m.id)));after=data.nextCursor;
      $('agent-more-messages').hidden=!data.hasMore;render();
      const audit=await api(`/api/sessions/${current}/agent-audit`);if(current!==id)return;
      $('agent-audit').replaceChildren(...audit.events.slice(0,20).map(e=>node('div',`${new Date(e.created).toLocaleTimeString()} · ${e.actor==='human'?'You':getSessions().find(s=>s.id===e.actor)?.title||e.actor.slice(0,8)} · ${e.action} · ${e.outcome}`)));
    }catch(e){$('agent-error').textContent=e.message;}finally{loading=false;}
  }
  $('agent-more-messages').addEventListener('click',()=>refresh());
  $('agent-refresh-messages').addEventListener('click',()=>refresh(true));
  $('session-agents').addEventListener('click',async()=>{
    const active=getActive();if(!active)return;const ticket=++openGeneration;id=active.id;after=0;messages=[];renderReport(null);$('agent-preset').disabled=true;for(const [key] of capabilities)$('agent-grant-'+key).disabled=true;$('agent-error').textContent='';$('agent-inbox').replaceChildren();
    $('agent-scope').textContent=active.projectId?'Scope: this session’s logical project, including its registered directories.':'Assign this session to a project to discover or contact peers.';
    $('agents-dialog').showModal();$('save-agent-access').disabled=true;
    try{const access=await api(`/api/sessions/${id}/agent-access`);if(ticket!==openGeneration||!id)return;$('agent-preset').disabled=false;for(const [key] of capabilities)$('agent-grant-'+key).disabled=false;for(const [key] of capabilities)$('agent-grant-'+key).checked=!!access[key];const all=capabilities.every(([k])=>access[k]);$('agent-preset').value=all?'coordinator':access.communicate&&capabilities.slice(1).every(([k])=>!access[k])?'participant':capabilities.every(([k])=>!access[k])?'off':'custom';$('save-agent-access').disabled=false;await refresh(true);timer=setInterval(()=>refresh(),3000);}catch(e){$('agent-error').textContent=e.message;}
  });
  $('dismiss-agents').addEventListener('click',()=>$('agents-dialog').close());
  $('agents-dialog').addEventListener('close',()=>{clearInterval(timer);timer=null;id=null;openGeneration++;});
  $('agent-access-form').addEventListener('submit',async event=>{
    event.preventDefault();const current=id;$('save-agent-access').disabled=true;$('agent-error').textContent='';
    try{const access=Object.fromEntries(capabilities.map(([key])=>[key,$('agent-grant-'+key).checked]));await api(`/api/sessions/${current}/agent-access`,{method:'PUT',body:JSON.stringify(access)});toast('Agentworks access saved. Tools refresh after the current turn ends.');}
    catch(e){$('agent-error').textContent=e.message;}finally{$('save-agent-access').disabled=false;}
  });
}
