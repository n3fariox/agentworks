import {appUrl} from './hosting.js';
import {emptyUsage, applyUsageEvent, STALE_MS, countdown} from './usage-status.js';

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
function freshness(value, now) {
  if (value?.at == null) return 'Freshness unknown · persisted observation has no source timestamp';
  return `Adapter observed ${new Date(value.at).toLocaleString()}${now - value.at > STALE_MS ? ' · stale (over 10 minutes old)' : ''}${value.at > now ? ' · source clock is ahead' : ''}`;
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
    detail:`${quotaReset(quota.resetsAt, now)} · Freshness unknown · persisted Claude event has no source timestamp`
  });
  for (const quota of Object.values(state.codexQuota)) {
    if (quota.usedPercent?.value === undefined) continue;
    const minutes = quota.windowDurationMins?.value;
    const duration = minutes === undefined ? 'window duration unknown' : `${minutes} minute window`;
    meters.push({label:`${quota.limitName || quota.limitId} · ${quota.role} · ${duration} · quota used`,
      percent:quota.usedPercent.value, detail:`${quotaReset(quota.resetsAt?.value, now)}${quota.resetsAt?.value !== undefined ? ` · Reset freshness: ${freshness(quota.resetsAt, now)}` : ''} · Usage freshness: ${freshness(quota.usedPercent, now)}`});
  }
  return meters;
}
async function read(path) {
  const response = await fetch(appUrl(path), {signal:AbortSignal.timeout(15000), cache:'no-store', credentials:'same-origin'});
  if (!response.ok) throw new Error('Read failed');
  return response.json();
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
    root.append(card);
  }
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
export function latestQuota(sessions, observations, provider) {
  const chosen = new Map();
  const rank = (value, session) => [value?.usedPercent?.at ?? value?.at ?? -1, session.created ?? 0];
  const consider = (key, value, session) => {
    const previous = chosen.get(key);
    const [at, created] = rank(value, session);
    const [oldAt, oldCreated] = previous ? rank(previous.value, previous.session) : [-2, -1];
    if (!previous || at > oldAt || (at === oldAt && created > oldCreated)) chosen.set(key, {value, session});
  };
  for (const session of sessions.filter(s => (s.provider === 'codex-rust' ? 'codex' : s.provider) === provider)) {
    const state = observations.get(session.id)?.state;
    if (!state) continue;
    for (const [label, value] of Object.entries(state.quota)) consider(`claude:${label}`, value, session);
    for (const [key, value] of Object.entries(state.codexQuota)) consider(`codex:${key}`, value, session);
  }
  const state = emptyUsage();
  for (const [key, {value}] of chosen) {
    if (key.startsWith('claude:')) state.quota[key.slice(7)] = value;
    else state.codexQuota[key.slice(6)] = value;
  }
  return {state, sources:chosen.size};
}
// No retained adapter contract currently proves an account-wide unlimited plan
// or reports account spend/limit in USD. Missing windows, enterprise plan names,
// session cost and unlimited *credits* are deliberately not such proof.
export function quotaAvailability(state, {readable = 0, failed = 0, truncated = 0} = {}) {
  const reported = usageMeters(state).some(value => value.label !== 'Session context occupancy');
  if (reported) return {label:'Reported quota windows', detail:'Percentages describe reported windows only; account spending cap and USD used / total are unavailable.'};
  if (!readable || failed || truncated) return {label:'Quota unavailable', detail:'Subscription quota has not been reported in the readable history. Missing or incomplete observations cannot establish a cap or an unmetered plan.'};
  return {label:'No cap reported', detail:'Subscription quota has not been reported by these retained sessions. This does not establish a no-usage-cap plan. Account spend / limit in USD is unavailable.'};
}
function renderQuotaMeters(root, provider, state) {
  const meters = usageMeters(state).filter(value => value.label !== 'Session context occupancy');
  if (!meters.length) return;
  for (const [i, value] of meters.entries()) {
    const box = el('div', undefined, 'usage-meter');
    const label = el('label', `${value.label}: ${value.percent.toFixed(1)}%`, 'meter-label');
    const meter = el('meter'); meter.id = `usage-${provider}-${i}`; label.htmlFor = meter.id;
    meter.min = 0; meter.max = 100; meter.value = Math.min(100, value.percent);
    meter.textContent = `${value.percent.toFixed(1)}%`;
    const detail = el('div', value.detail, 'usage-detail'); detail.id = `${meter.id}-detail`;
    meter.setAttribute('aria-describedby', detail.id);
    box.append(label, meter, detail); root.append(box);
  }
}
function renderAccounts(status, sessions, observations) {
  const root = document.querySelector('#accounts'); root.replaceChildren();
  for (const id of ['claude','codex']) {
    const configs = status?.providers.filter(p => p.id === id || (id === 'codex' && p.id === 'codex-rust')) || [];
    const provider = configs.find(p => p.installed) || configs[0];
    const card = el('article', undefined, 'card');
    const heading = el('div', undefined, 'card-heading'); heading.append(el('h3', id === 'claude' ? 'Claude' : 'Codex'), el('span', 'Quota report', 'badge'));
    card.append(heading);
    const config = el('p', undefined, 'account-config');
    if (!provider) config.textContent = 'Provider configuration unavailable.';
    else {
      const adapter = provider.installed === true ? 'available' : provider.installed === false ? 'unavailable' : 'unknown';
      const mode = provider.authMode === 'existing' ? 'existing host sign-in (unverified)' : provider.authMode === 'api_key' ? 'API key' : 'unknown';
      const key = provider.authMode === 'api_key' ? ` · API key presence: ${provider.keyPresent === true ? 'configured (unverified)' : provider.keyPresent === false ? 'not configured' : 'unknown'}` : '';
      config.textContent = `Adapter: ${adapter} · Authentication mode: ${mode}${key}`;
    }
    card.append(config);
    if (!sessions) card.append(el('h4', 'Quota unavailable', 'quota-state'), note('Session list could not be read; account cap and spend are unknown.'));
    else {
      const matching = sessions.filter(s => s.provider === id || (id === 'codex' && s.provider === 'codex-rust'));
      const latest = latestQuota(sessions, observations, id);
      card.append(note(`${matching.length} retained source session(s); latest report selected per quota window. Reports are not added together.`));
      const readable = matching.filter(s => observations.has(s.id)).length;
      const failed = matching.length - readable;
      const truncated = matching.filter(s => observations.get(s.id)?.truncated).length;
      const availability = quotaAvailability(latest.state, {readable, failed, truncated});
      card.append(el('h4', availability.label, 'quota-state'), note(availability.detail));
      card.append(note(`Read coverage: ${readable}/${matching.length} session histories · ${failed} unavailable · ${truncated} truncated. ${failed || truncated ? 'Latest report may be missing.' : ''}`));
      renderQuotaMeters(card, id, latest.state);
    }
    root.append(card);
  }
}
let refreshing = false;
async function refresh({quotaOnly = false} = {}) {
  if (refreshing) return;
  refreshing = true;
  const buttons = ['#refresh', '#refresh-quota'].map(id => document.querySelector(id));
  buttons.forEach(button => {button.disabled = true;});
  const statusLine = document.querySelector(quotaOnly ? '#quota-refresh-status' : '#refresh-status');
  statusLine.textContent = quotaOnly ? 'Rereading retained quota observations…' : 'Reading workspace snapshots…';
  try {
    const paths = quotaOnly ? ['api/sessions','api/status'] : ['api/catalog','api/sessions','api/status'];
    const reads = await Promise.allSettled(paths.map(read));
    const values = new Map(paths.map((path, i) => [path, reads[i].status === 'fulfilled' ? reads[i].value : null]));
    const catalog = values.get('api/catalog'), allSessions = values.get('api/sessions'), status = values.get('api/status');
    const sessions = allSessions?.filter(s => s.archived !== true) ?? null;
    if (!quotaOnly) {
      const errors = document.querySelector('#errors'); errors.replaceChildren();
      const names = ['Host/project catalog','Session list','Provider configuration'];
      reads.forEach((result, i) => { if (result.status === 'rejected') errors.append(el('p', `${names[i]} unavailable. Refresh to retry.`, 'error')); });
      const activeSessions = sessions?.filter(activelyTurning) ?? null;
      renderHosts(catalog, activeSessions); renderProjects(catalog, activeSessions);
    }
    const observations = new Map();
    // Read bounded session histories with four workers. Do not retain transcripts,
    // probe account services, open event streams, or launch harness processes.
    const queue = (sessions || []).filter(s => ['claude','codex','codex-rust'].includes(s.provider));
    let next = 0;
    await Promise.all(Array.from({length:Math.min(4,queue.length)}, async () => {
      while (next < queue.length) {
        const session = queue[next++];
        try {
          const snapshot = await read(`api/sessions/${encodeURIComponent(session.id)}`);
          let state = emptyUsage();
          for (const event of snapshot.events || []) state = applyUsageEvent(state, event);
          observations.set(session.id, {state, truncated:snapshot.truncated === true});
        } catch { /* unavailable per session, with no raw error or transcript output */ }
      }
    }));
    renderAccounts(status, sessions, observations);
    document.querySelector('#quota-refresh-status').textContent = `Retained quota observations reread ${new Date().toLocaleString()}. This is a local history read, not a live account refresh; quota source freshness is labeled separately and is not advanced by this read.${reads.some(r => r.status === 'rejected') ? ' Some reads failed; data is unavailable.' : ''}`;
    if (!quotaOnly) statusLine.textContent = `Snapshot read ${new Date().toLocaleString()}.${reads.some(r => r.status === 'rejected') ? ' Some data is unavailable.' : ''}`;
  } catch {
    statusLine.textContent = 'Snapshot read failed. Refresh to retry.';
  } finally {
    buttons.forEach(button => {button.disabled = false;});
    refreshing = false;
  }
}
if (typeof document !== 'undefined') {
  document.querySelector('#refresh').addEventListener('click', () => refresh());
  document.querySelector('#refresh-quota').addEventListener('click', () => refresh({quotaOnly:true}));
  refresh();
}
