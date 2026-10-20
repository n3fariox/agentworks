import {appBase, appUrl} from './hosting.js';

async function setup() {
  if (!globalThis.isSecureContext || !('serviceWorker' in navigator)) return;
  const enabled = document.querySelector('meta[name="agentworks-pwa"]')?.content === 'enabled';
  const script = appUrl('service-worker.js');
  if (!enabled) {
    // Retire only our exact registration. Never clear origin-wide storage/caches.
    for (const registration of await navigator.serviceWorker.getRegistrations()) {
      const workers = [registration.active, registration.waiting, registration.installing].filter(Boolean);
      if (registration.scope === appBase.href && workers.some(worker => worker.scriptURL === script)) {
        await registration.unregister();
      }
    }
    return;
  }
  const registration = await navigator.serviceWorker.register(script, {scope: appBase.href, updateViaCache: 'none'});
  let offered = false;
  const offer = () => {
    if (offered || !registration.waiting || !navigator.serviceWorker.controller) return;
    offered = true;
    const button = document.createElement('button');
    button.className = 'pwa-update';
    button.textContent = 'Agentworks update available · Refresh';
    button.onclick = () => {
      button.disabled = true;
      navigator.serviceWorker.addEventListener('controllerchange', () => location.reload(), {once: true});
      registration.waiting?.postMessage('activate-update');
    };
    document.body.append(button);
  };
  offer();
  registration.addEventListener('updatefound', () => {
    registration.installing?.addEventListener('statechange', offer);
  });
}
setup().catch(error => console.warn('Agentworks installation setup failed:', error));
