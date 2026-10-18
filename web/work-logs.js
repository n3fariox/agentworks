// U-06 compact turn work logs: a derived presentation over the existing transcript items.
// Raw items/events are never altered; grouping only moves already-rendered nodes into a fold.
// Background metadata: ACP tool calls expose no process lifetime or parent/subagent link here,
// so a call is "still running" only while its own status is pending/in_progress. No relationships are inferred.
const PREF='agentworks-work-log-mode', CHOICES='agentworks-work-log-choices';
const RUNNING=new Set(['pending','in_progress']);
const write=(key,value)=>{try{localStorage.setItem(key,JSON.stringify(value));}catch{}};
const read=(key,fallback)=>{try{return JSON.parse(localStorage.getItem(key))??fallback;}catch{return fallback;}};

// Turns start at each user message. A turn is complete only when a later user message exists
// or the session is idle/closed: imported history without turn_end markers is never closed early.
export function buildWorkLogs(items,{sessionIdle=false}={}){
  const turns=[];let turn=null;
  for(const item of items){
    if(item.type==='message'&&item.role==='user'){turn={key:item.id,items:[]};turns.push(turn);continue;}
    if(!turn){turn={key:'before-first-user',items:[]};turns.push(turn);}
    turn.items.push(item);
  }
  return turns.map((t,index)=>{
    const complete=index<turns.length-1||sessionIdle;
    const toolIdx=t.items.flatMap((item,i)=>item.type==='tool'?[i]:[]);
    if(!toolIdx.length)return null;
    // Fold from the first tool through the last tool, keeping intermediate text in chronological order.
    // The answer after the final tool, and anything before the first tool, stays prominent.
    const members=t.items.slice(toolIdx[0],toolIdx.at(-1)+1);
    const calls=new Map();
    for(const item of members)if(item.type==='tool')calls.set(item.id,item);
    const list=[...calls.values()];
    const failed=list.filter(c=>c.status==='failed').length;
    const running=list.filter(c=>RUNNING.has(c.status||'pending')).length;
    const interrupted=complete&&t.items.some(i=>i.type==='notice'&&i.interrupted);
    const starts=list.map(c=>c.observedStart).filter(Number.isFinite);
    const ends=list.map(c=>c.observedEnd).filter(Number.isFinite);
    const duration=starts.length===list.length&&ends.length&&!running?Math.max(...ends)-Math.min(...starts):null;
    return {key:'work-'+t.key,anchor:members[0].id,members:members.map(m=>m.id),calls:list.length,failed,running,interrupted,complete,duration};
  }).filter(Boolean);
}

export function formatDuration(ms){
  const s=Math.round(ms/1000);if(s<60)return s+'s';
  const m=Math.floor(s/60);return m<60?`${m}m ${s%60}s`:`${Math.floor(m/60)}h ${m%60}m`;
}
export function summaryText(log){
  const parts=[`${log.calls} tool call${log.calls===1?'':'s'}`];
  if(log.duration!==null)parts.push(formatDuration(log.duration));
  if(log.failed)parts.push(`${log.failed} failed`);
  if(log.interrupted)parts.push('interrupted');
  if(log.running)parts.push(log.complete?`${log.running} still running`:`${log.running} running`);
  return parts.join(' · ');
}

// Records browser-observed timing for live tool events only; replayed history has no timestamps,
// so its duration is omitted rather than guessed.
export function observeTool(tool,{live}){
  if(!live)return;
  const now=Date.now();
  tool.observedStart??=now;
  if(!RUNNING.has(tool.status||'pending'))tool.observedEnd??=now;else delete tool.observedEnd;
}

export function setupWorkLogs({el}){
  let mode=read(PREF,'compact')==='expanded'?'expanded':'compact';
  const choices=read(CHOICES,{});const groups=new Map();
  const save=()=>{const keys=Object.keys(choices);for(const k of keys.slice(0,Math.max(0,keys.length-200)))delete choices[k];write(CHOICES,choices);};
  function apply(area,items,nodes,{sessionIdle}){
    const logs=buildWorkLogs(items,{sessionIdle});const seen=new Set();
    for(const log of logs){
      seen.add(log.key);
      let group=groups.get(log.key);
      if(!group){
        group=el('details','work-log');group.dataset.workLog=log.key;
        const summary=el('summary','work-log-summary');summary.append(el('span','work-log-title','Work log'),el('span','work-log-counts'));
        group.append(summary,el('div','work-log-body'));
        summary.addEventListener('click',()=>{choices[log.key]=!group.open;save();});
        groups.set(log.key,group);
      }
      const anchor=nodes.get(log.anchor);
      if(anchor&&group.parentNode!==area)area.insertBefore(group,anchor.parentNode===area?anchor:null);
      const body=group.querySelector('.work-log-body');
      for(const id of log.members){const node=nodes.get(id);if(node&&node.parentNode!==body)body.append(node);}
      group.querySelector('.work-log-counts').textContent=summaryText(log);
      group.classList.toggle('has-failure',!!(log.failed||log.interrupted));
      group.classList.toggle('is-running',!!log.running);
      // Explicit user choice wins; otherwise open while working (or expanded mode) and fold after completion.
      // A group the user is inspecting (focus inside) is never folded automatically.
      const wanted=log.key in choices?choices[log.key]:mode==='expanded'||!log.complete;
      if(group.open!==wanted&&!(group.open&&group.querySelector('.work-log-body').contains(document.activeElement)))group.open=wanted;
    }
    for(const [key,group] of groups)if(!seen.has(key)){for(const child of [...group.querySelector('.work-log-body').children])area.insertBefore(child,group);group.remove();groups.delete(key);}
  }
  return {
    apply,
    reset(){groups.clear();},
    get mode(){return mode;},
    // Settings checkbox for the compact/expanded default; explicit per-log choices still win.
    mount(container,onChange){
      const label=document.createElement('label');label.className='work-log-preference';
      const box=document.createElement('input');box.type='checkbox';box.id='work-log-expanded';box.checked=mode==='expanded';
      box.onchange=()=>{this.setMode(box.checked?'expanded':'compact');onChange?.();};
      label.append(box,' Keep completed work logs expanded');container.append(label);
    },
    setMode(next){mode=next==='expanded'?'expanded':'compact';write(PREF,mode);},
  };
}
