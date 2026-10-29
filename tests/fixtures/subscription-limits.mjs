// Builds the _codex/subscriptionLimits metadata the Rust Codex bridge attaches to usage updates.
// Each field carries its own source observation time: sparse notifications and
// repeated token updates must not turn an older percentage into a fresh one.
const WINDOWS = ['primary', 'secondary'];
const FIELDS = ['usedPercent', 'windowDurationMins', 'resetsAt'];
const own = (o, key) => Object.hasOwn(o, key);

export function observeSubscriptionLimits(state, snapshot, observedAtMs = Date.now()) {
  if (!snapshot || typeof snapshot !== 'object') return;
  const limitId = typeof snapshot.limitId === 'string' ? snapshot.limitId : 'codex';
  const cache = state.agentworksSubscriptionLimits ??= new Map();
  const entry = cache.get(limitId) ?? {limitId};
  if (own(snapshot, 'limitName')) entry.limitName = snapshot.limitName;
  for (const key of WINDOWS) {
    if (!own(snapshot, key)) continue;
    const window = snapshot[key];
    // Explicit null clears a window; absence preserves its previous values.
    if (window === null) { entry[key] = null; continue; }
    if (!window || typeof window !== 'object') continue;
    const target = entry[key] ??= {};
    for (const field of FIELDS) {
      if (own(window, field)) target[field] = {value: window[field], observedAtMs};
    }
  }
  cache.set(limitId, entry);
}

export function subscriptionLimitsMeta(state) {
  if (!state.agentworksSubscriptionLimits?.size) return {};
  // Copy so an already emitted event cannot change after a later observation.
  const limits = JSON.parse(JSON.stringify([...state.agentworksSubscriptionLimits.values()]));
  return {_meta: {'_codex/subscriptionLimits': {version: 1, limits}}};
}
