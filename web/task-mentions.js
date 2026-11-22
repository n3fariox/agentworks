import {fuzzyScore} from './mentions.js';
import {taskReference} from './chat-references.js';
// Owns the shared picker only while a # query is active. app.js arbitrates @/#.
export function setupTaskMentions({prompt,list,api,getActive,getProject}){
  let tasks=[],projectId=null,generation=0,range=null,items=[],index=0;
  const enabled=()=>!!getProject(getActive()?.projectId)?.tasksEnabled;
  function clear(){tasks=[];++generation;close();}
  function close(){if(range){list.hidden=true;list.replaceChildren();prompt.removeAttribute('aria-activedescendant');}range=null;items=[];}
  async function refresh(){
    const gen=++generation,id=getActive()?.projectId||null;
    if(id!==projectId){projectId=id;tasks=[];close();}
    if(!projectId||!enabled()){clear();return;}
    try{const result=await api(`/api/projects/${encodeURIComponent(id)}/tasks`);
      if(gen!==generation||getActive()?.projectId!==id||!enabled())return;
      tasks=result.tasks.filter(t=>/^[A-Z][A-Z0-9]{1,19}-[1-9]\d*$/.test(t.key)&&taskReference(t.uri)?.projectId===id);
      if(wants())update();
    }catch{/* Keep the last successful cache only within the same project. */}
  }
  function wants(){return enabled()&&prompt.selectionStart===prompt.selectionEnd&&/(^|\s)#([\w-]*)$/.test(prompt.value.slice(0,prompt.selectionStart));}
  function validRange(){
    return enabled()&&range&&projectId===(getActive()?.projectId||null)&&range.projectId===projectId&&
      prompt.selectionStart===range.end&&prompt.selectionEnd===range.end&&
      prompt.value===range.value&&wants();
  }
  function choose(item){
    if(!validRange()||!items.includes(item)){close();return false;}const {start,end}=range,insert='#'+item.key+' ';
    prompt.value=prompt.value.slice(0,start)+insert+prompt.value.slice(end).replace(/^\s/,'');
    prompt.setSelectionRange(start+insert.length,start+insert.length);close();prompt.dispatchEvent(new Event('input',{bubbles:true}));prompt.focus();return true;
  }
  function render(){
    list.replaceChildren();list.hidden=false;
    if(!items.length){const p=document.createElement('p');p.className='field-note';p.textContent=projectId?'No matching project tickets.':'Assign this session to a project to reference tickets.';list.append(p);return;}
    items.forEach((item,i)=>{const b=document.createElement('button');b.type='button';b.id='task-mention-'+i;b.setAttribute('role','option');b.setAttribute('aria-selected',String(index===i));if(index===i)b.className='active';b.textContent='#'+item.key;const meta=document.createElement('small');meta.textContent=item.title+' · '+item.status;b.append(meta);b.onpointerdown=e=>e.preventDefault();b.onclick=()=>choose(item);list.append(b);});
    prompt.setAttribute('aria-activedescendant','task-mention-'+index);list.querySelector('.active')?.scrollIntoView({block:'nearest'});
  }
  function update(){
    if(!enabled()){clear();return false;}
    if(projectId!==(getActive()?.projectId||null)){tasks=[];projectId=getActive()?.projectId||null;++generation;close();}
    if(!wants()){close();return false;}
    const caret=prompt.selectionStart,m=/(^|\s)#([\w-]*)$/.exec(prompt.value.slice(0,caret));range={start:caret-m[2].length-1,end:caret,projectId,value:prompt.value};
    const q=m[2].toLowerCase();items=tasks.map(t=>({...t,score:Math.max(fuzzyScore(q,t.key.toLowerCase())??-Infinity,fuzzyScore(q,t.title.toLowerCase())??-Infinity)})).filter(t=>t.score>-Infinity).sort((a,b)=>b.score-a.score||a.key.localeCompare(b.key)).slice(0,8);index=0;render();return true;
  }
  function keydown(e){
    if(!validRange()){close();return false;}
    if(e.isComposing)return false;
    if(e.key==='Escape'){close();e.preventDefault();e.stopPropagation();return true;}
    if(!items.length)return false;
    if(e.key==='ArrowDown'||e.key==='ArrowUp'){index=(index+(e.key==='ArrowDown'?1:items.length-1))%items.length;render();e.preventDefault();return true;}
    if((e.key==='Enter'&&!e.shiftKey)||e.key==='Tab'){choose(items[index]);e.preventDefault();return true;}return false;
  }
  function expand(text){
    if(!enabled()){clear();return text;}
    if(projectId!==getActive()?.projectId)return text;
    const known=new Map(tasks.map(t=>[t.key,t]));
    const protectedText=protectedMarkdown(text);
    return text.replace(/(^|\s)#([A-Z][A-Z0-9]{1,19}-[1-9]\d*)(?![\w-])/g,(all,space,key,offset)=>{
      const task=known.get(key);return task&&!protectedText[offset+space.length]?`${space}[#${key}](${task.uri})`:all;
    });
  }
  prompt.addEventListener('blur',close);
  return {wants,update,keydown,expand,close,refresh};
}

