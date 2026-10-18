// L-07 execution timeout settings and transcript notices. Policies apply from the next turn launch.
const fields=[
  ['totalSeconds','Total turn limit (seconds)','Blank disables the limit for long work · 60–604800'],
  ['inactivityWarningSeconds','Inactivity warning (seconds)','Blank: no warning · 30–86400'],
  ['inactivityTimeoutSeconds','Inactivity timeout (seconds)','Blank: never stop quiet tools · 60–86400'],
  ['cancelGraceSeconds','Cancellation grace (seconds)','5–300'],
];
const num=v=>v===''?null:Number(v);
export function describe(p){
  if(!p)return'';
  const total=p.totalSeconds?`${p.totalSeconds}s total (${p.totalIncludesHumanWait?'includes':'excludes'} approval waits)`:'no total limit';
  return `${total} · ${p.inactivityWarningSeconds?`warn after ${p.inactivityWarningSeconds}s quiet`:'no inactivity warning'} · ${p.inactivityTimeoutSeconds?`stop after ${p.inactivityTimeoutSeconds}s quiet`:'quiet tools not stopped'} · ${p.cancelGraceSeconds}s cancel grace`;
}
export function setupTimeoutControls({$,api,getActive}){
  const root=$('timeout-controls');
  root.innerHTML=`<legend>Execution timeouts</legend>${['default','session'].map(scope=>`<div data-timeout-scope="${scope}"><h3>${scope==='default'?'Saved default':'This session override'}</h3>${scope==='session'?'<label class="workspace-check"><input type="checkbox" data-field="override"> Override the default for this session</label>':''}${fields.map(([k,l,n])=>`<label>${l}<input type="number" min="0" data-field="${k}" inputmode="numeric"></label><div class="field-note">${n}</div>`).join('')}<label class="workspace-check"><input type="checkbox" data-field="totalIncludesHumanWait"> Total limit counts time waiting for approvals</label><button type="button" class="text-button" data-save>Save ${scope==='default'?'default':'session override'}</button><div class="field-note" role="status" data-note></div></div>`).join('')}`;
  const part=s=>root.querySelector(`[data-timeout-scope="${s}"]`);
  const f=(s,k)=>part(s).querySelector(`[data-field="${k}"]`);
  function fill(s,p){for(const[k]of fields)f(s,k).value=p?.[k]??'';f(s,'totalIncludesHumanWait').checked=p?.totalIncludesHumanWait??true;}
  function read(s){const p={totalIncludesHumanWait:f(s,'totalIncludesHumanWait').checked};for(const[k]of fields)p[k]=num(f(s,k).value);return p;}
  const note=(s,t)=>part(s).querySelector('[data-note]').textContent=t;
  async function load(){
    let saved=null;
    try{const r=await api('/api/timeouts');saved=r.default;fill('default',saved);note('default','Applies to later turns: '+describe(saved));}catch(e){note('default','Cannot load timeouts: '+e.message);}
    const a=getActive();part('session').hidden=!a;
    // Next-turn policy is the override, else the freshly loaded default (never a stale snapshot).
    if(a&&!a.timeouts&&saved)a.effectiveTimeouts=saved;
    if(a){f('session','override').checked=!!a.timeouts;fill('session',a.timeouts||a.effectiveTimeouts);note('session','Next turn uses: '+describe(a.effectiveTimeouts));}
  }
  part('default').querySelector('[data-save]').onclick=async()=>{
    try{const r=await api('/api/timeouts',{method:'PUT',body:JSON.stringify(read('default'))});note('default','Saved. Later turns use: '+describe(r.default));
      const a=getActive();if(a&&!a.timeouts){a.effectiveTimeouts=r.default;fill('session',r.default);note('session','Next turn uses: '+describe(r.default));}}catch(e){note('default',e.message);}
  };
  part('session').querySelector('[data-save]').onclick=async()=>{
    const a=getActive();if(!a)return;
    try{const r=await api(`/api/sessions/${encodeURIComponent(a.id)}/timeouts`,{method:'PUT',body:JSON.stringify({policy:f('session','override').checked?read('session'):null})});
      a.timeouts=r.timeouts;a.effectiveTimeouts=r.effective;note('session','Saved. Next turn uses: '+describe(r.effective));}catch(e){note('session',e.message);}
  };
  // Transcript notice for timeout events; returns null for unrelated kinds.
  function notice(kind,d){
    // The running turn's fixed policy stays separate from the next-turn effective policy.
    if(kind==='timeout_policy'){const a=getActive();if(a)a.currentTimeouts=d.policy;return 'Turn limits: '+describe(d.policy);}
    if(kind==='timeout_warning')return d.kind==='total'?`Turn limit approaching: ${d.remainingSeconds}s left`:`No output for ${d.quietSeconds}s; the agent may be waiting on a quiet tool`;
    if(kind==='timeout')return `Turn stopped by ${d.kind==='total'?'total limit':'inactivity timeout'}; cancelling (${d.graceSeconds}s grace). It will not be retried.`;
    if(kind==='timeout_settings')return 'Session timeout settings changed for the next turn: '+describe(d.effective);
    if((kind==='turn_end'||kind==='error')&&d.timedOut)return `Turn ended after ${d.timedOut==='total'?'total limit':'inactivity timeout'}`;
    return null;
  }
  return {load,notice};
}
