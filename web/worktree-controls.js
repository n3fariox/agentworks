// Human-only workspace controls. Discovery never starts a harness.
export function setupWorktreeControls({$,api,onChange}) {
  const defaults={defaultMode:'directory',requireWorktree:false};
  const names={directory:'Use project directory',newWorktree:'Create new worktree',existingWorktree:'Use existing worktree'};
  let hostId='local';
  let policy={...defaults},policyGeneration=0,listGeneration=0,ready=true,loading=false,policyLoad=Promise.resolve();
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
  // `preferred` lets a caller suggest a mode (forking suggests its own worktree).
  // Project policy still wins: a required worktree is never downgraded to a directory.
  async function loadPolicy(preferred){
    const gen=++policyGeneration,id=$('create-project').value;
    ready=false;policy={...defaults};$('workspace-mode').value='directory';
    $('worktree-branch').value='';$('worktree-base').value='';clearList();
    $('workspace-policy-note').textContent=id?'Loading project workspace policy…':'Default: Use project directory';render();
    try{
      const result=id?await api(`/api/projects/${encodeURIComponent(id)}/workspace-policy`):defaults;
      if(gen!==policyGeneration)return;
      policy=result;ready=true;
      $('workspace-mode').value=preferred&&names[preferred]&&!(policy.requireWorktree&&preferred==='directory')?preferred:policy.defaultMode;
      $('workspace-policy-note').textContent=`Project default: ${names[policy.defaultMode]}${policy.requireWorktree?' · Isolated worktree required':''}`;render();
    }catch(error){if(gen===policyGeneration){$('workspace-policy-note').textContent=`Cannot load workspace policy: ${error.message}`;render();}}
  }
  function projectChanged(preferred=null){policyLoad=loadPolicy(preferred);return policyLoad;}
  $('workspace-mode').onchange=render;
  $('directory').addEventListener('input',()=>{clearList();render();});
  $('existing-worktree').onchange=render;
  $('discover-worktrees').onclick=async()=>{
    clearList();const gen=listGeneration,cwd=$('directory').value;
    loading=true;$('discover-worktrees').disabled=true;$('worktree-discovery-note').textContent='Discovering worktrees…';render();
    try{
      const result=await api('/api/worktrees/list',{method:'POST',body:JSON.stringify({cwd,...(hostId!=='local'?{hostId}:{})})});
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
    setHost(value){
      hostId=value||'local';
      clearList();render();
    },
    // Resolves once the current project policy has settled, so a caller that fills
    // in fields afterwards cannot race the mode it suggested.
    settled:()=>policyLoad,
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

// The global form saves defaults; the repository editor has its own explicit save.
export function setupGitSettings({$,api,picker,getHosts,getLocations,getActive}) {
  let loaded=null,generation=0,completionGeneration=0,completionTimer;
  const input=$('git-repo-cwd'),suggestions=$('git-repo-paths');
  const invalidate=()=>{generation++;loaded=null;$('git-repo-editor').hidden=true;$('git-repo-status').textContent='';};
  function clearSuggestions(){completionGeneration++;clearTimeout(completionTimer);suggestions.replaceChildren();}
  function showSuggestions(paths){
    suggestions.replaceChildren(...[...new Set(paths)].filter(Boolean).slice(0,100).map(path=>new Option(path,path)));
  }
  async function complete(){
    const current=++completionGeneration,hostId=$('git-repo-host').value,value=input.value.trim();
    const known=getLocations().filter(location=>(location.hostId||'local')===hostId).map(location=>location.path).filter(path=>path?.startsWith(value));
    showSuggestions(known);
    const slash=Math.max(value.lastIndexOf('/'),value.lastIndexOf('\\'));
    const parent=slash>=0?value.slice(0,slash+1):'';
    const query=new URLSearchParams({...(hostId!=='local'?{hostId}:{}),...(parent?{path:parent}:{}),...(value.slice(slash+1).startsWith('.')?{hidden:'true'}:{})});
    try{
      const listing=await api(`/api/directories?${query}`);
      if(current!==completionGeneration)return;
      showSuggestions([...known,...listing.entries.map(entry=>entry.path)]);
    }catch{
      if(current!==completionGeneration)return;
      // A partially typed parent may not exist yet or may be above an allowed root.
      try{
        const listing=await api(`/api/directories?${new URLSearchParams(hostId!=='local'?{hostId}:{})}`);
        if(current===completionGeneration)showSuggestions([...known,...listing.entries.map(entry=>entry.path)]);
      }catch{/* Browsing/loading still provides actionable host/path errors. */}
    }
  }
  function scheduleCompletion(){clearSuggestions();completionTimer=setTimeout(complete,150);}
  input.addEventListener('input',()=>{invalidate();scheduleCompletion();});
  input.addEventListener('focus',()=>{clearSuggestions();complete();});
  $('git-repo-host').addEventListener('change',()=>{input.value='';invalidate();clearSuggestions();complete();});
  $('git-repo-browse').onclick=async()=>{
    const current=generation,hostId=$('git-repo-host').value,host=getHosts().find(host=>host.id===hostId);
    const path=await picker.pick({hostId,hostName:host?.name||'Local',start:input.value.trim()});
    if(!path||current!==generation)return;
    input.value=path;invalidate();clearSuggestions();await load();
  };
  async function load(){
    const current=++generation;loaded=null;$('git-repo-editor').hidden=true;$('git-repo-status').textContent='Loading repository configuration…';
    const hostId=$('git-repo-host').value,cwd=$('git-repo-cwd').value.trim();
    try{
      const result=await api('/api/git/config',{method:'POST',body:JSON.stringify({hostId,cwd})});
      if(current!==generation)return;
      loaded={...result,hostId,cwd};$('git-repo-text').value=result.text||'version = 1\n\n[git.worktrees]\ncopy = "prefer-cow"\n';
      $('git-repo-editor').hidden=false;
      $('git-repo-effective').textContent=`Saved effective settings: ${result.effective.copy}; shared directories: ${result.effective.share.join(', ')||'none'}; hooks ${result.effective.runHooks?'enabled':'disabled'}.`;
      $('git-repo-status').textContent=result.path;
    }catch(error){if(current===generation)$('git-repo-status').textContent=error.message;}
  }
  $('git-repo-load').onclick=load;
  $('git-repo-save').onclick=async()=>{
    if(!loaded)return;
    const selected=loaded,current=generation,text=$('git-repo-text').value;
    $('git-repo-save').disabled=true;$('git-repo-status').textContent='Saving repository configuration…';
    try{
      const result=await api('/api/git/config',{method:'POST',body:JSON.stringify({hostId:selected.hostId,cwd:selected.cwd,text,fingerprint:selected.fingerprint})});
      if(current!==generation)return;
      loaded={...result,hostId:selected.hostId,cwd:selected.cwd};
      $('git-repo-status').textContent=`Saved ${result.path}. Applies to newly created session worktrees.`;
      $('git-repo-effective').textContent=`Saved effective settings: ${result.effective.copy}; shared directories: ${result.effective.share.join(', ')||'none'}; hooks ${result.effective.runHooks?'enabled':'disabled'}.`;
    }catch(error){if(current===generation)$('git-repo-status').textContent=error.message;}
    finally{$('git-repo-save').disabled=false;}
  };
  return {
    populate(settings){
      $('git-copy').value=settings?.copy||'plain';$('git-share').value=(settings?.share||[]).join('\n');$('git-run-hooks').checked=!!settings?.runHooks;
      const active=getActive();$('git-repo-host').replaceChildren(new Option('Local','local'));
      for(const host of getHosts().filter(h=>h.id!=='local'))$('git-repo-host').append(new Option(host.name,host.id));
      $('git-repo-host').value=active?.hostId||'local';$('git-repo-cwd').value=active?.cwd||'';invalidate();clearSuggestions();complete();
    },
    value(){return {copy:$('git-copy').value,share:$('git-share').value.split('\n').map(s=>s.trim()).filter(Boolean),runHooks:$('git-run-hooks').checked};},
    repository(cwd,hostId='local'){$('git-repo-cwd').value=cwd;$('git-repo-host').value=hostId;invalidate();clearSuggestions();complete();if(cwd)return load();},
  };
}
