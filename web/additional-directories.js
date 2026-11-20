import {directoryRow} from './directory-picker.js';

// ACP additionalDirectories: folders the agent may also work in, beside its working directory.
// Edited with the same rows as allowed roots and saved in one request, because each save
// reopens a running harness. The server canonicalizes paths and drops duplicates.
export function setupAdditionalDirectories({$,api,picker,getActive,host,toast}){
  const dialog=$('additional-directories-dialog'),list=$('additional-directories-list');
  let session=null,saved=[];
  const values=()=>[...list.querySelectorAll('input')].map(input=>input.value.trim()).filter(Boolean);
  const same=(a,b)=>a.length===b.length&&a.every((d,i)=>d===b[i]);
  function refresh(){
    $('additional-directories-empty').hidden=list.children.length>0;
    $('additional-directories-save').disabled=same(values(),saved);
  }
  function addRow(value=''){
    const where=host(session);
    const row=directoryRow(picker,{value,label:'Additional directory',placeholder:'/path/to/another/folder',onRemove:refresh,
      browse:{hostId:where.id,hostName:()=>where.name,scope:'roots',start:session.cwd}});
    list.append(row);refresh();return row.querySelector('input');
  }
  async function save(directories){
    const result=await api(`/api/sessions/${session.id}/additional-directories`,{method:'POST',body:JSON.stringify({directories})});
    toast(result.unchanged?'Additional directories unchanged':result.restarted?'Saved; restarting the harness with the new directories…':'Saved; the directories apply when the harness resumes','success');
    return result;
  }
  function open(){
    session=getActive();if(!session)return;
    saved=[...(session.additionalDirectories||[])];
    list.replaceChildren();for(const path of saved)addRow(path);
    $('additional-directories-error').textContent='';refresh();dialog.showModal();
  }
  // Palette shortcut: pick one folder and save it straight away.
  async function add(){
    session=getActive();if(!session)return;
    const id=session.id,where=host(session);
    const path=await picker.pick({hostId:where.id,hostName:where.name,start:session.cwd});
    if(!path||getActive()?.id!==id)return;
    const current=session.additionalDirectories||[];
    if(path===session.cwd||current.includes(path)){toast('That folder is already available to this session','success');return;}
    try{await save([...current,path]);}catch(error){toast(error.message);}
  }
  list.addEventListener('input',refresh);
  $('additional-directories-add').onclick=()=>addRow().focus();
  $('dismiss-additional-directories').onclick=()=>dialog.close();
  $('additional-directories-form').onsubmit=async event=>{
    event.preventDefault();
    const button=$('additional-directories-save');button.disabled=true;$('additional-directories-error').textContent='';
    try{await save(values());dialog.close();}
    catch(error){$('additional-directories-error').textContent=error.message;button.disabled=false;}
  };
  return {open,add};
}
