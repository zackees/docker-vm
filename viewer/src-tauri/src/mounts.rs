//! Command-line options for read-only archive mounts.
//!
//! `--mount` follows `docker run --mount`: comma-separated `key=value` fields,
//! with CSV double quotes around values that contain commas. Passwords are
//! never accepted as arguments, which are visible in `ps` and shell history.

use std::{ffi::OsString, path::PathBuf};

pub const USAGE: &str = "\
Usage: docker-vm-viewer [--mount type=zip,source=ARCHIVE,target=PATH[,readonly][,password-file=FILE]]...
                        [--password-stdin]

Options:
  --mount SPEC        Expose a ZIP archive read-only inside the desktop; repeatable.
                        type=zip                      required
                        source=ARCHIVE (src)          host path of the archive
                        target=PATH (dst, destination) under /home/desktop, /mnt or /media
                        readonly (ro)                 optional; mounts are always read-only
                        password-file=FILE            password file, mode 0600
  --password-stdin    Read the password for encrypted archives from standard input.
  -h, --help          Show this help.

Without password-file or --password-stdin, encrypted archives prompt for a password.
Archive contents are decrypted on demand and never extracted.
";

const TARGET_ROOTS: [&str; 3] = ["/home/desktop", "/mnt", "/media"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZipMount {
  pub source: PathBuf,
  pub target: String,
  pub password_file: Option<PathBuf>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Options {
  pub mounts: Vec<ZipMount>,
  pub password_stdin: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Invocation {
  Run(Options),
  Help,
}

pub fn parse_args(args: impl IntoIterator<Item = OsString>) -> Result<Invocation, String> {
  let mut options = Options::default();
  let mut args = args.into_iter();
  while let Some(arg) = args.next() {
    let arg = arg.into_string().map_err(|arg| format!("argument is not valid UTF-8: {arg:?}"))?;
    match arg.as_str() {
      "-h" | "--help" => return Ok(Invocation::Help),
      "--password-stdin" => options.password_stdin = true,
      "--mount" => {
        let spec = args.next().ok_or("--mount requires a value")?;
        options.mounts.push(parse_mount(&spec.into_string().map_err(|_| "--mount value is not valid UTF-8")?)?);
      }
      _ => match arg.strip_prefix("--mount=") {
        Some(spec) => options.mounts.push(parse_mount(spec)?),
        None => return Err(format!("unknown argument {arg:?}")),
      },
    }
  }
  if options.password_stdin && options.mounts.is_empty() {
    return Err("--password-stdin requires --mount".into());
  }
  check_overlaps(&options.mounts)?;
  Ok(Invocation::Run(options))
}

fn parse_mount(spec: &str) -> Result<ZipMount, String> {
  let (mut kind, mut source, mut target, mut password_file, mut readonly) = (None, None, None, None, false);
  for field in split_fields(spec)? {
    let (key, value) = match field.split_once('=') {
      Some((key, value)) => (key, Some(value)),
      None => (field.as_str(), None),
    };
    match key {
      "type" => set(&mut kind, key, value)?,
      "source" | "src" => set(&mut source, key, value)?,
      "target" | "destination" | "dst" => set(&mut target, key, value)?,
      "password-file" => set(&mut password_file, key, value)?,
      "readonly" | "ro" => {
        if std::mem::replace(&mut readonly, true) {
          return Err(format!("--mount field {key} given more than once"));
        }
        if let Some(value) = value.filter(|value| !matches!(*value, "true" | "1")) {
          return Err(format!("archive mounts are always read-only; {key}={value} is not supported"));
        }
      }
      "rw" => return Err("archive mounts are always read-only; rw is not supported".into()),
      _ => return Err(format!("unknown --mount field {key:?}")),
    }
  }
  match kind.as_deref() {
    Some("zip") => {}
    Some(other) => return Err(format!("unsupported mount type {other:?}; only type=zip is supported")),
    None => return Err("--mount requires type=zip".into()),
  }
  let source = source.ok_or("--mount requires source=ARCHIVE")?;
  let target = validate_target(&target.ok_or("--mount requires target=PATH")?)?;
  Ok(ZipMount { source: PathBuf::from(source), target, password_file: password_file.map(PathBuf::from) })
}

fn set(slot: &mut Option<String>, key: &str, value: Option<&str>) -> Result<(), String> {
  let value = value.filter(|value| !value.is_empty()).ok_or_else(|| format!("--mount field {key} requires a value"))?;
  match slot.replace(value.to_string()) {
    Some(_) => Err(format!("--mount field {key} given more than once")),
    None => Ok(()),
  }
}

fn split_fields(spec: &str) -> Result<Vec<String>, String> {
  let (mut fields, mut field, mut quoted) = (Vec::new(), String::new(), false);
  let mut chars = spec.chars().peekable();
  while let Some(char) = chars.next() {
    match char {
      '"' if quoted && chars.peek() == Some(&'"') => { chars.next(); field.push('"'); }
      '"' => quoted = !quoted,
      ',' if !quoted => fields.push(std::mem::take(&mut field)),
      _ => field.push(char),
    }
  }
  if quoted {
    return Err("unterminated quote in --mount".into());
  }
  fields.push(field);
  if fields.iter().any(String::is_empty) {
    return Err("empty field in --mount".into());
  }
  Ok(fields)
}

fn validate_target(target: &str) -> Result<String, String> {
  let invalid = |reason: &str| format!("invalid mount target {target:?}: {reason}");
  if !target.starts_with('/') {
    return Err(invalid("must be an absolute container path"));
  }
  let target = target.trim_end_matches('/');
  if target.len() > 255 {
    return Err(invalid("is too long"));
  }
  for component in target.split('/').skip(1) {
    if matches!(component, "" | "." | "..") {
      return Err(invalid("must not contain empty, '.' or '..' components"));
    }
    if !component.chars().all(|char| char.is_ascii_alphanumeric() || "._-+@".contains(char)) {
      return Err(invalid("may only contain ASCII letters, digits and . _ - + @"));
    }
  }
  let inside = |root: &&str| target.strip_prefix(root).is_some_and(|rest| rest.len() > 1 && rest.starts_with('/'));
  if !TARGET_ROOTS.iter().any(inside) {
    return Err(invalid("must be inside /home/desktop, /mnt or /media"));
  }
  Ok(target.to_string())
}

fn check_overlaps(mounts: &[ZipMount]) -> Result<(), String> {
  let contains = |outer: &str, inner: &str| inner.strip_prefix(outer).is_some_and(|rest| rest.is_empty() || rest.starts_with('/'));
  for (index, first) in mounts.iter().enumerate() {
    for second in &mounts[index + 1..] {
      if contains(&first.target, &second.target) || contains(&second.target, &first.target) {
        return Err(format!("mount targets {:?} and {:?} overlap", first.target, second.target));
      }
    }
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  fn parse(args: &[&str]) -> Result<Invocation, String> {
    parse_args(args.iter().map(OsString::from))
  }

  fn mounts(args: &[&str]) -> Vec<ZipMount> {
    match parse(args) {
      Ok(Invocation::Run(options)) => options.mounts,
      other => panic!("unexpected parse result: {other:?}"),
    }
  }

  fn error(args: &[&str]) -> String {
    parse(args).expect_err("arguments should be rejected")
  }

  #[test]
  fn parses_docker_style_mounts() {
    let parsed = mounts(&[
      "--mount", "type=zip,source=./assets.zip,target=/home/desktop/assets,readonly",
      "--mount=type=zip,src=/data/b.zip,dst=/mnt/b/,ro=true,password-file=/run/user/1000/pw",
      "--mount", "destination=/media/c,type=zip,src=\"odd,name.zip\"",
    ]);
    assert_eq!(parsed, vec![
      ZipMount { source: "./assets.zip".into(), target: "/home/desktop/assets".into(), password_file: None },
      ZipMount { source: "/data/b.zip".into(), target: "/mnt/b".into(), password_file: Some("/run/user/1000/pw".into()) },
      ZipMount { source: "odd,name.zip".into(), target: "/media/c".into(), password_file: None },
    ]);
  }

  #[test]
  fn defaults_to_no_mounts_and_supports_help() {
    assert_eq!(parse(&[]), Ok(Invocation::Run(Options::default())));
    assert_eq!(parse(&["--mount", "bogus", "--help"]).map_err(|_| ()), Err(()));
    assert_eq!(parse(&["-h"]), Ok(Invocation::Help));
    let Ok(Invocation::Run(options)) = parse(&["--password-stdin", "--mount", "type=zip,src=a.zip,dst=/mnt/a"]) else { panic!() };
    assert!(options.password_stdin);
  }

  #[test]
  fn rejects_writable_or_malformed_mounts() {
    assert!(error(&["--mount", "type=zip,src=a.zip,dst=/mnt/a,rw"]).contains("read-only"));
    assert!(error(&["--mount", "type=zip,src=a.zip,dst=/mnt/a,readonly=false"]).contains("read-only"));
    assert!(error(&["--mount", "type=bind,src=a.zip,dst=/mnt/a"]).contains("only type=zip"));
    assert!(error(&["--mount", "src=a.zip,dst=/mnt/a"]).contains("type=zip"));
    assert!(error(&["--mount", "type=zip,dst=/mnt/a"]).contains("source"));
    assert!(error(&["--mount", "type=zip,src=a.zip"]).contains("target"));
    assert!(error(&["--mount", "type=zip,src=a.zip,source=b.zip,dst=/mnt/a"]).contains("more than once"));
    assert!(error(&["--mount", "type=zip,src=,dst=/mnt/a"]).contains("requires a value"));
    assert!(error(&["--mount", "type=zip,,src=a.zip,dst=/mnt/a"]).contains("empty field"));
    assert!(error(&["--mount", "type=zip,src=\"a.zip,dst=/mnt/a"]).contains("unterminated"));
    assert!(error(&["--mount", "type=zip,src=a.zip,dst=/mnt/a,password=hunter2"]).contains("unknown --mount field"));
    assert!(error(&["--mount"]).contains("requires a value"));
    assert!(error(&["--password", "hunter2"]).contains("unknown argument"));
    assert!(error(&["--password-stdin"]).contains("requires --mount"));
  }

  #[test]
  fn restricts_targets_to_desktop_data_locations() {
    for target in ["mnt/a", "/", "/mnt", "/mnt/", "/mntx/a", "/etc/passwd", "/run/session/x", "/home/desktop", "/mnt/../etc", "/mnt/./a", "/mnt//a", "/mnt/a b", "/mnt/$HOME"] {
      let spec = format!("type=zip,src=a.zip,dst={target}");
      assert!(error(&["--mount", &spec]).contains("invalid mount target"), "{target} should be rejected");
    }
  }

  #[test]
  fn rejects_overlapping_targets() {
    assert!(error(&["--mount", "type=zip,src=a.zip,dst=/mnt/a", "--mount", "type=zip,src=b.zip,dst=/mnt/a/b"]).contains("overlap"));
    assert!(error(&["--mount", "type=zip,src=a.zip,dst=/mnt/a", "--mount", "type=zip,src=b.zip,dst=/mnt/a/"]).contains("overlap"));
    assert_eq!(mounts(&["--mount", "type=zip,src=a.zip,dst=/mnt/a", "--mount", "type=zip,src=b.zip,dst=/mnt/ab"]).len(), 2);
  }
}
