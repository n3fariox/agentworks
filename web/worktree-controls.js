// Human-only workspace controls. Discovery never starts a harness.
export function setupWorktreeControls({$,api,onChange}) {
  const defaults={defaultMode:'directory',requireWorktree:false};
  const names={directory:'Use project directory',newWorktree:'Create new worktree',existingWorktree:'Use existing worktree'};
  let policy={...defaults},policyGeneration=0,listGeneration=0,ready=true,loading=false;
  function render(){
    const mode=$('workspace-mode').value;
    document.querySelector('label[for=directory]').textContent=mode==='directory'?'Working directory':'Source directory';
    $('workspace-new-fields').hidden=mode!=='newWorktree';
    $('workspace-existing-fields').hidden=mode!=='existingWorktree';
    $('workspace-mode').querySelector('[value="directory"]').disabled=policy.requireWorktree;
    $('workspace-mode').disabled=!ready;
    onChange();
  }
  function clearList(){
    listGeneration++;loading=false;$('discover-worktrees').disabled=false;
    $('existing-worktree').replaceChildren(new Option('Discover and select a linked worktree',''));
    $('worktree-discovery-note').textContent='Main, locked and prunable worktrees cannot be selected.';
  }
  async function projectChanged(){
    const gen=++policyGeneration,id=$('create-project').value;
    ready=false;policy={...defaults};$('workspace-mode').value='directory';
    $('worktree-branch').value='';$('worktree-base').value='';clearList();
    $('workspace-policy-note').textContent=id?'Loading project workspace policy…':'Default: Use project directory';render();
    try{
      const result=id?await api(`/api/projects/${encodeURIComponent(id)}/workspace-policy`):defaults;
      if(gen!==policyGeneration)return;
      policy=result;ready=true;$('workspace-mode').value=policy.defaultMode;
      $('workspace-policy-note').textContent=`Project default: ${names[policy.defaultMode]}${policy.requireWorktree?' · Isolated worktree required':''}`;render();
    }catch(error){if(gen===policyGeneration){$('workspace-policy-note').textContent=`Cannot load workspace policy: ${error.message}`;render();}}
  }
  $('workspace-mode').onchange=render;
  $('directory').addEventListener('input',()=>{clearList();render();});
  $('existing-worktree').onchange=render;
  $('discover-worktrees').onclick=async()=>{
    clearList();const gen=listGeneration,cwd=$('directory').value;
    loading=true;$('discover-worktrees').disabled=true;$('worktree-discovery-note').textContent='Discovering worktrees…';render();
    try{
      const result=await api('/api/worktrees/list',{method:'POST',body:JSON.stringify({cwd})});
      if(gen!==listGeneration)return;
      for(const tree of result.worktrees){
        const reason=tree.isMain?'main checkout':tree.prunable?'prunable':tree.locked?'locked':'';
        const option=new Option(`${tree.branch||'Detached HEAD'} · ${tree.path}${reason?' · '+reason:''}`,tree.path);
        option.disabled=!!reason;$('existing-worktree').append(option);
      }
      $('worktree-discovery-note').textContent=result.worktrees.length?'Select an eligible linked worktree.':'No worktrees found inside allowed roots.';
    }catch(error){if(gen===listGeneration)$('worktree-discovery-note').textContent=error.message;}
    finally{if(gen===listGeneration){loading=false;$('discover-worktrees').disabled=false;render();}}
  };
  return {
    projectChanged,
    blocked:()=>!ready||loading||($('workspace-mode').value==='existingWorktree'&&!$('existing-worktree').value),
    submission(){
      if(this.blocked())throw new Error('Load the project policy and select an eligible workspace first.');
      const mode=$('workspace-mode').value,workspace={mode};let cwd=$('directory').value;
      if(mode==='newWorktree'){
        if($('worktree-branch').value.trim())workspace.branch=$('worktree-branch').value.trim();
        if($('worktree-base').value.trim())workspace.baseRef=$('worktree-base').value.trim();
      }
      if(mode==='existingWorktree')cwd=$('existing-worktree').value;
      return {cwd,workspace};
    },
  };
}

export function setupPolicyEditor({$,api}) {
  let generation=0,ready=true;
  function enforce(){
    $('project-default-mode').querySelector('[value="directory"]').disabled=$('project-require-worktree').checked;
    if($('project-require-worktree').checked&&$('project-default-mode').value==='directory')$('project-default-mode').value='newWorktree';
  }
  $('project-require-worktree').onchange=enforce;
  return {
    async open(id){
      const gen=++generation;ready=!id;
      $('project-default-mode').value='directory';$('project-require-worktree').checked=false;enforce();
      $('save-project').disabled=!!id;$('project-default-mode').disabled=!!id;$('project-require-worktree').disabled=!!id;
      if(!id)return;
      try{
        const policy=await api(`/api/projects/${encodeURIComponent(id)}/workspace-policy`);
        if(gen!==generation)return;
        $('project-default-mode').value=policy.defaultMode;$('project-require-worktree').checked=policy.requireWorktree;ready=true;enforce();
      }catch(error){if(gen===generation)$('project-error').textContent=`Cannot load workspace policy: ${error.message}. Close and reopen to retry.`;}
      finally{if(gen===generation){$('save-project').disabled=!ready;$('project-default-mode').disabled=!ready;$('project-require-worktree').disabled=!ready;}}
    },
    value(){if(!ready)throw new Error('Workspace policy has not loaded.');return {defaultMode:$('project-default-mode').value,requireWorktree:$('project-require-worktree').checked};},
  };
}
