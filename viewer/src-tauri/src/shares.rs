//! Optional inactive shares. Only source paths and choices are persisted;
//! archive passwords stay in memory for one mount.

use std::{fs, io::Write, os::unix::fs::{OpenOptionsExt, PermissionsExt}, path::PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind { Folder, Zip }

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedShare {
  pub source: PathBuf,
  pub kind: Kind,
  #[serde(default)]
  pub allow_changes: bool,
}

#[derive(Default, Deserialize, Serialize)]
struct ShareFile { shares: Vec<SavedShare> }

fn path() -> Result<PathBuf, String> {
  let base = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from)
    .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
    .ok_or("no XDG_CONFIG_HOME or HOME for saved shares")?;
  Ok(base.join("docker-vm").join("shares.json"))
}

pub fn load() -> Vec<SavedShare> {
  path().ok().and_then(|path| fs::read(path).ok())
    .and_then(|bytes| serde_json::from_slice::<ShareFile>(&bytes).ok())
    .map(|file| file.shares).unwrap_or_default()
}

pub fn remember(mut share: SavedShare) -> Result<(), String> {
  share.source = share.source.canonicalize().map_err(|error| format!("remember share: {error}"))?;
  if !share.source.is_absolute() { return Err("saved share needs an absolute source path".into()); }
  let mut shares = load();
  shares.retain(|saved| saved.source != share.source || saved.kind != share.kind);
  shares.push(share);
  save(&shares)
}

pub fn forget_all() -> Result<(), String> { save(&[]) }

fn save(shares: &[SavedShare]) -> Result<(), String> {
  let path = path()?;
  let directory = path.parent().ok_or("saved shares path has no directory")?;
  fs::create_dir_all(directory).map_err(|error| format!("create {}: {error}", directory.display()))?;
  let temporary = path.with_extension("json.next");
  let json = serde_json::to_vec_pretty(&ShareFile { shares: shares.to_vec() }).map_err(|error| error.to_string())?;
  let mut file = fs::File::options().write(true).create(true).truncate(true).mode(0o600).open(&temporary)
    .map_err(|error| format!("write saved shares: {error}"))?;
  file.set_permissions(fs::Permissions::from_mode(0o600)).map_err(|error| format!("secure saved shares: {error}"))?;
  file.write_all(&json).and_then(|_| file.write_all(b"\n")).map_err(|error| format!("write saved shares: {error}"))?;
  fs::rename(&temporary, &path).map_err(|error| format!("save shares: {error}"))
}

pub fn reason(share: &SavedShare, tier: crate::settings::SharingTier) -> Option<String> {
  use crate::settings::SharingTier;
  if !share.source.exists() { return Some("not found".into()); }
  match (tier, share.kind, share.allow_changes) {
    (SharingTier::Off, _, _) => Some("file sharing is off".into()),
    (_, Kind::Zip, true) => Some("ZIP archives are read-only".into()),
    (SharingTier::Encrypted, Kind::Folder, _) => Some("folders are blocked by Encrypted tier".into()),
    (SharingTier::Encrypted, Kind::Zip, _) => match crate::zipfs::Archive::open(&share.source) {
      Ok(archive) if archive.all_encrypted() => None,
      Ok(_) => Some("ZIP is not fully encrypted".into()),
      Err(error) => Some(error),
    },
    (SharingTier::Ro, Kind::Folder, true) => Some("Allow changes needs Read & write tier".into()),
    _ => None,
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::path::Path;

  #[test]
  fn saved_format_tolerates_unknown_fields_and_never_serializes_secrets() {
    let input = r#"{"shares":[{"source":"/tmp/docs","kind":"folder","allow_changes":true,"password":"secret"}],"future":42}"#;
    let file: ShareFile = serde_json::from_str(input).unwrap();
    assert_eq!(file.shares[0].source, Path::new("/tmp/docs"));
    assert!(file.shares[0].allow_changes);
    let output = serde_json::to_string(&file).unwrap();
    assert!(!output.contains("password"));
    assert!(!output.contains("secret"));
    assert!(!output.contains("future"));
  }
}
