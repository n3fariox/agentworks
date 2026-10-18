// Composer usage statusline (U-07). Reads only persisted/live ACP `usage_update`
// events already delivered to the browser; it never submits turns or calls usage
// commands. Context occupancy, cumulative cost and account quota stay separate.
// Account-quota retrieval is out of scope: quota windows appear only when an
// event explicitly reports them, otherwise they are labelled unknown.
export const STALE_MS=10*60*1000;

const num=v=>typeof v==='number'&&Number.isFinite(v)?v:undefined;
const fmt=n=>n>=1e6?(n/1e6).toFixed(n>=1e7?0:1)+'M':n>=1e3?(n/1e3).toFixed(n>=1e4?0:1)+'k':String(n);

export function emptyUsage(){return {context:null,cost:null,quota:{},observedAt:null,source:null,events:0,resetReason:null};}

// Normalise one reported quota window. Accepts Codex-style {usedPercent,
// windowDurationMins,resetsAt} or explicit {used,remaining} percentages.
function quotaWindow(w){
  if(!w||typeof w!=='object')return null;
  const used=num(w.usedPercent)??num(w.used_percent);
  const remaining=num(w.remainingPercent)??num(w.remaining_percent);
  if(used===undefined&&remaining===undefined)return null;
  const reset=num(w.resetsAt)??num(w.resets_at);
  return {used,remaining,resetsAt:reset===undefined?null:(reset<1e12?reset*1000:reset),minutes:num(w.windowDurationMins)??num(w.window_minutes)};
}
function quotaFrom(meta){
  const out={};
  const rl=meta?.rateLimits||meta?.rate_limits||meta?.['_codex/rateLimits'];
  if(!rl||typeof rl!=='object')return out;
  for(const key of ['primary','secondary','fiveHour','sevenDay']){
    const w=quotaWindow(rl[key]);if(!w)continue;
    const label=w.minutes===300||key==='fiveHour'?'5h':w.minutes===10080||key==='sevenDay'?'7d':null;
    if(label)out[label]=w; // Only windows whose duration is reported are shown.
  }
  return out;
}

// Apply one stored event. `e` is the persisted envelope ({kind,data,...}).
export function applyUsageEvent(state,e,now=Date.now()){
  const d=e?.data;
  if(e?.kind!=='acp'||d?.sessionUpdate!=='usage_update')return state;
  const at=num(e.at)??num(e.timestamp)??now;
  const next={...state,events:state.events+1,observedAt:at,source:'ACP usage_update',quota:{...state.quota}};
  const used=num(d.used),size=num(d.size);
  if(used!==undefined||size!==undefined){
    const prev=state.context;
    // A drop in occupancy for the same window size indicates compaction; a size
    // change indicates a model switch. Record the reason; never carry stale values.
    next.resetReason=prev&&used!==undefined&&prev.used!==undefined&&used<prev.used?(size!==prev.size?'model or context window changed':'context compacted'):null;
    next.context={used,size,percent:used!==undefined&&size?Math.min(100,used/size*100):undefined};
  }
  if(d.cost&&num(d.cost.amount)!==undefined)next.cost={amount:d.cost.amount,currency:d.cost.currency||'USD'};
  Object.assign(next.quota,quotaFrom(d._meta));
  return next;
}

export function usageView(state,{now=Date.now(),provider}={}){
  const stale=state.observedAt!==null&&now-state.observedAt>STALE_MS;
  const c=state.context;
  let label='Context unknown';
  if(c?.used!==undefined&&c.size)label=`${fmt(c.used)}/${fmt(c.size)} · ${Math.round(c.percent)}%`;
  else if(c?.used!==undefined)label=`${fmt(c.used)} ctx · size unknown`;
  const quota=['5h','7d'].map(k=>{const w=state.quota[k];if(!w)return `${k} quota: not reported`;
    const parts=[w.used!==undefined?`${Math.round(w.used)}% used`:'',w.remaining!==undefined?`${Math.round(w.remaining)}% remaining`:(w.used!==undefined?`${Math.round(100-w.used)}% remaining`:'')].filter(Boolean);
    return `${k} quota: ${parts.join(' / ')}${w.resetsAt?' · resets '+new Date(w.resetsAt).toLocaleString():' · reset unknown'}`;});
  const compactQuota=['5h','7d'].filter(k=>state.quota[k]?.used!==undefined).map(k=>`${k} ${Math.round(state.quota[k].used)}%`);
  const details=[
    c?`Context occupancy (current window): ${c.used!==undefined?c.used.toLocaleString():'unknown'} of ${c.size?c.size.toLocaleString():'unknown'} tokens${c.percent!==undefined?` (${c.percent.toFixed(1)}%)`:''}`:'Context occupancy: not reported yet',
    state.resetReason?`Occupancy dropped: ${state.resetReason}`:'',
    state.cost?`Reported session cost (cumulative, provider-reported ${state.cost.currency}; API-equivalent estimate — subscription accounts may not be billed this amount): ${state.cost.amount.toFixed(4)}`:'Session cost: not reported',
    ...quota,
    'Account quota is shown only when a provider event reports it; Agentworks does not query usage or infer quota from token counts.',
    `Source: ${state.source||'no usage events'}${provider?' · '+provider:''} · ${state.events} event(s)`,
    state.observedAt?`Observed ${new Date(state.observedAt).toLocaleString()}${stale?' — stale':''}`:'Freshness: no data',
  ].filter(Boolean);
  return {label:(stale?'⚠ ':'')+label+(compactQuota.length?' · '+compactQuota.join(' · '):''),stale,known:!!c,details};
}

// Wires the statusline into the composer. Hooks: reset() on session switch or
// snapshot replay, consume(event) per event, render() after updates.
export function setupUsageStatus({root,getProvider}){
  let state=emptyUsage();
  root.innerHTML='<button type="button" class="usage-status-toggle" aria-expanded="false" aria-controls="usage-status-details"></button><div id="usage-status-details" class="usage-status-details" role="region" aria-label="Usage details" hidden></div>';
  const button=root.firstChild,panel=root.lastChild;
  button.onclick=()=>{panel.hidden=!panel.hidden;button.setAttribute('aria-expanded',String(!panel.hidden));};
  const render=()=>{
    let provider;try{provider=getProvider?.();}catch{} // caller state may not be initialised yet
    const v=usageView(state,{provider});
    button.textContent=v.label;button.classList.toggle('stale',v.stale);button.classList.toggle('unknown',!v.known);
    button.title='Context usage details';
    panel.replaceChildren(...v.details.map(t=>{const p=document.createElement('div');p.textContent=t;return p;}));
  };
  render();
  return {
    reset(){state=emptyUsage();panel.hidden=true;button.setAttribute('aria-expanded','false');render();},
    consume(e){const next=applyUsageEvent(state,e);if(next!==state){state=next;return true;}return false;},
    render,
    get state(){return state;},
  };
}
