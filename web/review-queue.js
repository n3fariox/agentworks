// Manual coordination ledger: no Git operations or model turns are submitted here.
export function setupReviewQueue({api,uuid,toast}) {
  const panel=document.createElement('details');panel.id='review-queue';
  const node=(tag,text)=>{const n=document.createElement(tag);n.textContent=text;return n;};
  panel.append(node('summary','Review and merge queue'));
  panel.append(node('p','Independent review records. Begin reserves one manual integration slot across all projects. Inspect and merge in your terminal, then record the actual outcome. An unfinished reservation survives restart.'));
  const list=node('div',''),history=node('div',''),error=node('p','');error.setAttribute('role','status');
  const form=document.createElement('form');
  function input(label,key,tag='input',values=[]) {
    const row=node('label',label),field=document.createElement(tag);field.name=key;
    if(tag==='select')for(const value of values){const option=node('option',value);option.value=value;field.append(option);}
    if(tag==='textarea')field.rows=3;
    row.append(field);form.append(row);return field;
  }
  const action=input('Operation','action','select',['propose','review','test','begin','finish','reject']);
  const chosen=input('Queue item','id','select');
  const title=input('Proposal title','title'),branch=input('Source branch','branch'),commits=input('Proposed full commit IDs (one per line)','commits','textarea'),target=input('Target branch','target'),base=input('Reviewed target base (full commit ID)','base');
  const result=input('Result','result','select');
  const commit=input('Resulting integration commit (full ID)','commit'),note=input('Review/test evidence or decision note','note','textarea');
  const submit=node('button','Record operation');submit.type='submit';form.append(submit);
  const refresh=node('button','Refresh queue');refresh.type='button';refresh.className='text-button';
  panel.append(refresh,error,list,form,history);
  document.getElementById('agent-inbox').before(panel);
  let project=null,items=[],pending=null,generation=0,cursor=0,events=[];
  function fields() {
    const a=action.value;
    for(const field of [title,branch,commits,target,base])field.parentElement.hidden=a!=='propose';
    chosen.parentElement.hidden=a==='propose';note.parentElement.hidden=a==='propose';
    result.parentElement.hidden=!['review','test','finish'].includes(a);
    const options=a==='review'?['approved','changesRequested']:a==='test'?['passed','failed']:['integrated','failed','cancelled'];
    if(!options.includes(result.value)||result.dataset.action!==a){result.replaceChildren(...options.map(v=>{const n=node('option',v);n.value=v;return n;}));result.dataset.action=a;}
    commit.parentElement.hidden=a!=='finish'||result.value!=='integrated';
  }
  action.addEventListener('change',fields);result.addEventListener('change',fields);fields();
  async function load(reset=false) {
    if(!project)return;
    const current=project,ticket=generation;
    try {
      const data=await api(`/api/projects/${encodeURIComponent(current)}/review-queue?after=${reset?0:cursor}`);
      if(ticket!==generation)return;
      items=data.items;if(reset)events=[];
      const seen=new Set(events.map(e=>e.id));events.push(...data.events.filter(e=>!seen.has(e.id)));cursor=data.nextCursor;
      list.replaceChildren(...items.map(item=>{
        const card=node('article','');card.className='agent-message';
        card.append(node('strong',`#${item.id} ${item.title} · ${item.state} · revision ${item.revision}`),node('small',`${item.branch} → ${item.target} · base ${item.base}`),node('pre',item.commits.join('\n')),node('small',`Review: ${item.review||'pending'} · Tests: ${item.test||'pending'}`));
        if(item.integrationCommit)card.append(node('small','Integration commit: '+item.integrationCommit));return card;
      }));
      if(!items.length)list.append(node('p','No proposals. Completion claims do not create queue items.'));
      const selected=chosen.value;
      chosen.replaceChildren(...items.map(i=>{const option=node('option',`#${i.id} ${i.title}`);option.value=String(i.id);return option;}));
      if(items.some(i=>String(i.id)===selected))chosen.value=selected;
      history.replaceChildren(node('h4','Audit history'),...events.map(e=>node('pre',`#${e.id} · ${new Date(e.created).toLocaleString()} · ${e.actor} · ${e.requestId}\n${JSON.stringify(e.operation,null,2)}`)));
      if(data.hasMore){const more=node('button','Load more audit history');more.type='button';more.onclick=()=>load();history.append(more);}
    } catch(e) {if(ticket===generation)error.textContent=e.message;}
  }
  refresh.onclick=()=>load(true);
  form.onsubmit=async event=>{
    event.preventDefault();if(!project)return;
    const a=action.value;
    let body={action:a};
    if(a==='propose')Object.assign(body,{title:title.value,branch:branch.value,commits:commits.value.split(/\s+/).filter(Boolean),target:target.value,base:base.value});
    else {
      const item=items.find(i=>String(i.id)===chosen.value);if(!item){error.textContent='Choose a queue item.';return;}
      Object.assign(body,{id:item.id,revision:item.revision,note:note.value});
      if(a==='review'||a==='test')body.result=result.value;
      if(a==='finish')Object.assign(body,{outcome:result.value,commit:result.value==='integrated'?commit.value:null});
    }
    const signature=JSON.stringify({project,body});
    if(pending?.signature!==signature)pending={signature,body:{...body,requestId:uuid()}};
    const ticket=generation,current=project;submit.disabled=true;error.textContent='';
    try {
      await api(`/api/projects/${encodeURIComponent(current)}/review-queue`,{method:'POST',body:JSON.stringify(pending.body)});
      if(ticket!==generation)return;
      pending=null;toast('Review queue updated.','success');await load();
    } catch(e) {if(ticket===generation)error.textContent=e.message+' Retry unchanged input with the same request ID; refresh after a revision conflict.';}
    finally {submit.disabled=false;}
  };
  return {open(id){generation++;project=id;items=[];events=[];cursor=0;pending=null;panel.hidden=!id;list.replaceChildren();history.replaceChildren();error.textContent='';if(id)load();},close(){generation++;project=null;}};
}
