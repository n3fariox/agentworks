import {localState as localStorage} from './hosting.js';
// U-06 compact turn work logs: a derived presentation over the existing transcript items.
// Raw items/events are never altered; grouping only moves already-rendered nodes into a fold.
// Background metadata: ACP tool calls expose no process lifetime or parent/subagent link here,
// so a pending/in_progress call is shown as "in progress" during its turn and "without final status" afterwards;
// neither claims a background process is alive. Adapter-owned jobs (AIR asyncTasks) report their own
// outcome, which appears as a `job` item where it ended. Durations are browser-observed for live events only and
// omitted after replay (persisted events carry no timestamps). No parent/subagent relationships are inferred.
const PREF='agentworks-work-log-mode', CHOICES='agentworks-work-log-choices';
const RUNNING=new Set(['pending','in_progress']);
const write=(key,value)=>{try{localStorage.setItem(key,JSON.stringify(value));}catch{}};
const read=(key,fallback)=>{try{return JSON.parse(localStorage.getItem(key))??fallback;}catch{return fallback;}};

// Turns start at each user message or after a persisted turn_end (`turnEnds`: id of the last item
// before it -> {seq, stopReason}). Keys come from persisted event sequence numbers scoped by session,
// so choices survive replay; transcript item ids are random per replay and are never used as keys.
// A turn without turn_end is complete only when a later turn exists or the session is idle/closed,
// so imported history lacking turn_end markers is never closed early.
// Errors, images and notices (interruption, agent-initiated turns) split a turn into separate
// segments so they are never folded away.
// Background-job results (`job` items, one per persisted async_task_finished event) fold with the
// tools around them; outside that range a run of them gets its own log, so a job ending after
// the final answer never folds the answer away.
const SEPARATORS=new Set(['error','image','notice']);
const JOB_FAILED=new Set(['failed']);
export function buildWorkLogs(items,{sessionIdle=false,scope='',turnEnds=new Map()}={}){
  const turns=[];let turn=null;
  const open=key=>{turn={key,items:[],end:null};turns.push(turn);};
  for(const item of items){
    if(item.type==='message'&&item.role==='user'){open(`${scope}:${item.seq??item.id}`);}
    else{if(!turn)open(`${scope}:start`);if(!turn.items.includes(item))turn.items.push(item);}
    const end=turnEnds.get(item.id);
    if(end){turn.end=end;open(`${scope}:end-${end.seq}`);}
  }
  const logs=[];
  turns.forEach((t,index)=>{
    const complete=!!t.end||index<turns.length-1||sessionIdle;
    const segments=[[]];
    for(const item of t.items)if(SEPARATORS.has(item.type))segments.push([]);else segments.at(-1).push(item);
    const interrupted=t.items.some(i=>i.type==='notice'&&i.interrupted)||t.end?.stopReason==='cancelled';
    const stopReason=t.end&&!['end_turn','cancelled'].includes(t.end.stopReason)?t.end.stopReason:null;
    const log=(key,members)=>{
      const list=members.filter(m=>m.type==='tool'),jobs=members.filter(m=>m.type==='job');
      const failed=list.filter(c=>c.status==='failed').length;
      const unresolved=list.filter(c=>RUNNING.has(c.status||'pending')).length;
      const starts=list.map(c=>c.observedStart).filter(Number.isFinite);
      const ends=list.map(c=>c.observedEnd).filter(Number.isFinite);
      const duration=list.length&&starts.length===list.length&&ends.length===list.length&&!unresolved?Math.max(...ends)-Math.min(...starts):null;
      logs.push({key,anchor:members[0].id,members:members.map(m=>m.id),calls:list.length,failed,running:unresolved,
        jobs:jobs.length,jobsFailed:jobs.filter(j=>JOB_FAILED.has(j.job.state)).length,jobsUnknown:jobs.filter(j=>j.job.state==='unknown').length,
        job:jobs.length===1&&!list.length?jobs[0].job:null,
        interrupted:complete&&interrupted,stopReason:complete?stopReason:null,complete,duration});
    };
    segments.forEach((segment,part)=>{
      const toolIdx=segment.flatMap((item,i)=>item.type==='tool'?[i]:[]);
      // Fold from the first tool through the last tool, keeping intermediate text in chronological order.
      // The answer after the final tool, and anything before the first tool, stays prominent.
      const [first,last]=toolIdx.length?[toolIdx[0],toolIdx.at(-1)]:[segment.length,-1];
      let run=[];
      const flush=()=>{if(run.length)log(`work-${t.key}:${part}:job-${run[0].seq??run[0].id}`,run);run=[];};
      segment.forEach((item,i)=>{
        if(i===first){flush();log(`work-${t.key}:${part}`,segment.slice(first,last+1));}
        if(i>=first&&i<=last)return;
        if(item.type==='job')run.push(item);else flush();
      });
      flush();
    });
  });
  return logs;
}

export function formatDuration(ms){
  const s=Math.round(ms/1000);if(s<60)return s+'s';
  const m=Math.floor(s/60);return m<60?`${m}m ${s%60}s`:`${Math.floor(m/60)}h ${m%60}m`;
}
const JOB_LABELS={completed:'completed',failed:'failed',stopped:'stopped',cancelled:'cancelled',unknown:'status unknown'};
export function summaryText(log){
  if(log.job)return `Background job · ${log.job.name||'Background job'} · ${JOB_LABELS[log.job.state]||'status unknown'}`;
  const parts=log.calls?[`${log.calls} tool call${log.calls===1?'':'s'}`]:[];
  if(log.jobs)parts.push(`${log.jobs} background job${log.jobs===1?'':'s'} ended`);
  if(log.jobsFailed)parts.push(`${log.jobsFailed} job${log.jobsFailed===1?'':'s'} failed`);
  if(log.jobsUnknown)parts.push(`${log.jobsUnknown} with unknown status`);
  if(log.duration!==null)parts.push(formatDuration(log.duration));
  if(log.failed)parts.push(`${log.failed} failed`);
  if(log.interrupted)parts.push('interrupted');
  if(log.stopReason)parts.push('stopped: '+log.stopReason.replace(/_/g,' '));
  // Without process metadata a pending status after the turn ended is only an unresolved report, not proof of a live process.
  if(log.running)parts.push(log.complete?`${log.running} without final status`:`${log.running} in progress`);
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
  function apply(area,items,nodes,{sessionIdle,scope,turnEnds}){
    const logs=buildWorkLogs(items,{sessionIdle,scope,turnEnds});const seen=new Set();
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
      const wantedNodes=log.members.map(id=>nodes.get(id)).filter(Boolean);
      // Re-append in chronological order whenever membership or order changed (e.g. an answer absorbed by a later tool).
      if(wantedNodes.length!==body.children.length||wantedNodes.some((n,i)=>body.children[i]!==n))body.append(...wantedNodes);
      group.querySelector('.work-log-counts').textContent=summaryText(log);
      group.classList.toggle('has-failure',!!(log.failed||log.jobsFailed||log.interrupted));
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
