//! Read-only FUSE view of a ZIP archive, decrypted on demand.
//!
//! Nothing is extracted ahead of time: a read decrypts and decompresses only as
//! far as the requested range, and plaintext is held only in RAM. FUSE's default
//! policy admits only the mounting user and group, so neither host root nor
//! other container users can read the contents.

use std::{
  collections::{BTreeMap, HashMap},
  ffi::{OsStr, OsString},
  fs::File,
  io::{self, Read, Seek, SeekFrom},
  os::unix::fs::FileExt,
  path::{Component, Path},
  sync::{Arc, Mutex, atomic::{AtomicU64, Ordering}, mpsc},
  thread,
  time::{Duration, SystemTime, UNIX_EPOCH},
};

use fuser::{
  BackgroundSession, Config, Errno, FileAttr, FileHandle, FileType, Filesystem, FopenFlags, Generation, INodeNo,
  LockOwner, MountOption, OpenFlags, ReplyAttr, ReplyData, ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyOpen,
  ReplyStatfs, Request,
};
use zip::{CompressionMethod, DateTime, ZipArchive, result::ZipError};

// Archive contents cannot change while mounted.
const TTL: Duration = Duration::from_secs(3600);
// A wrong ZipCrypto password passes one entry's check 1 time in 256.
const PASSWORD_SAMPLES: usize = 4;

/// An archive password, zeroed when dropped.
pub struct Password(Vec<u8>);

impl Password {
  /// Takes one line of input without its line ending.
  pub fn from_line(mut bytes: Vec<u8>) -> Self {
    if bytes.last() == Some(&b'\n') { bytes.pop(); }
    if bytes.last() == Some(&b'\r') { bytes.pop(); }
    Self(bytes)
  }

  pub fn is_empty(&self) -> bool { self.0.is_empty() }

  pub fn duplicate(&self) -> Self { Self(self.0.clone()) }
}

impl Drop for Password {
  fn drop(&mut self) { self.0.fill(0); }
}

/// A positioned reader over a shared file descriptor, so every open entry can
/// seek independently without reopening or re-parsing the archive.
#[derive(Clone)]
struct SharedFile {
  file: Arc<File>,
  position: u64,
}

impl Read for SharedFile {
  fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
    let count = self.file.read_at(buffer, self.position)?;
    self.position += count as u64;
    Ok(count)
  }
}

impl Seek for SharedFile {
  fn seek(&mut self, target: SeekFrom) -> io::Result<u64> {
    let (base, delta) = match target {
      SeekFrom::Start(offset) => (offset, 0),
      SeekFrom::End(delta) => (self.file.metadata()?.len(), delta),
      SeekFrom::Current(delta) => (self.position, delta),
    };
    self.position = base.checked_add_signed(delta).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek out of range"))?;
    Ok(self.position)
  }
}

struct Node {
  parent: usize,
  mtime: SystemTime,
  executable: bool,
  kind: Kind,
}

enum Kind {
  Directory(BTreeMap<OsString, usize>),
  /// `direct` is the data offset of a stored, unencrypted entry.
  File { entry: usize, size: u64, direct: Option<u64> },
}

impl Node {
  fn directory(parent: usize) -> Self {
    Self { parent, mtime: UNIX_EPOCH, executable: true, kind: Kind::Directory(BTreeMap::new()) }
  }

  fn children(&self) -> Option<&BTreeMap<OsString, usize>> {
    match &self.kind { Kind::Directory(children) => Some(children), Kind::File { .. } => None }
  }

  fn children_mut(&mut self) -> Option<&mut BTreeMap<OsString, usize>> {
    match &mut self.kind { Kind::Directory(children) => Some(children), Kind::File { .. } => None }
  }
}

pub struct Archive {
  zip: ZipArchive<SharedFile>,
  file: Arc<File>,
  nodes: Vec<Node>,
  encrypted: Vec<usize>,
}

