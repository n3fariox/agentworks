import {setupNotificationTray} from './notification-tray.js';
import {renderHostUsage, startHostUsage} from './host-usage.js';
import {appUrl, socketUrl} from './hosting.js';
import {STALE_MS, countdown, shortTime, watchSharedQuota, antigravityWindows} from './usage-status.js';

const STATES = ['running', 'waiting', 'idle', 'stopped', 'unknown'];
const LABELS = {running:'Running', waiting:'Waiting for approval/input', idle:'Idle', stopped:'Stopped', unknown:'Unknown'};
const el = (tag, text, className) => {
  const node = document.createElement(tag);
  if (text !== undefined) node.textContent = text;
  if (className) node.className = className;
  return node;
};
const note = text => el('p', text);

// Status is authoritative; approval counts supplement it when available.
export function sessionState(session, activity) {
  if (activity?.pendingApprovals > 0 || session.status === 'waiting') return 'waiting';
  if (['closed','suspended','error','stopped'].includes(session.status)) return 'stopped';
  if (['running','starting','resuming','configuring','cancelling'].includes(session.status) || session.turnRunning === true) return 'running';
  return session.status === 'idle' ? 'idle' : 'unknown';
}
export function counts(sessions, activity = new Map()) {
  const result = Object.fromEntries(STATES.map(s => [s, 0]));
  for (const session of sessions) result[sessionState(session, activity.get(session.id))]++;
  return result;
}
export function activelyTurning(session) { return session.turnRunning === true && session.archived !== true; }
export function awakeIdle(session) {
  return session.archived !== true && !activelyTurning(session) && session.status === 'idle' && session.asleep !== true;
}
// Kept short; where each time comes from is explained in docs/USER-GUIDE.md.
function freshness(at, now) {
  if (at == null) return 'Update time unknown';
  return `Updated ${shortTime(at, now)}${now - at > STALE_MS ? ' · stale' : ''}${at > now ? ' · source clock ahead' : ''}`;
}
function quotaReset(at, now) {
  if (typeof at !== 'number' || !Number.isFinite(at) || at < 0 || at > 8.64e15) return 'Reset unknown';
  const remaining = countdown(at - now);
  return remaining ? `Resets in ${remaining}` : 'Reset time passed';
}
const windowLength = minutes => minutes % 1440 === 0 ? `${minutes / 1440}d` : minutes % 60 === 0 ? `${minutes / 60}h` : `${minutes}m`;
export function usageMeters(state, now = Date.now()) {
  const meters = [];
  if (state.context?.percent !== undefined) meters.push({
    kind:'context', label:'Context', percent:state.context.percent,
    detail:`${state.context.used} of ${state.context.size} tokens`
  });
  for (const [label, quota] of Object.entries(state.quota)) meters.push({
    label:`${label} quota used`, percent:quota.usedPercent,
    detail:`${quotaReset(quota.resetsAt, now)} · ${freshness(quota.at, now)}`
  });
  for (const quota of Object.values(state.codexQuota)) {
    if (quota.usedPercent?.value === undefined) continue;
    const minutes = quota.windowDurationMins?.value;
    const name = [quota.limitId === 'codex' ? '' : quota.limitName || quota.limitId, minutes === undefined ? quota.role : windowLength(minutes)].filter(Boolean).join(' ');
    meters.push({label:`${name} quota used`, percent:quota.usedPercent.value,
      detail:`${quotaReset(quota.resetsAt?.value, now)} · ${freshness(quota.usedPercent.at, now)}`});
  }
  for(const quota of antigravityWindows(state,now)){
    if(quota.percent===null)continue;
    meters.push({label:`${quota.label} quota used`,percent:100-quota.percent,detail:`${quotaReset(quota.resetsAt,now)} · ${freshness(quota.observedAtMs,now)}`});
  }
  return meters;
}
async function read(path) {
  const response = await fetch(appUrl(path), {signal:AbortSignal.timeout(15000), cache:'no-store', credentials:'same-origin'});
  if (!response.ok) throw new Error('Read failed');
  return response.json();
}
let hostSample = null, hostMetricStatus = 'Waiting for host metrics';
function renderHostMetrics() {
  for (const root of document.querySelectorAll('[data-host-metrics]')) {
    const sample = hostSample?.hosts?.[root.dataset.hostMetrics];
    renderHostUsage(root, sample, hostSample?.reason || hostMetricStatus);
  }
}
function renderHosts(catalog, sessions) {
  const root = document.querySelector('#hosts'); root.replaceChildren();
  if (!catalog) { root.append(note('Host status unavailable.')); return; }
  const hosts = [...catalog.hosts];
  for (const session of sessions || []) if (!hosts.some(h => h.id === session.hostId)) hosts.push({id:session.hostId, name:session.hostId || 'Unreported host', status:'unknown'});
  if (!hosts.length) root.append(note('No hosts reported.'));
  for (const host of hosts) {
    const card = el('article', undefined, 'card');
    const heading = el('div', undefined, 'card-heading');
    heading.append(el('h3', host.name || host.id || 'Unknown host'), el('span', host.status || 'unknown', `badge ${host.status === 'connected' ? 'connected' : ''}`));
    card.append(heading);
    // Remote hosts say how they are reached and, when down, why.
    if (host.kind === 'remote') {
      const facts = [host.destination && `ssh ${host.destination}`, host.worker?.providers?.length && host.worker.providers.join(', ')].filter(Boolean).join(' · ');
      if (facts) card.append(el('p', facts, 'host-meta'));
      if (host.status !== 'connected' && host.detail) card.append(el('p', host.detail, 'host-detail'));
    }
    if (sessions) {
      const matching = sessions.filter(s => s.hostId === host.id);
      const count = el('p', undefined, 'count'); count.append(String(matching.length), el('small', ' active turns'));
      card.append(count);
    } else card.append(note('Active turn count unavailable.'));
    const metrics = el('div'); metrics.dataset.hostMetrics = host.id;
    card.append(metrics);
    root.append(card);
  }
  renderHostMetrics();
}
function sessionRow(session, projectName, hostName, {showWaiting = false} = {}) {
  const item = el('li', undefined, 'session-row');
  const identity = el('div', undefined, 'session-identity');
  const link = el('a', session.title || 'Untitled session', 'session-link');
  link.href = appUrl('/?' + new URLSearchParams({session:session.id}));
  const meta = [projectName, session.provider || 'Provider unknown', hostName].filter(Boolean).join(' · ');
  identity.append(link, el('small', meta));
  item.append(identity);
  if (showWaiting && session.status === 'waiting') item.append(el('span', 'Waiting for input', 'badge waiting-badge'));
  return item;
}
function renderActiveTurns(catalog, sessions) {
  const root = document.querySelector('#projects');
  const summary = document.querySelector('#active-turns-summary');
  const idleWasOpen = !!root.querySelector('.idle-sessions')?.open;
  root.replaceChildren();
  if (!sessions) {
    if (summary) summary.textContent = '';
    root.append(note('Active turns unavailable.'));
    return;
  }
  const projectName = id => !id ? 'Ungrouped' : catalog?.projects?.find(p => p.id === id)?.name || id;
  const hostName = id => !id ? '' : catalog?.hosts?.find(h => h.id === id)?.name || id;
  // The orchestrator is labelled as itself and listed first.
  const isOrchestrator = s => !!catalog?.orchestratorId && s.id === catalog.orchestratorId;
  const place = s => isOrchestrator(s) ? 'Orchestrator' : projectName(s.projectId);
  const byName = (a, b) => isOrchestrator(b) - isOrchestrator(a) || place(a).localeCompare(place(b)) || (a.title || '').localeCompare(b.title || '');
  const active = sessions.filter(s => activelyTurning(s) || s.status === 'waiting')
    .sort((a, b) => (b.status === 'waiting') - (a.status === 'waiting') || byName(a, b));
  const idle = sessions.filter(awakeIdle).sort(byName);
  const waiting = active.filter(s => s.status === 'waiting').length;
  if (summary) {
    const parts = [];
    if (waiting) parts.push(`${waiting} waiting`);
    if (active.length - waiting > 0 || (!waiting && active.length > 0)) parts.push(`${active.length - waiting} running`);
    if (!active.length) parts.push('None running');
    if (idle.length) parts.push(`${idle.length} idle`);
    summary.textContent = parts.join(' · ');
  }
  if (!active.length && !idle.length) {
    root.append(el('p', 'No active turns.', 'empty'));
    return;
  }
  const card = el('div', undefined, 'card turns-card');
  if (active.length) {
    const list = el('ul', undefined, 'session-list');
    for (const session of active) list.append(sessionRow(session, place(session), hostName(session.hostId), {showWaiting:true}));
    card.append(list);
  } else {
    card.append(el('p', 'No active turns.', 'turns-empty'));
  }
  if (idle.length) {
    const details = el('details', undefined, 'idle-sessions');
    details.open = idleWasOpen;
    details.append(el('summary', `${idle.length} idle ${idle.length === 1 ? 'session' : 'sessions'}`));
    const list = el('ul', undefined, 'session-list');
    for (const session of idle) list.append(sessionRow(session, place(session), hostName(session.hostId)));
    details.append(list);
    card.append(details);
  }
  root.append(card);
}
// Goose reaches models through provider profiles, so its quota is per profile.
export function familyLabel(family, profiles = []) {
  if (family === 'claude') return 'Claude';
  if (family === 'codex') return 'Codex';
  if (family === 'antigravity') return 'Antigravity';
  if (!family.startsWith('goose/')) return family;
  const id = family.slice(6);
  return `Goose · ${id === 'default' ? 'default provider' : profiles.find(p => p.id === id)?.name || id}`;
}
function renderQuotaMeters(root, key, state) {
  const meters = usageMeters(state).filter(value => value.kind !== 'context');
  if (!meters.length) { root.append(note('No usable quota windows reported.')); return; }
  for (const [i, value] of meters.entries()) {
    const box = el('div', undefined, 'usage-meter');
    const label = el('label', `${value.label}: ${Math.round(value.percent)}%`, 'meter-label');
    const meter = el('meter'); meter.id = `usage-${key.replace(/[^\w-]/g, '_')}-${i}`; label.htmlFor = meter.id;
    meter.min = 0; meter.max = 100; meter.value = Math.min(100, value.percent);
    meter.textContent = `${Math.round(value.percent)}%`;
    const detail = el('div', value.detail, 'usage-detail'); detail.id = `${meter.id}-detail`;
    meter.setAttribute('aria-describedby', detail.id);
    box.append(label, meter, detail); root.append(box);
  }
}
// One card per host and provider family: every session there shares one sign-in.
// Claude and Codex always get a card so a missing report is visible.
function renderAccounts({status, catalog, profiles} = {}) {
  const root = document.querySelector('#accounts'); root.replaceChildren();
  const hostName = id => catalog?.hosts?.find(h => h.id === id)?.name || id;
  const accounts = sharedQuota.entries().map(a => ({...a, label:familyLabel(a.family, profiles)}))
    .sort((a, b) => hostName(a.hostId).localeCompare(hostName(b.hostId)) || a.label.localeCompare(b.label));
  for (const family of ['claude', 'codex'])
    if (!accounts.some(a => a.family === family)) accounts.push({hostId:null, family, label:familyLabel(family), state:null});
  for (const account of accounts) {
    const card = el('article', undefined, 'card');
    const heading = el('div', undefined, 'card-heading');
    heading.append(el('h3', account.hostId ? `${account.label} · ${hostName(account.hostId)}` : account.label), el('span', 'Live quota', 'badge'));
    card.append(heading);
    const configs = status?.providers.filter(p => p.id === account.family || p.id === account.family + '-rust') || [];
    const provider = configs.find(p => p.installed) || configs[0];
    if (provider) {
      const adapter = provider.installed === true ? 'available' : provider.installed === false ? 'unavailable' : 'unknown';
      const key = provider.keyPresent === true ? 'configured' : provider.keyPresent === false ? 'not configured' : 'unknown';
      const auth = provider.authMode === 'existing' ? 'host sign-in' : provider.authMode === 'api_key' ? `API key (${key})` : 'unknown';
      card.append(el('p', `Adapter: ${adapter} · Auth: ${auth}`, 'account-config'));
    }
    if (account.state) renderQuotaMeters(card, `${account.hostId}-${account.family}`, account.state);
    else card.append(el('h4', 'No quota reported', 'quota-state'));
    if(account.family==='antigravity'&&account.state?.antigravityQuota?.sessionId){
      const refresh=el('button','Refresh quota');refresh.type='button';
      refresh.onclick=async()=>{
        refresh.disabled=true;
        try{
          const response=await fetch(appUrl(`api/sessions/${encodeURIComponent(account.state.antigravityQuota.sessionId)}/quota/refresh`),{method:'POST',headers:{'Content-Type':'application/json'},body:'{}'});
          if(!response.ok)throw Error(await response.text());
        }catch(error){card.append(note(error.message));}finally{refresh.disabled=false;}
      };
      card.append(refresh);
    }
    root.append(card);
  }
}
let context = {};
const sharedQuota = typeof document === 'undefined' ? null : watchSharedQuota(socketUrl('api/account-quota/events'), connected => {
  document.querySelector('#quota-status').textContent = connected ? 'Live' : 'Disconnected · reconnecting…';
  renderAccounts(context);
});
let refreshing = false;
async function refresh() {
  if (refreshing) return;
  refreshing = true;
  const button = document.querySelector('#refresh');
  button.disabled = true;
  const statusLine = document.querySelector('#refresh-status');
  statusLine.textContent = 'Refreshing…';
  try {
    const paths = ['api/catalog','api/sessions','api/status','api/model-providers'];
    const reads = await Promise.allSettled(paths.map(read));
    const [catalog, allSessions, status, profiles] = reads.map(r => r.status === 'fulfilled' ? r.value : null);
    const sessions = allSessions?.filter(s => s.archived !== true) ?? null;
    const errors = document.querySelector('#errors'); errors.replaceChildren();
    const names = ['Host/project catalog','Session list','Provider configuration'];
    reads.slice(0, 3).forEach((result, i) => { if (result.status === 'rejected') errors.append(el('p', `${names[i]} unavailable. Refresh to retry.`, 'error')); });
    const activeSessions = sessions?.filter(s => activelyTurning(s) || s.status === 'waiting') ?? null;
    renderHosts(catalog, activeSessions); renderActiveTurns(catalog, sessions);
    context = {status, catalog, profiles:profiles?.profiles || []};
    renderAccounts(context);
    statusLine.textContent = `Updated ${shortTime(Date.now())}${reads.slice(0, 3).some(r => r.status === 'rejected') ? ' · some data unavailable' : ''}`;
  } catch {
    statusLine.textContent = 'Refresh failed. Try again.';
  } finally {
    button.disabled = false;
    refreshing = false;
  }
}
if (typeof document !== 'undefined') {
  startHostUsage({onState(sample, status) { hostSample = sample; hostMetricStatus = status; renderHostMetrics(); }});
  document.querySelector('#refresh').addEventListener('click', () => refresh());
  const tips = () => [...document.querySelectorAll('details.help-tip[open]')];
  document.addEventListener('click', e => { for (const t of tips()) if (!t.contains(e.target)) t.open = false; });
  document.addEventListener('keydown', e => { if (e.key === 'Escape') for (const t of tips()) t.open = false; });
  refresh();
}

if(typeof document!=='undefined')setupNotificationTray();
