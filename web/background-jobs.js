// Adapter-owned jobs outlive individual tool calls and turns.
const terminal=state=>['completed','failed','stopped','cancelled'].includes(state);
const key=job=>JSON.stringify([job.acpSessionId,job.asyncTaskId]);
const label=state=>({running:'Running',pending:'Pending',stopping:'Stopping',completed:'Completed',failed:'Failed',stopped:'Stopped',cancelled:'Cancelled',unknown:'Status unknown'}[state]||'Status unknown');
export function createBackgroundJobs({root,api,getActive,isConnected,onChange=()=>{}}){
  let jobs=new Map(),supported=false,revision=0;
  const pending=new Set(),errors=new Map(),rows=new Map();
  const summary=document.createElement('summary'),list=document.createElement('div');
  list.className='background-job-list';list.setAttribute('aria-label','Background jobs');root.append(summary,list);
  const text=(tag,className)=>{const node=document.createElement(tag);node.className=className;return node;};
  function reset(){revision++;jobs.clear();supported=false;pending.clear();errors.clear();rows.clear();list.replaceChildren();root.open=false;render();}
  function set(snapshot){supported=snapshot?.supported===true;jobs=new Map((snapshot?.tasks||[]).map(job=>[key(job),{...job}]));render();}
  function consume(event){
    const d=event.data||{};
    if(event.kind==='async_task_connection'){
      supported=d.supported===true;
      for(const job of jobs.values())if(!terminal(job.state))job.state='unknown';
      return;
    }
    if(event.kind!=='acp'||!d.sessionUpdate?.startsWith('async_task_'))return;
    const id=key(d),existing=jobs.get(id);
    if(d.sessionUpdate==='async_task_spawned'){
      jobs.set(id,{...existing,...d,state:terminal(existing?.state)?existing.state:'running'});
    }else if(existing){
      const old=existing.state;Object.assign(existing,d);if(terminal(old))existing.state=old;
    }
  }
  function forTool(tool){return [...jobs.values()].find(job=>job.toolCallId===tool.toolCallId&&job.acpSessionId===tool.acpSessionId);}
  function render(){
    root.hidden=!jobs.size;
    const live=[...jobs.values()].filter(j=>!terminal(j.state)),unknown=live.filter(j=>j.state==='unknown').length;
    summary.textContent='Background jobs · '+[live.length>unknown?`${live.length-unknown} active`:'',unknown?`${unknown} unknown`:'',!live.length?`${jobs.size} finished`:''].filter(Boolean).join(' · ');
    const current=new Set();
    for(const [id,job]of [...jobs].reverse()){
      current.add(id);let row=rows.get(id);
      if(!row){
        const node=text('div','background-job'),name=text('strong','background-job-name'),state=text('span','background-job-state'),description=text('div','background-job-description'),detail=text('div','background-job-detail'),button=text('button','text-button'),error=text('div','error-text');
        button.type='button';error.setAttribute('role','alert');node.append(name,state,description,detail,button,error);row={node,name,state,description,detail,button,error};rows.set(id,row);
      }
      list.append(row.node);row.name.textContent=job.name||'Background job';row.state.textContent=label(job.state);
      row.description.textContent=job.summary||job.description||'';row.description.hidden=!row.description.textContent;
      row.detail.textContent=[job.parentSessionId?'Subagent job':'',job.outputFilePath?`Output: ${job.outputFilePath}`:'',job.progress?typeof job.progress==='string'?job.progress:JSON.stringify(job.progress):''].filter(Boolean).join(' · ');row.detail.hidden=!row.detail.textContent;
      row.button.hidden=!job.canStop||terminal(job.state);row.button.disabled=!supported||!isConnected()||job.state!=='running'||pending.has(id);
      row.button.textContent=pending.has(id)?'Stopping…':'Stop';row.button.setAttribute('aria-label',`Stop ${job.name||'background job'}`);row.error.textContent=errors.get(id)||'';
      row.button.onclick=async()=>{
        const session=getActive()?.id,version=revision;if(!session||pending.has(id))return;
        pending.add(id);errors.delete(id);render();
        try{
          const result=await api(`/api/sessions/${session}/async-tasks/stop`,{method:'POST',body:JSON.stringify({asyncTaskId:job.asyncTaskId,acpSessionId:job.acpSessionId})});
          if(version===revision&&!result.stopped)errors.set(id,'The adapter did not stop this job. It may already have finished.');
        }catch(e){if(version===revision)errors.set(id,e.message);}
        finally{if(version===revision){pending.delete(id);render();onChange();}}
      };
    }
    for(const [id,row]of rows)if(!current.has(id)){row.node.remove();rows.delete(id);}
  }
  return {reset,set,consume,render,forTool,label,snapshot:()=>({supported,tasks:[...jobs.values()]})};
}