impl Archive {
  /// Reads only the archive's directory; entry data is untouched until read.
  pub fn open(path: &Path) -> Result<Self, String> {
    let describe = |error: &dyn std::fmt::Display| format!("{}: {error}", path.display());
    let file = Arc::new(File::open(path).map_err(|error| describe(&error))?);
    if !file.metadata().map_err(|error| describe(&error))?.is_file() {
      return Err(format!("{} is not a regular file", path.display()));
    }
    let mut zip = ZipArchive::new(SharedFile { file: Arc::clone(&file), position: 0 }).map_err(|error| describe(&error))?;
    let (mut nodes, mut encrypted) = (vec![Node::directory(0)], Vec::new());
    for index in 0..zip.len() {
      let entry = zip.by_index_raw(index).map_err(|error| describe(&error))?;
      // `enclosed_name` rejects absolute and escaping paths. Symlinks are hidden.
      let Some(name) = entry.enclosed_name().filter(|_| !entry.is_symlink()) else { continue };
      let parts: Vec<&OsStr> = name.components().filter_map(|part| match part { Component::Normal(part) => Some(part), _ => None }).collect();
      let Some((leaf, parents)) = parts.split_last() else { continue };
      let mtime = entry.last_modified().and_then(unix_time).unwrap_or(UNIX_EPOCH);
      let executable = entry.unix_mode().is_some_and(|mode| mode & 0o111 != 0);
      let file = (!entry.is_dir()).then(|| Kind::File {
        entry: index,
        size: entry.size(),
        direct: if entry.encrypted() || entry.compression() != CompressionMethod::Stored { None } else { entry.data_start() },
      });
      if file.is_some() && entry.encrypted() {
        encrypted.push(index);
      }
      let Some(parent) = parents.iter().try_fold(0, |parent, part| child_directory(&mut nodes, parent, part)) else { continue };
      match file {
        None => {
          if let Some(directory) = child_directory(&mut nodes, parent, leaf) {
            nodes[directory].mtime = mtime;
          }
        }
        Some(kind) => {
          let node = nodes.len();
          let Some(children) = nodes[parent].children_mut() else { continue };
          // The first entry for a path wins; later duplicates are hidden.
          if children.contains_key(*leaf) { continue; }
          children.insert(leaf.to_os_string(), node);
          nodes.push(Node { parent, mtime, executable, kind });
        }
      }
    }
    Ok(Self { zip, file, nodes, encrypted })
  }

  pub fn is_encrypted(&self) -> bool { !self.encrypted.is_empty() }

  pub fn check_password(&mut self, password: &Password) -> Result<bool, String> {
    for &index in self.encrypted.iter().take(PASSWORD_SAMPLES) {
      match self.zip.by_index_decrypt(index, &password.0) {
        Ok(_) => {}
        Err(ZipError::InvalidPassword) => return Ok(false),
        Err(error) => return Err(format!("decrypt archive entry: {error}")),
      }
    }
    Ok(true)
  }
}

/// Returns the directory `name` under `parent`, creating it if needed, or
/// `None` when a file already occupies that name.
fn child_directory(nodes: &mut Vec<Node>, parent: usize, name: &OsStr) -> Option<usize> {
  match nodes[parent].children()?.get(name).copied() {
    Some(existing) => nodes[existing].children().map(|_| existing),
    None => {
      let directory = nodes.len();
      nodes[parent].children_mut()?.insert(name.to_os_string(), directory);
      nodes.push(Node::directory(parent));
      Some(directory)
    }
  }
}

/// ZIP timestamps carry no zone; they are presented as UTC.
fn unix_time(time: DateTime) -> Option<SystemTime> {
  let (month, day) = (i64::from(time.month()), i64::from(time.day()));
  let year = i64::from(time.year()) - i64::from(month <= 2);
  let era = year.div_euclid(400);
  let year_of_era = year - era * 400;
  let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
  let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
  let days = era * 146_097 + day_of_era - 719_468;
  let seconds = days * 86_400 + i64::from(time.hour()) * 3600 + i64::from(time.minute()) * 60 + i64::from(time.second());
  u64::try_from(seconds).ok().map(|seconds| UNIX_EPOCH + Duration::from_secs(seconds))
}

pub struct ZipFs {
  archive: Archive,
  password: Option<Arc<Password>>,
  uid: u32,
  gid: u32,
  handles: Mutex<HashMap<u64, Handle>>,
  next_handle: AtomicU64,
}

