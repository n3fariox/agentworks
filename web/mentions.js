// `@` session mentions in the composer. Typing `@` anywhere after whitespace
// opens a fuzzy picker of messageable peers (same logical project, not
// archived). Choosing one inserts `@slug`. The orchestrator sees every project's
// sessions as `@project/slug` (typing `@project/` narrows to one project), and
// project sessions also see the orchestrator. On send, known mentions become
// Markdown links to agentworks://sessions/<id>, a URI the Agentworks MCP
// tools accept verbatim, so agents need not resolve titles.
export function slug(title){return (title||'session').toLowerCase().replace(/[^a-z0-9]+/g,'-').replace(/^-+|-+$/g,'')||'session';}

// Subsequence match; consecutive characters and word starts score higher.
export function fuzzyScore(query,text){
  if(!query)return 0;
  let score=0,at=-1,run=0;
  for(const ch of query){
    const next=text.indexOf(ch,at+1);if(next<0)return null;
    run=next===at+1?run+1:0;
    score+=1+run*3+(next===0||/[\s\-_.]/.test(text[next-1])?2:0)-Math.min(next-at-1,5)*0.1;
    at=next;
  }
  return score;
}

export function setupMentions({prompt,list,getActive,getSessions,getProjects=()=>[],getOrchestrator=()=>null,canCommunicate=()=>false}){
  let items=[],index=0,range=null;
  function peers(){
    const active=getActive();if(!active)return [];
    const orchestrator=getOrchestrator(),everywhere=orchestrator===active.id&&!active.projectId;
    if(!active.projectId&&!everywhere)return [];
    const all=getSessions().filter(s=>s.id!==active.id&&!s.archived);
    const pool=everywhere?all.filter(s=>s.projectId):all.filter(s=>s.projectId===active.projectId);
    const prefix=s=>{if(!everywhere)return '';const p=getProjects().find(p=>p.id===s.projectId);return (p?.slug||slug(p?.name))+'/';};
    const counts={};for(const s of pool){const k=prefix(s)+slug(s.title);counts[k]=(counts[k]||0)+1;}
    const items=pool.map(s=>{const k=prefix(s)+slug(s.title);return {session:s,slug:counts[k]>1?`${k}-${s.id.slice(0,4)}`:k};});
    // Only sessions that can message see the orchestrator; its slug never shadows a peer.
    const hub=!everywhere&&canCommunicate()&&all.find(s=>s.id===orchestrator&&!s.projectId);
    if(hub&&!items.some(i=>i.slug==='orchestrator'))items.push({session:hub,slug:'orchestrator'});
    return items;
  }
  function close(){range=null;items=[];list.hidden=true;list.replaceChildren();prompt.removeAttribute('aria-activedescendant');}
  function choose(item){
    const {start,end}=range,value=prompt.value,insert='@'+item.slug+' ';
    prompt.value=value.slice(0,start)+insert+value.slice(end).replace(/^\s/,'');
    const caret=start+insert.length;prompt.setSelectionRange(caret,caret);close();
    prompt.dispatchEvent(new Event('input',{bubbles:true}));prompt.focus();
  }
  function render(){
    list.replaceChildren();list.hidden=false;
    if(!getActive()?.projectId&&getOrchestrator()!==getActive()?.id){const note=document.createElement('p');note.className='field-note';note.textContent='Assign this session to a project, or make it the orchestrator, to mention peers.';list.append(note);return;}
    if(!items.length){const note=document.createElement('p');note.className='field-note';note.textContent='No matching project sessions.';list.append(note);return;}
    items.forEach((item,i)=>{
      const button=document.createElement('button');button.type='button';button.id='mention-option-'+i;button.setAttribute('role','option');button.setAttribute('aria-selected',String(i===index));
      if(i===index)button.classList.add('active');
      button.textContent='@'+item.slug;const meta=document.createElement('small');meta.textContent=`${item.session.title} · ${item.session.provider} · ${item.session.status||''}`.replace(/ · $/,'');button.append(meta);
      // Keep textarea focus so the caret range stays valid.
      button.addEventListener('pointerdown',e=>e.preventDefault());button.addEventListener('click',()=>choose(item));
      list.append(button);
    });
    prompt.setAttribute('aria-activedescendant','mention-option-'+index);list.querySelector('.active')?.scrollIntoView({block:'nearest'});
  }
  function update(){
    const caret=prompt.selectionStart;
    if(caret!==prompt.selectionEnd){close();return;}
    const match=/(^|\s)@([\w./-]*)$/.exec(prompt.value.slice(0,caret));
    if(!match){close();return;}
    const query=match[2].toLowerCase();range={start:caret-match[2].length-1,end:caret};
    items=peers().map(p=>({...p,score:Math.max(fuzzyScore(query,p.slug)??-Infinity,fuzzyScore(query,p.session.title.toLowerCase())??-Infinity)}))
      .filter(p=>p.score>-Infinity).sort((a,b)=>b.score-a.score||a.slug.localeCompare(b.slug)).slice(0,8);
    index=0;render();
  }
  // Returns true when the key was consumed by the picker.
  function keydown(event){
    if(!range||event.isComposing)return false;
    if(event.key==='Escape'){close();event.preventDefault();event.stopPropagation();return true;}
    if(!items.length)return false;
    if(event.key==='ArrowDown'||event.key==='ArrowUp'){index=(index+(event.key==='ArrowDown'?1:items.length-1))%items.length;render();event.preventDefault();return true;}
    if((event.key==='Enter'&&!event.shiftKey)||event.key==='Tab'){choose(items[index]);event.preventDefault();return true;}
    return false;
  }
  function expand(text){
    const known=new Map(peers().map(p=>[p.slug,p.session]));
    return text.replace(/(^|\s)@([\w./-]+)/g,(all,space,name)=>{
      // Longest known mention first, so `@peer/notes` still links `@peer`.
      let key=name.replace(/[./-]+$/,'');
      while(key&&!known.has(key)){const cut=key.lastIndexOf('/');if(cut<0)break;key=key.slice(0,cut);}
      const s=known.get(key);
      return s?`${space}[@${key}](agentworks://sessions/${s.id})${name.slice(key.length)}`:all;
    });
  }
  prompt.addEventListener('click',update);
  prompt.addEventListener('blur',close);
  close();
  return {update,keydown,expand,close};
}
