use std::{
  fs::{self, File},
  io::Write,
  net::SocketAddr,
  os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
  path::{Path, PathBuf},
  process::Command,
  sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}},
  thread,
  time::Duration,
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use futures_util::{SinkExt, StreamExt};
use http::{HeaderValue, StatusCode};
use serde::Serialize;
use tokio::{io::AsyncWriteExt, net::{TcpListener, TcpStream}, sync::watch};
use tokio_tungstenite::{accept_hdr_async, tungstenite::{handshake::server::{Request, Response}, protocol::Message}};
use uuid::Uuid;

const VIEWER_ORIGIN: &str = "http://tauri.localhost";

#[derive(Clone, Serialize)]
pub struct Connection {
  pub websocket_url: String,
  pub capability: String,
  pub vnc_password: String,
}

pub struct SessionManager {
  root: PathBuf,
  project: String,
  connection: Mutex<Option<Connection>>,
  shutdown: watch::Sender<bool>,
}

impl SessionManager {
  pub fn start() -> Result<Self, String> {
    let runtime = runtime_dir()?;
    ensure_tmpfs(&runtime)?;
    let project = format!("docker-vm-{}", Uuid::new_v4().simple());
    let root = runtime.join(&project);
    fs::create_dir(&root).map_err(|error| format!("create session directory: {error}"))?;
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).map_err(|error| error.to_string())?;

    let password = secret(24)?;
    write_secret(&root.join("vnc-password"), password.as_bytes())?;
    let result = (|| {
      compose(&project, &root, "up", &["-d", "--build"])?;
      let vnc = container_vnc_address(&project)?;
      wait_for_vnc(vnc)?;
      let capability = secret(32)?;
      let (address, shutdown) = bridge(vnc, capability.clone())?;
      let connection = Connection { websocket_url: format!("ws://{address}"), capability, vnc_password: password };
      Ok((connection, shutdown))
    })();
    match result {
      Ok((connection, shutdown)) => Ok(Self { root, project, connection: Mutex::new(Some(connection)), shutdown }),
      Err(error) => {
        let _ = compose(&project, &root, "down", &["--remove-orphans"]);
        let _ = fs::remove_dir_all(&root);
        Err(error)
      }
    }
  }

  pub fn cef_root(&self) -> PathBuf { self.root.join("cef-root") }

  pub fn connection(&self) -> Option<Connection> {
    self.connection.lock().ok()?.clone()
  }

  pub fn stop(&self) {
    let _ = self.shutdown.send(true);
    let was_live = self.connection.lock().map(|mut connection| connection.take().is_some()).unwrap_or(false);
    if was_live {
      let _ = compose(&self.project, &self.root, "down", &["--remove-orphans"]);
    }
    // Do not remove a non-tmpfs path. `start` only creates this after tmpfs
    // verification; removal is merely prompt release, not the privacy control.
    let _ = fs::remove_dir_all(&self.root);
  }
}

impl Drop for SessionManager { fn drop(&mut self) { self.stop(); } }

fn runtime_dir() -> Result<PathBuf, String> {
  let value = std::env::var_os("XDG_RUNTIME_DIR").ok_or("XDG_RUNTIME_DIR is required; refusing an arbitrary /tmp")?;
  let path = PathBuf::from(value).join("docker-vm");
  fs::create_dir_all(&path).map_err(|error| format!("create runtime directory: {error}"))?;
  fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).map_err(|error| error.to_string())?;
  Ok(path)
}

fn ensure_tmpfs(path: &Path) -> Result<(), String> {
  let canonical = path.canonicalize().map_err(|error| error.to_string())?;
  let mounts = fs::read_to_string("/proc/self/mountinfo").map_err(|error| format!("read mountinfo: {error}"))?;
  let mount = mounts.lines().filter_map(|line| {
    let (before, after) = line.split_once(" - ")?;
    let target = before.split_whitespace().nth(4)?;
    let filesystem = after.split_whitespace().next()?;
    Some((PathBuf::from(target.replace("\\040", " ")), filesystem))
  }).filter(|(target, _)| canonical.starts_with(target)).max_by_key(|(target, _)| target.as_os_str().len());
  match mount {
    Some((_, "tmpfs")) => Ok(()),
    Some((target, filesystem)) => Err(format!("{canonical:?} is on {filesystem} at {target:?}, not tmpfs")),
    None => Err("cannot identify the runtime filesystem".into()),
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

fn compose(project: &str, runtime: &Path, action: &str, args: &[&str]) -> Result<(), String> {
  let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
  let status = Command::new("docker")
    .current_dir(root)
    .env("SESSION_RUNTIME_DIR", runtime)
    .env("SESSION_UID", runtime.metadata().map_err(|error| error.to_string())?.uid().to_string())
    .env("SESSION_GID", runtime.metadata().map_err(|error| error.to_string())?.gid().to_string())
    .args(["compose", "-p", project, "-f", "compose.yaml", action])
    .args(args)
    .status().map_err(|error| format!("start Docker Compose: {error}"))?;
  status.success().then_some(()).ok_or_else(|| format!("docker compose {action} failed"))
}

fn container_vnc_address(project: &str) -> Result<SocketAddr, String> {
  let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
  for _ in 0..30 {
    let output = Command::new("docker").current_dir(&root).args(["compose", "-p", project, "-f", "compose.yaml", "ps", "-q", "desktop"]).output().map_err(|error| error.to_string())?;
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

fn wait_for_vnc(address: SocketAddr) -> Result<(), String> {
  for _ in 0..50 {
    if std::net::TcpStream::connect_timeout(&address, Duration::from_millis(200)).is_ok() { return Ok(()); }
    thread::sleep(Duration::from_millis(100));
  }
  Err("desktop VNC server did not become ready".into())
}

fn bridge(vnc: SocketAddr, capability: String) -> Result<(SocketAddr, watch::Sender<bool>), String> {
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
            Ok((stream, _)) => { let capability = capability.clone(); let capability_used = Arc::clone(&capability_used); tokio::spawn(proxy(stream, vnc, capability, capability_used)); }
            Err(_) => break,
          }
        }
      }
    });
  });
  Ok((address, shutdown))
}

async fn proxy(stream: TcpStream, vnc: SocketAddr, capability: String, capability_used: Arc<AtomicBool>) {
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
      if ws_write.send(Message::Binary(buffer[..count].to_vec().into())).await.is_err() { break; }
    }
  };
  tokio::select! { _ = client_to_vnc => {}, _ = vnc_to_client => {} }
}
