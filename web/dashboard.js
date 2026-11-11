import {renderHostUsage, startHostUsage} from './host-usage.js';
import {appUrl, socketUrl} from './hosting.js';
import {STALE_MS, countdown, watchSharedQuota, antigravityWindows} from './usage-status.js';

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
function freshness(value, now, observer = 'Adapter observed') {
  if (value?.at == null) return 'Freshness unknown · persisted observation has no source timestamp';
  return `${observer} ${new Date(value.at).toLocaleString()}${now - value.at > STALE_MS ? ' · stale (over 10 minutes old)' : ''}${value.at > now ? ' · source clock is ahead' : ''}`;
}
function quotaReset(at, now) {
  if (typeof at !== 'number' || !Number.isFinite(at) || at < 0 || at > 8.64e15) return 'Reset unavailable';
  const date = new Date(at).toLocaleString(undefined, {timeZoneName:'short'});
  const remaining = countdown(at - now);
  return remaining ? `Resets in ${remaining} · ${date}` : `Reported reset time passed · ${date} · awaiting a new provider report`;
}
export function usageMeters(state, now = Date.now()) {
  const meters = [];
  if (state.context?.percent !== undefined) meters.push({
    label:'Session context occupancy', percent:state.context.percent,
    detail:`${state.context.used} of ${state.context.size} reported tokens · freshness unknown (persisted event)`
  });
  for (const [label, quota] of Object.entries(state.quota)) meters.push({
    label:`Reported ${label} quota used`, percent:quota.usedPercent,
    detail:`${quotaReset(quota.resetsAt, now)} · ${freshness(quota, now, 'Agentworks received')}`
  });
  for (const quota of Object.values(state.codexQuota)) {
    if (quota.usedPercent?.value === undefined) continue;
    const minutes = quota.windowDurationMins?.value;
    const duration = minutes === undefined ? 'window duration unknown' : `${minutes} minute window`;
    meters.push({label:`${quota.limitName || quota.limitId} · ${quota.role} · ${duration} · quota used`,
      percent:quota.usedPercent.value, detail:`${quotaReset(quota.resetsAt?.value, now)}${quota.resetsAt?.value !== undefined ? ` · Reset freshness: ${freshness(quota.resetsAt, now)}` : ''} · Usage freshness: ${freshness(quota.usedPercent, now)}`});
  }
  for(const quota of antigravityWindows(state,now)){
    if(quota.percent===null)continue;
    meters.push({label:`${quota.label} · quota used`,percent:100-quota.percent,detail:quota.detail});
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
function renderProjects(catalog, sessions) {
  const root = document.querySelector('#projects'); root.replaceChildren();
  if (!sessions) { root.append(note('Project active turn counts unavailable.')); return; }
  const projects = catalog ? [...catalog.projects] : [];
  for (const session of sessions) if (session.projectId && !projects.some(p => p.id === session.projectId)) projects.push({id:session.projectId, name:`Unknown project (${session.projectId})`});
  projects.push({id:null, name:'Ungrouped'});
  if (!catalog) root.append(note('Project catalog unavailable. Session project IDs are shown where reported.'));
  for (const project of projects) {
    const matching = sessions.filter(s => (s.projectId || null) === project.id);
    const details = el('details', undefined, 'project');
    const summary = el('summary');
    summary.append(el('strong', `${project.name}${project.archived ? ' · archived' : ''} · ${matching.length} active ${matching.length === 1 ? 'turn' : 'turns'}`));
    details.append(summary);
    if (!matching.length) details.append(el('p', 'No sessions in this project.', 'session-list empty'));
    const list = el('ul', undefined, 'session-list');
    for (const session of matching) {
      const item = el('li', undefined, 'session-row');
      const identity = el('div', session.title || 'Untitled session');
      identity.append(el('small', `${session.provider || 'Provider unknown'} · ${session.id}${session.archived ? ' · archived' : ''}`));
      const status = el('div', 'Taking a turn', 'session-state running');
      item.append(identity, status); list.append(item);
    }
    details.append(list); root.append(details);
  }
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
  const meters = usageMeters(state).filter(value => value.label !== 'Session context occupancy');
  if (!meters.length) { root.append(note('This account reported no usable quota windows.')); return; }
  for (const [i, value] of meters.entries()) {
    const box = el('div', undefined, 'usage-meter');
    const label = el('label', `${value.label}: ${value.percent.toFixed(1)}%`, 'meter-label');
    const meter = el('meter'); meter.id = `usage-${key.replace(/[^\w-]/g, '_')}-${i}`; label.htmlFor = meter.id;
    meter.min = 0; meter.max = 100; meter.value = Math.min(100, value.percent);
    meter.textContent = `${value.percent.toFixed(1)}%`;
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
      const mode = provider.authMode === 'existing' ? 'existing host sign-in (unverified)' : provider.authMode === 'api_key' ? 'API key' : 'unknown';
      const key = provider.authMode === 'api_key' ? ` · API key presence: ${provider.keyPresent === true ? 'configured (unverified)' : provider.keyPresent === false ? 'not configured' : 'unknown'}` : '';
      card.append(el('p', `Adapter: ${adapter} · Authentication mode: ${mode}${key}`, 'account-config'));
    }
    if (account.state) {
      card.append(el('h4', 'Reported quota windows', 'quota-state'), note('Latest report from any session on this host and account. Percentages describe reported windows only; account spending cap and USD used / total are unavailable.'));
      renderQuotaMeters(card, `${account.hostId}-${account.family}`, account.state);
    } else card.append(el('h4', 'No quota reported', 'quota-state'), note('No retained or live session has reported subscription quota for this provider. This does not establish a no-usage-cap plan. Account spend / limit in USD is unavailable.'));
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
  document.querySelector('#quota-status').textContent = connected
    ? 'Live: updates as soon as any session reports quota.'
    : 'Live quota feed disconnected. Reconnecting…';
  renderAccounts(context);
});
let refreshing = false;
async function refresh() {
  if (refreshing) return;
  refreshing = true;
  const button = document.querySelector('#refresh');
  button.disabled = true;
  const statusLine = document.querySelector('#refresh-status');
  statusLine.textContent = 'Reading workspace snapshots…';
  try {
    const paths = ['api/catalog','api/sessions','api/status','api/model-providers'];
    const reads = await Promise.allSettled(paths.map(read));
    const [catalog, allSessions, status, profiles] = reads.map(r => r.status === 'fulfilled' ? r.value : null);
    const sessions = allSessions?.filter(s => s.archived !== true) ?? null;
    const errors = document.querySelector('#errors'); errors.replaceChildren();
    const names = ['Host/project catalog','Session list','Provider configuration'];
    reads.slice(0, 3).forEach((result, i) => { if (result.status === 'rejected') errors.append(el('p', `${names[i]} unavailable. Refresh to retry.`, 'error')); });
    const activeSessions = sessions?.filter(activelyTurning) ?? null;
    renderHosts(catalog, activeSessions); renderProjects(catalog, activeSessions);
    context = {status, catalog, profiles:profiles?.profiles || []};
    renderAccounts(context);
    statusLine.textContent = `Snapshot read ${new Date().toLocaleString()}.${reads.slice(0, 3).some(r => r.status === 'rejected') ? ' Some data is unavailable.' : ''}`;
  } catch {
    statusLine.textContent = 'Snapshot read failed. Refresh to retry.';
  } finally {
    button.disabled = false;
    refreshing = false;
  }
}
if (typeof document !== 'undefined') {
  startHostUsage({onState(sample, status) { hostSample = sample; hostMetricStatus = status; renderHostMetrics(); }});
  document.querySelector('#refresh').addEventListener('click', () => refresh());
  refresh();
}
