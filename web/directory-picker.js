// Folder picker for project and session directories. The server lists and
// creates folders only beneath the host's allowed roots; paths shown here are
// the server's canonical answers, never trusted browser input.
export function setupDirectoryPicker({$,api}) {
  let state=null,generation=0;
  const query=(hostId,path)=>`/api/directories?${new URLSearchParams({...(hostId!=='local'?{hostId}:{}),...(path?{path}:{})})}`;
  function render(listing){
    const at=listing.path;state.path=at;state.root=listing.root;
    const crumbs=$('directory-picker-crumbs');crumbs.replaceChildren();
    const crumb=(label,path)=>{const b=document.createElement('button');b.type='button';b.className='text-button';b.textContent=label;b.onclick=()=>load(path);crumbs.append(b);};
    crumb('Allowed roots',null);
    if(at){
      crumb(listing.root,listing.root);
      const rest=at.slice(listing.root.length).split('/').filter(Boolean);
      rest.forEach((name,i)=>crumb(name,listing.root.replace(/\/$/,'')+'/'+rest.slice(0,i+1).join('/')));
      crumbs.lastElementChild.setAttribute('aria-current','location');
    }
    const list=$('directory-picker-list');list.replaceChildren();
    if(listing.parent){const up=document.createElement('li');const b=document.createElement('button');b.type='button';b.textContent='↑ Up';b.onclick=()=>load(listing.parent);up.append(b);list.append(up);}
    for(const entry of listing.entries){
      const item=document.createElement('li'),b=document.createElement('button');
      b.type='button';b.textContent=(at?'':'⌂ ')+entry.name;b.title=entry.path;b.onclick=()=>load(entry.path);
      item.append(b);list.append(item);
    }
    $('directory-picker-note').textContent=!at?(listing.entries.length?'Open an allowed root to choose or create a folder inside it.':'No allowed roots exist on this host. Add them in Settings (local) or in the host’s working directories.'):listing.entries.length?(listing.truncated?'Showing the first folders only.':''):'No folders here yet.';
    $('directory-picker-current').textContent=at||'';
    $('directory-picker-select').disabled=!at;$('directory-picker-create').disabled=!at;$('directory-picker-name').disabled=!at;
  }
  async function load(path){
    // Actions wait for the listing so they never apply to the previous folder.
    const gen=++generation;$('directory-picker-error').textContent='';
    $('directory-picker-select').disabled=true;$('directory-picker-create').disabled=true;
    try{const listing=await api(query(state.hostId,path));if(gen===generation&&state)render(listing);}
    catch(error){
      if(gen!==generation||!state)return;if(path&&path===state.start){state.start=null;return load(null);}
      $('directory-picker-error').textContent=error.message;$('directory-picker-select').disabled=!state.path;$('directory-picker-create').disabled=!state.path;
    }
  }
  // Resolves to the chosen canonical path, or null when dismissed.
  function pick({hostId='local',hostName='Local',start=''}={}){
    state?.resolve(null);
    return new Promise(resolve=>{
      state={hostId,start:start||null,path:null,root:null,resolve};
      $('directory-picker-host').textContent=`Folders on ${hostName}. Only folders inside allowed roots are shown.`;
      $('directory-picker-name').value='';$('directory-picker-list').replaceChildren();
      $('directory-picker-select').disabled=true;$('directory-picker-create').disabled=true;
      $('directory-picker-dialog').showModal();load(state.start);
    });
  }
  function finish(path){const current=state;state=null;generation++;$('directory-picker-dialog').close();current?.resolve(path);}
  $('directory-picker-select').onclick=()=>state?.path&&finish(state.path);
  $('dismiss-directory-picker').onclick=()=>finish(null);
  $('directory-picker-dialog').addEventListener('cancel',event=>{event.preventDefault();finish(null);});
  $('directory-picker-form').onsubmit=event=>{event.preventDefault();$('directory-picker-create').click();};
  $('directory-picker-create').onclick=async()=>{
    const name=$('directory-picker-name').value.trim();if(!state?.path||!name)return;
    const button=$('directory-picker-create');button.disabled=true;$('directory-picker-error').textContent='';
    try{
      const created=await api('/api/directories',{method:'POST',body:JSON.stringify({...(state.hostId!=='local'?{hostId:state.hostId}:{}),parent:state.path,name})});
      $('directory-picker-name').value='';await load(created.path);
    }catch(error){$('directory-picker-error').textContent=error.message;}finally{button.disabled=!state?.path;}
  };
  return {pick};
}

// Project directory field: optional, explicit, and only sent when changed,
// so saving other project settings never moves an existing project.
export function setupProjectDirectory({$,picker,hosts}) {
  let original={hostId:'local',path:''};
  function fill(selected){
    const select=$('project-directory-host');select.replaceChildren(new Option('Local','local'));
    for(const host of hosts().filter(h=>h.kind==='remote'))select.append(new Option(host.name+(host.status==='connected'?'':' · offline'),host.id));
    select.value=[...select.options].some(o=>o.value===selected)?selected:'local';
  }
  function note(){
    const changed=dirty();
    $('project-directory-note').textContent=$('project-directory').value.trim()?(changed?'New sessions in this project will start here. Existing sessions keep their directories.':'New sessions in this project start here unless you choose another directory.'):(original.path&&changed?'The project directory will be cleared. Sessions keep their directories.':'Optional. Choose an existing folder or create one beneath an allowed root.');
  }
  function open(location){
    original={hostId:location?.hostId||'local',path:location?.path||''};
    fill(original.hostId);$('project-directory').value=original.path;note();
  }
  function dirty(){const path=$('project-directory').value.trim();return path!==original.path||(path&&$('project-directory-host').value!==original.hostId);}
  // `undefined` leaves the saved directory unchanged; `null` clears it.
  function value(){
    if(!dirty())return undefined;
    const path=$('project-directory').value.trim();
    return path?{hostId:$('project-directory-host').value,path}:null;
  }
  $('project-directory').addEventListener('input',note);
  $('project-directory-host').onchange=()=>{$('project-directory').value='';note();};
  $('project-directory-clear').onclick=()=>{$('project-directory').value='';note();};
  $('project-directory-browse').onclick=async()=>{
    const hostId=$('project-directory-host').value,host=hosts().find(h=>h.id===hostId);
    const path=await picker.pick({hostId,hostName:host?.name||'Local',start:$('project-directory').value.trim()});
    if(path){$('project-directory').value=path;note();}
  };
  return {open,value,saved:()=>({...original}),commit:()=>{original={hostId:$('project-directory-host').value,path:$('project-directory').value.trim()};note();}};
}
