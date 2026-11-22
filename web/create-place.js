// One searchable field for where a new session runs: a project on a host. The
// hidden project and host selects stay the source of truth; picking a place sets
// both and fires their change handlers. Places already used come first; every
// other pair stays reachable by search.
export function setupCreatePlace({$,el,used}) {
  const input=$('create-place'),list=$('create-place-list'),project=$('create-project'),host=$('create-host');
  let items=[],active=0;
  const label=(p,h)=>`${p.textContent} · ${h.textContent.split(' · ')[0]}`;
  function sync(){
    const p=project.selectedOptions[0],h=host.selectedOptions[0];
    if(document.activeElement!==input&&p&&h)input.value=label(p,h);
  }
  // A token matches when its letters appear in order in the project or host name.
  const subsequence=(token,text)=>{let i=0;for(const c of text)if(c===token[i])i++;return i===token.length;};
  function entries(query){
    const tokens=query.toLowerCase().split(/\s+/).filter(Boolean),all=[];
    for(const p of project.options)for(const h of host.options){
      const names=[p.textContent.toLowerCase(),h.textContent.toLowerCase()];
      if(tokens.every(t=>names.some(n=>subsequence(t,n))))all.push({p,h,used:used(h.value,p.value||null),disabled:h.disabled});
    }
    return [...all.filter(e=>e.used),...all.filter(e=>!e.used)];
  }
  function render(){
    items=entries(input.value===label(project.selectedOptions[0],host.selectedOptions[0])?'':input.value);
    active=Math.max(0,Math.min(active,items.length-1));
    list.replaceChildren();
    let heading=null;
    items.forEach((item,i)=>{
      const group=item.used?'Used':'Other';
      if(group!==heading&&items.some(e=>!e.used)&&items.some(e=>e.used)){heading=group;list.append(el('div','create-place-group',group==='Used'?'Used before':'Other places'));}
      const row=el('div','create-place-option'+(i===active?' active':'')+(item.disabled?' disabled':''),label(item.p,item.h)+(item.disabled?' · '+item.h.textContent.split(' · ')[1]:''));
      row.dataset.project=item.p.value;row.dataset.host=item.h.value;
      // Pointerdown keeps focus in the input so the list does not close first.
      row.onpointerdown=event=>{event.preventDefault();if(!item.disabled)pick(item);};
      list.append(row);
    });
    if(!items.length)list.append(el('div','create-place-empty','No matching project or host'));
    list.querySelector('.active')?.scrollIntoView({block:'nearest'});
  }
  function open(){list.hidden=false;render();}
  function close(){list.hidden=true;sync();}
  function pick({p,h}){
    const projectChanged=project.value!==p.value,hostChanged=host.value!==h.value;
    project.value=p.value;host.value=h.value;
    // The host handler resolves the directory for the chosen project; the project
    // handler then reloads that project's workspace policy.
    if(hostChanged)host.dispatchEvent(new Event('change'));
    if(projectChanged)project.dispatchEvent(new Event('change'));
    input.blur();close();
  }
  input.addEventListener('focus',()=>{active=0;input.select();open();});
  input.addEventListener('click',()=>{if(list.hidden)open();});
  input.addEventListener('blur',close);
  input.addEventListener('input',()=>{active=0;render();});
  input.addEventListener('keydown',event=>{
    if(list.hidden)return;
    const step=dir=>{for(let i=active+dir;i>=0&&i<items.length;i+=dir)if(!items[i].disabled){active=i;break;}render();};
    if(event.key==='ArrowDown'){event.preventDefault();step(1);}
    else if(event.key==='ArrowUp'){event.preventDefault();step(-1);}
    else if(event.key==='Enter'){event.preventDefault();if(items[active]&&!items[active].disabled)pick(items[active]);}
    // Escape closes the list without closing the dialog.
    else if(event.key==='Escape'){event.preventDefault();event.stopPropagation();input.blur();}
  });
  return {sync};
}
