// A classic script, deliberately: it still runs when main.js cannot. If any
// of main.js's ES-module imports fails to load, the browser skips the whole
// module without reaching its try/catch, and the page would otherwise sit on
// "Starting..." forever with no error anywhere. main.js marks the document
// as booted as soon as its imports have resolved.
setTimeout(() => {
  if (document.documentElement.dataset.booted) return;
  document.querySelector('#status').textContent =
    'The viewer failed to load its VNC client (novnc/core/rfb.js), so it never asked for the bridge endpoint. Close this window; the session will be torn down.';
}, 15000);
