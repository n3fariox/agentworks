export function setupGitContext({api,getActive}) {
  const $=id=>document.getElementById(id);
  const dialog=$('workspace-info-dialog'),fields=$('workspace-info-fields'),status=$('workspace-info-status');
  let generation=0;
  function field(label,value){
    const term=document.createElement('dt'),description=document.createElement('dd');
    term.textContent=label;description.textContent=value??'Unavailable';fields.append(term,description);
  }
  $('dismiss-workspace-info').addEventListener('click',()=>dialog.close());
  dialog.addEventListener('close',()=>{generation++;});
  $('session-workspace').addEventListener('click',async()=>{
    const active=getActive();if(!active)return;
    const current=++generation;
    fields.replaceChildren();status.textContent='Loading workspace…';dialog.showModal();
    try{
      const context=await api(`/api/sessions/${encodeURIComponent(active.id)}/workspace`);
      if(current!==generation||!dialog.open)return;
      fields.replaceChildren();field('Session directory',context.cwd);
      if(context.status==='git'){
        status.textContent=context.head==='detached'?'Detached HEAD':context.linkedWorktree?'Linked Git worktree':'Git worktree';
        field('Branch',context.head==='detached'?'Detached HEAD':context.branch);
        field('Worktree root',context.worktreeRoot);field('Repository root',context.repositoryRoot);
        field('Commit',context.commit||context.message||'Unavailable');
        field('Linked worktree',context.linkedWorktree?'Yes':'No');
      }else{
        status.textContent=context.status==='non_git'?'Not a Git worktree':'Workspace context unavailable';
        field('Details',context.message);
      }
    }catch(error){
      if(current!==generation||!dialog.open)return;
      status.textContent='Workspace context unavailable';field('Details',error.message);
    }
  });
}
