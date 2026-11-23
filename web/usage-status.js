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
// Antigravity quota comes separately from its native /usage command; receipt
// times are preserved in antigravity_quota events and the shared account feed.
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
// Clock time for "updated …" text; earlier days also name the date.
export function shortTime(at,now=Date.now()){
  const d=new Date(at),time=d.toLocaleTimeString(undefined,{hour:'numeric',minute:'2-digit'});
  return d.toDateString()===new Date(now).toDateString()?time:`${d.toLocaleDateString(undefined,{month:'short',day:'numeric'})} ${time}`;
}
function resetText(at,now){
  if(at==null)return null;
  const left=countdown(at-now);
  return left?`resets in ${left}`:'reset time passed';
}

export function emptyUsage(){return {context:null,cost:null,quota:{},codexQuota:{},antigravityQuota:null,model:null,events:0,ignored:0};}

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
  if(d.parentSessionId!=null)return true;
  const m=d._meta||{};
  return ['parentToolUseId','parent_tool_use_id','_claude/parentToolUseId','subagent','_claude/subagent','agentId'].some(k=>d[k]!=null||m[k]!=null);
}

// `observedAt` is a browser receipt time for live events, or null for replay.
export function applyUsageEvent(state,e,observedAt=null){
  const d=e?.data;
  if(e?.kind==='antigravity_quota'){
    const report=antigravityQuota(d);
    return report&&report.observedAtMs>(state.antigravityQuota?.observedAtMs??-1)?{...state,antigravityQuota:report}:state;
  }
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

function antigravityQuota(raw){
  const at=nonneg(raw?.observedAtMs);
  if(at===undefined||!Array.isArray(raw?.windows))return null;
  return {...raw,observedAtMs:at,windows:raw.windows.filter(w=>typeof w?.id==='string'&&typeof w.group==='string').map(w=>{
    const value=num(w.remainingFraction),reset=typeof w.resetTime==='string'?Date.parse(w.resetTime):NaN;
    return {...w,remainingFraction:value!==undefined&&value>=0&&value<=1?value:null,resetsAt:Number.isFinite(reset)?reset:null};
  })};
}
// Window length in the same 5h/7d form as Claude, for the narrow composer pill.
const WINDOW_LENGTHS={weekly:'7d',daily:'1d',hourly:'1h','5hr':'5h','7day':'7d'};
function windowLength(w){
  const raw=(w.window||/(?:^|-)(\d+(?:hr|[hdm])|weekly|daily|hourly)$/i.exec(w.id)?.[1]||'').toLowerCase();
  return raw?WINDOW_LENGTHS[raw]||raw.replace(/hr$/,'h'):w.name||w.id;
}
export function antigravityWindows(state,now=Date.now()){
  const report=state.antigravityQuota;if(!report)return [];
  const fresh=freshness({at:report.observedAtMs,source:'server'},now);
  return report.windows.map(w=>{
    const label=`${w.group} · ${w.window||w.name||w.id}`;
    const percent=w.remainingFraction===null?null:100*w.remainingFraction;
    const remaining=percent===null?'quota unknown':`${Math.round(percent)}% left`;
    const warn=fresh.stale?' ⚠':'';
    return {...w,label,percent,stale:fresh.stale,observedAtMs:report.observedAtMs,compact:`${label} ${remaining}${warn}`,pill:`${windowLength(w)} ${remaining}${warn}`,
      detail:`${label}: ${remaining} · ${w.resetsAt===null?'reset unknown':resetText(w.resetsAt,now)} · ${fresh.text}`};
  });
}

// Account quota shared by every session on one host and provider family
// (`crates/agentworks/src/account_quota.rs`). Goose families are its provider profile.
export function quotaFamily(provider,modelProvider){
  if(provider==='claude'||provider==='claude-rust')return 'claude';
  if(provider==='codex'||provider==='codex-rust')return 'codex';
  return provider==='goose'?`goose/${modelProvider||'default'}`:provider;
}
export function quotaKey(hostId,family){return JSON.stringify([hostId||'local',family]);}

// Converts a server entry to usage state. Claude windows carry Agentworks receipt
// time; Codex cells keep their adapter observation times.
export function sharedQuota(entry){
  if(!entry)return null;
  let state=emptyUsage();
  state.antigravityQuota=antigravityQuota(entry.antigravity);
  for(const [key,w] of Object.entries(entry?.claude?.windows||{})){
    const at=nonneg(w?.at)??null;
    const rateLimit={status:w.status,rateLimitType:w.status==null?null:key,unifiedWindows:{[key]:w}};
    state=applyUsageEvent(state,{kind:'acp',data:{sessionUpdate:'usage_update',_meta:{'_claude/rateLimit':rateLimit}}},at);
  }
  for(const label of Object.keys(state.quota))state.quota[label].source='server';
  const limits=Object.values(entry?.codex?.limits||{});
  state.codexQuota=applyCodexQuota({},{version:1,limits},{at:nonneg(entry?.codex?.at)??null,seq:null});
  return state;
}
const newer=(mine,shared)=>!mine?shared:!shared?mine:(shared.at??-1)>(mine.at??-1)?shared:mine;
// Each quota window independently shows whichever report is newest; the
// session's own report wins ties.
export function withSharedQuota(state,shared){
  if(!shared)return state;
  const quota={...state.quota},codexQuota={...state.codexQuota};
  for(const [k,v] of Object.entries(shared.quota))quota[k]=newer(quota[k],v);
  for(const [k,w] of Object.entries(shared.codexQuota)){
    const mine=codexQuota[k];
    if(!mine){codexQuota[k]=w;continue;}
    const merged={...w,...mine};
    for(const field of ['usedPercent','windowDurationMins','resetsAt'])merged[field]=newer(mine[field],w[field]);
    codexQuota[k]=merged;
  }
  const mine=state.antigravityQuota,other=shared.antigravityQuota;
  const antigravityQuota=(other?.observedAtMs??-1)>(mine?.observedAtMs??-1)?other:mine;
  return {...state,quota,codexQuota,antigravityQuota};
}

// Replayed values without a source time have unknown age and count as stale.
// Where each time comes from is documented in docs/USER-GUIDE.md.
function freshness(v,now){
  if(v.at==null)return {text:'update time unknown',stale:true};
  const stale=now-v.at>STALE_MS;
  return {text:`updated ${shortTime(v.at,now)}${stale?' · stale':''}`,stale};
}

function codexWindows(state,now){
  return Object.values(state.codexQuota||{}).map(w=>{
    const mins=w.windowDurationMins?.value;
    const duration=mins===undefined?'duration unknown':mins%1440===0?`${mins/1440}d`:mins%60===0?`${mins/60}h`:`${mins}m`;
    const name=w.limitName&&w.limitName!==w.limitId?`${w.limitName} (${w.limitId})`:w.limitId;
    const label=`${name} ${w.role} (${duration})`;
    // The pill names a window by its length, like Claude's 5h/7d; only extra
    // limit families keep a name, and the role stands in for an unknown length.
    const brief=[w.limitId==='codex'?'':w.limitName||w.limitId,mins===undefined?w.role:duration].filter(Boolean).join(' ');
    const used=w.usedPercent?.value;
    const f=w.usedPercent?freshness(w.usedPercent,now):{text:'freshness unknown',stale:true};
    const remaining=used===undefined?'usage unknown':`${Math.round(100-used)}% left`;
    const reset=w.resetsAt?.value;
    return {window:w,label,brief,remaining,stale:f.stale,
      compact:`${brief} ${remaining}${f.stale?' ⚠':''}`,
      detail:`${label}: ${used===undefined?'usage unknown':`${+(100-used).toFixed(1)}% left`} · ${reset===undefined?'reset unknown':resetText(reset,now)} · ${f.text}`};
  });
}

// Native /usage currently has no model-to-bucket IDs. These mappings cover the
// installed model catalog and reported bucket IDs; unknown models stay unknown.
export function activeAntigravityWindows(state,model,now=Date.now()){
  const windows=antigravityWindows(state,now);
  const effort='(?:-(?:low|medium|high|xhigh|max))?';
  let ids=[];
  if(new RegExp('^gemini-4-argon'+effort+'$').test(model||''))ids=['gemini-argon-wtu-weekly'];
  // Hydrogen has no dedicated bucket in /usage (agy 1.5.8), so it draws on the
  // shared Gemini buckets; a hydrogen-specific bucket wins if one appears.
  else if(new RegExp('^gemini-4-hydrogen'+effort+'$').test(model||'')){
    const own=windows.filter(w=>/(^|-)hydrogen(-|$)/.test(w.id)).map(w=>w.id);
    ids=own.length?own:['gemini-weekly','gemini-5h'];
  }
  // The Argon orchestrator draws on both Argon and the shared Gemini buckets;
  // Argon is listed first and leads the label (see leadAntigravityWindow).
  else if(new RegExp('^gemini-pro-orchestrator-g1-pro'+effort+'$').test(model||''))ids=['gemini-argon-wtu-weekly','gemini-weekly','gemini-5h'];
  else if(new RegExp('^gemini-3\\.8-flash'+effort+'$').test(model||''))ids=['gemini-weekly','gemini-5h'];
  else if(new RegExp('^(?:claude-(?:opus|sonnet)-5-5|gpt-oss-120b)'+effort+'$').test(model||''))ids=['3p-weekly','3p-5h'];
  return windows.filter(w=>ids.includes(w.id)).sort((a,b)=>ids.indexOf(a.id)-ids.indexOf(b.id));
}

// Models that draw on several groups lead with a fixed bucket; others show
// whichever mapped bucket is tightest.
const ANTIGRAVITY_LEAD={'gemini-pro-orchestrator-g1-pro':'gemini-argon-wtu-weekly'};
function leadAntigravityWindow(windows,model){
  const lead=ANTIGRAVITY_LEAD[(model||'').replace(/-(?:low|medium|high|xhigh|max)$/,'')];
  const fixed=lead&&windows.find(w=>w.id===lead&&w.percent!==null);
  return fixed||windows.filter(w=>w.percent!==null).sort((a,b)=>a.percent-b.percent)[0]||windows[0];
}

export function usageView(state,{now=Date.now(),provider,model}={}){
  const c=state.context;
  const cf=c?freshness(c,now):null;
  let label='Context unknown';
  if(c?.used!==undefined&&c.size)label=`${fmt(c.used)}/${fmt(c.size)} · ${Math.round(Math.min(100,c.percent))}%`;
  else if(c?.used!==undefined)label=`${fmt(c.used)} ctx · size unknown`;
  const pct=n=>Math.round(n);
  // Quota is shown as what is left; the used share is its complement.
  const compactQuota=['5h','7d'].filter(k=>state.quota[k]).map(k=>{const w=state.quota[k],reset=resetText(w.resetsAt,now);return `${k} ${pct(100-w.usedPercent)}% left${freshness(w,now).stale?' ⚠':''}${reset?` (${reset})`:''}`;});
  const quota=['5h','7d'].map(k=>{const w=state.quota[k];if(!w)return `${k} quota: not reported`;
    return `${k} quota: ${pct(100-w.usedPercent)}% left${w.status&&w.status!=='allowed'?' · '+w.status.replace(/_/g,' '):''} · ${w.resetsAt?resetText(w.resetsAt,now):'reset unknown'} · ${freshness(w,now).text}`;});
  const codex=codexWindows(state,now);
  const antigravity=activeAntigravityWindows(state,model,now);
  const tightest=leadAntigravityWindow(antigravity,model);
  const quotaDetails=provider==='antigravity'?(antigravity.length?antigravity.map(w=>w.detail):[state.antigravityQuota?'Quota mapping unavailable for this model; see the dashboard.':'Antigravity quota: not reported yet']):codex.length?codex.map(w=>w.detail):['codex','codex-rust'].includes(provider)?['Subscription quota: not reported']:quota;
  const tokens=n=>n!==undefined?n.toLocaleString():'?';
  const details=[
    c?`Context: ${tokens(c.used)} / ${tokens(c.size)} tokens${c.percent!==undefined?` (${c.percent.toFixed(1)}%)`:''} · ${cf.text}`:'Context: not reported yet',
    c?.dropped?'Context shrank since the last report':'',
    c?.sizeChanged?'Context window size changed':'',
    state.model?`Model: ${state.model.name}`:'',
    state.cost?`Cost: ${state.cost.amount.toFixed(4)} ${state.cost.currency||'(currency unknown)'}`:'',
    ...quotaDetails,
  ].filter(Boolean);
  // How the numbers are gathered sits behind the panel's "?" (full notes in docs/USER-GUIDE.md).
  const help=[provider==='antigravity'
    ?'Quota comes from Antigravity’s /usage command, read when the session starts, after each turn, or on Refresh quota (at most every 30 seconds). No prompt is sent.'
    :'Context, cost and quota come from the agent’s own usage reports. Quota is shared by every session on the same host and account; Agentworks never queries it.',
    '⚠ marks values over 10 minutes old or of unknown age.'];
  const stale=c?cf.stale:antigravity.length?antigravity.every(w=>w.stale):true;
  // Glanceable form for the phone composer chip: context used and 5h quota left.
  // Windows are named only by length so the pill fits narrow screens.
  const primary=codex.find(w=>w.window.role==='primary'&&w.window.limitId==='codex')||codex.find(w=>w.window.role==='primary')||codex[0];
  const short=[c?.percent!==undefined?`ctx ${Math.round(Math.min(100,c.percent))}%`:'',state.quota['5h']?`5h ${pct(100-state.quota['5h'].usedPercent)}% left`:'',primary?primary.compact:'',tightest?.pill||''].filter(Boolean).join(' · ');
  const allQuota=[...compactQuota,...codex.map(w=>w.compact),...(tightest?[tightest.compact]:[])];
  return {label:(c&&stale?'⚠ ':'')+label+(allQuota.length?' · '+allQuota.join(' · '):''),short:short&&(c&&stale?'⚠ ':'')+short,stale,known:!!c||antigravity.length>0,details,help};
}

// Keeps every host/family's shared quota current from the server push, and
// reconnects after a drop; each connection starts with a full snapshot.
// `onChange(connected)` runs after every message and connection change.
export function watchSharedQuota(url,onChange){
  const entries=new Map();let delay=1000,connected=false;
  const connect=()=>{
    const ws=new WebSocket(url);
    ws.onopen=()=>{delay=1000;connected=true;onChange(true);};
    ws.onmessage=message=>{
      let m;try{m=JSON.parse(message.data);}catch{return;}
      if(m.kind==='snapshot')entries.clear();
      for(const entry of m.kind==='snapshot'?m.entries||[]:m.kind==='quota'?[m.entry]:[])
        if(typeof entry?.family==='string')entries.set(quotaKey(entry.hostId,entry.family),{hostId:entry.hostId||'local',family:entry.family,state:sharedQuota(entry)});
      onChange(connected);
    };
    ws.onclose=()=>{connected=false;onChange(false);setTimeout(connect,delay);delay=Math.min(15000,delay*2);};
    ws.onerror=()=>{try{ws.close();}catch{}};
  };
  connect();
  return {get:(hostId,family)=>entries.get(quotaKey(hostId,family))?.state||null,entries:()=>[...entries.values()]};
}

// Wires the statusline into the composer. Hooks: reset() on session switch or
// snapshot replay, consume(event,{live}) per event, render() after updates.
// An optional `chip` mirrors the short label (hidden while nothing is reported).
// `getShared` returns the account quota reported by any session sharing this one's account.
export function setupUsageStatus({root,getProvider,getModel=()=>null,getShared=()=>null,chip}){
  let state=emptyUsage();
  root.innerHTML='<button type="button" class="usage-status-toggle" aria-expanded="false" aria-controls="usage-status-details"></button><div id="usage-status-details" class="usage-status-details" role="region" aria-label="Usage details" hidden></div>';
  const button=root.firstChild,panel=root.lastChild;
  button.onclick=()=>{panel.hidden=!panel.hidden;button.setAttribute('aria-expanded',String(!panel.hidden));};
  const render=()=>{
    const v=usageView(withSharedQuota(state,getShared()),{provider:getProvider(),model:getModel()});
    button.textContent=v.label;button.classList.toggle('stale',v.stale);button.classList.toggle('unknown',!v.known);
    button.title='Context usage details';
    if(chip){chip.textContent=v.short;chip.hidden=!v.short;chip.classList.toggle('stale',v.stale);chip.setAttribute('aria-label',`Usage: ${v.label}. More options`);}
    const help=document.createElement('details'),summary=document.createElement('summary');
    help.className='usage-help';help.open=!!panel.querySelector('.usage-help')?.open;
    summary.textContent='?';summary.title='How usage is reported';summary.setAttribute('aria-label','How usage is reported');
    help.append(summary,...v.help.map(t=>{const p=document.createElement('p');p.textContent=t;return p;}));
    panel.replaceChildren(...v.details.map(t=>{const p=document.createElement('div');p.textContent=t;return p;}),help);
  };
  render();
  return {
    reset(){state=emptyUsage();panel.hidden=true;button.setAttribute('aria-expanded','false');render();},
    consume(e,{live=false}={}){state=applyUsageEvent(state,e,live?Date.now():null);},
    render,
    get state(){return state;},
  };
}
