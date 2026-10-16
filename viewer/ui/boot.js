// Classic script: runs even when the module graph below fails to load (for
// example a build without the noVNC submodule), so the window never sits on
// a silent "Starting…" forever.
window.addEventListener('error', event => {
  document.querySelector('#stage').textContent = `Viewer error: ${event.message}`;
});
setTimeout(() => {
  if (!window.__viewerLoaded) {
    document.documentElement.classList.add('failed');
    document.querySelector('#stage').textContent =
      'The viewer failed to load its scripts (is the noVNC submodule missing from this build?).';
  }
}, 4000);
