import {socketUrl} from './hosting.js';

const bytes = value => `${(value / 1073741824).toFixed(2)} GiB`;
export function hostUsageLines(sample, now = Date.now()) {
  if (!sample || sample.reason) return [sample?.reason || 'Waiting for host metrics'];
  const age = now - sample.sampledAt;
  const lines = [`Sampled ${new Date(sample.sampledAt).toLocaleString()} · ${age > 15000 ? 'stale' : age < 0 ? 'source clock ahead' : 'fresh'} · every 5 seconds`];
  lines.push(sample.cpu?.percent == null ? `CPU unavailable: ${sample.cpu?.reason || 'No report'}` : `CPU: ${sample.cpu.percent.toFixed(1)}% (interval delta; idle and I/O wait excluded)`);
  for (const [key, label] of [['memory','Memory'],['swap','Swap'],['disk','Disk /']]) {
    const metric = sample[key];
    lines.push(metric?.total == null ? `${label} unavailable: ${metric?.reason || 'No report'}` : `${label}: ${bytes(metric.used)} / ${bytes(metric.total)} used${key === 'memory' ? ' (total minus available)' : key === 'disk' ? ' (root filesystem only)' : metric.total === 0 ? ' (no swap configured)' : ''}`);
  }
  return lines;
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
