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
  const orphaned=status=>status.detached?.orphaned||0;
  const plural=(n,word)=>`${n} ${word}${n===1?'':'s'}`;
  const items=(list,values,more)=>{list.replaceChildren(...values.map(text=>{const item=document.createElement('li');item.textContent=text;return item;}));if(more){const item=document.createElement('li');item.textContent='…and more';list.append(item);}list.hidden=!values.length;};
  // archived: the session is already archived, so the only choice is removing the retained checkout.
  function choose(status,archived=false){
    const dirty=isDirty(status),lost=orphaned(status),inUse=status.cleanup.overridable,risky=dirty||lost>0;
    $('worktree-archive-title').textContent=inUse?'Worktree still in use':dirty&&lost?'Unsaved worktree work':dirty?'Uncommitted worktree changes':lost?'Commits on no branch':'Remove worktree';
    const parts=[dirty&&`${counts(status.dirty)} change(s)`,lost&&`a detached HEAD (${status.detached.head.slice(0,10)}) with ${plural(lost,'commit')} on no branch`].filter(Boolean);
    $('worktree-archive-summary').textContent=parts.length?`${status.path} has ${parts.join(' and ')}.`:`${status.path} has no uncommitted changes.`;
    items($('worktree-archive-paths'),dirty?status.dirty.paths:[],dirty&&status.dirty.truncated);
    items($('worktree-archive-commits'),lost?status.detached.commits:[],lost>status.detached?.commits.length);
    $('worktree-archive-in-use').hidden=!inUse;
    $('worktree-archive-users').replaceChildren(...status.sharedWith.map(other=>{const item=document.createElement('li');item.textContent=other.title||other.id;return item;}));
    $('worktree-archive-unused').checked=false;
    $('worktree-archive-loss').textContent=[dirty&&'Deleting removes these staged, unstaged and untracked changes permanently, along with ignored files in the checkout.',lost&&`The listed commits are on no branch; after removal they can be recovered only by hash (git branch <name> ${status.detached.head.slice(0,10)}) until Git garbage-collects them.`,!risky&&'Removing deletes the checkout directory, including ignored files.'].filter(Boolean).join(' ');
    // A detached HEAD is not a branch: only the recorded branch is kept for restore, not the detached commits.
    const code=document.createElement('code');code.id='worktree-archive-branch';code.textContent=status.checkedOut||status.branch;
    $('worktree-archive-kept').replaceChildren(...(status.detached
      ?['HEAD is detached; recorded branch ',code,' is kept for restore, but commits only on the detached HEAD are not. Nothing is merged.']
      :['Branch ',code,' and its commits are kept either way; nothing is merged.']));
    $('worktree-archive-keep').hidden=archived;
    $('worktree-archive-remove').textContent=archived?(risky?'Remove, discard work':'Remove worktree'):(risky?'Archive, discard work':'Archive, remove worktree');
    $('worktree-archive-remove').disabled=inUse;
    return new Promise(resolve=>{pending=resolve;dialog.showModal();(archived?$('worktree-archive-cancel'):$('worktree-archive-keep')).focus();});
  }
  const discarded=outcome=>outcome.discardedHead?` Detached commits are recoverable by hash: ${outcome.discardedHead}.`:'';
  const removal=status=>({confirmDirty:status.cleanup.requiresConfirmation?status.fingerprint:undefined,confirmUnused:status.cleanup.overridable?status.sharedWith.map(other=>other.id):undefined});
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
    if(cleanup?.outcome==='removed'&&cleanup.branch)toast(`Archived. Worktree removed; branch ${cleanup.branch} retained for restore.${discarded(cleanup)}`,'success');
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
    toast((outcome.branch?`Worktree removed; branch ${outcome.branch} retained for restore.`:'Worktree removed.')+discarded(outcome),'success');
    return true;
  }
  return {archive,removeRetained};
}
