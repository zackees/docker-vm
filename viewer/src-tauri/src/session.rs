use std::{
  fs::{self, File},
  io::{self, BufRead, Write},
  net::SocketAddr,
  os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
  path::{Path, PathBuf},
  process::{Child, Command, Stdio},
  sync::{Arc, Mutex, atomic::{AtomicBool, AtomicU64, Ordering}},
  thread,
  time::Duration,
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use fuser::BackgroundSession;
use futures_util::{SinkExt, StreamExt};
use http::{HeaderValue, StatusCode};
use serde::Serialize;
use tokio::{io::AsyncWriteExt, net::{TcpListener, TcpStream}, sync::watch};
use tokio_tungstenite::{accept_hdr_async, tungstenite::{handshake::server::{Request, Response}, protocol::Message}};
use uuid::Uuid;

use crate::{
  settings::{self, HostSupport, Mode, Settings, SettingsView, SharingTier},
  mounts::{Options, ZipMount},
  zipfs::{self, Archive, Password, ZipFs},
};

const VIEWER_ORIGIN: &str = "tauri://localhost";
const PASSWORD_ATTEMPTS: usize = 3;

#[derive(Clone, Serialize)]
pub struct Connection {
  pub websocket_url: String,
  pub capability: String,
  pub vnc_password: String,
  /// Hardware H.264 stream (GPU mode with NVENC only); same one-use
  /// capability scheme as RFB, its own capability.
  pub video_url: Option<String>,
  pub video_capability: Option<String>,
}

/// An archive opened, with its password verified, before any container starts.
pub struct PreparedMount {
  target: String,
  archive: Archive,
  password: Option<Password>,
}

pub struct SessionManager {
  root: PathBuf,
  project: String,
  // Archive-mount and GPU compose overrides; the GPU one is dropped again if
  // the container cannot start with the device.
  overrides: Mutex<Vec<PathBuf>>,
  mounts_dir: PathBuf,
  mount_points: Vec<PathBuf>,
  archive_mounts: Mutex<Vec<BackgroundSession>>,
  progress: Arc<Progress>,
  startup: Mutex<Option<thread::JoinHandle<()>>>,
  shutdown: Mutex<Vec<watch::Sender<bool>>>,
  // Set when the container was started with the NVENC video service.
  video: AtomicBool,
  // The container's NVENC stream, for the viewer's native presenter.
  video_address: Mutex<Option<SocketAddr>>,
  // The container's audio stream (Wayland desktop only; absent otherwise).
  audio_address: Mutex<Option<SocketAddr>>,
  // Choices this session started with, and what the host supports (filled in
  // by the worker before any Docker work).
  settings: Settings,
  file_sharing: SharingTier,
  host: Mutex<Option<HostSupport>>,
  // RFB bytes forwarded to the viewer; read only by `DOCKER_VM_STATS` reports.
  bridged_bytes: Arc<AtomicU64>,
}

/// What the window shows while the session starts. Polled by the UI, so the
/// window never waits on Docker before it can paint.
#[derive(Clone, Serialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum Status {
  Starting { stage: String, log: Vec<String> },
  // The capability is fetched once through `connection`, never polled.
  Ready { #[serde(skip)] connection: Connection },
  Failed { error: String, log: Vec<String> },
}

const LOG_LINES: usize = 12;

/// Shared startup state plus the running child process, so closing the window
/// mid-build kills Docker instead of waiting for it.
struct Progress {
  status: Mutex<Status>,
  child: Mutex<Option<Child>>,
  stopping: AtomicBool,
}

impl Progress {
  fn stage(&self, stage: &str) {
    if let Ok(mut status) = self.status.lock() {
      *status = Status::Starting { stage: stage.into(), log: Vec::new() };
    }
  }

  fn line(&self, line: &str) {
    let line = line.trim();
    if line.is_empty() { return; }
    if let Ok(mut status) = self.status.lock() && let Status::Starting { log, .. } = &mut *status {
      if log.len() == LOG_LINES { log.remove(0); }
      log.push(line.chars().take(240).collect());
    }
  }

  fn stopping(&self) -> bool { self.stopping.load(Ordering::Acquire) }

  /// Runs a command, streaming its merged output into the status log.
  fn run(self: &Arc<Self>, mut command: Command, what: &str) -> Result<(), String> {
    if self.stopping() { return Err("session cancelled".into()); }
    let mut child = command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()
      .map_err(|error| format!("start {what}: {error}"))?;
    let readers: Vec<_> = [child.stdout.take().map(|out| Box::new(out) as Box<dyn io::Read + Send>), child.stderr.take().map(|err| Box::new(err) as Box<dyn io::Read + Send>)]
      .into_iter().flatten().map(|stream| {
        let progress = Arc::clone(self);
        thread::spawn(move || {
          for line in io::BufReader::new(stream).lines().map_while(Result::ok) {
            eprintln!("{line}");
            progress.line(&line);
          }
        })
      }).collect();
    *self.child.lock().map_err(|_| "child lock unavailable")? = Some(child);
    for reader in readers { let _ = reader.join(); }
    let child = self.child.lock().map_err(|_| "child lock unavailable")?.take();
    let status = match child { Some(mut child) => child.wait().map_err(|error| error.to_string())?, None => return Err("session cancelled".into()) };
    if self.stopping() { return Err("session cancelled".into()); }
    status.success().then_some(()).ok_or_else(|| format!("{what} failed ({status})"))
  }

  fn kill(&self) {
    self.stopping.store(true, Ordering::Release);
    if let Ok(mut child) = self.child.lock() && let Some(child) = child.as_mut() {
      let _ = child.kill();
    }
  }
}

impl SessionManager {
  /// Prepares the private session directory and returns immediately; Docker
  /// work (image build, container start, VNC wait) runs on a worker thread.
  pub fn start(mounts: Vec<PreparedMount>, settings: Settings) -> Result<Arc<Self>, String> {
    let file_sharing = SharingTier::effective(settings.file_sharing);
    let runtime = runtime_dir()?;
    ensure_tmpfs(&runtime)?;
    let project = format!("docker-vm-{}", Uuid::new_v4().simple());
    let root = runtime.join(&project);
    fs::create_dir(&root).map_err(|error| format!("create session directory: {error}"))?;
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).map_err(|error| error.to_string())?;
    // Outside `root`, which the container already sees at /run/session.
    let mounts_dir = runtime.join(format!("{project}.mounts"));
    let mount_points: Vec<PathBuf> = (0..mounts.len()).map(|index| mounts_dir.join(index.to_string())).collect();
    let overrides = if mounts.is_empty() { Vec::new() } else { vec![mounts_dir.join("compose.yaml")] };
    let progress = Arc::new(Progress {
      status: Mutex::new(Status::Starting { stage: "Preparing private session…".into(), log: Vec::new() }),
      child: Mutex::new(None),
      stopping: AtomicBool::new(false),
    });
    let manager = Arc::new(Self {
      root, project, overrides: Mutex::new(overrides), mounts_dir, mount_points,
      archive_mounts: Mutex::new(Vec::new()), progress, startup: Mutex::new(None), shutdown: Mutex::new(Vec::new()), video: AtomicBool::new(false), video_address: Mutex::new(None), audio_address: Mutex::new(None),
      settings, file_sharing, host: Mutex::new(None),
      bridged_bytes: Arc::new(AtomicU64::new(0)),
    });
    let password = secret(24)?;
    write_secret(&manager.root.join("vnc-password"), password.as_bytes())?;
    let worker = Arc::clone(&manager);
    let handle = thread::spawn(move || {
      let result = worker.launch(mounts, password);
      let status = match result {
        Ok(connection) => Status::Ready { connection },
        Err(error) => {
          eprintln!("private session failed: {error}");
          let log = match worker.progress.status.lock().map(|status| status.clone()) { Ok(Status::Starting { log, .. }) => log, _ => Vec::new() };
          Status::Failed { error, log }
        }
      };
      if let Ok(mut current) = worker.progress.status.lock() { *current = status; }
    });
    *manager.startup.lock().map_err(|_| "startup lock unavailable")? = Some(handle);
    Ok(manager)
  }

  fn launch(&self, mounts: Vec<PreparedMount>, password: String) -> Result<Connection, String> {
    let progress = &self.progress;
    if !mounts.is_empty() {
      ensure_mount_owner(&self.root)?;
      ensure_propagating(self.mounts_dir.parent().ok_or("runtime directory missing")?)?;
      for directory in std::iter::once(&self.mounts_dir).chain(&self.mount_points) {
        create_private_dir(directory)?;
      }
      let binds: Vec<(&Path, &str)> = self.mount_points.iter().map(PathBuf::as_path).zip(mounts.iter().map(|mount| mount.target.as_str())).collect();
      write_secret(&self.overrides()[0], mount_override(&binds)?.as_bytes())?;
    }
    progress.stage("Checking for GPU acceleration…");
    let host = host_support();
    *self.host.lock().map_err(|_| "host lock unavailable")? = Some(host.clone());
    ensure_image(progress)?;
    match self.gpu_device(&host) {
      Ok(device) => self.up_with_gpu(&device)?,
      Err(reason) => {
        eprintln!("CPU rendering: {reason}");
        progress.stage(&format!("Starting desktop container (CPU rendering: {reason})…"));
        self.up()?;
      }
    }
    // Mount only once the container exists: its rslave binds receive the FUSE
    // mounts, so Docker (root) never needs access to them.
    let owner = self.root.metadata().map_err(|error| error.to_string())?;
    for (mount, point) in mounts.into_iter().zip(&self.mount_points) {
      let session = zipfs::mount(ZipFs::new(mount.archive, mount.password, owner.uid(), owner.gid()), point)?;
      self.archive_mounts.lock().map_err(|_| "archive mount lock unavailable")?.push(session);
    }
    progress.stage("Waiting for the desktop to accept connections…");
    let vnc = container_vnc_address(&self.project, &self.root, &self.overrides())?;
    wait_for_vnc(vnc, progress)?;
    *self.audio_address.lock().map_err(|_| "audio lock unavailable")? = Some(SocketAddr::new(vnc.ip(), AUDIO_PORT));
    let capability = secret(32)?;
    let (address, shutdown) = bridge(vnc, capability.clone(), Arc::clone(&self.bridged_bytes))?;
    self.shutdown.lock().map_err(|_| "shutdown lock unavailable")?.push(shutdown);
    let (video_url, video_capability) = match self.video_bridge(vnc.ip())? {
      Some((address, capability)) => (Some(format!("ws://{address}")), Some(capability)),
      None => (None, None),
    };
    Ok(Connection { websocket_url: format!("ws://{address}"), capability, vnc_password: password, video_url, video_capability })
  }

  /// Bridges the container's NVENC stream if its service came up. It exits
  /// by itself when NVENC cannot open a session; the session then stays on
  /// RFB frames.
  fn video_bridge(&self, container: std::net::IpAddr) -> Result<Option<(SocketAddr, String)>, String> {
    if !self.video.load(Ordering::Acquire) { return Ok(None); }
    let address = SocketAddr::new(container, VIDEO_PORT);
    // The first desktop start after an image build is slow (cold caches).
    if !wait_for_port(address, Duration::from_secs(20), &self.progress) {
      eprintln!("hardware video unavailable; using VNC frames");
      return Ok(None);
    }
    *self.video_address.lock().map_err(|_| "video lock unavailable")? = Some(address);
    let capability = secret(32)?;
    let (bridged, shutdown) = bridge(address, capability.clone(), Arc::clone(&self.bridged_bytes))?;
    self.shutdown.lock().map_err(|_| "shutdown lock unavailable")?.push(shutdown);
    Ok(Some((bridged, capability)))
  }

  /// Where the container serves its audio, once the session is up.
  pub fn audio_address(&self) -> Option<SocketAddr> {
    self.audio_address.lock().ok().and_then(|address| *address)
  }

  /// Where the container serves its NVENC stream, once it is up.
  pub fn video_address(&self) -> Option<SocketAddr> {
    self.video_address.lock().ok().and_then(|address| *address)
  }

  /// The device for GPU rendering, or why this session renders on the CPU.
  fn gpu_device(&self, host: &HostSupport) -> Result<String, String> {
    if settings::env_off("DOCKER_VM_GPU") { return Err("disabled by DOCKER_VM_GPU=off".into()); }
    if self.settings.gpu == Mode::Off { return Err("GPU rendering is off in settings".into()); }
    host.device().map(str::to_string).map_err(str::to_string)
  }

  /// Saved settings (as they will apply at the next start) plus what this
  /// host supports, for the settings panel.
  pub fn settings_view(&self) -> SettingsView {
    let host = self.host.lock().ok().and_then(|host| host.clone());
    SettingsView::new(settings::load(), self.file_sharing, host.as_ref())
  }

  fn overrides(&self) -> Vec<PathBuf> {
    self.overrides.lock().map(|overrides| overrides.clone()).unwrap_or_default()
  }

  fn up(&self) -> Result<(), String> {
    let mut up = compose_command(&self.project, &self.root, &self.overrides())?;
    up.args(["up", "-d", "--no-build"]);
    self.progress.run(up, "docker compose up")
  }

  /// Starts the container with the CDI GPU device. If Docker cannot provide
  /// it after all, removes that container and starts the CPU-only session.
  fn up_with_gpu(&self, device: &str) -> Result<(), String> {
    let progress = &self.progress;
    progress.stage(&format!("Starting desktop container (GPU rendering on {device})…"));
    // Beside the session directory, so the container never sees it.
    let file = self.root.with_extension("gpu.yaml");
    let video = (self.settings.video == Mode::Auto).then(|| video_devices(device)).flatten();
    let xorg = xorg_display(device);
    // The X server is an explicit opt-in; otherwise the Wayland GPU desktop.
    let wayland = if xorg.is_some() || self.settings.wayland == Mode::Off { None } else { wayland_display() };
    if let Some(xorg) = &xorg {
      progress.stage(&format!("Starting desktop container (GPU display server on {})…", xorg.bus_id));
    } else if wayland.is_some() {
      progress.stage("Starting desktop container (GPU Wayland desktop)…");
    }
    write_secret(&file, gpu_override(device, video.as_ref(), xorg.as_ref(), wayland.as_ref()).as_bytes())?;
    self.video.store(video.is_some(), Ordering::Release);
    self.overrides.lock().map_err(|_| "override lock unavailable")?.push(file.clone());
    match self.up() {
      Err(error) if !progress.stopping() => {
        eprintln!("GPU rendering unavailable ({error}); falling back to CPU");
        let _ = compose(&self.project, &self.root, &self.overrides(), "down", &["--remove-orphans", "--timeout", "0"]);
        self.overrides.lock().map_err(|_| "override lock unavailable")?.retain(|path| path != &file);
        self.video.store(false, Ordering::Release);
        let _ = fs::remove_file(&file);
        progress.stage(&format!("Starting desktop container (CPU rendering: {device} failed to start)…"));
        self.up()
      }
      result => result,
    }
  }

  /// Total RFB bytes sent to the viewer so far (`DOCKER_VM_STATS` only).
  pub fn bridged_bytes(&self) -> u64 { self.bridged_bytes.load(Ordering::Relaxed) }

  pub fn status(&self) -> Status {
    self.progress.status.lock().map(|status| status.clone()).unwrap_or(Status::Failed { error: "status unavailable".into(), log: Vec::new() })
  }

  /// Hands out the one-use bridge capability only once the session is ready.
  pub fn connection(&self) -> Option<Connection> {
    match &*self.progress.status.lock().ok()? { Status::Ready { connection } => Some(connection.clone()), _ => None }
  }

  fn live(&self) -> bool { !self.progress.stopping() }

  /// The GPU X server's framebuffer size (it cannot honour RFB resizes); the
  /// container's display-size service applies it. Bounds match the viewer's
  /// physical-size cap.
  pub fn set_display_size(&self, width: u32, height: u32) -> Result<(), String> {
    if !(32..=8192).contains(&width) || !(32..=8192).contains(&height) || u64::from(width) * u64::from(height) > 3840 * 2160 {
      return Err("display size out of range".into());
    }
    let _status = self.progress.status.lock().map_err(|_| "session lock unavailable")?;
    if !self.live() { return Err("session ended".into()); }
    let temporary = self.root.join("display-size.next");
    fs::write(&temporary, format!("{width}x{height}")).map_err(|_| "cannot write display size")?;
    fs::rename(temporary, self.root.join("display-size")).map_err(|_| "cannot publish display size".into())
  }

  pub fn set_display_scale(&self, scale: f64) -> Result<(), String> {
    if !scale.is_finite() || !(0.5..=4.0).contains(&scale) {
      return Err("display scale must be between 0.5 and 4".into());
    }
    // Serialize writes with session teardown. Only this numeric setting crosses
    // the read-only container bind; no shell, Docker, or arbitrary-path API.
    let _status = self.progress.status.lock().map_err(|_| "session lock unavailable")?;
    if !self.live() { return Err("session ended".into()); }
    let temporary = self.root.join("display-scale.next");
    fs::write(&temporary, scale.to_string()).map_err(|_| "cannot write display scale")?;
    fs::rename(temporary, self.root.join("display-scale")).map_err(|_| "cannot publish display scale".into())
  }

  pub fn stop(&self) {
    let first = !self.progress.stopping.swap(true, Ordering::AcqRel);
    self.progress.kill();
    if let Ok(mut shutdown) = self.shutdown.lock() { for sender in shutdown.drain(..) { let _ = sender.send(true); } }
    // Wait for the worker so a half-finished `up` cannot recreate the
    // container after `down`.
    let handle = self.startup.lock().ok().and_then(|mut handle| handle.take());
    if let Some(handle) = handle && handle.thread().id() != thread::current().id() { let _ = handle.join(); }
    // RAM-only session: nothing to flush, so skip Docker's 10 s stop grace.
    if first {
      let _ = compose(&self.project, &self.root, &self.overrides(), "down", &["--remove-orphans", "--timeout", "0"]);
    }
    // Archives stay mounted until the container is gone.
    release_archives(&self.archive_mounts, &self.mounts_dir, &self.mount_points, &self.overrides());
    // Do not remove a non-tmpfs path. `start` only creates this after tmpfs
    // verification; removal is merely prompt release, not the privacy control.
    let _ = fs::remove_dir_all(&self.root);
  }
}

