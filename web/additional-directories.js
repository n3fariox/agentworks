// ACP additionalDirectories: folders the agent may also work in, beside its working directory.
// The list is edited here and saved in one request, because each save reopens a running harness.
export function setupAdditionalDirectories({$,api,picker,getActive,host,toast}){
  const dialog=$('additional-directories-dialog'),list=$('additional-directories-list');
  let session=null,saved=[],draft=[];
  const same=(a,b)=>a.length===b.length&&a.every((d,i)=>d===b[i]);
  function render(){
    list.replaceChildren();
    for(const path of draft){
      const item=document.createElement('li'),label=document.createElement('span'),remove=document.createElement('button');
      label.textContent=path;label.title=path;
      remove.type='button';remove.className='text-button';remove.textContent='Remove';remove.setAttribute('aria-label','Remove '+path);
      remove.onclick=()=>{draft=draft.filter(d=>d!==path);render();$('additional-directories-add').focus();};
      item.append(label,remove);list.append(item);
    }
    $('additional-directories-empty').hidden=draft.length>0;
    $('additional-directories-save').disabled=same(draft,saved);
  }
  async function choose(start){
    const where=host(session);
    return picker.pick({hostId:where.id,hostName:where.name,start:start||session.cwd});
  }
  async function save(directories){
    const result=await api(`/api/sessions/${session.id}/additional-directories`,{method:'POST',body:JSON.stringify({directories})});
    toast(result.unchanged?'Additional directories unchanged':result.restarted?'Saved; restarting the harness with the new directories…':'Saved; the directories apply when the harness resumes','success');
    return result;
  }
  function open(){
    session=getActive();if(!session)return;
    saved=[...(session.additionalDirectories||[])];draft=[...saved];
    $('additional-directories-error').textContent='';render();dialog.showModal();
  }
  // Palette shortcut: pick one folder and save it straight away.
  async function add(){
    session=getActive();if(!session)return;
    const id=session.id,path=await choose();
    if(!path||getActive()?.id!==id)return;
    const current=session.additionalDirectories||[];
    if(path===session.cwd||current.includes(path)){toast('That folder is already available to this session','success');return;}
    try{await save([...current,path]);}catch(error){toast(error.message);}
  }
  $('additional-directories-add').onclick=async()=>{
    const path=await choose(draft.at(-1));
    if(!path||!dialog.open)return;
    if(path===session.cwd){$('additional-directories-error').textContent='That is the working directory.';return;}
    if(!draft.includes(path))draft.push(path);
    $('additional-directories-error').textContent='';render();
  };
  $('dismiss-additional-directories').onclick=()=>dialog.close();
  $('additional-directories-form').onsubmit=async event=>{
    event.preventDefault();
    const button=$('additional-directories-save');button.disabled=true;$('additional-directories-error').textContent='';
    try{await save(draft);dialog.close();}
    catch(error){$('additional-directories-error').textContent=error.message;button.disabled=false;}
  };
  return {open,add};
}
