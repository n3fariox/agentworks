// C-01: named, revocable external MCP controllers per project. Tokens are shown once.
const GRANTS=[['communicate','Project messages'],['startSessions','Start sessions'],['promptSessions','Start turns'],['cancelSessions','Cancel turns'],['archiveSessions','Archive sessions'],['runAutomations','Run automations'],['manageReviewQueue','Manage review queue']];
export function setupControllers({api,toast,getProject}){
  const $=id=>document.getElementById(id);
  const dialog=$('controllers-dialog'),list=$('controllers-list'),grants=$('controller-grants');
  let project=null,generation=0;
  for(const [key,label] of GRANTS){
    const row=document.createElement('label');row.className='workspace-check';
    const box=document.createElement('input');box.type='checkbox';box.dataset.grant=key;box.checked=key==='communicate';
    row.append(box,' '+label);grants.append(row);
  }
  const node=(tag,text,className)=>{const n=document.createElement(tag);if(text!==undefined)n.textContent=text;if(className)n.className=className;return n;};
  const summary=access=>GRANTS.filter(([k])=>access?.[k]).map(([,l])=>l).join(', ')||'No grants';
  async function refresh(){
    const current=++generation;
    try{
      const {controllers}=await api(`/api/projects/${encodeURIComponent(project)}/controllers`);
      if(current!==generation)return;
      list.replaceChildren(...(controllers.length?controllers.map(c=>{
        const item=node('div',undefined,'controller-item');
        const copy=node('div');copy.append(node('strong',c.name),node('small',`${c.revoked?'Revoked':'Active'} · ${summary(c.access)} · ${c.lastUsed?'last used '+new Date(c.lastUsed).toLocaleString():'never used'}`),node('code',c.actor));
        item.append(copy);
        if(!c.revoked){const revoke=node('button','Revoke','text-button');revoke.type='button';revoke.onclick=async()=>{if(!confirm(`Revoke ${c.name}? Its token stops working immediately.`))return;try{await api(`/api/controllers/${encodeURIComponent(c.id)}/revoke`,{method:'POST',body:'{}'});await refresh();}catch(e){toast(e.message);}};item.append(revoke);}
        return item;
      }):[node('p','No external controllers.','field-note')]));
    }catch(error){if(current===generation)list.replaceChildren(node('p',error.message,'error-text'));}
  }
  $('open-controllers').addEventListener('click',()=>{
    project=getProject();if(!project)return;
    $('controller-name').value='';$('controller-error').textContent='';$('controller-secret').hidden=true;$('controller-config').value='';
    dialog.showModal();refresh();
  });
  $('dismiss-controllers').addEventListener('click',()=>dialog.close());
  dialog.addEventListener('close',()=>{generation++;$('controller-config').value='';});
  $('controller-form').addEventListener('submit',async event=>{
    event.preventDefault();$('controller-error').textContent='';
    const access=Object.fromEntries([...grants.querySelectorAll('input')].map(b=>[b.dataset.grant,b.checked]));
    try{
      const created=await api(`/api/projects/${encodeURIComponent(project)}/controllers`,{method:'POST',body:JSON.stringify({name:$('controller-name').value,access})});
      $('controller-config').value=JSON.stringify({mcpServers:{agentworks:created.mcpServer}},null,2);
      $('controller-secret').hidden=false;$('controller-name').value='';await refresh();
    }catch(error){$('controller-error').textContent=error.message;}
  });
  $('copy-controller-config').addEventListener('click',async()=>{try{await navigator.clipboard.writeText($('controller-config').value);toast('MCP configuration copied','success');}catch{$('controller-config').select();}});
}
