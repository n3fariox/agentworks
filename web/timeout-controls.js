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
  const scopeRoot=scope=>scope==='default'?root:$('session-timeout-controls');
  for(const scope of ['default','session'])scopeRoot(scope).innerHTML=`<legend>${scope==='default'?'Execution timeouts · global default':'Execution timeouts'}</legend><div data-timeout-scope="${scope}">${scope==='default'?'<p class="field-note">Applies to every session without its own override, starting with its next turn.</p>':'<label class="workspace-check"><input type="checkbox" data-field="override"> Override the global default for this session</label>'}${scope==='session'?'<div class="field-note" role="status" data-note></div>':''}${fields.map(([k,l,n])=>`<label>${l}<input type="number" min="0" data-field="${k}" inputmode="numeric"></label><div class="field-note">${n}</div>`).join('')}<label class="workspace-check"><input type="checkbox" data-field="totalIncludesHumanWait"> Total limit counts time waiting for approvals</label>${scope==='default'?'<button type="button" class="text-button" data-save>Save default</button>':''}${scope==='default'?'<div class="field-note" role="status" data-note></div>':''}</div>`;
  const part=s=>scopeRoot(s).querySelector(`[data-timeout-scope="${s}"]`);
  const f=(s,k)=>part(s).querySelector(`[data-field="${k}"]`);
  function fill(s,p){for(const[k]of fields)f(s,k).value=p?.[k]??'';f(s,'totalIncludesHumanWait').checked=p?.totalIncludesHumanWait??true;}
  function read(s){const p={totalIncludesHumanWait:f(s,'totalIncludesHumanWait').checked};for(const[k]of fields)p[k]=num(f(s,k).value);return p;}
  const policyKey=policy=>JSON.stringify([...fields.map(([key])=>policy?.[key]??null),policy?.totalIncludesHumanWait??true]);
  let defaultBaseline=null,sessionBaseline=null;
  const sessionDraft=()=>f('session','override').checked?read('session'):null;
  const defaultDirty=()=>defaultBaseline!==null&&policyKey(read('default'))!==defaultBaseline;
  const sessionDirty=()=>sessionBaseline!==null&&JSON.stringify(sessionDraft())!==sessionBaseline;
  const syncSessionInputs=()=>{for(const input of part('session').querySelectorAll('[data-field]:not([data-field="override"])'))input.disabled=!f('session','override').checked;};
  const note=(s,t)=>part(s).querySelector('[data-note]').textContent=t;
  async function load(){
    let saved=null;defaultBaseline=null;sessionBaseline=null;
    for(const control of part('default').querySelectorAll('input,button'))control.disabled=true;
    try{const r=await api('/api/timeouts');saved=r.default;fill('default',saved);note('default','Applies to later turns: '+describe(saved));defaultBaseline=policyKey(read('default'));for(const control of part('default').querySelectorAll('input,button'))control.disabled=false;}catch(e){note('default','Cannot load timeouts: '+e.message);}
    const a=getActive();part('session').hidden=!a;
    // Next-turn policy is the override, else the freshly loaded default (never a stale snapshot).
    if(a&&!a.timeouts&&saved)a.effectiveTimeouts=saved;
    if(a){f('session','override').checked=!!a.timeouts;fill('session',a.timeouts||a.effectiveTimeouts);note('session',(a.timeouts?'Session override. ':'Using global default. ')+'Next turn uses: '+describe(a.effectiveTimeouts));sessionBaseline=JSON.stringify(sessionDraft());syncSessionInputs();}
  }
  function acceptDefault(policy){
    const keepSessionDraft=sessionDirty();
    defaultBaseline=policyKey(policy);
    note('default','Saved. Later turns use: '+describe(policy));
    const a=getActive();
    if(a&&!a.timeouts){
      a.effectiveTimeouts=policy;
      if(!keepSessionDraft){fill('session',policy);note('session','Using global default. Next turn uses: '+describe(policy));}
    }
  }
  async function saveDefault(){
    try{
      const r=await api('/api/timeouts',{method:'PUT',body:JSON.stringify(read('default'))});
      acceptDefault(r.default);
    }catch(e){note('default','Not saved: '+e.message);throw e;}
  }
  async function saveSession(){
    const a=getActive();if(!a)return;
    try{
      const r=await api(`/api/sessions/${encodeURIComponent(a.id)}/timeouts`,{method:'PUT',body:JSON.stringify({policy:sessionDraft()})});
      a.timeouts=r.timeouts;a.effectiveTimeouts=r.effective;
      fill('session',r.effective);f('session','override').checked=!!r.timeouts;
      sessionBaseline=JSON.stringify(sessionDraft());syncSessionInputs();
      note('session','Saved. Next turn uses: '+describe(r.effective));
    }catch(e){note('session','Not saved: '+e.message);throw e;}
  }
  for(const scope of ['default','session']){
    part(scope).addEventListener('input',()=>{
      if(scope==='session')syncSessionInputs();
      if(scope==='default'?defaultDirty():sessionDirty())note(scope,scope==='default'?'Unsaved changes. Use Save settings or Save default.':'Unsaved changes. Use Save session settings.');
    });
    if(scope==='default')part(scope).querySelector('[data-save]').onclick=async()=>{
      const button=part(scope).querySelector('[data-save]');button.disabled=true;
      try{await(scope==='default'?saveDefault():saveSession());}catch{}finally{button.disabled=false;}
    };
  }
  const pendingDefault=()=>defaultDirty()?read('default'):undefined;
  $('session-settings').onclick=async()=>{
    const active=getActive();if(!active)return;
    $('session-settings-scope').textContent='Applies only to “'+active.title+'”.';
    $('session-settings-error').textContent='';
    await load();$('session-settings-dialog').showModal();
  };
  $('dismiss-session-settings').onclick=()=>$('session-settings-dialog').close();
  $('session-settings-form').onsubmit=async event=>{
    event.preventDefault();$('save-session-settings').disabled=true;$('session-settings-error').textContent='';
    try{await saveSession();$('session-settings-dialog').close();}
    catch(e){$('session-settings-error').textContent='Session settings were not saved: '+e.message;}
    finally{$('save-session-settings').disabled=false;}
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
  return {load,notice,pendingDefault,acceptDefault};
}