enum Handle {
  Direct { offset: u64, size: u64 },
  Stream(mpsc::Sender<ReadRequest>),
}

struct ReadRequest {
  offset: u64,
  size: u32,
  reply: mpsc::Sender<io::Result<Vec<u8>>>,
}

impl ZipFs {
  pub fn new(archive: Archive, password: Option<Password>, uid: u32, gid: u32) -> Self {
    Self { archive, password: password.map(Arc::new), uid, gid, handles: Mutex::new(HashMap::new()), next_handle: AtomicU64::new(1) }
  }

  fn index(&self, ino: INodeNo) -> Option<usize> {
    let index = usize::try_from(ino.0.checked_sub(1)?).ok()?;
    (index < self.archive.nodes.len()).then_some(index)
  }

  fn kind(&self, index: usize) -> FileType {
    match self.archive.nodes[index].kind { Kind::Directory(_) => FileType::Directory, Kind::File { .. } => FileType::RegularFile }
  }

  fn attr(&self, index: usize) -> FileAttr {
    let node = &self.archive.nodes[index];
    let (size, nlink) = match &node.kind { Kind::Directory(_) => (0, 2), Kind::File { size, .. } => (*size, 1) };
    FileAttr {
      ino: INodeNo(index as u64 + 1),
      size,
      blocks: size.div_ceil(512),
      atime: node.mtime,
      mtime: node.mtime,
      ctime: node.mtime,
      crtime: node.mtime,
      kind: self.kind(index),
      perm: if node.executable { 0o555 } else { 0o444 },
      nlink,
      uid: self.uid,
      gid: self.gid,
      rdev: 0,
      blksize: 4096,
      flags: 0,
    }
  }

  fn open_handle(&self, index: usize) -> Result<u64, Errno> {
    let &Kind::File { entry, size, direct } = &self.archive.nodes[index].kind else { return Err(Errno::EISDIR) };
    let handle = match direct {
      Some(offset) => Handle::Direct { offset, size },
      None => {
        let (sender, requests) = mpsc::channel();
        let (zip, password) = (self.archive.zip.clone(), self.password.clone());
        thread::Builder::new()
          .name("zip-entry".into())
          .spawn(move || stream_entry(zip, entry, password, requests))
          .map_err(|_| Errno::EIO)?;
        Handle::Stream(sender)
      }
    };
    let id = self.next_handle.fetch_add(1, Ordering::Relaxed);
    self.handles.lock().map_err(|_| Errno::EIO)?.insert(id, handle);
    Ok(id)
  }

  fn read_handle(&self, handle: u64, offset: u64, size: u32) -> Result<Vec<u8>, Errno> {
    let stream = match self.handles.lock().map_err(|_| Errno::EIO)?.get(&handle).ok_or(Errno::EBADF)? {
      &Handle::Direct { offset: start, size: length } => {
        let mut buffer = vec![0; u64::from(size).min(length.saturating_sub(offset)) as usize];
        self.archive.file.read_exact_at(&mut buffer, start + offset).map_err(|_| Errno::EIO)?;
        return Ok(buffer);
      }
      Handle::Stream(sender) => sender.clone(),
    };
    let (reply, response) = mpsc::channel();
    stream.send(ReadRequest { offset, size, reply }).map_err(|_| Errno::EIO)?;
    response.recv().map_err(|_| Errno::EIO)?.map_err(|_| Errno::EIO)
  }

  fn release_handle(&self, handle: u64) {
    if let Ok(mut handles) = self.handles.lock() {
      handles.remove(&handle);
    }
  }
}

