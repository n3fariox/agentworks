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
  dialog.addEventListener('close',()=>{if(pending)settle(null);});
  function counts(d){return [[d.staged,'staged'],[d.unstaged,'unstaged'],[d.untracked,'untracked']].filter(([n])=>n).map(([n,l])=>`${n} ${l}`).join(', ');}
  function choose(status){
    $('worktree-archive-summary').textContent=`${status.path} has ${counts(status.dirty)} change(s).`;
    const list=$('worktree-archive-paths');list.replaceChildren();
    for(const path of status.dirty.paths){const item=document.createElement('li');item.textContent=path;list.append(item);}
    if(status.dirty.truncated){const item=document.createElement('li');item.textContent='…and more';list.append(item);}
    $('worktree-archive-branch').textContent=status.branch;
    return new Promise(resolve=>{pending=resolve;dialog.showModal();$('worktree-archive-keep').focus();});
  }
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
      if(status.cleanup.allowed&&status.cleanup.requiresConfirmation){
        const choice=await choose(status);
        if(!choice)return null;
        body=choice==='remove'?{archived:true,worktree:'remove',confirmDirty:status.fingerprint}:{archived:true,worktree:'keep'};
      }
    }
    const updated=await api(path,{method:'PATCH',body:JSON.stringify(body)});
    const cleanup=updated.worktreeCleanup;
    if(cleanup?.outcome==='removed'&&cleanup.branch)toast(`Archived. Worktree removed; branch ${cleanup.branch} retained for restore.`,'success');
    else if(cleanup?.outcome==='retained'&&body.worktree!=='keep')toast(`Archived. Worktree kept: ${cleanup.reason}`);
    return updated;
  }
  return {archive};
}
