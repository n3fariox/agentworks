// Session diff, shown as the Diff tab of the side panel. show(session) loads it
// for that session; hide() drops any in-flight response.
export function setupDiff({api,root}) {
  root.innerHTML=`<p id="diff-context" class="field-note"></p>
    <form id="diff-form" class="diff-controls"><label>Compare<select id="diff-mode"><option value="unstaged">Current unstaged changes</option><option value="branch">Branch against base</option><option value="commit">Specific commit</option></select></label>
    <label id="diff-base-label" hidden>Base revision (up to 500 suggestions)<input id="diff-base" value="main" maxlength="256" list="diff-bases" spellcheck="false" autocomplete="off"></label><datalist id="diff-bases"></datalist>
    <label id="diff-commit-label" hidden>Commit revision<input id="diff-commit" value="HEAD" maxlength="256" spellcheck="false" autocomplete="off"></label><button type="submit" class="text-button">Refresh diff</button></form>
    <p id="diff-status" role="status"></p><p id="diff-comparison" class="field-note"></p>
    <div class="diff-layout"><label id="diff-file-label">Changed files<select id="diff-files" size="8" aria-label="Changed files"></select></label><div class="diff-patch-area"><p id="diff-patch-note" role="status"></p><pre id="diff-patch" tabindex="0" aria-label="Selected file diff"></pre></div></div>
    <p id="diff-handling" class="field-note"></p><details><summary>Staged and untracked files (excluded)</summary><pre id="diff-other"></pre></details>`;
  const $=id=>document.getElementById(id);
  let sessionId=null,generation=0,shown=false;
  function clear(){ $('diff-files').replaceChildren();$('diff-patch').textContent='';$('diff-patch-note').textContent='';$('diff-comparison').textContent='';$('diff-handling').textContent='';$('diff-other').textContent=''; }
  function controls(){
    $('diff-base-label').hidden=$('diff-mode').value!=='branch';
    $('diff-commit-label').hidden=$('diff-mode').value!=='commit';
  }
  async function load(file){
    if(!sessionId)return;
    const current=++generation;
    const params=new URLSearchParams({mode:$('diff-mode').value,base:$('diff-base').value,commit:$('diff-commit').value});
    if(file!==undefined)params.set('file',file);
    clear();$('diff-status').textContent='Loading diff…';
    try{
      const data=await api(`/api/sessions/${encodeURIComponent(sessionId)}/diff?${params}`);
      if(current!==generation||!shown)return;
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
    }catch(error){if(current===generation&&shown)$('diff-status').textContent=error.message;}
  }
  $('diff-mode').addEventListener('change',()=>{controls();load();});
  $('diff-form').addEventListener('submit',event=>{event.preventDefault();load();});
  $('diff-files').addEventListener('change',()=>load($('diff-files').value));
  return {
    show(session){
      shown=true;
      // Reopening the same session keeps the selected file; a new session starts fresh.
      if(session.id===sessionId&&$('diff-files').value)return load($('diff-files').value);
      sessionId=session.id;$('diff-context').textContent=`Session directory: ${session.cwd}`;controls();load();
    },
    hide(){shown=false;generation++;},
  };
}