/// Serves one open file. A `ZipFile` borrows its archive, so each open file
/// owns a clone (sharing parsed metadata) on its own thread; the thread exits
/// when the handle is released. Seeking backwards restarts the entry stream.
fn stream_entry(mut zip: ZipArchive<SharedFile>, index: usize, password: Option<Arc<Password>>, requests: mpsc::Receiver<ReadRequest>) {
  let mut pending = None;
  loop {
    let Some(mut request) = pending.take().or_else(|| requests.recv().ok()) else { return };
    let opened = match &password {
      Some(password) => zip.by_index_decrypt(index, &password.0),
      None => zip.by_index(index),
    };
    let mut entry = match opened {
      Ok(entry) => entry,
      Err(error) => {
        let _ = request.reply.send(Err(io::Error::other(error)));
        continue;
      }
    };
    let (total, mut position) = (entry.size(), 0);
    loop {
      if request.offset < position {
        pending = Some(request);
        break;
      }
      let result = read_range(&mut entry, &mut position, total, request.offset, request.size);
      let failed = result.is_err();
      let _ = request.reply.send(result);
      if failed { break; }
      let Ok(next) = requests.recv() else { return };
      request = next;
    }
  }
}

fn read_range(entry: &mut impl Read, position: &mut u64, total: u64, offset: u64, size: u32) -> io::Result<Vec<u8>> {
  *position += io::copy(&mut (&mut *entry).take(offset - *position), &mut io::sink())?;
  let mut buffer = Vec::new();
  if *position == offset {
    *position += (&mut *entry).take(u64::from(size)).read_to_end(&mut buffer)? as u64;
  }
  // The CRC or AES authentication code is checked when the decoder reaches the
  // end of the entry, and the kernel never reads past the size reported.
  if *position == total && entry.read(&mut [0u8])? != 0 {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "entry is longer than its recorded size"));
  }
  Ok(buffer)
}

impl Filesystem for ZipFs {
  fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
    let child = self.index(parent).and_then(|parent| self.archive.nodes[parent].children()?.get(name).copied());
    match child {
      Some(index) => reply.entry(&TTL, &self.attr(index), Generation(0)),
      None => reply.error(Errno::ENOENT),
    }
  }

  fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
    match self.index(ino) {
      Some(index) => reply.attr(&TTL, &self.attr(index)),
      None => reply.error(Errno::ENOENT),
    }
  }

  fn open(&self, _req: &Request, ino: INodeNo, _flags: OpenFlags, reply: ReplyOpen) {
    match self.index(ino).ok_or(Errno::ENOENT).and_then(|index| self.open_handle(index)) {
      Ok(handle) => reply.opened(FileHandle(handle), FopenFlags::FOPEN_KEEP_CACHE),
      Err(error) => reply.error(error),
    }
  }

  fn read(&self, _req: &Request, _ino: INodeNo, fh: FileHandle, offset: u64, size: u32, _flags: OpenFlags, _lock_owner: Option<LockOwner>, reply: ReplyData) {
    match self.read_handle(fh.0, offset, size) {
      Ok(data) => reply.data(&data),
      Err(error) => reply.error(error),
    }
  }

  fn release(&self, _req: &Request, _ino: INodeNo, fh: FileHandle, _flags: OpenFlags, _lock_owner: Option<LockOwner>, _flush: bool, reply: ReplyEmpty) {
    self.release_handle(fh.0);
    reply.ok();
  }

  fn readdir(&self, _req: &Request, ino: INodeNo, _fh: FileHandle, offset: u64, mut reply: ReplyDirectory) {
    let Some(index) = self.index(ino) else { return reply.error(Errno::ENOENT) };
    let node = &self.archive.nodes[index];
    let Some(children) = node.children() else { return reply.error(Errno::ENOTDIR) };
    let special = [(index, OsStr::new(".")), (node.parent, OsStr::new(".."))];
    let entries = special.into_iter().chain(children.iter().map(|(name, &child)| (child, name.as_os_str())));
    for (position, (child, name)) in entries.enumerate().skip(offset as usize) {
      if reply.add(INodeNo(child as u64 + 1), position as u64 + 1, self.kind(child), name) {
        break;
      }
    }
    reply.ok();
  }

  fn statfs(&self, _req: &Request, _ino: INodeNo, reply: ReplyStatfs) {
    reply.statfs(0, 0, 0, self.archive.nodes.len() as u64, 0, 4096, 255, 4096);
  }
}

