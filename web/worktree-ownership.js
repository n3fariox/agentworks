// C-04b: archive/unarchive with owned-worktree cleanup. Removing changes always needs an explicit choice.
export function setupWorktreeOwnership({api,toast}){
  const $=id=>document.getElementById(id);
  const dialog=$('worktree-archive-dialog');
  let pending=null;
  const settle=choice=>{const resolve=pending;pending=null;if(dialog.open)dialog.close();resolve?.(choice);};
  $('worktree-archive-remove').addEventListener('click',()=>settle('remove'));
  $('worktree-archive-keep').addEventListener('click',()=>settle('keep'));
  $('worktree-archive-cancel').addEventListener('click',()=>settle(null));
  $('dismiss-worktree-archive').addEventListener('click',()=>settle(null));
  $('worktree-archive-unused').addEventListener('change',event=>{$('worktree-archive-remove').disabled=!event.target.checked;});
  dialog.addEventListener('close',()=>{if(pending)settle(null);});
  function counts(d){return [[d.staged,'staged'],[d.unstaged,'unstaged'],[d.untracked,'untracked']].filter(([n])=>n).map(([n,l])=>`${n} ${l}`).join(', ');}
  const isDirty=status=>Boolean(status.dirty&&counts(status.dirty));
  // archived: the session is already archived, so the only choice is removing the retained checkout.
  function choose(status,archived=false){
    const dirty=isDirty(status),inUse=status.cleanup.overridable;
    $('worktree-archive-title').textContent=inUse?'Worktree still in use':dirty?'Uncommitted worktree changes':'Remove worktree';
    $('worktree-archive-summary').textContent=dirty?`${status.path} has ${counts(status.dirty)} change(s).`:`${status.path} has no uncommitted changes.`;
    const list=$('worktree-archive-paths');list.replaceChildren();list.hidden=!dirty;
    for(const path of dirty?status.dirty.paths:[]){const item=document.createElement('li');item.textContent=path;list.append(item);}
    if(dirty&&status.dirty.truncated){const item=document.createElement('li');item.textContent='…and more';list.append(item);}
    $('worktree-archive-in-use').hidden=!inUse;
    $('worktree-archive-users').replaceChildren(...status.sharedWith.map(other=>{const item=document.createElement('li');item.textContent=other.title||other.id;return item;}));
    $('worktree-archive-unused').checked=false;
    $('worktree-archive-loss').textContent=dirty?'Deleting removes these staged, unstaged and untracked changes permanently, along with ignored files in the checkout.':'Removing deletes the checkout directory, including ignored files.';
    $('worktree-archive-branch').textContent=status.branch;
    $('worktree-archive-keep').hidden=archived;
    $('worktree-archive-remove').textContent=archived?(dirty?'Remove, delete changes':'Remove worktree'):(dirty?'Archive, delete changes':'Archive, remove worktree');
    $('worktree-archive-remove').disabled=inUse;
    return new Promise(resolve=>{pending=resolve;dialog.showModal();(archived?$('worktree-archive-cancel'):$('worktree-archive-keep')).focus();});
  }
  const removal=status=>({confirmDirty:isDirty(status)?status.fingerprint:undefined,confirmUnused:status.cleanup.overridable?status.sharedWith.map(other=>other.id):undefined});
  // Returns the updated session, or null when the human cancels.
  async function archive(session){
    const path=`/api/sessions/${encodeURIComponent(session.id)}`;
    if(session.archived){
      const updated=await api(path,{method:'PATCH',body:JSON.stringify({archived:false})});
      const restore=updated.worktreeRestore;
      if(restore)toast(`Worktree ${restore.outcome} at ${restore.path} on ${restore.branch}. Resume the session when ready.`,'success');
      return updated;
    }
    let body={archived:true};
    if(session.worktree?.owned&&!session.worktree.removed){
      const status=await api(`${path}/worktree`);
      if((status.cleanup.allowed&&status.cleanup.requiresConfirmation)||status.cleanup.overridable){
        const choice=await choose(status);
        if(!choice)return null;
        body=choice==='remove'?{archived:true,worktree:'remove',...removal(status)}:{archived:true,worktree:'keep'};
      }
    }
    const updated=await api(path,{method:'PATCH',body:JSON.stringify(body)});
    const cleanup=updated.worktreeCleanup;
    if(cleanup?.outcome==='removed'&&cleanup.branch)toast(`Archived. Worktree removed; branch ${cleanup.branch} retained for restore.`,'success');
    else if(cleanup?.outcome==='retained'&&body.worktree!=='keep')toast(`Archived. Worktree kept: ${cleanup.reason}`);
    return updated;
  }
  // Removes the checkout an archived session retained. Returns false when cancelled.
  async function removeRetained(session){
    const path=`/api/sessions/${encodeURIComponent(session.id)}/worktree`;
    const status=await api(path);
    if(!status.cleanup.allowed&&!status.cleanup.overridable)throw new Error(`Worktree not removed: ${status.cleanup.reason||`worktree is ${status.state}`}`);
    if(await choose(status,true)!=='remove')return false;
    const outcome=await api(`${path}/remove`,{method:'POST',body:JSON.stringify(removal(status))});
    toast(outcome.branch?`Worktree removed; branch ${outcome.branch} retained for restore.`:'Worktree removed.','success');
    return true;
  }
  return {archive,removeRetained};
}
