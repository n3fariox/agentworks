import {appUrl} from './hosting.js';
import {emptyUsage, applyUsageEvent, usageView, STALE_MS} from './usage-status.js';

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
function stateCounts(sessions, activity, inline = false) {
  const list = el(inline ? 'span' : 'ul', undefined, 'state-counts');
  for (const [key, value] of Object.entries(counts(sessions, activity))) {
    const item = el(inline ? 'span' : 'li'); item.append(el('strong', String(value)), ` ${LABELS[key]}`); list.append(item);
  }
  return list;
}
function sessionSource(session) {
  return `${session.title || 'Untitled session'} · ${session.id}${session.archived ? ' · archived' : ''}`;
}
function freshness(value, now) {
  if (value?.at == null) return 'Freshness unknown · persisted observation has no source timestamp';
  return `Adapter observed ${new Date(value.at).toLocaleString()}${now - value.at > STALE_MS ? ' · stale (over 10 minutes old)' : ''}${value.at > now ? ' · source clock is ahead' : ''}`;
}
export function usageMeters(state, now = Date.now()) {
  const meters = [];
  if (state.context?.percent !== undefined) meters.push({
    label:'Session context occupancy', percent:state.context.percent,
    detail:`${state.context.used} of ${state.context.size} reported tokens · freshness unknown (persisted event)`
  });
  for (const [label, quota] of Object.entries(state.quota)) meters.push({
    label:`Reported ${label} quota used`, percent:quota.usedPercent,
    detail:'Freshness unknown · persisted Claude event has no source timestamp'
  });
  for (const quota of Object.values(state.codexQuota)) {
    if (quota.usedPercent?.value === undefined) continue;
    const minutes = quota.windowDurationMins?.value;
    const duration = minutes === undefined ? 'window duration unknown' : `${minutes} minute window`;
    meters.push({label:`${quota.limitName || quota.limitId} · ${quota.role} · ${duration} · quota used`,
      percent:quota.usedPercent.value, detail:freshness(quota.usedPercent, now)});
  }
  return meters;
}
async function read(path) {
  const response = await fetch(appUrl(path), {signal:AbortSignal.timeout(15000), cache:'no-store', credentials:'same-origin'});
  if (!response.ok) throw new Error('Read failed');
  return response.json();
}
function renderHosts(catalog, sessions, activity) {
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
    if (sessions) {
      const matching = sessions.filter(s => s.hostId === host.id);
      const count = el('p', undefined, 'count'); count.append(String(matching.length), el('small', ' sessions'));
      card.append(count, stateCounts(matching, activity));
    } else card.append(note('Session count unavailable.'));
    root.append(card);
  }
}
function renderProjects(catalog, sessions, activity) {
  const root = document.querySelector('#projects'); root.replaceChildren();
  if (!sessions) { root.append(note('Project session counts and states unavailable.')); return; }
  const projects = catalog ? [...catalog.projects] : [];
  for (const session of sessions) if (session.projectId && !projects.some(p => p.id === session.projectId)) projects.push({id:session.projectId, name:`Unknown project (${session.projectId})`});
  projects.push({id:null, name:'Ungrouped'});
  if (!catalog) root.append(note('Project catalog unavailable. Session project IDs are shown where reported.'));
  for (const project of projects) {
    const matching = sessions.filter(s => (s.projectId || null) === project.id);
    const details = el('details', undefined, 'project');
    const summary = el('summary');
    summary.append(el('strong', `${project.name}${project.archived ? ' · archived' : ''} · ${matching.length} sessions`), stateCounts(matching, activity, true));
    details.append(summary);
    if (!matching.length) details.append(el('p', 'No sessions in this project.', 'session-list empty'));
    const list = el('ul', undefined, 'session-list');
    for (const session of matching) {
      const item = el('li', undefined, 'session-row');
      const identity = el('div', session.title || 'Untitled session');
      identity.append(el('small', `${session.provider || 'Provider unknown'} · ${session.id}${session.archived ? ' · archived' : ''}`));
      const state = sessionState(session, activity.get(session.id));
      const status = el('div', LABELS[state], `session-state ${state}`);
      status.append(el('small', `Reported: ${session.status || 'unknown'}${activity.get(session.id)?.pendingApprovals > 0 ? ` · ${activity.get(session.id).pendingApprovals} pending approval(s)` : ''}`));
      item.append(identity, status); list.append(item);
    }
    details.append(list); root.append(details);
  }
}
function renderUsage(session, observation) {
  const root = el('article', undefined, 'usage-session');
  root.append(el('h4', session.title || 'Untitled session'), note(`Source session: ${sessionSource(session)}`));
  if (!observation) { root.append(note('Usage unavailable: session snapshot could not be read.')); return root; }
  const {state, truncated} = observation;
  root.append(note('Source: persisted ACP usage_update events. Session observations are not account totals.'));
  if (truncated) root.append(note('History is truncated; older usage observations may be unavailable.'));
  const meters = usageMeters(state);
  for (const [i, value] of meters.entries()) {
    const box = el('div', undefined, 'usage-meter');
    const label = el('label', `${value.label}: ${value.percent.toFixed(1)}%`, 'meter-label');
    const meter = el('meter'); meter.id = `usage-${session.id}-${i}`; label.htmlFor = meter.id;
    meter.min = 0; meter.max = 100; meter.value = Math.min(100, value.percent);
    meter.textContent = `${value.percent.toFixed(1)}%`;
    const detail = el('div', value.detail, 'usage-detail'); detail.id = `${meter.id}-detail`;
    meter.setAttribute('aria-describedby', detail.id);
    box.append(label, meter, detail); root.append(box);
  }
  if (!state.events) root.append(note('Usage unknown: no supported usage observations in retained session history.'));
  else if (!Object.keys(state.quota).length && !Object.keys(state.codexQuota).length) root.append(note('Subscription quota unavailable: not reported by this session.'));
  const details = el('details', undefined, 'usage-details'); details.append(el('summary', 'Reported usage details & freshness'));
  const list = el('ul');
  for (const text of usageView(state, {provider:session.provider}).details) list.append(el('li', text));
  details.append(list); root.append(details);
  return root;
}
function renderAccounts(status, sessions, observations) {
  const root = document.querySelector('#accounts'); root.replaceChildren();
  for (const id of ['claude','codex']) {
    const provider = status?.providers.find(p => p.id === id);
    const card = el('article', undefined, 'card');
    const heading = el('div', undefined, 'card-heading'); heading.append(el('h3', id === 'claude' ? 'Claude' : 'Codex'), el('span', 'Connection unknown', 'badge'));
    card.append(heading, note('Account identity unavailable · authentication not verified.'));
    const config = el('p', undefined, 'account-config');
    if (!provider) config.textContent = 'Provider configuration unavailable.';
    else {
      const adapter = provider.installed === true ? 'available' : provider.installed === false ? 'unavailable' : 'unknown';
      const mode = provider.authMode === 'existing' ? 'existing host sign-in (unverified)' : provider.authMode === 'api_key' ? 'API key' : 'unknown';
      const key = provider.authMode === 'api_key' ? ` · API key presence: ${provider.keyPresent === true ? 'configured (unverified)' : provider.keyPresent === false ? 'not configured' : 'unknown'}` : '';
      config.textContent = `Adapter: ${adapter} · Authentication mode: ${mode}${key}`;
    }
    card.append(config);
    if (!sessions) card.append(note('Session usage unavailable.'));
    else {
      const matching = sessions.filter(s => s.provider === id);
      card.append(note(`${matching.length} source session(s). Account-level usage total unavailable.`));
      if (!matching.length) card.append(note('No source sessions. Usage and subscription quota unknown.'));
      for (const session of matching) card.append(renderUsage(session, observations.get(session.id)));
    }
    root.append(card);
  }
}
async function refresh() {
  const button = document.querySelector('#refresh'); button.disabled = true;
  const statusLine = document.querySelector('#refresh-status'); statusLine.textContent = 'Reading workspace snapshots…';
  const errors = document.querySelector('#errors'); errors.replaceChildren();
  const reads = await Promise.allSettled(['api/catalog','api/sessions?archived=all','api/status','api/activity'].map(read));
  const names = ['Host/project catalog','Session list','Provider configuration','Approval activity'];
  reads.forEach((result, i) => { if (result.status === 'rejected') errors.append(el('p', `${names[i]} unavailable. Refresh to retry.`, 'error')); });
  const [catalog, sessions, status, activityData] = reads.map(r => r.status === 'fulfilled' ? r.value : null);
  const activity = new Map((activityData?.sessions || []).map(s => [s.id, s]));
  renderHosts(catalog, sessions, activity); renderProjects(catalog, sessions, activity);
  const observations = new Map();
  // Read bounded session histories with four workers. Do not retain transcripts,
  // probe account services, open event streams, or launch harness processes.
  const queue = (sessions || []).filter(s => ['claude','codex'].includes(s.provider));
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
  statusLine.textContent = `Snapshot read ${new Date().toLocaleString()}. Refresh to update; usage freshness is labeled separately.${reads.some(r => r.status === 'rejected') ? ' Some data is unavailable.' : ''}`;
  button.disabled = false;
}
if (typeof document !== 'undefined') {
  document.querySelector('#refresh').addEventListener('click', refresh);
  refresh();
}