/// Mounts with FUSE's default owner-only access: no other user, root included,
/// may enter. Dropping the returned session unmounts it.
pub fn mount(filesystem: ZipFs, mountpoint: &Path) -> Result<BackgroundSession, String> {
  let mut config = Config::default();
  config.mount_options = vec![
    MountOption::RO,
    MountOption::NoDev,
    MountOption::NoSuid,
    MountOption::DefaultPermissions,
    MountOption::FSName("docker-vm-zip".into()),
    MountOption::Subtype("zip".into()),
  ];
  fuser::spawn_mount(filesystem, mountpoint, &config).map_err(|error| format!("mount archive at {}: {error}", mountpoint.display()))
}

#[cfg(test)]
mod tests {
  use std::io::{Cursor, Write};

  use zip::{AesMode, ZipWriter, unstable::write::FileOptionsExt, write::SimpleFileOptions};

  use super::*;

  const PASSWORD: &str = "correct horse";

  struct TempZip(std::path::PathBuf);

  impl Drop for TempZip {
    fn drop(&mut self) { let _ = std::fs::remove_file(&self.0); }
  }

  fn temp_zip(bytes: &[u8]) -> TempZip {
    let path = std::env::temp_dir().join(format!("docker-vm-zipfs-{}.zip", uuid::Uuid::new_v4().simple()));
    std::fs::write(&path, bytes).unwrap();
    TempZip(path)
  }

  fn password() -> Password { Password::from_line(format!("{PASSWORD}\n").into_bytes()) }

  fn sample(len: usize) -> Vec<u8> {
    let mut state = 0x2545_f491_u32;
    (0..len).map(|_| { state ^= state << 13; state ^= state >> 17; state ^= state << 5; b'a' + (state % 7) as u8 }).collect()
  }

  fn deflated() -> SimpleFileOptions { SimpleFileOptions::default().compression_method(CompressionMethod::Deflated) }

  fn stored() -> SimpleFileOptions { SimpleFileOptions::default().compression_method(CompressionMethod::Stored) }

  fn fixture() -> Vec<u8> {
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    zip.add_directory("docs/", stored()).unwrap();
    zip.start_file("docs/readme.txt", deflated().with_aes_encryption(AesMode::Aes256, PASSWORD)).unwrap();
    zip.write_all(b"hello secret\n").unwrap();
    zip.start_file("big.bin", deflated().with_aes_encryption(AesMode::Aes256, PASSWORD)).unwrap();
    zip.write_all(&sample(300_000)).unwrap();
    zip.start_file("legacy.txt", deflated().with_deprecated_encryption(PASSWORD.as_bytes()).unwrap()).unwrap();
    zip.write_all(b"zipcrypto\n").unwrap();
    zip.start_file("plain.bin", stored()).unwrap();
    zip.write_all(&sample(100_000)).unwrap();
    zip.start_file("tool.sh", stored().unix_permissions(0o755)).unwrap();
    zip.write_all(b"#!/bin/sh\n").unwrap();
    zip.start_file("../escape.txt", stored()).unwrap();
    zip.write_all(b"outside\n").unwrap();
    zip.finish().unwrap().into_inner()
  }

  fn names(fs: &ZipFs, index: usize) -> Vec<String> {
    fs.archive.nodes[index].children().unwrap().keys().map(|name| name.to_string_lossy().into_owned()).collect()
  }

  fn lookup(fs: &ZipFs, path: &str) -> usize {
    path.split('/').fold(0, |parent, part| fs.archive.nodes[parent].children().unwrap()[OsStr::new(part)])
  }

  fn read_all(fs: &ZipFs, handle: u64, chunk: u32) -> Result<Vec<u8>, Errno> {
    let mut data = Vec::new();
    loop {
      let part = fs.read_handle(handle, data.len() as u64, chunk)?;
      if part.is_empty() { return Ok(data); }
      data.extend(part);
    }
  }

