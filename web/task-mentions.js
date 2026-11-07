import {fuzzyScore} from './mentions.js';
import {taskReference} from './chat-references.js';
// Owns the shared picker only while a # query is active. app.js arbitrates @/#.
export function setupTaskMentions({prompt,list,api,getActive}){
  let tasks=[],projectId=null,generation=0,range=null,items=[],index=0;
  function close(){if(range){list.hidden=true;list.replaceChildren();prompt.removeAttribute('aria-activedescendant');}range=null;items=[];}
  async function refresh(){
    const gen=++generation;projectId=getActive()?.projectId||null;tasks=[];close();
    if(!projectId)return;
    const id=projectId;
    try{const result=await api(`/api/projects/${encodeURIComponent(id)}/tasks`);
      if(gen!==generation||getActive()?.projectId!==id)return;
      tasks=result.tasks.filter(t=>/^[A-Z][A-Z0-9]{1,19}-[1-9]\d*$/.test(t.key)&&taskReference(t.uri)?.projectId===id);
      if(wants())update();
    }catch{/* Unknown keys are never expanded when the list is unavailable. */}
  }
  function wants(){return prompt.selectionStart===prompt.selectionEnd&&/(^|\s)#([\w-]*)$/.test(prompt.value.slice(0,prompt.selectionStart));}
  function choose(item){
    if(!range)return;const {start,end}=range,insert='#'+item.key+' ';
    prompt.value=prompt.value.slice(0,start)+insert+prompt.value.slice(end).replace(/^\s/,'');
    prompt.setSelectionRange(start+insert.length,start+insert.length);close();prompt.dispatchEvent(new Event('input',{bubbles:true}));prompt.focus();
  }
  function render(){
    list.replaceChildren();list.hidden=false;
    if(!items.length){const p=document.createElement('p');p.className='field-note';p.textContent=projectId?'No matching project tickets.':'Assign this session to a project to reference tickets.';list.append(p);return;}
    items.forEach((item,i)=>{const b=document.createElement('button');b.type='button';b.id='task-mention-'+i;b.setAttribute('role','option');b.setAttribute('aria-selected',String(index===i));if(index===i)b.className='active';b.textContent='#'+item.key;const meta=document.createElement('small');meta.textContent=item.title+' · '+item.status;b.append(meta);b.onpointerdown=e=>e.preventDefault();b.onclick=()=>choose(item);list.append(b);});
    prompt.setAttribute('aria-activedescendant','task-mention-'+index);list.querySelector('.active')?.scrollIntoView({block:'nearest'});
  }
  function update(){
    if(!wants()){close();return false;}
    const caret=prompt.selectionStart,m=/(^|\s)#([\w-]*)$/.exec(prompt.value.slice(0,caret));range={start:caret-m[2].length-1,end:caret};
    const q=m[2].toLowerCase();items=tasks.map(t=>({...t,score:Math.max(fuzzyScore(q,t.key.toLowerCase())??-Infinity,fuzzyScore(q,t.title.toLowerCase())??-Infinity)})).filter(t=>t.score>-Infinity).sort((a,b)=>b.score-a.score||a.key.localeCompare(b.key)).slice(0,8);index=0;render();return true;
  }
  function keydown(e){
    if(!range||e.isComposing)return false;
    if(e.key==='Escape'){close();e.preventDefault();e.stopPropagation();return true;}
    if(!items.length)return false;
    if(e.key==='ArrowDown'||e.key==='ArrowUp'){index=(index+(e.key==='ArrowDown'?1:items.length-1))%items.length;render();e.preventDefault();return true;}
    if((e.key==='Enter'&&!e.shiftKey)||e.key==='Tab'){choose(items[index]);e.preventDefault();return true;}return false;
  }
  function expand(text){
    if(projectId!==getActive()?.projectId)return text;
    const known=new Map(tasks.map(t=>[t.key,t]));
    // Leave Markdown links, images, code spans/fences and escaped text intact.
    // Only plain composer tokens are expanded, never arbitrary Markdown bodies.
    return text.replace(/(```[\s\S]*?(?:```|$)|~~~[\s\S]*?(?:~~~|$)|`+[^`]*`+|!?\[[^\]\n]*\]\([^\n)]*\)|\\.|(^|\s)#([A-Z][A-Z0-9]{1,19}-[1-9]\d*)(?![\w-]))/g,(all,protectedOrToken,space,key)=>{
      const task=key&&known.get(key);return task?`${space}[#${key}](${task.uri})`:all;
    });
  }
  prompt.addEventListener('blur',close);
  return {wants,update,keydown,expand,close,refresh};
}
