import {socketUrl} from './hosting.js';

const bytes = value => `${(value / 1073741824).toFixed(2)} GiB`;
function metric(label, value, {cpu = false, disk = false} = {}) {
  const total = value?.total, used = value?.used;
  const valid = cpu ? Number.isFinite(value?.percent) && value.percent >= 0 && value.percent <= 100
    : Number.isFinite(total) && Number.isFinite(used) && total >= 0 && used >= 0 && used <= total;
  const percent = valid && (cpu || total > 0) ? (cpu ? value.percent : used / total * 100) : null;
  return {label, percent, capacity:valid && !cpu ? `${used}/${total}` : null, value:percent == null ? '—' : `${Math.round(percent)}%`,
    detail:!valid ? value?.reason || 'Unavailable' : !cpu && total === 0 ? 'Not configured' : cpu ? 'CPU utilization' : `${bytes(used)} / ${bytes(total)}${disk ? ' used or reserved' : ''}`,
    tone:percent == null ? 'unknown' : percent >= (disk ? 90 : 95) ? 'critical' : percent >= 80 ? 'high' : 'normal'};
}
export function hostUsageModel(sample, now = Date.now()) {
  if (!sample || sample.reason) return {reason:sample?.reason || 'Waiting for host metrics'};
  const disks = (sample.disks || []).map(disk => ({...metric(disk.path, disk, {disk:true}), path:disk.path}));
  const visible = [];
  const add = disk => { if (disk && !visible.includes(disk) && visible.length < 3) visible.push(disk); };
  add(disks.find(disk => disk.path === '/' || /^c:\\$/i.test(disk.path)));
  disks.filter(disk => /(?:build|workspace|projects|data)(?:[\\/]|$)/i.test(disk.path)).forEach(add);
  // Pick a few distinct capacity summaries for the overview. Every mount,
  // including equal-capacity mounts, remains available in the expanded list.
  for (const disk of disks) if (disk.percent != null && !visible.some(other => other.capacity === disk.capacity)) add(disk);
  return {freshness:now - sample.sampledAt > 15000 ? 'stale' : now < sample.sampledAt ? 'ahead' : 'live',
    sampledAt:sample.sampledAt, tiles:[metric('CPU',sample.cpu,{cpu:true}),metric('Memory',sample.memory),metric('Swap',sample.swap)],
    disks:visible, moreDisks:disks.filter(disk => !visible.includes(disk)), diskReason:sample.diskReason, truncated:sample.disksTruncated};
}
const rendered = new WeakMap();
export function renderHostUsage(root, sample, status, now = Date.now()) {
  const model = hostUsageModel(sample,now);
  const doc = root.ownerDocument;
  const node = (tag,text,className) => {
    const result = doc.createElement(tag); if (text != null) result.textContent=text;
    if (className) result.className=className; return result;
  };
  const freshness = () => {
    const indicator=root.querySelector('.metrics-freshness'); if (!indicator) return;
    indicator.dataset.state=model.freshness; root.dataset.freshness=model.freshness;
    indicator.textContent=model.freshness === 'live' ? 'Live · 5s' : model.freshness === 'stale' ? 'Stale' : 'Clock ahead';
    indicator.title=`Sampled ${new Date(model.sampledAt).toLocaleString()}`;
  };
  const previous=rendered.get(root);
  if (previous?.sample === sample && previous?.status === status) { freshness(); return; }
  rendered.set(root,{sample,status});
  const expanded=root.querySelector('.mount-details')?.open;
  const focused=root.querySelector('.mount-details > summary') === doc.activeElement;
  root.replaceChildren(); root.className='host-usage';
  if (model.reason) { root.append(node('p',sample ? model.reason : status || model.reason,'metrics-unavailable')); return; }
  const header=node('div',null,'metrics-heading');
  header.append(node('span','HOST USAGE','metrics-eyebrow'),node('span',null,'metrics-freshness')); root.append(header);
  function bar(value, compact=false) {
    const box=node('div',null,compact ? 'disk-metric' : 'metric-tile'); box.dataset.tone=value.tone;
    const heading=node('div',null,'metric-heading');
    heading.append(node('span',value.label,'metric-name'),node('strong',value.value,'metric-value')); box.append(heading);
    const track=node('div',null,'metric-track');
    if (value.percent != null) {
      track.setAttribute('role','meter'); track.setAttribute('aria-label',`${value.label} usage`);
      track.setAttribute('aria-valuemin','0'); track.setAttribute('aria-valuemax','100'); track.setAttribute('aria-valuenow',value.percent.toFixed(1));
      track.setAttribute('aria-valuetext',`${value.value} · ${value.detail}`);
      const fill=node('span'); fill.style.width=`${value.percent}%`; track.append(fill);
    }
    box.append(track,node('span',value.detail,'metric-caption')); return box;
  }
  const grid=node('div',null,'host-metric-grid'); model.tiles.forEach(value=>grid.append(bar(value))); root.append(grid);
  const storage=node('div',null,'host-storage'); storage.append(node('div','STORAGE','metrics-eyebrow'));
  model.disks.forEach(value=>storage.append(bar(value,true)));
  if (!model.disks.length && !model.moreDisks.length) storage.append(node('p',model.diskReason || 'No storage volumes reported','metrics-unavailable'));
  if (model.moreDisks.length) {
    const details=node('details',null,'mount-details'); details.open=!!expanded;
    const unavailable=model.moreDisks.filter(disk=>disk.percent == null).length;
    details.append(node('summary',`${model.moreDisks.length} more mounts${unavailable ? ` · ${unavailable} unavailable` : ''}`));
    model.moreDisks.forEach(value=>details.append(bar(value,true))); storage.append(details);

  }
  if (model.truncated) storage.append(node('p','Mount list limited to 64 entries or event size.','metric-caption'));
  root.append(storage); freshness();
  if (focused) root.querySelector('.mount-details > summary')?.focus({preventScroll:true});
}

