// Composer usage statusline (U-07). Reads only persisted/live ACP `usage_update`
// events already delivered to the browser; it never submits turns or calls usage
// commands. Context occupancy, reported cost and account quota stay separate,
// each with its own freshness.
//
// Verified adapter shapes (installed sources, read-only):
// - claude-agent-acp: {used,size}, optional cost {amount: SDK total_cost_usd,
//   currency}, _meta "_claude/model", "_claude/origin" and "_claude/rateLimit"
//   (SDKRateLimitInfo: status, resetsAt, rateLimitType, utilization).
// - local codex-acp fork: genuine {used,size} plus versioned
//   _codex/subscriptionLimits metadata with per-field adapter observation times.
// - OpenCode: {used,size} only.
// Persisted envelopes have no timestamps. Codex metadata retains its source
// times on replay; other replayed values have unknown freshness. Live context
// receipt is tagged as browser-observed time.
// Account-quota retrieval is out of scope: quota appears only when reported.
export const STALE_MS=10*60*1000;

const num=v=>typeof v==='number'&&Number.isFinite(v)?v:undefined;
const nonneg=v=>{const n=num(v);return n!==undefined&&n>=0?n:undefined;};
const fmt=n=>n>=1e6?(n/1e6).toFixed(n>=1e7?0:1)+'M':n>=1e3?(n/1e3).toFixed(n>=1e4?0:1)+'k':String(n);
const QUOTA_LABELS={five_hour:'5h',seven_day:'7d'};
// Time until a reported reset, coarse enough to read at a glance.
export function countdown(ms){
  if(ms<=0)return null;
  const m=Math.ceil(ms/60000),d=Math.floor(m/1440),h=Math.floor(m%1440/60),r=m%60;
  return d?`${d}d ${h}h`:h?`${h}h ${r}m`:`${r}m`;
}
function resetText(at,now){
  if(at==null)return null;
  const left=countdown(at-now);
  return left?`resets in ${left}`:'reset time passed';
}

export function emptyUsage(){return {context:null,cost:null,quota:{},codexQuota:{},model:null,events:0,ignored:0};}

// Codex percentages are 0..100, not Claude's 0..1 utilization fractions.
// Namespaced metadata is additive: it never supplies or changes context values.
function applyCodexQuota(previous,meta,fresh){
  if(meta?.version!==1||!Array.isArray(meta.limits))return previous;
  const result={...previous};
  for(const limit of meta.limits){
    if(typeof limit?.limitId!=='string')continue;
    for(const role of ['primary','secondary']){
      if(!Object.hasOwn(limit,role))continue;
      const key=JSON.stringify([limit.limitId,role]),raw=limit[role];
      if(raw===null){delete result[key];continue;}
      if(!raw||typeof raw!=='object')continue;
      const entry={...result[key],limitId:limit.limitId,role};
      if(Object.hasOwn(limit,'limitName'))entry.limitName=typeof limit.limitName==='string'?limit.limitName:null;
      for(const field of ['usedPercent','windowDurationMins','resetsAt']){
        if(!Object.hasOwn(raw,field))continue;
        const cell=raw[field];
        let value=nonneg(cell?.value);
        if(field==='usedPercent'&&value>100)value=undefined;
        if(field==='windowDurationMins'&&value===0)value=undefined;
        if(field==='resetsAt'&&value!==undefined)value*=1000;
        if(field==='resetsAt'&&value>8.64e15)value=undefined;
        if(value!==undefined&&!Number.isFinite(value))value=undefined;
        const reportedAt=nonneg(cell?.observedAtMs);
        const at=reportedAt!==undefined&&reportedAt<=8.64e15?reportedAt:null;
        // Preserve each field's source time even during replay or repeated cache
        // emission. Missing cells retain their own old values and observation.
        entry[field]={value,at,source:'adapter',replayed:fresh.at===null,seq:fresh.seq};
      }
      result[key]=entry;
    }
  }
  return result;
}

// Usage explicitly attributed to a parent tool call or subagent must not
// overwrite the composer's own session values.
function attributedElsewhere(d){
  const m=d._meta||{};
  return ['parentToolUseId','parent_tool_use_id','_claude/parentToolUseId','subagent','_claude/subagent','agentId'].some(k=>d[k]!=null||m[k]!=null);
}