impl Drop for SessionManager { fn drop(&mut self) { self.stop(); } }

/// Opens every archive and obtains a verified password before any container
/// starts, so a wrong password fails fast and leaves no session behind.
pub fn prepare_mounts(options: &Options) -> Result<Vec<PreparedMount>, String> {
  let stdin = if options.password_stdin { Some(stdin_password()?) } else { None };
  options.mounts.iter().map(|mount| -> Result<PreparedMount, String> {
    let mut archive = Archive::open(&mount.source)?;
    let password = if archive.is_encrypted() { Some(archive_password(&mut archive, mount, stdin.as_ref())?) } else { None };
    Ok(PreparedMount { target: mount.target.clone(), archive, password })
  }).collect()
}

fn archive_password(archive: &mut Archive, mount: &ZipMount, stdin: Option<&Password>) -> Result<Password, String> {
  let source = mount.source.display();
  let (password, origin) = match (&mount.password_file, stdin) {
    (Some(file), _) => (read_password_file(file)?, format!("password-file {}", file.display())),
    (None, Some(password)) => (password.duplicate(), "standard input".to_string()),
    (None, None) => {
      for attempt in 0..PASSWORD_ATTEMPTS {
        let password = prompt_password(&mount.source, attempt > 0)?;
        if archive.check_password(&password)? { return Ok(password); }
      }
      return Err(format!("wrong password for {source}"));
    }
  };
  if archive.check_password(&password)? { Ok(password) } else { Err(format!("wrong password for {source} from {origin}")) }
}

