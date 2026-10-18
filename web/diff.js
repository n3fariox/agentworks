export function setupDiff({api,getActive}) {
  const dialog=document.createElement('dialog');dialog.id='diff-dialog';dialog.setAttribute('aria-labelledby','diff-title');
  dialog.innerHTML=`<div class="dialog-heading"><h2 id="diff-title">Session diff</h2><button id="dismiss-diff" class="icon-button" aria-label="Close diff">×</button></div>
    <p id="diff-context" class="field-note"></p>
    <form id="diff-form" class="diff-controls"><label>Compare<select id="diff-mode"><option value="unstaged">Current unstaged changes</option><option value="branch">Branch against base</option><option value="commit">Specific commit</option></select></label>
    <label id="diff-base-label" hidden>Base revision (up to 500 suggestions)<input id="diff-base" value="main" maxlength="256" list="diff-bases" spellcheck="false" autocomplete="off"></label><datalist id="diff-bases"></datalist>
    <label id="diff-commit-label" hidden>Commit revision<input id="diff-commit" value="HEAD" maxlength="256" spellcheck="false" autocomplete="off"></label><button type="submit" class="text-button">Refresh diff</button></form>
    <p id="diff-status" role="status"></p><p id="diff-comparison" class="field-note"></p>
    <div class="diff-layout"><label id="diff-file-label">Changed files<select id="diff-files" size="8" aria-label="Changed files"></select></label><div class="diff-patch-area"><p id="diff-patch-note" role="status"></p><pre id="diff-patch" tabindex="0" aria-label="Selected file diff"></pre></div></div>
    <p id="diff-handling" class="field-note"></p><details><summary>Staged and untracked files (excluded)</summary><pre id="diff-other"></pre></details>`;
  document.body.append(dialog);
  const $=id=>document.getElementById(id);
  let sessionId=null,generation=0;
  function clear(){ $('diff-files').replaceChildren();$('diff-patch').textContent='';$('diff-patch-note').textContent='';$('diff-comparison').textContent='';$('diff-handling').textContent='';$('diff-other').textContent=''; }
  function controls(){
    $('diff-base-label').hidden=$('diff-mode').value!=='branch';
    $('diff-commit-label').hidden=$('diff-mode').value!=='commit';
  }
  async function load(file){
    const current=++generation;
    const params=new URLSearchParams({mode:$('diff-mode').value,base:$('diff-base').value,commit:$('diff-commit').value});
    if(file!==undefined)params.set('file',file);
    clear();$('diff-status').textContent='Loading diff…';
    try{
      const data=await api(`/api/sessions/${encodeURIComponent(sessionId)}/diff?${params}`);
      if(current!==generation||!dialog.open)return;
      const c=data.context;
      $('diff-context').textContent=`Session directory: ${c.cwd}\nWorktree: ${c.worktreeRoot||'Unavailable'}\nBranch: ${c.branch||c.head||'Unavailable'} · HEAD at context inspection: ${c.commit||'No commit'}`;
      if(data.unavailable){$('diff-status').textContent=c.message||data.unavailable;return;}
      $('diff-status').textContent=data.files.length?`${data.files.length} changed file(s)`:'No changes in this comparison.';
      $('diff-comparison').textContent=data.mode==='unstaged'?'Index → current worktree (unstaged only)':data.mode==='branch'?`Merge base ${data.mergeBase} → HEAD ${data.to} (selected base: ${data.base||'main'} = ${data.resolvedBase})`:`${data.rootCommit?'Empty tree':`First parent ${data.from}`} → commit ${data.to}`;
      $('diff-bases').replaceChildren(...data.bases.map(value=>{const option=document.createElement('option');option.value=value;return option;}));
      $('diff-files').replaceChildren(...data.files.map(value=>{const option=document.createElement('option');option.value=value;option.textContent=value;return option;}));
      $('diff-files').value=file??'';
      $('diff-handling').textContent=data.handling;
      $('diff-other').textContent=[['Staged',data.staged],['Untracked',data.untracked]].map(([name,value])=>`${name}:\n${value.files.join('\n')||'None'}${value.unavailable?'\n'+value.unavailable:''}`).join('\n\n');
      $('diff-patch-note').textContent=data.patchUnavailable?`${data.patchUnavailable}. Preview unavailable (256 KiB maximum; binary contents are not rendered).`:file!==undefined?(data.patch?'Read-only patch · maximum 256 KiB · binary contents are not rendered.':'No patch remains; files may have changed during inspection. Refresh diff.'):data.files.length?'Choose a file to read its patch.':'';
      $('diff-patch').textContent=data.patch||'';
    }catch(error){if(current===generation&&dialog.open)$('diff-status').textContent=error.message;}
  }
  $('session-diff').addEventListener('click',()=>{
    const active=getActive();if(!active)return;
    sessionId=active.id;$('diff-title').textContent=`Session diff · ${active.title||'Untitled'}`;
    $('diff-context').textContent=`Session directory: ${active.cwd}`;controls();dialog.showModal();load();
  });
  $('dismiss-diff').addEventListener('click',()=>dialog.close());
  dialog.addEventListener('close',()=>{generation++;});
  $('diff-mode').addEventListener('change',()=>{controls();load();});
  $('diff-form').addEventListener('submit',event=>{event.preventDefault();load();});
  $('diff-files').addEventListener('change',()=>load($('diff-files').value));
}
