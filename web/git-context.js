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
      const owner=await api(`/api/sessions/${encodeURIComponent(active.id)}/worktree`).catch(()=>null);
      if(current!==generation||!dialog.open||!owner)return;
      field('Ownership',owner.owned?`Created by this session (${owner.state})`:'Not created by this session; never removed by Agentworks');
      if(owner.dirty){const d=owner.dirty,total=d.staged+d.unstaged+d.untracked;field('Changes',total?`${d.staged} staged, ${d.unstaged} unstaged, ${d.untracked} untracked`:'Clean');}
      if(owner.owned)field('Archive cleanup',owner.cleanup.allowed?(owner.cleanup.requiresConfirmation?'Asks before deleting changes':'Removed automatically on archive; branch kept'):owner.cleanup.reason);
      if(owner.population)field('Worktree contents',owner.population);
      if(owner.hooks){
        field('Enter hooks',owner.hooks.enter);field('Exit hooks',owner.hooks.exit);
        if(owner.hooks.lastMessage)field('Hook output',owner.hooks.lastMessage);
        for(const event of ['enter','exit'])if(['failed','running'].includes(owner.hooks[event])){
          const button=document.createElement('button');button.type='button';button.className='text-button';button.textContent=`Retry ${event} hooks`;
          button.onclick=async()=>{button.disabled=true;try{await api(`/api/sessions/${encodeURIComponent(active.id)}/worktree/hooks/retry`,{method:'POST',body:JSON.stringify({event})});dialog.close();$('session-workspace').click();}catch(error){status.textContent=error.message;}finally{button.disabled=false;}};
          const term=document.createElement('dt'),description=document.createElement('dd');term.textContent='Recovery';description.append(button);fields.append(term,description);
        }
      }
      field('Integration',owner.integration.note);
    }catch(error){
      if(current!==generation||!dialog.open)return;
      status.textContent='Workspace context unavailable';field('Details',error.message);
    }
  });
}
