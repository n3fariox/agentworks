// Adapter-owned jobs outlive individual tool calls and turns. The panel lists only jobs
// whose outcome is unresolved; an `async_task_finished` event moves a job's outcome into
// the transcript work log (its `resultSeq`), with no separate acknowledgement.
const terminal=state=>['completed','failed','stopped','cancelled'].includes(state);
const activeJob=job=>!terminal(job.state)&&!Number.isSafeInteger(job.resultSeq);
const key=job=>JSON.stringify([job.acpSessionId,job.asyncTaskId]);
const label=state=>({running:'Running',pending:'Pending',paused:'Paused',stopping:'Stopping',completed:'Completed',failed:'Failed',stopped:'Stopped',cancelled:'Cancelled',unknown:'Status unknown'}[state]||'Status unknown');
// Only what the adapter reported: Claude gives a summary, usage and an output file path;
// Codex gives the state, and the command output stays on the launching tool call.
export function jobResultLines(job){
  const lines=[];
  const text=job.summary||(job.description&&job.description!==job.name?job.description:'');
  if(text)lines.push(text);
  if(job.reason==='connection_ended')lines.push('The agent process ended before this job reported an outcome. It may have stopped with the process or kept running unobserved.');
  if(job.outputFilePath)lines.push(`Output file: ${job.outputFilePath}`);
  const u=job.usage;
  if(u&&typeof u==='object'){
    const parts=[Number.isFinite(u.totalTokens)?`${u.totalTokens.toLocaleString('en-US')} tokens`:'',Number.isFinite(u.toolUses)?`${u.toolUses} tool use${u.toolUses===1?'':'s'}`:'',Number.isFinite(u.durationMs)?`${Math.round(u.durationMs/1000)}s`:''].filter(Boolean);
    if(parts.length)lines.push(parts.join(' · '));
  }
  if(job.progress)lines.push(typeof job.progress==='string'?job.progress:JSON.stringify(job.progress));
  return lines;
}
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
    if(event.kind==='async_task_finished'){
      const id=key(d);jobs.set(id,{...jobs.get(id),...d,resultSeq:event.seq});return;
    }
    if(event.kind!=='acp'||!d.sessionUpdate?.startsWith('async_task_'))return;
    const id=key(d),existing=jobs.get(id);
    if(d.sessionUpdate==='async_task_spawned'){
      // Relaunching a job whose earlier outcome was unknown makes it active again.
      const job={...existing,...d,state:terminal(existing?.state)?existing.state:'running'};
      if(!terminal(job.state))delete job.resultSeq;
      jobs.set(id,job);
    }else if(existing){
      const old=existing.state;Object.assign(existing,d);if(terminal(old))existing.state=old;
    }
  }
  function forTool(tool){return [...jobs.values()].find(job=>job.toolCallId===tool.toolCallId&&job.acpSessionId===tool.acpSessionId);}
  function render(){
    const live=[...jobs].filter(([,job])=>activeJob(job)),unknown=live.filter(([,j])=>j.state==='unknown').length;
    root.hidden=!live.length;
    summary.textContent='Background jobs · '+[live.length>unknown?`${live.length-unknown} active`:'',unknown?`${unknown} unknown`:''].filter(Boolean).join(' · ');
    const current=new Set();
    for(const [id,job]of live.reverse()){
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
