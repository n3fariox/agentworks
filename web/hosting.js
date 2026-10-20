// The public module URL retains a reverse proxy's stripped mount. Never use an
// HTML <base> or origin-root URLs to infer the public application directory.
export const appBase = new URL('./', import.meta.url);
export function appUrl(path='') {
  const url = new URL(path.replace(/^\//, ''), appBase);
  if (url.origin !== appBase.origin || !url.pathname.startsWith(appBase.pathname)) {
    throw new Error('Application URL escapes its deployment');
  }
  return url.href;
}
export function socketUrl(path) {
  const url = new URL(appUrl(path));
  url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:';
  return url.href;
}
// Preserve existing root-hosted state; mounts never inherit a sibling's drafts,
// session selection, request IDs, preferences, or notification claims.
export const deploymentKey = key => appBase.pathname === '/' ? key : `${key}@${appBase.pathname}`;
function storage(name) {
  return {
    getItem: key => globalThis[name].getItem(deploymentKey(key)),
    setItem: (key, value) => globalThis[name].setItem(deploymentKey(key), value),
    removeItem: key => globalThis[name].removeItem(deploymentKey(key)),
  };
}
export const localState = storage('localStorage');
export const sessionState = storage('sessionStorage');