// Browser timers only renew a lease and update freshness; they never request samples.
// Inject dependencies so navigation, reconnect and lease behavior can be tested.
export function startHostUsage({onState, socket = url => new WebSocket(url), win = window, doc = document, timers = globalThis}) {
  let ws = null, retry = null, heartbeat = null, freshness = null, sample = null, status = 'Connecting to host metrics…', stopped = false;
  const render = () => onState(sample, status);
  function disconnect() {
    timers.clearTimeout(retry); timers.clearInterval(heartbeat); timers.clearInterval(freshness);
    retry = heartbeat = freshness = null;
    const previous = ws; ws = null;
    previous?.close();
  }
  function connect() {
    if (stopped || doc.hidden || ws) return;
    status = 'Connecting to host metrics…'; sample = null; render();
    const current = ws = socket(socketUrl('api/dashboard/host-usage/events'));
    current.onopen = () => {
      if (ws !== current) return;
      current.send('alive');
      heartbeat = timers.setInterval(() => { if (current.readyState === 1) current.send('alive'); }, 10000);
      freshness = timers.setInterval(render, 1000);
    };
    current.onmessage = event => {
      if (ws !== current) return;
      try { sample = JSON.parse(event.data); status = 'Connected'; render(); } catch { /* retain timestamp; never mark old data fresh */ }
    };
    current.onerror = () => { if (ws === current) current.close(); };
    current.onclose = () => {
      if (ws !== current) return;
      disconnect(); sample = null; status = 'Host metrics unavailable: stream disconnected. Retrying…'; render();
      if (!stopped && !doc.hidden) retry = timers.setTimeout(connect, 5000);
    };
  }
  function visibility() {
    if (doc.hidden) { disconnect(); sample = null; status = 'Host metrics paused while dashboard is hidden'; render(); }
    else connect();
  }
  function hide() { stopped = true; disconnect(); sample = null; status = 'Host metrics stopped'; render(); }
  function show() { stopped = false; connect(); }
  doc.addEventListener('visibilitychange', visibility);
  win.addEventListener('pagehide', hide);
  win.addEventListener('pageshow', show);
  visibility();
  return () => {
    hide(); doc.removeEventListener('visibilitychange', visibility);
    win.removeEventListener('pagehide', hide); win.removeEventListener('pageshow', show);
  };
}
