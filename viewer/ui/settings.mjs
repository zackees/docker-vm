// Settings panel: GPU rendering, the GPU desktop and video transport. Choices are saved by the
// viewer (applied when the next session starts); options this host cannot
// honour (no KVM, no NVIDIA GPU exposed to Docker) are greyed out with why.
const invoke = (command, args) => window.__TAURI__.core.invoke(command, args);

export function settingsPanel(document) {
  const toggle = document.querySelector('#settings-toggle');
  const panel = document.querySelector('#settings');
  const overrides = document.querySelector('#settings-overrides');
  const restart = document.querySelector('#settings-restart');
  const groups = {
    gpu: document.querySelector('#gpu-setting'),
    wayland: document.querySelector('#wayland-setting'),
    video: document.querySelector('#video-setting'),
    file_sharing: document.querySelector('#file-sharing-setting'),
  };
  let view = null;

  const chosen = name => groups[name].querySelector('input:checked')?.value ?? (name === 'file_sharing' ? 'ro' : 'auto');

  function render() {
    for (const [name, unavailable] of [['gpu', view.gpu_unavailable], ['wayland', view.wayland_unavailable],
      ['video', view.video_unavailable]]) {
      const group = groups[name];
      // The GPU desktop and video also need GPU rendering switched on.
      const reason = unavailable ?? (name !== 'gpu' && chosen('gpu') === 'off' ? 'needs GPU rendering switched on' : null);
      group.disabled = Boolean(reason) || view.checking;
      group.classList.toggle('greyed', group.disabled);
      const note = group.querySelector('.unavailable');
      note.hidden = !reason && !view.checking;
      note.textContent = view.checking ? 'Checking this host…' : `Unavailable: ${reason}`;
    }
    overrides.hidden = view.overrides.length === 0;
    overrides.textContent = `Overridden for this launch by ${view.overrides.join(', ')}.`;
    const labels = { off: 'Off', encrypted: 'Encrypted archives only', ro: 'Read-only', rw: 'Read & write' };
    document.querySelector('#file-sharing-running').textContent =
      `Running session: ${labels[view.effective_file_sharing]}; saved setting: ${labels[view.settings.file_sharing]}.`;
    const sharing = view.sharing_status;
    document.querySelector('#file-sharing-status').textContent = sharing.unavailable
      ? `Sharing unavailable: ${sharing.unavailable}`
      : sharing.allowed_now === 'off' ? 'No folders shared.'
        : sharing.mounts.length ? `Shared now: ${sharing.mounts.join(', ')}.` : 'Available but not active: no folders shared.';
  }

  async function refresh() {
    view = await invoke('settings_view');
    view.sharing_status = await invoke('sharing_status');
    for (const name of Object.keys(groups)) {
      groups[name].querySelector(`input[value="${view.settings[name]}"]`).checked = true;
    }
    render();
    // The host check runs with session startup; poll until it has an answer.
    if (view.checking && !panel.hidden) setTimeout(refresh, 1000);
  }

  for (const group of Object.values(groups)) {
    group.addEventListener('change', async () => {
      const settings = { gpu: chosen('gpu'), wayland: chosen('wayland'), video: chosen('video'), file_sharing: chosen('file_sharing') };
      try {
        await invoke('save_settings', { gpu: settings.gpu, wayland: settings.wayland,
          video: settings.video, fileSharing: settings.file_sharing });
      } catch (error) {
        await refresh();
        document.querySelector('#file-sharing-status').textContent = `Settings change could not finish: ${error}`;
        return;
      }
      view.settings = settings;
      view.sharing_status = await invoke('sharing_status');
      render();
    });
  }

  // The native video layer covers the page: the viewer then draws its own
  // settings button above it, and the video is hidden while the panel is open.
  invoke('native_controls').then(native => { toggle.hidden = native; }).catch(() => {});
  const showPanel = open => {
    panel.hidden = !open;
    toggle.setAttribute('aria-expanded', String(open));
    invoke('native_video_hidden', { hidden: open }).catch(() => {});
    if (open) refresh();
  };
  window.__toggleSettings = () => showPanel(panel.hidden);
  toggle.addEventListener('click', () => showPanel(panel.hidden));
  document.addEventListener('keydown', event => {
    if (event.key === 'Escape' && !panel.hidden) { event.stopPropagation(); showPanel(false); }
  }, true);
  restart.addEventListener('click', () => {
    restart.disabled = true;
    restart.textContent = 'Restarting…';
    invoke('restart_session');
  });
}
