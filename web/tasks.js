// Human project tickets. Identity is the immutable project UUID + task number.
export function setupTasks({api,uuid,getProject,decorate,onWrite}){
  const $=id=>document.getElementById(id),dialog=$('tasks-dialog');
  let project=null,selected=null,generation=0,busy=false,pending=null,loaded=false,loading=false;
  const node=(tag,text)=>{const n=document.createElement(tag);if(text!==undefined)n.textContent=text;return n;};
  function controls(){
    const readonly=!loaded||!!project?.archived;
    for(const id of ['task-title','task-body','task-status','task-save'])$(id).disabled=readonly||busy;
    $('task-delete').disabled=readonly||busy||!selected;
    $('task-new').disabled=readonly||busy;$('tasks-refresh').disabled=busy;$('tasks-show-done').disabled=busy;
    $('tasks-note').textContent=loading?'Loading project tickets…':!loaded?'Tickets unavailable · editing is disabled.':project?.archived?'Archived project · tickets are read-only.':'Project tickets · changes are attributed to you (human).';
  }
  async function preview(){
    const gen=generation,text=$('task-body').value;$('task-preview').textContent=text;
    try{const result=await api('/api/markdown',{method:'POST',body:JSON.stringify({text})});
      if(gen!==generation||text!==$('task-body').value)return;
      // Only the server's sanitized Markdown output is interpreted as HTML.
      $('task-preview').innerHTML=result.html;decorate($('task-preview'));
    }catch{/* Plain text remains available when rendering is unavailable. */}
  }
  function edit(task){
    generation++;selected=task;pending=null;$('task-error').textContent='';
    $('task-heading').textContent=task?task.key:'New ticket';
    $('task-title').value=task?.title||'';$('task-body').value=task?.body||'';$('task-status').value=task?.status||'open';
    $('task-meta').textContent=task?`Revision ${task.revision} · created by ${task.createdBy} · updated ${new Date(task.updated).toLocaleString()} · ${task.uri}`:'';
    controls();preview();
  }
  async function refresh(number=null){
    const gen=++generation,id=project.id;loaded=false;loading=true;controls();$('task-error').textContent='';
    try{const result=await api(`/api/projects/${encodeURIComponent(id)}/tasks?includeDone=${$('tasks-show-done').checked}`);
      if(gen!==generation||!dialog.open)return;
      project={...project,...result.project};$('tasks-title').textContent=`${project.name} · Tasks`;
      $('tasks-list').replaceChildren();
      for(const task of result.tasks){const b=node('button',`${task.key} · ${task.title} · ${task.status}`);b.type='button';b.className='text-button';b.onclick=()=>{if(!busy&&loaded)edit(task);};$('tasks-list').append(b);}
      if(!result.tasks.length)$('tasks-list').append(node('p',$('tasks-show-done').checked?'No tickets yet.':'No open tickets.'));
      if(number!==null){
        const detail=await api(`/api/projects/${encodeURIComponent(id)}/tasks/${number}`);
        if(gen!==generation||!dialog.open)return;
        loaded=true;loading=false;edit(detail.task||detail);
      }
      else{loaded=true;loading=false;edit(null);}
    }catch(e){if(gen===generation){loaded=false;loading=false;$('task-error').textContent=e.message;controls();}}
  }
  async function open(projectId,number=null){
    if(busy)return;
    $('tasks-show-done').checked=false;loaded=false;project=getProject(projectId)||{id:projectId,name:'Project'};selected=null;pending=null;
    if(!dialog.open)dialog.showModal();edit(null);$('tasks-list').replaceChildren();await refresh(number);
  }
  async function mutate(method){
    if(busy||!loaded||!project||project.archived)return;
    const id=project.id,task=selected;
    const payload=method==='DELETE'?{revision:task.revision}:{title:$('task-title').value,body:$('task-body').value,status:$('task-status').value,...(task?{revision:task.revision}:{})};
    const path=`/api/projects/${encodeURIComponent(id)}/tasks${task?'/'+task.id:''}`;
    const fingerprint=JSON.stringify({path,method,payload});
    if(pending?.fingerprint!==fingerprint)pending={fingerprint,requestId:uuid()};
    const gen=generation;busy=true;controls();$('task-error').textContent='';
    try{const result=await api(path,{method,body:JSON.stringify({...payload,requestId:pending.requestId})});
      onWrite(id);
      if(gen!==generation||!dialog.open)return;
      pending=null;busy=false;await refresh(method==='DELETE'?null:(result.task||result).id);
    }catch(e){if(gen===generation)$('task-error').textContent=e.status===409?`Conflict: ${e.message}. Refresh tickets before editing again; unsaved changes will be discarded.`:e.message;}
    finally{busy=false;controls();}
  }
  $('task-form').onsubmit=e=>{e.preventDefault();mutate(selected?'PUT':'POST');};
  $('task-delete').onclick=()=>{if(selected&&confirm(`Delete ${selected.key}?`))mutate('DELETE');};
  $('task-new').onclick=()=>{if(!busy&&loaded)edit(null);};
  $('tasks-show-done').onchange=()=>refresh();
  $('tasks-refresh').onclick=()=>refresh(selected?.id??null);
  $('dismiss-tasks').onclick=()=>dialog.close();dialog.addEventListener('close',()=>{generation++;});
  $('task-body').addEventListener('input',preview);
  return {open};
}
