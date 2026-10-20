// Agentworks installation worker v1. Network-only: no fetch interception, caches,
// background sync, push, or mutation replay. Updates never force a page reload.
self.addEventListener('message', event => {
  if (event.data === 'activate-update') self.skipWaiting();
});