// `observedAt` is a browser receipt time for live events, or null for replay.
export function applyUsageEvent(state,e,observedAt=null){
  const d=e?.data;
  if(e?.kind!=='acp'||d?.sessionUpdate!=='usage_update')return state;
  if(attributedElsewhere(d))return {...state,ignored:state.ignored+1};
  const fresh={at:observedAt,seq:num(e.seq)??null};
  const next={...state,events:state.events+1,quota:{...state.quota}};
  const used=nonneg(d.used),size=nonneg(d.size)||undefined; // size 0 means unknown
  if(used!==undefined||size!==undefined){
    const prev=state.context;
    next.context={used,size,percent:used!==undefined&&size?used/size*100:undefined,...fresh,
      // An occupancy drop is reported as observed only; its cause is not inferred.
      dropped:!!(prev&&used!==undefined&&prev.used!==undefined&&used<prev.used),
      sizeChanged:!!(prev&&size!==undefined&&prev.size!==undefined&&size!==prev.size)};
  }
  const amount=nonneg(d.cost?.amount);
  if(amount!==undefined)next.cost={amount,currency:typeof d.cost.currency==='string'&&d.cost.currency?d.cost.currency:null,...fresh};
  const model=d._meta?.['_claude/model'];
  if(typeof model==='string'&&model)next.model={name:model,...fresh};
  const rl=d._meta?.['_claude/rateLimit'];
  // Only `unifiedWindows` fractions (0..1, verified from live persisted events) give
  // a used share. Scalar `utilization` has an unverified scale and is not shown.
  const windows=rl?.unifiedWindows&&typeof rl.unifiedWindows==='object'?rl.unifiedWindows:{};
  for(const [key,label] of Object.entries(QUOTA_LABELS)){
    const w=windows[key];
    const u=num(w?.utilization);
    if(u===undefined||u<0||u>1)continue;
    const reset=nonneg(w.resetsAt);
    next.quota[label]={usedPercent:u*100,status:rl.rateLimitType===key&&typeof rl.status==='string'?rl.status:null,
      resetsAt:reset===undefined?null:(reset<1e12?reset*1000:reset),...fresh};
  }
  next.codexQuota=applyCodexQuota(state.codexQuota,d._meta?.['_codex/subscriptionLimits'],fresh);
  return next;
}

function freshness(v,now){
  if(v.at==null&&v.source==='adapter')return {text:`freshness unknown (${v.replayed?'replayed, ':''}no source observation time)`,stale:true};
  if(v.at==null)return {text:`freshness unknown (replayed${v.seq!=null?' event #'+v.seq:''}, no stored timestamp)`,stale:true};
  const stale=now-v.at>STALE_MS;
  return {text:`${v.source==='adapter'?'adapter':'browser'}-observed ${new Date(v.at).toLocaleString()}${v.replayed?' (replayed)':''}${stale?' — stale':''}`,stale};
}

function codexWindows(state,now){
  return Object.values(state.codexQuota||{}).map(w=>{
    const mins=w.windowDurationMins?.value;
    const duration=mins===undefined?'duration unknown':mins%1440===0?`${mins/1440}d`:mins%60===0?`${mins/60}h`:`${mins}m`;
    const name=w.limitName&&w.limitName!==w.limitId?`${w.limitName} (${w.limitId})`:w.limitId;
    const label=`${name} ${w.role} (${duration})`;
    const used=w.usedPercent?.value;
    const f=w.usedPercent?freshness(w.usedPercent,now):{text:'freshness unknown',stale:true};
    const remaining=used===undefined?'usage unknown':`${Math.round(100-used)}% left`;
    const reset=w.resetsAt?.value;
    const rt=reset===undefined?'reset unknown':`${resetText(reset,now)} (${new Date(reset).toLocaleString()}) · ${freshness(w.resetsAt,now).text}`;
    return {window:w,label,remaining,stale:f.stale,
      compact:`${label} ${remaining}${f.stale?' ⚠':''}`,
      detail:`${label}: ${used===undefined?'usage unknown':`${used}% used / ${100-used}% remaining`} · ${f.text} · ${rt}${w.windowDurationMins?` · duration ${freshness(w.windowDurationMins,now).text}`:''}`};
  });
}

