//! Viewer settings: GPU rendering, the Wayland GPU desktop and the hardware
//! video transport.
//!
//! Persisted as three choices in `$XDG_CONFIG_HOME/docker-vm/settings.json`
//! (default `~/.config`); nothing about sessions is ever written. Settings take
//! effect when a session starts. `DOCKER_VM_GPU=off`, `DOCKER_VM_WAYLAND=off`
//! and `DOCKER_VM_VIDEO=off` still override them for one launch.

use std::{fs, io::Write, os::unix::fs::OpenOptionsExt, path::PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
  /// Use it whenever the host supports it.
  #[default]
  Auto,
  Off,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
  pub gpu: Mode,
  /// The Wayland GPU desktop (with audio); off = the classic X desktop.
  pub wayland: Mode,
  pub video: Mode,
}

/// Whether this host can do GPU rendering at all, and if not, why.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HostSupport {
  pub kvm: Result<(), String>,
  /// The NVIDIA CDI device Docker exposes (`nvidia.com/gpu=N`).
  pub gpu: Result<String, String>,
}

impl HostSupport {
  /// The device to use, or the first reason GPU rendering is impossible.
  pub fn device(&self) -> Result<&str, &str> {
    self.kvm.as_ref().map_err(String::as_str)?;
    self.gpu.as_deref().map_err(String::as_str)
  }
}

/// What the settings panel shows: the saved choices, which ones the host can
/// honour (greyed out otherwise, with the reason), and launch-time overrides.
#[derive(Clone, Debug, Serialize)]
pub struct SettingsView {
  pub settings: Settings,
  /// `None` while the host is still being checked.
  pub gpu_unavailable: Option<String>,
  pub wayland_unavailable: Option<String>,
  pub video_unavailable: Option<String>,
  pub checking: bool,
  pub overrides: Vec<String>,
}

impl SettingsView {
  pub fn new(settings: Settings, host: Option<&HostSupport>) -> Self {
    let gpu_unavailable = host.and_then(|host| host.device().err().map(str::to_string));
    let video_unavailable = gpu_unavailable.clone().map(|reason| format!("needs GPU rendering: {reason}"));
    let wayland_unavailable = video_unavailable.clone();
    let overrides = ["DOCKER_VM_GPU", "DOCKER_VM_WAYLAND", "DOCKER_VM_VIDEO"].into_iter()
      .filter(|name| env_off(name))
      .map(|name| format!("{name}=off"))
      .collect();
    Self { settings, gpu_unavailable, wayland_unavailable, video_unavailable, checking: host.is_none(), overrides }
  }
}

pub fn env_off(name: &str) -> bool {
  std::env::var_os(name).is_some_and(|value| value == "off")
}

fn path() -> Option<PathBuf> {
  let base = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from)
    .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
  Some(base.join("docker-vm").join("settings.json"))
}

/// Missing or unreadable settings are the defaults (all Auto).
pub fn load() -> Settings {
  path().and_then(|path| fs::read(path).ok())
    .and_then(|bytes| serde_json::from_slice(&bytes).ok())
    .unwrap_or_default()
}

pub fn save(settings: Settings) -> Result<(), String> {
  let path = path().ok_or("no XDG_CONFIG_HOME or HOME for settings")?;
  let directory = path.parent().ok_or("settings path has no directory")?;
  fs::create_dir_all(directory).map_err(|error| format!("create {}: {error}", directory.display()))?;
  let json = serde_json::to_vec_pretty(&settings).map_err(|error| error.to_string())?;
  let temporary = path.with_extension("json.next");
  let mut file = fs::File::options().write(true).create(true).truncate(true).mode(0o600).open(&temporary)
    .map_err(|error| format!("write settings: {error}"))?;
  file.write_all(&json).and_then(|_| file.write_all(b"\n")).map_err(|error| format!("write settings: {error}"))?;
  fs::rename(&temporary, &path).map_err(|error| format!("save settings: {error}"))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn settings_default_to_auto_and_tolerate_missing_fields() {
    assert_eq!(Settings::default(), Settings { gpu: Mode::Auto, wayland: Mode::Auto, video: Mode::Auto });
    let partial: Settings = serde_json::from_str(r#"{"video":"off"}"#).unwrap();
    assert_eq!(partial, Settings { gpu: Mode::Auto, wayland: Mode::Auto, video: Mode::Off });
  }

  #[test]
  fn missing_kvm_or_gpu_greys_out_both_settings_with_the_reason() {
    let no_kvm = HostSupport { kvm: Err("KVM is not enabled".into()), gpu: Ok("nvidia.com/gpu=0".into()) };
    let view = SettingsView::new(Settings::default(), Some(&no_kvm));
    assert_eq!(view.gpu_unavailable.as_deref(), Some("KVM is not enabled"));
    assert_eq!(view.video_unavailable.as_deref(), Some("needs GPU rendering: KVM is not enabled"));
    let ready = HostSupport { kvm: Ok(()), gpu: Ok("nvidia.com/gpu=0".into()) };
    let view = SettingsView::new(Settings::default(), Some(&ready));
    assert_eq!((view.gpu_unavailable, view.video_unavailable, view.checking), (None, None, false));
    assert!(SettingsView::new(Settings::default(), None).checking);
  }
}