// Conservative, bounded Markdown scanner. Protect structural lines first, then
// spans. No nested regex backtracking; every pass visits at most O(text.length).
function protectedMarkdown(text){
  const mask=new Uint8Array(text.length);
  const protect=(start,end)=>mask.fill(1,start,end);
  let fence=null,previousStart=0;
  for(let start=0;start<text.length;){
    const newline=text.indexOf('\n',start),end=newline<0?text.length:newline+1;
    const line=text.slice(start,end).replace(/\r?\n$/,'');
    const marker=/^ {0,3}(`{3,}|~{3,})(.*)$/.exec(line);
    if(fence){
      protect(start,end);
      if(marker&&marker[1][0]===fence.char&&marker[1].length>=fence.length&&/^\s*$/.test(marker[2]))fence=null;
    }else if(marker){fence={char:marker[1][0],length:marker[1].length};protect(start,end);}
    else if(/^(?: {4}| {0,3}\t| {0,3}#{1,6}(?:\s|$)| {0,3}>)/.test(line)||
      /\\|(?:\b[a-z][a-z0-9+.-]{0,31}:|www\.)/i.test(line))protect(start,end);
    if(/^ {0,3}(?:=+|-+)\s*$/.test(line))protect(previousStart,end);
    previousStart=start;start=end;
  }
  // Index exact-length runs, so shorter runs inside a code span cannot close it.
  const runs=[],next=new Map();
  for(let i=0;i<text.length;){
    if(text[i]!=='`'||mask[i]){i++;continue;}
    let end=i+1;while(text[end]==='`')end++;
    runs.push({start:i,end});i=end;
  }
  for(let i=runs.length-1;i>=0;i--){const run=runs[i],length=run.end-run.start;run.close=next.get(length);next.set(length,i);}
  for(let i=0;i<runs.length;){
    const run=runs[i];if(run.close===undefined){protect(run.start,text.length);break;}
    protect(run.start,runs[run.close].end);i=run.close+1;
  }
  // A bracket construct is protected even when incomplete. This intentionally
  // skips some plain bracketed prose rather than rewriting Markdown labels.
  for(let i=0;i<text.length;){
    if(mask[i]||text[i]!=='['){i++;continue;}
    const start=i;let depth=1;i++;
    while(i<text.length&&depth){if(!mask[i]){if(text[i]==='[')depth++;else if(text[i]===']')depth--;}i++;}
    let after=i;while(/\s/.test(text[after]||'')&&after<text.length)after++;
    if(text[after]==='('){
      i=after+1;depth=1;
      while(i<text.length&&depth){if(text[i]==='(')depth++;else if(text[i]===')')depth--;i++;}
    }
    protect(start,i);
  }
  return mask;
}