export function usageView(state,{now=Date.now(),provider}={}){
  const c=state.context;
  const cf=c?freshness(c,now):null;
  let label='Context unknown';
  if(c?.used!==undefined&&c.size)label=`${fmt(c.used)}/${fmt(c.size)} · ${Math.round(Math.min(100,c.percent))}%`;
  else if(c?.used!==undefined)label=`${fmt(c.used)} ctx · size unknown`;
  const pct=n=>Math.round(n);
  // Quota is shown as what is left; the used share is its complement.
  const compactQuota=['5h','7d'].filter(k=>state.quota[k]).map(k=>{const w=state.quota[k],reset=resetText(w.resetsAt,now);return `${k} ${pct(100-w.usedPercent)}% left${freshness(w,now).stale?' ⚠':''}${reset?` (${reset})`:''}`;});
  const quota=['5h','7d'].map(k=>{const w=state.quota[k];if(!w)return `${k} quota: not reported`;
    return `${k} quota (reported): ${pct(100-w.usedPercent)}% remaining${w.status?' · status '+w.status:''} · ${w.resetsAt?`${resetText(w.resetsAt,now)} (${new Date(w.resetsAt).toLocaleString()})`:'reset unknown'} · ${freshness(w,now).text}`;});
  const codex=codexWindows(state,now);
  const quotaDetails=provider==='codex'?(codex.length?codex.map(w=>w.detail):['Subscription quota: not reported']):[...quota,...codex.map(w=>w.detail)];
  const details=[
    c?`Context occupancy (current window): ${c.used!==undefined?c.used.toLocaleString():'unknown'} of ${c.size!==undefined?c.size.toLocaleString():'unknown'} tokens${c.percent!==undefined?` (${c.percent.toFixed(1)}%)`:''} · ${cf.text}`:'Context occupancy: not reported yet',
    c?.dropped?'Occupancy decreased since the previous report (cause not reported)':'',
    c?.sizeChanged?'Reported context window size changed':'',
    state.model?`Reported model: ${state.model.name} · ${freshness(state.model,now).text}`:'',
    state.cost?`Provider-reported cost: ${state.cost.amount.toFixed(4)} ${state.cost.currency||'(currency unknown)'} · scope and account billing not verified · ${freshness(state.cost,now).text}`:'Cost: not reported',
    ...quotaDetails,
    'Quota appears only when a provider event reports it; Agentworks does not query usage or infer quota from token counts.',
    `Source: ACP usage_update${provider?' · '+provider:''} · ${state.events} event(s)${state.ignored?` · ${state.ignored} subagent/parent-attributed ignored`:''}`,
  ].filter(Boolean);
  const stale=!c||cf.stale;
  // Glanceable form for the phone composer chip: context used and 5h quota left.
  const primary=codex.find(w=>w.window.role==='primary')||codex[0];
  const short=[c?.percent!==undefined?`ctx ${Math.round(Math.min(100,c.percent))}%`:'',state.quota['5h']?`5h ${pct(100-state.quota['5h'].usedPercent)}% left`:'',primary?`${primary.label} ${primary.remaining}${primary.stale?' ⚠':''}`:''].filter(Boolean).join(' · ');
  const allQuota=[...compactQuota,...codex.map(w=>w.compact)];
  return {label:(c&&stale?'⚠ ':'')+label+(allQuota.length?' · '+allQuota.join(' · '):''),short:short&&(c&&stale?'⚠ ':'')+short,stale,known:!!c,details};
}

// Wires the statusline into the composer. Hooks: reset() on session switch or
// snapshot replay, consume(event,{live}) per event, render() after updates.
// An optional `chip` mirrors the short label (hidden while nothing is reported).
export function setupUsageStatus({root,getProvider,chip}){
  let state=emptyUsage();
  root.innerHTML='<button type="button" class="usage-status-toggle" aria-expanded="false" aria-controls="usage-status-details"></button><div id="usage-status-details" class="usage-status-details" role="region" aria-label="Usage details" hidden></div>';
  const button=root.firstChild,panel=root.lastChild;
  button.onclick=()=>{panel.hidden=!panel.hidden;button.setAttribute('aria-expanded',String(!panel.hidden));};
  const render=()=>{
    const v=usageView(state,{provider:getProvider()});
    button.textContent=v.label;button.classList.toggle('stale',v.stale);button.classList.toggle('unknown',!v.known);
    button.title='Context usage details';
    if(chip){chip.textContent=v.short;chip.hidden=!v.short;chip.classList.toggle('stale',v.stale);chip.setAttribute('aria-label',`Usage: ${v.label}. More options`);}
    panel.replaceChildren(...v.details.map(t=>{const p=document.createElement('div');p.textContent=t;return p;}));
  };
  render();
  return {
    reset(){state=emptyUsage();panel.hidden=true;button.setAttribute('aria-expanded','false');render();},
    consume(e,{live=false}={}){state=applyUsageEvent(state,e,live?Date.now():null);},
    render,
    get state(){return state;},
  };
}
