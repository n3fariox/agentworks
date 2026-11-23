// Agentworks installation worker v2. Network-only: no fetch interception, caches,
// background sync, push, or mutation replay. Updates never force a page reload.
self.addEventListener('message', event => {
  if (event.data === 'activate-update') self.skipWaiting();
});

self.addEventListener('notificationclick', event => {
  event.notification.close();
  const data = event.notification.data || {};
  const scope = self.registration.scope;
  const scopeOrigin = new URL(scope);
  let targetUrl = scope;
  if (typeof data.url === 'string') {
    try {
      const resolved = new URL(data.url, scope);
      if (resolved.origin === scopeOrigin.origin && resolved.pathname.startsWith(scopeOrigin.pathname)) {
        targetUrl = resolved.href;
      }
    } catch {}
  }
  event.waitUntil((async () => {
    const list = await self.clients.matchAll({type: 'window', includeUncontrolled: true});
    const scoped = list.filter(client => client.url.startsWith(scope));
    const client = scoped.find(item => item.focused) || scoped[0];
    if (client) {
      try { await client.focus(); } catch {}
      if (data.sessionId && data.sessionId !== 'test') {
        client.postMessage({type: 'agentworks-notification-click', sessionId: data.sessionId});
      }
      return;
    }
    if (self.clients.openWindow) {
      await self.clients.openWindow(targetUrl);
    }
  })());
});
