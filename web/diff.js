// Session diff, shown as the Diff tab of the side panel. show(session) loads it
// for that session; hide() drops any in-flight response.
export function setupDiff({api,root,enhanceBlocks}) {
  root.innerHTML=`<p id="diff-context" class="field-note"></p>
    <form id="diff-form" class="diff-controls"><label>Compare<select id="diff-mode"><option value="unstaged">Current unstaged changes</option><option value="branch">Branch against base</option><option value="commit">Specific commit</option></select></label>
    <label id="diff-base-label" hidden>Base revision (up to 500 suggestions)<input id="diff-base" value="main" maxlength="256" list="diff-bases" spellcheck="false" autocomplete="off"></label><datalist id="diff-bases"></datalist>
    <label id="diff-commit-label" hidden>Commit revision<input id="diff-commit" value="HEAD" maxlength="256" spellcheck="false" autocomplete="off"></label><button type="submit" class="text-button">Refresh diff</button></form>
    <p id="diff-status" role="status"></p><p id="diff-comparison" class="field-note"></p>
    <div class="diff-layout"><nav id="diff-file-label" aria-label="Changed files"><h3>Changed files</h3><div id="diff-files"></div></nav><div class="diff-patch-area"><p id="diff-patch-note" role="status"></p><pre id="diff-patch" tabindex="0" aria-label="Selected file diff"></pre></div></div>
    <p id="diff-handling" class="field-note"></p><details><summary>Staged and untracked files (excluded)</summary><pre id="diff-other"></pre></details>`;
  const $=id=>document.getElementById(id);
  let sessionId=null,generation=0,shown=false,selectedFile=null;
  const collapsed=new Set();
  function renderFiles(files){
    const tree=new Map();
    for(const file of files){
      const parts=file.split('/');let children=tree;
      parts.forEach((name,index)=>{
        if(!children.has(name))children.set(name,{name,path:parts.slice(0,index+1).join('/'),children:new Map(),file:index===parts.length-1?file:null});
        children=children.get(name).children;
      });
    }
    function branch(children){
      const list=document.createElement('ul');
      for(let node of [...children.values()].sort((a,b)=>Number(!!b.children.size)-Number(!!a.children.size)||a.name.localeCompare(b.name))){
        let name=node.name;
        // Compact directory-only chains without losing the exact file path.
        while(node.children.size===1){
          const child=node.children.values().next().value;
          if(!child.children.size)break;
          name+='/'+child.name;node=child;
        }
        const item=document.createElement('li');
        if(node.children.size){
          const folder=document.createElement('details'),label=document.createElement('summary');
          folder.open=!collapsed.has(node.path);label.textContent=name;label.title=node.path;
          folder.addEventListener('toggle',()=>{if(folder.open)collapsed.delete(node.path);else collapsed.add(node.path);});
          folder.append(label,branch(node.children));item.append(folder);
        }else{
          const button=document.createElement('button');button.type='button';button.textContent=node.name;button.title=node.file;button.dataset.path=node.file;
          button.setAttribute('aria-current',String(node.file===selectedFile));button.onclick=()=>load(node.file);item.append(button);
        }
        list.append(item);
      }
      return list;
    }
    $('diff-files').replaceChildren(branch(tree));
  }
  function clear(){ $('diff-files').replaceChildren();const patch=$('diff-patch'),block=patch.closest('.code-block');if(block)block.replaceWith(patch);patch.textContent='';$('diff-patch-note').textContent='';$('diff-comparison').textContent='';$('diff-handling').textContent='';$('diff-other').textContent=''; }
  function controls(){
    $('diff-base-label').hidden=$('diff-mode').value!=='branch';
    $('diff-commit-label').hidden=$('diff-mode').value!=='commit';
  }
  async function load(file){
    if(!sessionId)return;
    const current=++generation;selectedFile=file??null;
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
      renderFiles(data.files);
      $('diff-handling').textContent=data.handling;
      $('diff-other').textContent=[['Staged',data.staged],['Untracked',data.untracked]].map(([name,value])=>`${name}:\n${value.files.join('\n')||'None'}${value.unavailable?'\n'+value.unavailable:''}`).join('\n\n');
      $('diff-patch-note').textContent=data.patchUnavailable?`${data.patchUnavailable}. Preview unavailable (256 KiB maximum; binary contents are not rendered).`:file!==undefined?(data.patch?'Read-only patch · maximum 256 KiB · binary contents are not rendered.':'No patch remains; files may have changed during inspection. Refresh diff.'):data.files.length?'Choose a file to read its patch.':'';
      $('diff-patch').textContent=data.patch||'';
      if(data.patch)enhanceBlocks?.(root.querySelector('.diff-patch-area'),`diff:${sessionId}:${file}`,{language:/\.([\w+-]+)$/.exec(file||'')?.[1]||'',diff:true});
    }catch(error){if(current===generation&&shown)$('diff-status').textContent=error.message;}
  }
  $('diff-mode').addEventListener('change',()=>{controls();load();});
  $('diff-form').addEventListener('submit',event=>{event.preventDefault();load();});
  return {
    show(session){
      shown=true;
      // Reopening the same session keeps the selected file; a new session starts fresh.
      if(session.id===sessionId&&selectedFile!==null)return load(selectedFile);
      collapsed.clear();
      sessionId=session.id;$('diff-context').textContent=`Session directory: ${session.cwd}`;controls();load();
    },
    hide(){shown=false;generation++;},
  };
}