  #[test]
  fn exposes_safe_entries_as_a_read_only_tree() {
    let zip = temp_zip(&fixture());
    let archive = Archive::open(&zip.0).unwrap();
    assert!(archive.is_encrypted());
    let fs = ZipFs::new(archive, None, 1000, 100);
    assert_eq!(names(&fs, 0), ["big.bin", "docs", "legacy.txt", "plain.bin", "tool.sh"]);
    assert_eq!(names(&fs, lookup(&fs, "docs")), ["readme.txt"]);
    let readme = fs.attr(lookup(&fs, "docs/readme.txt"));
    assert_eq!((readme.kind, readme.size, readme.perm, readme.uid, readme.gid), (FileType::RegularFile, 13, 0o444, 1000, 100));
    assert_eq!(fs.attr(lookup(&fs, "tool.sh")).perm, 0o555);
    assert_eq!(fs.attr(0).kind, FileType::Directory);
    assert!(matches!(fs.archive.nodes[lookup(&fs, "plain.bin")].kind, Kind::File { direct: Some(_), .. }));
    assert!(matches!(fs.archive.nodes[lookup(&fs, "big.bin")].kind, Kind::File { direct: None, .. }));
  }

  #[test]
  fn verifies_the_password_before_mounting() {
    let zip = temp_zip(&fixture());
    let mut archive = Archive::open(&zip.0).unwrap();
    assert!(!archive.check_password(&Password::from_line(b"wrong\n".to_vec())).unwrap());
    assert!(archive.check_password(&password()).unwrap());
  }

  #[test]
  fn decrypts_entries_on_demand_with_random_access() {
    let zip = temp_zip(&fixture());
    let fs = ZipFs::new(Archive::open(&zip.0).unwrap(), Some(password()), 1000, 100);
    let expected = sample(300_000);
    let big = fs.open_handle(lookup(&fs, "big.bin")).unwrap();
    assert_eq!(read_all(&fs, big, 65_536).unwrap(), expected);
    assert_eq!(fs.read_handle(big, 10, 100).unwrap(), &expected[10..110]);
    assert_eq!(fs.read_handle(big, 250_000, 4096).unwrap(), &expected[250_000..254_096]);
    assert_eq!(fs.read_handle(big, 299_990, 4096).unwrap(), &expected[299_990..]);
    assert!(fs.read_handle(big, 400_000, 4096).unwrap().is_empty());
    fs.release_handle(big);
    assert!(fs.read_handle(big, 0, 1).is_err());

    let legacy = fs.open_handle(lookup(&fs, "legacy.txt")).unwrap();
    assert_eq!(read_all(&fs, legacy, 4).unwrap(), b"zipcrypto\n");
    let plain = fs.open_handle(lookup(&fs, "plain.bin")).unwrap();
    assert_eq!(read_all(&fs, plain, 4096).unwrap(), sample(100_000));
    assert_eq!(fs.read_handle(plain, 99_000, 4096).unwrap(), &sample(100_000)[99_000..]);
  }

  #[test]
  fn refuses_encrypted_reads_without_a_password() {
    let zip = temp_zip(&fixture());
    let fs = ZipFs::new(Archive::open(&zip.0).unwrap(), None, 1000, 100);
    let readme = fs.open_handle(lookup(&fs, "docs/readme.txt")).unwrap();
    assert!(fs.read_handle(readme, 0, 4096).is_err());
    assert!(fs.open_handle(lookup(&fs, "docs")).is_err());
  }

  #[test]
  fn reports_tampered_ciphertext_at_the_end_of_the_entry() {
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file("data.bin", stored().with_aes_encryption(AesMode::Aes256, PASSWORD)).unwrap();
    zip.write_all(&sample(10_000)).unwrap();
    let mut bytes = zip.finish().unwrap().into_inner();
    let start = ZipArchive::new(Cursor::new(&bytes)).unwrap().by_index_raw(0).unwrap().data_start().unwrap() as usize;
    // Skip the AES-256 salt (16 bytes) and password verifier (2 bytes).
    bytes[start + 18 + 5000] ^= 1;
    let file = temp_zip(&bytes);
    let mut archive = Archive::open(&file.0).unwrap();
    assert!(archive.check_password(&password()).unwrap());
    let fs = ZipFs::new(archive, Some(password()), 1000, 100);
    let handle = fs.open_handle(lookup(&fs, "data.bin")).unwrap();
    assert_eq!(fs.read_handle(handle, 0, 4096).unwrap(), &sample(10_000)[..4096]);
    assert!(read_all(&fs, handle, 4096).is_err());
  }
}