fn read_password_file(path: &Path) -> Result<Password, String> {
  let describe = |error: io::Error| format!("password-file {}: {error}", path.display());
  let metadata = fs::metadata(path).map_err(describe)?;
  if !metadata.is_file() {
    return Err(format!("password-file {} is not a regular file", path.display()));
  }
  if metadata.mode() & 0o077 != 0 {
    return Err(format!("password-file {} must not be accessible to group or others (chmod 600)", path.display()));
  }
  fs::read(path).map(Password::from_line).map_err(describe)
}

fn stdin_password() -> Result<Password, String> {
  let mut line = Vec::new();
  io::stdin().lock().read_until(b'\n', &mut line).map_err(|error| format!("--password-stdin: {error}"))?;
  let password = Password::from_line(line);
  if password.is_empty() { return Err("--password-stdin: no password on standard input".into()); }
  Ok(password)
}

fn prompt_password(archive: &Path, retry: bool) -> Result<Password, String> {
  let name = archive.file_name().unwrap_or(archive.as_os_str()).to_string_lossy();
  let title = if retry { format!("Wrong password for {name}") } else { format!("Password for {name}") };
  // The password returns over a private pipe, never argv or the environment.
  let output = Command::new("zenity").args(["--password", "--title"]).arg(title).stdin(Stdio::null()).stderr(Stdio::null()).output()
    .map_err(|error| format!("{} is encrypted; use password-file= or --password-stdin (no prompt available: {error})", archive.display()))?;
  if !output.status.success() {
    return Err(format!("password entry for {} was cancelled", archive.display()));
  }
  Ok(Password::from_line(output.stdout))
}

