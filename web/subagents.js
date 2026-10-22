// Native ACP child sessions share a connection, but have independent transcripts
// and tool ID namespaces. Both released-adapter and current draft events work.
export function activityLabel(state,rootBusy){
  const value=typeof state==='string'?state:state?.state;
  if(value==='idle')return ({end_turn:'Completed',cancelled:'Cancelled',error:'Failed',max_tokens:'Token limit',max_turn_requests:'Request limit',refusal:'Refused'})[state.stopReason]||'Idle';
  if(value==='running')return rootBusy?'Working':'Activity unconfirmed';
  if(value==='requires_action')return 'Needs attention';
  if(['completed','failed','cancelled','disconnected'].includes(value))return value[0].toUpperCase()+value.slice(1);
  return 'Activity unconfirmed';
}

export function createSubagents({append,uuid,applyToolUpdate,renderToolActivity,renderMessage,el,tabs,panel,messages,onSelect}){
  const children=new Map();
  let lastSeq=0,selected=null;
  function ensure(id,parent){
    let child=children.get(id);
    if(!child){
      child={id:uuid(),sessionId:id,parentSessionId:parent,type:'subagent',title:null,description:null,state:null,items:[],tools:new Map(),sessionMessages:new Map(),nodes:new Map(),message:null};
      children.set(id,child);
      append(child);
    }
    return child;
  }
  function consume(e,{live=false}={}){
    const d=e.data,kind=d?.sessionUpdate;
    if(e.kind!=='acp')return false;
    const lifecycle=['subagent_spawned','subagent_state_update','subagent_update'].includes(kind);
    if(!lifecycle&&typeof d?.parentSessionId!=='string')return false;
    if(Number.isSafeInteger(e.seq)){if(e.seq<=lastSeq)return true;lastSeq=e.seq;}
    if(['subagent_spawned','subagent_state_update','subagent_update'].includes(kind)){
      const id=kind==='subagent_update'?d.sessionId:d.subagentSessionId;
      if(typeof id!=='string')return true;
      const child=ensure(id,d.acpSessionId);
      if(kind==='subagent_spawned'){
        child.title=d.name;child.description=d.task;child.state='running';
      }else if(kind==='subagent_state_update')child.state=d.state;
      else for(const key of ['title','description','state','capabilities'])if(Object.hasOwn(d,key))child[key]=d[key];
      return true;
    }
    if(typeof d?.parentSessionId!=='string')return false;
    const child=ensure(d.acpSessionId,d.parentSessionId);
    if(['agent_message_chunk','agent_thought_chunk','user_message_chunk'].includes(kind)){
      const role=kind==='user_message_chunk'?'Instruction':kind==='agent_thought_chunk'?'Thinking':'Agent';
      if(d.content?.type==='text'){
        if(!child.message||child.message.role!==role||child.message.messageId!==d.messageId){
          child.message={id:uuid(),type:'text',role,messageId:d.messageId,text:''};child.items.push(child.message);
        }
        child.message.text+=d.content.text;
      }else child.message=null;
    }else if(kind==='session_message'||kind==='session_message_chunk'){
      child.message=null;
      let message=child.sessionMessages.get(d.messageId);
      if(!message){message={id:uuid(),type:'text',role:'Message',text:''};child.sessionMessages.set(d.messageId,message);child.items.push(message);}
      if(d.senderSessionId!=null)message.sender=d.senderSessionId;
      if(d.recipientSessionId!=null)message.recipient=d.recipientSessionId;
      message.role=message.recipient===child.sessionId?'Instruction':'Message sent';
      const text=blocks=>(blocks||[]).filter(c=>c.type==='text').map(c=>c.text).join('\n');
      if(kind==='session_message_chunk')message.text+=text([d.content]);
      else if(Object.hasOwn(d,'content'))message.text=text(d.content);
    }else if(kind==='tool_call'||kind==='tool_call_update'){
      child.message=null;
      let tool=child.tools.get(d.toolCallId);
      if(!tool){tool={id:uuid(),type:'tool'};child.tools.set(d.toolCallId,tool);child.items.push(tool);}
      applyToolUpdate(tool,d,e.seq,live?Date.now():null);
    }else if(kind==='plan'){
      child.message=null;
      child.items.push({id:uuid(),type:'text',role:'Plan',text:(d.entries||[]).map(p=>`${p.status==='completed'?'✓':'○'} ${p.content}`).join('\n')});
    }
    return true; // Child configuration/usage never changes the parent's UI.
  }
  function select(view){
    if(selected&&panel&&children.has(selected))children.get(selected).scroll=panel.scrollTop;
    selected=view.startsWith('sub:')?view.slice(4):null;
    if(messages)messages.hidden=selected!==null;
    if(panel)panel.hidden=selected===null;
  }
  function sync(rootBusy){
    if(!tabs||!panel)return;
    for(const child of children.values()){
      if(!child.tab){
        child.tab=el('button','subagent-tab');child.tab.type='button';child.tab.id=`session-tab-subagent-${child.id}`;
        child.tab.setAttribute('role','tab');child.tab.setAttribute('aria-controls','session-chat');
        child.tab.dataset.sessionView=`sub:${child.sessionId}`;child.tab.dataset.subagentSession=child.sessionId;
        child.tab.append(el('span','subagent-tab-label'),el('span','subagent-dot'));
        child.tab.querySelector('.subagent-dot').setAttribute('aria-hidden','true');
        child.tab.onclick=()=>onSelect(`sub:${child.sessionId}`);tabs.append(child.tab);
      }
      const name=child.title||'Subagent',state=activityLabel(child.state,rootBusy),active=selected===child.sessionId;
      child.tab.querySelector('.subagent-tab-label').textContent=`Sub: ${name}`;
      child.tab.setAttribute('aria-label',`Sub: ${name}`);child.tab.title=`${name} · ${state}`;
      child.tab.setAttribute('aria-selected',String(active));child.tab.tabIndex=active?0:-1;
      child.tab.dataset.activity=state==='Working'?'running':state==='Needs attention'?'waiting':state==='Failed'?'failed':state==='Completed'?'completed':'idle';
      if(active)document.getElementById('session-chat').setAttribute('aria-labelledby',child.tab.id);
    }
    if(messages)messages.hidden=selected!==null;
    panel.hidden=selected===null;
    if(selected===null)return;
    const child=children.get(selected);
    if(!child){panel.replaceChildren(el('p','subagent-description','Loading subagent activity…'));return;}
    const changed=panel.dataset.subagentSession!==selected;
    const atBottom=panel.scrollHeight-panel.scrollTop-panel.clientHeight<100;
    if(changed){
      panel.replaceChildren();panel.dataset.subagentSession=selected;
      const heading=el('div','subagent-heading');heading.append(el('strong','subagent-title'),el('span','subagent-state'));
      panel.append(heading,el('p','subagent-description'),el('p','subagent-parent'),el('p','subagent-reply-note','Your messages are sent to the main agent.'),el('div','subagent-transcript'));
    }
    panel.querySelector('.subagent-title').textContent=child.title||'Subagent';
    panel.querySelector('.subagent-state').textContent=activityLabel(child.state,rootBusy);
    const description=panel.querySelector('.subagent-description');description.textContent=child.description||'';description.hidden=!child.description;
    const parent=children.get(child.parentSessionId);
    panel.querySelector('.subagent-parent').textContent=parent?`Delegated by ${parent.title||'Subagent'}`:'Delegated by the main agent';
    const area=panel.querySelector('.subagent-transcript');
    for(const item of child.items){
      let entry=child.nodes.get(item.id);
      if(!entry){
        if(item.type==='tool'){entry=el('details','tool-card');entry.append(el('summary'),el('pre'));}
        else{entry=el('div','subagent-message');entry.append(el('strong','',item.role),el('div','subagent-text'));}
        child.nodes.set(item.id,entry);
      }
      if(entry.parentNode!==area)area.append(entry);
      if(item.type==='tool')renderToolActivity(entry,item,rootBusy&&(child.state==='running'||child.state?.state==='running'));
      else{
        entry.querySelector('strong').textContent=item.role;
        const body=entry.querySelector('.subagent-text');
        if(renderMessage)renderMessage(body,item);else body.textContent=item.text;
      }
    }
    if(changed)panel.scrollTop=child.scroll||0;else if(atBottom)panel.scrollTop=panel.scrollHeight;
  }
  function reset(){
    children.clear();lastSeq=0;
    for(const button of tabs?.querySelectorAll('.subagent-tab')||[])button.remove();
    if(panel){panel.replaceChildren();delete panel.dataset.subagentSession;}
  }
  return {consume,sync,select,reset,viewingChild:()=>selected!==null,name:id=>children.get(id)?.title};
}
