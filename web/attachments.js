// Chat prompt attachments. Files upload as soon as they are picked, dropped or pasted;
// the prompt then carries only their server IDs. The server enforces the type, size
// and provider policy; checks here only give earlier feedback.
const TEXT_ACCEPT='.txt,.md,.markdown,.json,.csv,.tsv,.log,.yaml,.yml,.toml,.xml,.html,.css,.js,.mjs,.ts,.tsx,.jsx,.py,.rs,.go,.java,.c,.h,.cpp,.hpp,.sh,.sql,.diff,.patch,text/*';
const size=bytes=>bytes<1024?`${bytes} B`:bytes<1048576?`${Math.round(bytes/1024)} KB`:`${(bytes/1048576).toFixed(1)} MB`;

export function setupAttachments({appUrl,el,toast,uuid,getSession,canAttach,button,input,list,dropTarget,onChange}){
  const bySession=new Map();let policy=null;
  const items=sid=>{if(!bySession.has(sid))bySession.set(sid,[]);return bySession.get(sid);};
  const sid=()=>getSession()?.id;
  function reason(){
    if(!policy)return 'Attachments are available once the agent has started.';
    if(!policy.image&&!policy.text)return policy.imageReason||'This agent does not accept attachments.';
    return null;
  }
  function describe(){
    if(!policy||(!policy.image&&!policy.text))return reason();
    const kinds=[policy.image&&`images (PNG, JPEG, GIF, WebP · up to ${size(policy.maxImageBytes)})`,policy.text&&`UTF-8 text files (up to ${size(policy.maxTextBytes)})`].filter(Boolean).join(' or ');
    const missing=!policy.image?` Images: ${policy.imageReason}.`:!policy.text?` Text files: ${policy.textReason}.`:'';
    return `Attach ${kinds}; ${policy.maxPerPrompt} per message.${missing} ${policy.unsupported}`;
  }
  function render(){
    const id=sid(),current=id?items(id):[];
    const blocked=reason();
    button.disabled=!!blocked||!canAttach()||current.length>=(policy?.maxPerPrompt??0);
    button.title=describe();button.setAttribute('aria-label','Attach files. '+describe());
    input.accept=policy?.image&&!policy?.text?policy.imageTypes.join(','):policy?.text&&!policy?.image?TEXT_ACCEPT:'';
    list.hidden=!current.length;
    list.replaceChildren(...current.map(item=>{
      const chip=el('div','attachment-chip '+item.status);chip.dataset.attachment=item.id||'';chip.title=item.error||`${item.name} · ${size(item.size)}`;
      if(item.preview){const img=el('img','attachment-thumb');img.src=item.preview;img.alt='';chip.append(img);}else chip.append(el('span','attachment-icon',item.kind==='image'?'▣':'¶'));
      chip.append(el('span','attachment-name',item.name),el('span','attachment-meta',item.status==='uploading'?'Uploading…':item.status==='error'?item.error:size(item.size)));
      const remove=el('button','attachment-remove','×');remove.type='button';remove.setAttribute('aria-label','Remove '+item.name);remove.onclick=()=>discard(id,item);chip.append(remove);
      return chip;
    }));
    onChange?.();
  }
  async function discard(id,item){
    const list=items(id),index=list.indexOf(item);if(index>=0)list.splice(index,1);
    if(item.preview)URL.revokeObjectURL(item.preview);render();
    if(item.id)fetch(appUrl(`/api/sessions/${id}/attachments/${item.id}`),{method:'DELETE'}).catch(()=>{});
  }
  async function add(files){
    const id=sid();if(!id)return;
    const blocked=reason();if(blocked){toast(blocked);return;}
    for(const file of files){
      const current=items(id);
      if(current.length>=policy.maxPerPrompt){toast(`Attach up to ${policy.maxPerPrompt} files per message`);break;}
      const image=/^image\//.test(file.type);
      if(image&&!policy.image){toast(`${file.name}: ${policy.imageReason}`);continue;}
      if(file.type==='application/pdf'||/\.pdf$/i.test(file.name)){toast(`${file.name}: ${policy.unsupported}`);continue;}
      const limit=image?policy.maxImageBytes:policy.maxTextBytes;
      if(file.size>limit){toast(`${file.name} is larger than ${size(limit)}`);continue;}
      const item={key:uuid(),name:file.name||'pasted',size:file.size,kind:image?'image':'text',status:'uploading',preview:image?URL.createObjectURL(file):null};
      current.push(item);render();
      try{
        const response=await fetch(appUrl(`/api/sessions/${id}/attachments?name=${encodeURIComponent(item.name)}`),{method:'POST',headers:{'Content-Type':'application/octet-stream'},body:file});
        if(!response.ok)throw new Error(await response.text());
        const saved=await response.json();
        if(!items(id).includes(item)){fetch(appUrl(`/api/sessions/${id}/attachments/${saved.id}`),{method:'DELETE'}).catch(()=>{});continue;}
        Object.assign(item,{id:saved.id,name:saved.name,kind:saved.kind,size:saved.size,status:'ready'});
        if(saved.kind!=='image'&&item.preview){URL.revokeObjectURL(item.preview);item.preview=null;}
      }catch(e){item.status='error';item.error=e.message||'Upload failed';toast(`${item.name}: ${item.error}`);}
      render();
    }
  }
  button.onclick=()=>{if(!button.disabled)input.click();};
  input.onchange=()=>{const files=[...input.files];input.value='';add(files);};
  dropTarget.addEventListener('dragover',event=>{if(event.dataTransfer?.types?.includes('Files')){event.preventDefault();dropTarget.classList.add('dragging');}});
  dropTarget.addEventListener('dragleave',()=>dropTarget.classList.remove('dragging'));
  dropTarget.addEventListener('drop',event=>{dropTarget.classList.remove('dragging');if(!event.dataTransfer?.files?.length)return;event.preventDefault();if(canAttach())add([...event.dataTransfer.files]);});
  dropTarget.addEventListener('paste',event=>{const files=[...(event.clipboardData?.files||[])];if(!files.length||!canAttach())return;event.preventDefault();add(files);});
  return {
    render,
    setPolicy(next){policy=next||null;render();},
    ids:id=>items(id).filter(i=>i.status==='ready').map(i=>i.id),
    uploading:id=>items(id).some(i=>i.status==='uploading'),
    failed:id=>items(id).some(i=>i.status==='error'),
    count:id=>items(id).length,
    // After a successful send the server owns the files; keep only local previews tidy.
    clear(id,sent){const list=items(id);for(const item of [...list])if(sent.includes(item.id)){if(item.preview)URL.revokeObjectURL(item.preview);list.splice(list.indexOf(item),1);}render();},
    // Sent attachments in the transcript: image thumbnails and text file links.
    renderSent(container,sessionId,attachments){
      container.replaceChildren(...attachments.map(a=>{
        const url=appUrl(`/api/sessions/${sessionId}/attachments/${encodeURIComponent(a.id)}`);
        if(a.kind==='image'){const img=el('img','attachment-sent-image');img.src=url;img.alt=a.name;img.title=`${a.name} · ${size(a.size)}`;img.loading='lazy';return img;}
        const link=el('a','attachment-chip ready');link.href=url;link.download=a.name;link.append(el('span','attachment-icon','¶'),el('span','attachment-name',a.name),el('span','attachment-meta',size(a.size)));return link;
      }));
    },
  };
}