/// FUSE admits only the mounting user and group. The container runs as the
/// session directory's owner, which must therefore be the viewer's real IDs.
fn ensure_mount_owner(root: &Path) -> Result<(), String> {
  let owner = root.metadata().map_err(|error| error.to_string())?;
  let status = fs::read_to_string("/proc/self/status").map_err(|error| format!("read process status: {error}"))?;
  let real = |key: &str| status.lines().find_map(|line| line.strip_prefix(key)).and_then(|ids| ids.split_whitespace().next()?.parse::<u32>().ok());
  if real("Uid:") == Some(owner.uid()) && real("Gid:") == Some(owner.gid()) {
    Ok(())
  } else {
    Err("archive mounts require the session directory to be owned by the viewer's real user and group".into())
  }
}

fn create_private_dir(path: &Path) -> Result<(), String> {
  fs::create_dir(path).map_err(|error| format!("create {}: {error}", path.display()))?;
  fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|error| error.to_string())
}

/// Compose override binding each empty mount point read-only at its target.
/// JSON is valid YAML, so values need JSON escaping plus Compose's `$$`.
fn mount_override(binds: &[(&Path, &str)]) -> Result<String, String> {
  let volumes = binds.iter().map(|(source, target)| -> Result<String, String> {
    let source = source.to_str().ok_or("session runtime path is not valid UTF-8")?;
    Ok(format!(
      r#"{{"type":"bind","source":{},"target":{},"read_only":true,"bind":{{"propagation":"rslave","create_host_path":false}}}}"#,
      compose_string(source),
      compose_string(target),
    ))
  }).collect::<Result<Vec<_>, _>>()?;
  Ok(format!(r#"{{"services":{{"desktop":{{"volumes":[{}]}}}}}}"#, volumes.join(",")))
}

/// What this host offers for GPU rendering. It needs KVM enabled (switched on
/// only selectively on some hosts) and an NVIDIA GPU that Docker exposes via
/// CDI. Settings and `DOCKER_VM_GPU=off` can still turn it off.
fn host_support() -> HostSupport {
  let kvm = fs::File::options().read(true).write(true).open("/dev/kvm")
    .map(drop)
    .map_err(|error| format!("KVM is not enabled (/dev/kvm: {error})"));
  let gpu = Command::new("docker").args(["info", "-f", "{{json .DiscoveredDevices}}"]).stderr(Stdio::null()).output()
    .map_err(|error| format!("run docker info: {error}"))
    .and_then(|output| pick_gpu(&String::from_utf8_lossy(&output.stdout)).ok_or_else(|| "no NVIDIA GPU is exposed to Docker".into()));
  HostSupport { kvm, gpu }
}

/// Lowest-numbered `nvidia.com/gpu=N` in `docker info`'s DiscoveredDevices
/// JSON; never `=all`, so a session holds one GPU.
fn pick_gpu(devices: &str) -> Option<String> {
  const PREFIX: &str = "\"nvidia.com/gpu=";
  devices.match_indices(PREFIX)
    .filter_map(|(start, _)| {
      let rest = &devices[start + PREFIX.len()..];
      let digits = &rest[..rest.find('"')?];
      Some((digits.parse::<u32>().ok()?, digits))
    })
    .min()
    .map(|(_, digits)| format!("nvidia.com/gpu={digits}"))
}

const VIDEO_PORT: u16 = 5901;
const AUDIO_PORT: u16 = 5902;

/// What the container needs for NVENC on the chosen GPU.
#[derive(Debug, PartialEq)]
struct VideoDevices {
  /// CUDA index of the chosen GPU under CUDA_DEVICE_ORDER=PCI_BUS_ID.
  cuda_index: u32,
  /// Bare /dev/nvidiaN nodes of the other GPUs. With only the chosen GPU's
  /// node present, this driver refuses NVENC sessions ("unsupported
  /// device"); CUDA itself stays pinned to the chosen GPU.
  other_nodes: Vec<String>,
}

/// NVENC needs for `nvidia.com/gpu=N`, unless `DOCKER_VM_VIDEO=off`. The CDI
/// index N is the driver's PCI-ordered index, so it is also the CUDA index
/// under PCI_BUS_ID ordering and the minor of /dev/nvidiaN.
fn video_devices(device: &str) -> Option<VideoDevices> {
  if std::env::var_os("DOCKER_VM_VIDEO").is_some_and(|value| value == "off") { return None; }
  let cuda_index: u32 = device.strip_prefix("nvidia.com/gpu=")?.parse().ok()?;
  let nodes = fs::read_dir("/dev").ok()?.filter_map(|entry| entry.ok()?.file_name().into_string().ok());
  Some(VideoDevices { cuda_index, other_nodes: other_gpu_nodes(nodes, cuda_index) })
}

fn other_gpu_nodes(names: impl Iterator<Item = String>, chosen: u32) -> Vec<String> {
  let mut nodes: Vec<String> = names
    .filter(|name| name.strip_prefix("nvidia").and_then(|index| index.parse::<u32>().ok()).is_some_and(|index| index != chosen))
    .map(|name| format!("/dev/{name}"))
    .collect();
  nodes.sort();
  nodes
}

/// The host's NVIDIA X driver for a GPU-backed X server in the container.
/// Xvnc has no GPU, so Chromium there must composite in software; headless
/// Xorg on the NVIDIA driver (with TigerVNC's Xorg module) composites on the
/// GPU. The host's own modules always match its kernel driver.
#[derive(Debug, PartialEq)]
struct XorgDisplay {
  driver: PathBuf,
  glx: PathBuf,
  /// X BusID of the chosen GPU, e.g. `PCI:6:0:0` (decimal).
  bus_id: String,
}

const XORG_MODULE_DIRS: [&str; 4] = [
  "/run/current-system/sw/lib/xorg/modules", // NixOS
  "/usr/lib/xorg/modules",                    // Debian, Ubuntu, Arch
  "/usr/lib64/xorg/modules",                  // Fedora, openSUSE
  "/usr/lib/x86_64-linux-gnu/nvidia/xorg",    // Debian's nvidia packages
];

/// Opt-in (`DOCKER_VM_XORG=on`) until frames can leave the GPU without a
/// framebuffer readback: on a headless NVIDIA screen, VNC scraping and
/// ximagesrc both read the framebuffer back and fall far below Xtigervnc.
fn xorg_display(device: &str) -> Option<XorgDisplay> {
  if std::env::var_os("DOCKER_VM_XORG").is_none_or(|value| value != "on") { return None; }
  let minor: u32 = device.strip_prefix("nvidia.com/gpu=")?.parse().ok()?;
  let (driver, glx) = XORG_MODULE_DIRS.iter().find_map(|dir| {
    let dir = Path::new(dir);
    let driver = [dir.join("drivers/nvidia_drv.so"), dir.join("nvidia_drv.so")].into_iter().find_map(|path| path.canonicalize().ok())?;
    let glx = [dir.join("extensions/libglxserver_nvidia.so"), dir.join("libglxserver_nvidia.so")].into_iter().find_map(|path| path.canonicalize().ok())?;
    Some((driver, glx))
  })?;
  let bus_id = fs::read_dir("/proc/driver/nvidia/gpus").ok()?.filter_map(|entry| entry.ok()).find_map(|entry| {
    let information = fs::read_to_string(entry.path().join("information")).ok()?;
    (gpu_minor(&information)? == minor).then(|| x_bus_id(&entry.file_name().to_string_lossy())).flatten()
  })?;
  Some(XorgDisplay { driver, glx, bus_id })
}

fn gpu_minor(information: &str) -> Option<u32> {
  information.lines().find_map(|line| line.strip_prefix("Device Minor:")?.trim().parse().ok())
}

/// `0000:06:00.0` (hex PCI address) to X's decimal `PCI:6:0:0`.
fn x_bus_id(address: &str) -> Option<String> {
  let (_domain, rest) = address.split_once(':')?;
  let (bus, rest) = rest.split_once(':')?;
  let (slot, function) = rest.split_once('.')?;
  let hex = |value: &str| u32::from_str_radix(value, 16).ok();
  Some(format!("PCI:{}:{}:{}", hex(bus)?, hex(slot)?, hex(function)?))
}

const XORG_MODULES: &str = "/opt/nvidia-xorg";

/// The Wayland GPU desktop needs NVIDIA's GBM backend, which the CDI spec
/// does not mount: the host's own `libnvidia-allocator` (it always matches
/// the kernel driver), loaded as `gbm/nvidia-drm_gbm.so`.
#[derive(Debug, PartialEq)]
struct WaylandDisplay {
  allocator: PathBuf,
}

const ALLOCATOR_CANDIDATES: [&str; 4] = [
  "/run/opengl-driver/lib/libnvidia-allocator.so.1", // NixOS
  "/usr/lib/x86_64-linux-gnu/libnvidia-allocator.so.1", // Debian, Ubuntu
  "/usr/lib64/libnvidia-allocator.so.1",             // Fedora, openSUSE
  "/usr/lib/libnvidia-allocator.so.1",               // Arch
];
const CONTAINER_LIBS: &str = "/usr/lib/x86_64-linux-gnu";

/// Automatic in GPU mode; `DOCKER_VM_WAYLAND=off` opts out. The container
/// falls back to its X session by itself if the compositor cannot start.
fn wayland_display() -> Option<WaylandDisplay> {
  if std::env::var_os("DOCKER_VM_WAYLAND").is_some_and(|value| value == "off") { return None; }
  // DOCKER_VM_GBM_ALLOCATOR names it on hosts with another library layout.
  let allocator = match std::env::var_os("DOCKER_VM_GBM_ALLOCATOR") {
    Some(path) => Path::new(&path).canonicalize().ok()?,
    None => ALLOCATOR_CANDIDATES.iter().find_map(|path| Path::new(path).canonicalize().ok())?,
  };
  Some(WaylandDisplay { allocator })
}

/// Compose override adding the CDI device and telling the session script to
/// enable Chromium's GPU path, plus the NVENC video service and the GPU X
/// server when possible.
fn gpu_override(device: &str, video: Option<&VideoDevices>, xorg: Option<&XorgDisplay>, wayland: Option<&WaylandDisplay>) -> String {
  let mut devices = vec![compose_string(device)];
  let mut volumes = Vec::new();
  let mut environment = vec![r#""DESKTOP_GPU":"nvidia""#.to_string()];
  if let Some(wayland) = wayland {
    environment.push(r#""DESKTOP_DISPLAY":"wayland""#.into());
    for target in ["gbm/nvidia-drm_gbm.so", "libnvidia-allocator.so.1"] {
      volumes.push(format!(
        r#"{{"type":"bind","source":{},"target":"{CONTAINER_LIBS}/{target}","read_only":true,"bind":{{"create_host_path":false}}}}"#,
        compose_string(&wayland.allocator.to_string_lossy()),
      ));
    }
  }
  if let Some(xorg) = xorg {
    environment.push(r#""DESKTOP_XORG":"nvidia""#.into());
    environment.push(format!(r#""DESKTOP_XORG_BUSID":{}"#, compose_string(&xorg.bus_id)));
    for (source, target) in [(&xorg.driver, "drivers/nvidia_drv.so"), (&xorg.glx, "extensions/libglxserver_nvidia.so")] {
      volumes.push(format!(
        r#"{{"type":"bind","source":{},"target":"{XORG_MODULES}/{target}","read_only":true,"bind":{{"create_host_path":false}}}}"#,
        compose_string(&source.to_string_lossy()),
      ));
    }
  }
  if let Some(video) = video {
    devices.extend(video.other_nodes.iter().map(|node| compose_string(node)));
    environment.push(r#""DESKTOP_VIDEO":"nvenc""#.into());
    environment.push(r#""CUDA_DEVICE_ORDER":"PCI_BUS_ID""#.into());
    environment.push(format!(r#""DESKTOP_VIDEO_CUDA_DEVICE":"{}""#, video.cuda_index));
  }
  let volumes = if volumes.is_empty() { String::new() } else { format!(r#","volumes":[{}]"#, volumes.join(",")) };
  format!(r#"{{"services":{{"desktop":{{"devices":[{}],"environment":{{{}}}{volumes}}}}}}}"#, devices.join(","), environment.join(","))
}

fn compose_string(value: &str) -> String {
  let mut quoted = String::from('"');
  for char in value.chars() {
    match char {
      '"' => quoted.push_str("\\\""),
      '\\' => quoted.push_str("\\\\"),
      '$' => quoted.push_str("$$"),
      char if char.is_control() => quoted.push_str(&format!("\\u{:04x}", u32::from(char))),
      char => quoted.push(char),
    }
  }
  quoted.push('"');
  quoted
}

/// Removes archive mounts and their directories. Never recursive: a mount that
/// is somehow still attached must not be walked.
fn release_archives(sessions: &Mutex<Vec<BackgroundSession>>, directory: &Path, points: &[PathBuf], overrides: &[PathBuf]) {
  // Dropping a session unmounts it through fusermount3.
  if let Ok(mut sessions) = sessions.lock() { sessions.clear(); }
  for point in points {
    if matches!(fs::remove_dir(point), Err(error) if error.kind() != io::ErrorKind::NotFound) {
      let _ = Command::new("fusermount3").arg("-uz").arg(point).stderr(Stdio::null()).status();
      let _ = fs::remove_dir(point);
    }
  }
  for file in overrides { let _ = fs::remove_file(file); }
  let _ = fs::remove_dir(directory);
}

fn runtime_dir() -> Result<PathBuf, String> {
  let value = std::env::var_os("XDG_RUNTIME_DIR").ok_or("XDG_RUNTIME_DIR is required; refusing an arbitrary /tmp")?;
  let path = PathBuf::from(value).join("docker-vm");
  fs::create_dir_all(&path).map_err(|error| format!("create runtime directory: {error}"))?;
  fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).map_err(|error| error.to_string())?;
  Ok(path)
}

struct MountInfo {
  target: PathBuf,
  filesystem: String,
  propagates: bool,
}

fn mount_info(path: &Path) -> Result<(PathBuf, MountInfo), String> {
  let canonical = path.canonicalize().map_err(|error| error.to_string())?;
  let mounts = fs::read_to_string("/proc/self/mountinfo").map_err(|error| format!("read mountinfo: {error}"))?;
  let mount = mounts.lines().filter_map(|line| {
    let (before, after) = line.split_once(" - ")?;
    let fields: Vec<&str> = before.split_whitespace().collect();
    Some(MountInfo {
      target: PathBuf::from(fields.get(4)?.replace("\\040", " ")),
      filesystem: after.split_whitespace().next()?.to_string(),
      propagates: fields.iter().skip(6).any(|field| field.starts_with("shared:") || field.starts_with("master:")),
    })
  }).filter(|mount| canonical.starts_with(&mount.target)).max_by_key(|mount| mount.target.as_os_str().len());
  mount.map(|mount| (canonical, mount)).ok_or_else(|| "cannot identify the runtime filesystem".into())
}

fn ensure_tmpfs(path: &Path) -> Result<(), String> {
  match mount_info(path)? {
    (_, mount) if mount.filesystem == "tmpfs" => Ok(()),
    (canonical, mount) => Err(format!("{canonical:?} is on {} at {:?}, not tmpfs", mount.filesystem, mount.target)),
  }
}

/// Docker's rslave bind carries later host mounts into the container only when
/// the source mount is shared (or itself a slave).
fn ensure_propagating(path: &Path) -> Result<(), String> {
  match mount_info(path)? {
    (_, mount) if mount.propagates => Ok(()),
    (canonical, mount) => Err(format!("{canonical:?} is on a private mount at {:?}; archive mounts need shared propagation", mount.target)),
  }
}

fn secret(bytes: usize) -> Result<String, String> {
  let mut value = vec![0; bytes];
  getrandom::fill(&mut value).map_err(|error| format!("random session secret: {error}"))?;
  Ok(URL_SAFE_NO_PAD.encode(value))
}

fn write_secret(path: &Path, value: &[u8]) -> Result<(), String> {
  let mut file = File::options().write(true).create_new(true).mode(0o600).open(path).map_err(|error| error.to_string())?;
  file.write_all(value).and_then(|_| file.write_all(b"\n")).map_err(|error| error.to_string())
}

// Set by the trusted installed launcher, never exposed as a webview command.
// Development builds retain their checkout-relative default.
fn resource_root() -> PathBuf {
  std::env::var_os("DOCKER_VM_RESOURCE_DIR")
    .map(PathBuf::from)
    .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

fn compose_command(project: &str, runtime: &Path, overrides: &[PathBuf]) -> Result<Command, String> {
  let metadata = runtime.metadata().map_err(|error| error.to_string())?;
  let mut command = Command::new("docker");
  command
    .current_dir(resource_root())
    .env("SESSION_RUNTIME_DIR", runtime)
    .env("SESSION_UID", metadata.uid().to_string())
    .env("SESSION_GID", metadata.gid().to_string())
    .args(["compose", "-p", project, "-f", "compose.yaml"]);
  for file in overrides {
    command.arg("-f").arg(file);
  }
  Ok(command)
}

fn compose(project: &str, runtime: &Path, overrides: &[PathBuf], action: &str, args: &[&str]) -> Result<(), String> {
  let status = compose_command(project, runtime, overrides)?
    .arg(action)
    .args(args)
    .status().map_err(|error| format!("start Docker Compose: {error}"))?;
  status.success().then_some(()).ok_or_else(|| format!("docker compose {action} failed"))
}

fn container_vnc_address(project: &str, runtime: &Path, overrides: &[PathBuf]) -> Result<SocketAddr, String> {
  for _ in 0..30 {
    let output = compose_command(project, runtime, overrides)?
      .args(["ps", "-q", "desktop"])
      .output()
      .map_err(|error| error.to_string())?;
    let id = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !id.is_empty() {
      let output = Command::new("docker").args(["inspect", "-f", "{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}", &id]).output().map_err(|error| error.to_string())?;
      let ip = String::from_utf8_lossy(&output.stdout).trim().parse().map_err(|error| format!("container IP: {error}"))?;
      return Ok(SocketAddr::new(ip, 5900));
    }
    thread::sleep(Duration::from_millis(200));
  }
  Err("desktop container did not become available".into())
}

fn wait_for_vnc(address: SocketAddr, progress: &Progress) -> Result<(), String> {
  for _ in 0..300 {
    if progress.stopping() { return Err("session cancelled".into()); }
    if std::net::TcpStream::connect_timeout(&address, Duration::from_millis(200)).is_ok() { return Ok(()); }
    thread::sleep(Duration::from_millis(100));
  }
  Err("desktop VNC server did not become ready within 30 seconds".into())
}

const IMAGE: &str = "docker-vm-desktop:local";
/// Prebuilt images, published by CI for each source fingerprint.
const PREBUILT: &str = "ghcr.io/zackees/docker-vm-desktop";
const SOURCE_LABEL: &str = "docker-vm.source";

/// Fingerprint of everything the desktop Dockerfile copies: SHA-256 over the
/// files directly in `images/desktop`, byte-sorted by name, each fed as
/// name, NUL, contents, NUL; first 16 hex digits. `scripts/image-fingerprint`
/// computes the same value for CI's image tags. A local image with another
/// fingerprint is replaced; a matching one starts with no build step at all.
fn image_fingerprint() -> Result<String, String> {
  fingerprint_dir(&resource_root().join("images/desktop"))
}

fn fingerprint_dir(directory: &Path) -> Result<String, String> {
  use sha2::{Digest, Sha256};
  let mut files: Vec<PathBuf> = fs::read_dir(directory).map_err(|error| format!("read {}: {error}", directory.display()))?
    .filter_map(|entry| entry.ok().map(|entry| entry.path())).filter(|path| path.is_file()).collect();
  files.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
  let mut digest = Sha256::new();
  for file in files {
    digest.update(file.file_name().unwrap_or_default().as_encoded_bytes());
    digest.update([0]);
    digest.update(fs::read(&file).map_err(|error| format!("read {}: {error}", file.display()))?);
    digest.update([0]);
  }
  Ok(digest.finalize().iter().take(8).map(|byte| format!("{byte:02x}")).collect())
}

fn image_source(image: &str) -> Option<String> {
  let output = Command::new("docker").args(["image", "inspect", "-f", &format!("{{{{index .Config.Labels \"{SOURCE_LABEL}\"}}}}"), image])
    .stderr(Stdio::null()).output().ok()?;
  output.status.success().then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn ensure_image(progress: &Arc<Progress>) -> Result<(), String> {
  progress.stage("Checking desktop image…");
  let fingerprint = image_fingerprint()?;
  let current = image_source(IMAGE);
  if current.as_deref() == Some(fingerprint.as_str()) { return Ok(()); }
  // A prebuilt image for exactly these sources downloads in a fraction of
  // the time a local build takes; anything else falls through to building.
  if std::env::var_os("DOCKER_VM_PREBUILT").is_none_or(|value| value != "off") {
    let prebuilt = format!("{PREBUILT}:{fingerprint}");
    progress.stage("Downloading the prebuilt desktop image…");
    let mut pull = Command::new("docker");
    pull.args(["pull", &prebuilt]);
    if progress.run(pull, "prebuilt image download").is_ok() && image_source(&prebuilt).as_deref() == Some(fingerprint.as_str()) {
      let tagged = Command::new("docker").args(["tag", &prebuilt, IMAGE]).status().map_err(|error| format!("run docker: {error}"))?;
      if tagged.success() { return Ok(()); }
    }
    if progress.stopping() { return Err("session cancelled".into()); }
    eprintln!("no prebuilt desktop image {prebuilt}; building locally");
  }
  progress.stage(if current.is_some() {
    "Updating the desktop image (sources changed)… this can take a few minutes."
  } else {
    "Building the desktop image for first use… this can take several minutes."
  });
  let mut build = Command::new("docker");
  build.current_dir(resource_root())
    .args(["build", "--progress=plain", "--label"]).arg(format!("{SOURCE_LABEL}={fingerprint}"))
    .args(["-t", IMAGE, "-f", "images/desktop/Dockerfile", "."]);
  progress.run(build, "desktop image build")
}

fn wait_for_port(address: SocketAddr, limit: Duration, progress: &Progress) -> bool {
  let deadline = std::time::Instant::now() + limit;
  while std::time::Instant::now() < deadline && !progress.stopping() {
    if std::net::TcpStream::connect_timeout(&address, Duration::from_millis(200)).is_ok() { return true; }
    thread::sleep(Duration::from_millis(200));
  }
  false
}

fn bridge(vnc: SocketAddr, capability: String, bytes: Arc<AtomicU64>) -> Result<(SocketAddr, watch::Sender<bool>), String> {
  let listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(|error| format!("bind bridge: {error}"))?;
  let address = listener.local_addr().map_err(|error| error.to_string())?;
  listener.set_nonblocking(true).map_err(|error| error.to_string())?;
  let (shutdown, mut stopped) = watch::channel(false);
  let capability_used = Arc::new(AtomicBool::new(false));
  thread::spawn(move || {
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_io().build().expect("bridge runtime");
    runtime.block_on(async move {
      let listener = TcpListener::from_std(listener).expect("async bridge listener");
      loop {
        tokio::select! {
          _ = stopped.changed() => break,
          accepted = listener.accept() => match accepted {
            Ok((stream, _)) => { let capability = capability.clone(); let capability_used = Arc::clone(&capability_used); tokio::spawn(proxy(stream, vnc, capability, capability_used, Arc::clone(&bytes))); }
            Err(_) => break,
          }
        }
      }
    });
  });
  Ok((address, shutdown))
}

async fn proxy(stream: TcpStream, vnc: SocketAddr, capability: String, capability_used: Arc<AtomicBool>, bytes: Arc<AtomicU64>) {
  let callback = move |request: &Request, mut response: Response| {
    let origin = request.headers().get("origin").and_then(|value| value.to_str().ok());
    let protocols = request.headers().get("sec-websocket-protocol").and_then(|value| value.to_str().ok()).unwrap_or("");
    let supplied = protocols.split(',').map(str::trim).any(|value| value == capability);
    if origin != Some(VIEWER_ORIGIN) || !supplied || capability_used.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() {
      let response = http::Response::builder().status(StatusCode::FORBIDDEN).body(Some("forbidden".into())).unwrap();
      return Err(response);
    }
    response.headers_mut().insert("sec-websocket-protocol", HeaderValue::from_static("binary"));
    Ok(response)
  };
  let Ok(websocket) = accept_hdr_async(stream, callback).await else { return; };
  let Ok(vnc) = TcpStream::connect(vnc).await else { return; };
  let (mut ws_write, mut ws_read) = websocket.split();
  let (mut vnc_read, mut vnc_write) = vnc.into_split();
  let client_to_vnc = async {
    while let Some(Ok(message)) = ws_read.next().await {
      match message {
        Message::Binary(bytes) => { if vnc_write.write_all(&bytes).await.is_err() { break; } }
        Message::Close(_) => break,
        _ => {}
      }
    }
  };
  let vnc_to_client = async {
    let mut buffer = vec![0; 16 * 1024];
    loop {
      let count = match tokio::io::AsyncReadExt::read(&mut vnc_read, &mut buffer).await { Ok(0) | Err(_) => break, Ok(count) => count };
      bytes.fetch_add(count as u64, Ordering::Relaxed);
      if ws_write.send(Message::Binary(buffer[..count].to_vec().into())).await.is_err() { break; }
    }
  };
  tokio::select! { _ = client_to_vnc => {}, _ = vnc_to_client => {} }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn image_fingerprint_matches_the_ci_script() {
    // scripts/image-fingerprint on the same two files gives this value.
    let directory = std::env::temp_dir().join(format!("docker-vm-fingerprint-{}", std::process::id()));
    fs::create_dir_all(directory.join("ignored-subdirectory")).unwrap();
    fs::write(directory.join("b"), "second\n").unwrap();
    fs::write(directory.join("a"), "first\n").unwrap();
    let value = fingerprint_dir(&directory);
    fs::remove_dir_all(&directory).unwrap();
    assert_eq!(value.unwrap(), "7275e4dd341a460f");
  }

  #[test]
  fn escapes_values_for_compose() {
    assert_eq!(compose_string(r#"/a "b" \c $HOME"#), r#""/a \"b\" \\c $$HOME""#);
    let control = compose_string(&char::from(1u8).to_string());
    assert!(control.len() == 8 && control.ends_with("u0001\""), "{control}");
  }

  #[test]
  fn picks_the_lowest_numbered_nvidia_gpu() {
    let devices = r#"[{"Source":"cdi","ID":"nvidia.com/gpu=all"},{"Source":"cdi","ID":"nvidia.com/gpu=1"},{"Source":"cdi","ID":"nvidia.com/gpu=0"}]"#;
    assert_eq!(pick_gpu(devices).as_deref(), Some("nvidia.com/gpu=0"));
    assert_eq!(pick_gpu(r#"[{"Source":"cdi","ID":"nvidia.com/gpu=all"}]"#), None);
    assert_eq!(pick_gpu("[]"), None);
    assert_eq!(pick_gpu("null"), None);
  }

  #[test]
  fn gpu_override_adds_the_cdi_device() {
    assert_eq!(
      gpu_override("nvidia.com/gpu=0", None, None, None),
      r#"{"services":{"desktop":{"devices":["nvidia.com/gpu=0"],"environment":{"DESKTOP_GPU":"nvidia"}}}}"#,
    );
  }

  #[test]
  fn gpu_override_adds_nvenc_service_and_other_gpu_nodes() {
    let video = VideoDevices { cuda_index: 0, other_nodes: vec!["/dev/nvidia1".into()] };
    assert_eq!(
      gpu_override("nvidia.com/gpu=0", Some(&video), None, None),
      r#"{"services":{"desktop":{"devices":["nvidia.com/gpu=0","/dev/nvidia1"],"environment":{"DESKTOP_GPU":"nvidia","DESKTOP_VIDEO":"nvenc","CUDA_DEVICE_ORDER":"PCI_BUS_ID","DESKTOP_VIDEO_CUDA_DEVICE":"0"}}}}"#,
    );
  }

  #[test]
  fn gpu_override_mounts_the_host_x_driver() {
    let xorg = XorgDisplay { driver: "/x/nvidia_drv.so".into(), glx: "/x/libglxserver_nvidia.so.595".into(), bus_id: "PCI:6:0:0".into() };
    assert_eq!(
      gpu_override("nvidia.com/gpu=0", None, Some(&xorg), None),
      concat!(
        r#"{"services":{"desktop":{"devices":["nvidia.com/gpu=0"],"environment":{"DESKTOP_GPU":"nvidia","DESKTOP_XORG":"nvidia","DESKTOP_XORG_BUSID":"PCI:6:0:0"},"#,
        r#""volumes":[{"type":"bind","source":"/x/nvidia_drv.so","target":"/opt/nvidia-xorg/drivers/nvidia_drv.so","read_only":true,"bind":{"create_host_path":false}},"#,
        r#"{"type":"bind","source":"/x/libglxserver_nvidia.so.595","target":"/opt/nvidia-xorg/extensions/libglxserver_nvidia.so","read_only":true,"bind":{"create_host_path":false}}]}}}"#,
      ),
    );
  }

  #[test]
  fn gpu_override_mounts_the_gbm_allocator_for_wayland() {
    let wayland = WaylandDisplay { allocator: "/x/libnvidia-allocator.so.595".into() };
    assert_eq!(
      gpu_override("nvidia.com/gpu=0", None, None, Some(&wayland)),
      concat!(
        r#"{"services":{"desktop":{"devices":["nvidia.com/gpu=0"],"environment":{"DESKTOP_GPU":"nvidia","DESKTOP_DISPLAY":"wayland"},"#,
        r#""volumes":[{"type":"bind","source":"/x/libnvidia-allocator.so.595","target":"/usr/lib/x86_64-linux-gnu/gbm/nvidia-drm_gbm.so","read_only":true,"bind":{"create_host_path":false}},"#,
        r#"{"type":"bind","source":"/x/libnvidia-allocator.so.595","target":"/usr/lib/x86_64-linux-gnu/libnvidia-allocator.so.1","read_only":true,"bind":{"create_host_path":false}}]}}}"#,
      ),
    );
  }

  #[test]
  fn x_bus_ids_are_decimal_and_match_the_device_minor() {
    assert_eq!(x_bus_id("0000:06:00.0").as_deref(), Some("PCI:6:0:0"));
    assert_eq!(x_bus_id("0000:1a:1f.7").as_deref(), Some("PCI:26:31:7"));
    assert_eq!(gpu_minor("Model: \t RTX\nDevice Minor: \t 1\n"), Some(1));
  }

  #[test]
  fn other_gpu_nodes_skip_the_chosen_gpu_and_control_nodes() {
    let names = ["nvidia0", "nvidia1", "nvidia2", "nvidiactl", "nvidia-uvm", "nvidia-modeset", "null"].map(String::from);
    assert_eq!(other_gpu_nodes(names.into_iter(), 1), vec!["/dev/nvidia0", "/dev/nvidia2"]);
  }

  #[test]
  fn binds_mount_points_read_only_with_slave_propagation() {
    let document = mount_override(&[(Path::new("/run/user/1000/docker-vm/p.mounts/0"), "/home/desktop/assets")]).unwrap();
    assert_eq!(
      document,
      r#"{"services":{"desktop":{"volumes":[{"type":"bind","source":"/run/user/1000/docker-vm/p.mounts/0","target":"/home/desktop/assets","read_only":true,"bind":{"propagation":"rslave","create_host_path":false}}]}}}"#,
    );
  }
}
