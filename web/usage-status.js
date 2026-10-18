// Composer usage statusline (U-07). Reads only persisted/live ACP `usage_update`
// events already delivered to the browser; it never submits turns or calls usage
// commands. Context occupancy, reported cost and account quota stay separate,
// each with its own freshness.
//
// Verified adapter shapes (installed sources, read-only):
// - claude-agent-acp: {used,size}, optional cost {amount: SDK total_cost_usd,
//   currency}, _meta "_claude/model", "_claude/origin" and "_claude/rateLimit"
//   (SDKRateLimitInfo: status, resetsAt, rateLimitType, utilization).
// - codex-acp / OpenCode: {used,size} only; Codex rate limits are not forwarded.
// Persisted envelopes are {seq,kind,data} without timestamps, so replayed values
// have unknown freshness; only live receipt is tagged as browser-observed time.
// Account-quota retrieval is out of scope: quota appears only when reported.
export const STALE_MS=10*60*1000;

const num=v=>typeof v==='number'&&Number.isFinite(v)?v:undefined;
const nonneg=v=>{const n=num(v);return n!==undefined&&n>=0?n:undefined;};
const fmt=n=>n>=1e6?(n/1e6).toFixed(n>=1e7?0:1)+'M':n>=1e3?(n/1e3).toFixed(n>=1e4?0:1)+'k':String(n);
const QUOTA_LABELS={five_hour:'5h',seven_day:'7d'};

export function emptyUsage(){return {context:null,cost:null,quota:{},model:null,events:0,ignored:0};}

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
  const used=nonneg(d.used),size=nonneg(d.size);
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
  const label=QUOTA_LABELS[rl?.rateLimitType];
  if(label){
    const u=num(rl.utilization);
    next.quota[label]={status:typeof rl.status==='string'?rl.status:null,
      utilization:u!==undefined&&u>=0&&u<=100?u:undefined,
      resetsAt:nonneg(rl.resetsAt)!==undefined?(rl.resetsAt<1e12?rl.resetsAt*1000:rl.resetsAt):null,...fresh};
  }
  return next;
}

function freshness(v,now){
  if(v.at==null)return {text:`freshness unknown (replayed${v.seq!=null?' event #'+v.seq:''}, no stored timestamp)`,stale:true};
  const stale=now-v.at>STALE_MS;
  return {text:`browser-observed ${new Date(v.at).toLocaleString()}${stale?' — stale':''}`,stale};
}

export function usageView(state,{now=Date.now(),provider}={}){
  const c=state.context;
  const cf=c?freshness(c,now):null;
  let label='Context unknown';
  if(c?.used!==undefined&&c.size)label=`${fmt(c.used)}/${fmt(c.size)} · ${Math.round(Math.min(100,c.percent))}%`;
  else if(c?.used!==undefined)label=`${fmt(c.used)} ctx · size unknown`;
  const compactQuota=Object.entries(state.quota).filter(([,w])=>w.utilization!==undefined&&!freshness(w,now).stale).map(([k,w])=>`${k} ${Math.round(w.utilization)}`);
  const quota=['5h','7d'].map(k=>{const w=state.quota[k];if(!w)return `${k} quota: not reported`;
    return `${k} quota (reported): status ${w.status||'unknown'} · utilization ${w.utilization!==undefined?w.utilization+' (provider scale, used not remaining)':'unknown'} · ${w.resetsAt?'resets '+new Date(w.resetsAt).toLocaleString():'reset unknown'} · ${freshness(w,now).text}`;});
  const details=[
    c?`Context occupancy (current window): ${c.used!==undefined?c.used.toLocaleString():'unknown'} of ${c.size!==undefined?c.size.toLocaleString():'unknown'} tokens${c.percent!==undefined?` (${c.percent.toFixed(1)}%)`:''} · ${cf.text}`:'Context occupancy: not reported yet',
    c?.dropped?'Occupancy decreased since the previous report (cause not reported)':'',
    c?.sizeChanged?'Reported context window size changed':'',
    state.model?`Reported model: ${state.model.name} · ${freshness(state.model,now).text}`:'',
    state.cost?`Provider-reported cost: ${state.cost.amount.toFixed(4)} ${state.cost.currency||'(currency unknown)'} · scope and account billing not verified · ${freshness(state.cost,now).text}`:'Cost: not reported',
    ...quota,
    'Quota appears only when a provider event reports it; Agentworks does not query usage or infer quota from token counts.',
    `Source: ACP usage_update${provider?' · '+provider:''} · ${state.events} event(s)${state.ignored?` · ${state.ignored} subagent/parent-attributed ignored`:''}`,
  ].filter(Boolean);
  const stale=!c||cf.stale;
  return {label:(c&&stale?'⚠ ':'')+label+(compactQuota.length?' · '+compactQuota.join(' · '):''),stale,known:!!c,details};
}

// Wires the statusline into the composer. Hooks: reset() on session switch or
// snapshot replay, consume(event,{live}) per event, render() after updates.
export function setupUsageStatus({root,getProvider}){
  let state=emptyUsage();
  root.innerHTML='<button type="button" class="usage-status-toggle" aria-expanded="false" aria-controls="usage-status-details"></button><div id="usage-status-details" class="usage-status-details" role="region" aria-label="Usage details" hidden></div>';
  const button=root.firstChild,panel=root.lastChild;
  button.onclick=()=>{panel.hidden=!panel.hidden;button.setAttribute('aria-expanded',String(!panel.hidden));};
  const render=()=>{
    const v=usageView(state,{provider:getProvider()});
    button.textContent=v.label;button.classList.toggle('stale',v.stale);button.classList.toggle('unknown',!v.known);
    button.title='Context usage details';
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
