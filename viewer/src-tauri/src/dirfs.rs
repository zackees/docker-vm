//! Root-confined, read-only FUSE view of a host directory.
//!
//! Every component is opened relative to an already-open directory with
//! O_NOFOLLOW. A rename or symlink swap can change which entry is seen, but
//! cannot redirect a lookup or read outside the selected root.

use std::{
  collections::HashMap,
  ffi::{CStr, CString, OsStr, OsString},
  fs::{self, File},
  io,
  os::{fd::{AsRawFd, FromRawFd, IntoRawFd}, unix::{ffi::OsStrExt, fs::{FileExt, MetadataExt}}},
  path::Path,
  sync::{Mutex, atomic::{AtomicU64, Ordering}},
  time::{Duration, SystemTime, UNIX_EPOCH},
};

use fuser::{
  BackgroundSession, BsdFileFlags, Config, Errno, FileAttr, FileHandle, FileType, Filesystem,
  FopenFlags, Generation, INodeNo, LockOwner, MountOption, OpenFlags, RenameFlags,
  ReplyAttr, ReplyCreate, ReplyData, ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyOpen,
  ReplyWrite, Request, TimeOrNow, WriteFlags,
};

const TTL: Duration = Duration::from_secs(1);

struct Node { parent: usize, name: OsString }

struct Nodes {
  entries: Vec<Node>,
  by_name: HashMap<(usize, OsString), usize>,
}

pub struct DirFs {
  root: File,
  nodes: Mutex<Nodes>,
  handles: Mutex<HashMap<u64, File>>,
  next_handle: AtomicU64,
  uid: u32,
  gid: u32,
}

impl DirFs {
  /// `writable` is reserved for the writable sibling implementation.
  pub fn new(root: &Path, writable: bool) -> Result<Self, String> {
    if writable { return Err("writable directory sharing is not available yet".into()); }
    let path = CString::new(root.as_os_str().as_bytes()).map_err(|_| "directory path contains NUL")?;
    let fd = unsafe { libc::open(path.as_ptr(), libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
    if fd < 0 { return Err(format!("open share directory {}: {}", root.display(), io::Error::last_os_error())); }
    Ok(Self {
      root: unsafe { File::from_raw_fd(fd) },
      nodes: Mutex::new(Nodes { entries: vec![Node { parent: 0, name: OsString::new() }], by_name: HashMap::new() }),
      handles: Mutex::new(HashMap::new()),
      next_handle: AtomicU64::new(1),
      uid: unsafe { libc::geteuid() },
      gid: unsafe { libc::getegid() },
    })
  }

  fn components(&self, index: usize) -> Result<Vec<OsString>, Errno> {
    let nodes = self.nodes.lock().map_err(|_| Errno::EIO)?;
    if index >= nodes.entries.len() { return Err(Errno::ENOENT); }
    let mut path = Vec::new();
    let mut current = index;
    while current != 0 {
      let node = &nodes.entries[current];
      path.push(node.name.clone());
      current = node.parent;
    }
    path.reverse();
    Ok(path)
  }

  fn open_node(&self, index: usize, flags: i32) -> Result<File, Errno> {
    let components = self.components(index)?;
    let mut file = open_at(self.root.as_raw_fd(), OsStr::new("."),
      if components.is_empty() { flags } else { libc::O_PATH | libc::O_DIRECTORY })?;
    for (position, name) in components.iter().enumerate() {
      let last = position + 1 == components.len();
      file = open_at(file.as_raw_fd(), name,
        if last { flags } else { libc::O_PATH | libc::O_DIRECTORY })?;
    }
    Ok(file)
  }

  fn directory(&self, index: usize) -> Result<File, Errno> {
    let file = self.open_node(index, libc::O_RDONLY | libc::O_DIRECTORY)?;
    if !file.metadata().map_err(Errno::from)?.is_dir() { return Err(Errno::ENOTDIR); }
    Ok(file)
  }

  fn child(&self, parent: usize, name: &OsStr) -> Result<(usize, FileType), Errno> {
    valid_name(name)?;
    let directory = self.directory(parent)?;
    let file = open_at(directory.as_raw_fd(), name, libc::O_PATH)?;
    let kind = visible_kind(&file.metadata().map_err(Errno::from)?)?;
    let mut nodes = self.nodes.lock().map_err(|_| Errno::EIO)?;
    let key = (parent, name.to_os_string());
    let index = match nodes.by_name.get(&key) {
      Some(&index) => index,
      None => {
        let index = nodes.entries.len();
        nodes.entries.push(Node { parent, name: name.to_os_string() });
        nodes.by_name.insert(key, index);
        index
      }
    };
    Ok((index, kind))
  }

  fn attr(&self, index: usize) -> Result<FileAttr, Errno> {
    let file = self.open_node(index, libc::O_PATH)?;
    let metadata = file.metadata().map_err(Errno::from)?;
    let kind = visible_kind(&metadata)?;
    let mtime = timestamp(metadata.mtime(), metadata.mtime_nsec());
    let atime = timestamp(metadata.atime(), metadata.atime_nsec());
    let ctime = timestamp(metadata.ctime(), metadata.ctime_nsec());
    Ok(FileAttr {
      ino: INodeNo(index as u64 + 1), size: metadata.len(), blocks: metadata.blocks(),
      atime, mtime, ctime, crtime: ctime, kind,
      perm: (metadata.mode() & if kind == FileType::Directory { 0o555 } else { 0o444 }) as u16,
      nlink: metadata.nlink() as u32, uid: self.uid, gid: self.gid,
      rdev: 0, blksize: metadata.blksize() as u32, flags: 0,
    })
  }

  fn entries(&self, index: usize) -> Result<Vec<(OsString, usize, FileType)>, Errno> {
    let file = self.directory(index)?;
    let raw = file.into_raw_fd();
    let directory = unsafe { libc::fdopendir(raw) };
    if directory.is_null() {
      unsafe { libc::close(raw); }
      return Err(Errno::from(io::Error::last_os_error()));
    }
    let directory = OpenDir(directory);
    let mut entries = Vec::new();
    loop {
      let entry = unsafe { libc::readdir(directory.0) };
      if entry.is_null() { break; }
      let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
      let name = OsStr::from_bytes(name.to_bytes());
      if name.as_bytes() == b"." || name.as_bytes() == b".." { continue; }
      if let Ok((child, kind)) = self.child(index, name) {
        entries.push((name.to_os_string(), child, kind));
      }
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(entries)
  }

  fn open_handle(&self, index: usize, flags: OpenFlags) -> Result<u64, Errno> {
    if flags.0 & libc::O_ACCMODE != libc::O_RDONLY || flags.0 & libc::O_TRUNC != 0 { return Err(Errno::EROFS); }
    let path = self.open_node(index, libc::O_PATH)?;
    if !path.metadata().map_err(Errno::from)?.is_file() { return Err(Errno::EISDIR); }
    // Reopen the already-checked inode, not its name: a swap to a FIFO or
    // device between lookup and open cannot make this call block or touch it.
    let file = File::open(format!("/proc/self/fd/{}", path.as_raw_fd())).map_err(Errno::from)?;
    let handle = self.next_handle.fetch_add(1, Ordering::Relaxed);
    self.handles.lock().map_err(|_| Errno::EIO)?.insert(handle, file);
    Ok(handle)
  }

  fn read_handle(&self, handle: u64, offset: u64, size: u32) -> Result<Vec<u8>, Errno> {
    let file = self.handles.lock().map_err(|_| Errno::EIO)?.get(&handle)
      .ok_or(Errno::EBADF)?.try_clone().map_err(Errno::from)?;
    let mut bytes = vec![0; size as usize];
    let count = file.read_at(&mut bytes, offset).map_err(Errno::from)?;
    bytes.truncate(count);
    Ok(bytes)
  }
}

struct OpenDir(*mut libc::DIR);
impl Drop for OpenDir { fn drop(&mut self) { unsafe { libc::closedir(self.0); } } }

fn valid_name(name: &OsStr) -> Result<(), Errno> {
  let bytes = name.as_bytes();
  if bytes.is_empty() || bytes == b"." || bytes == b".." || bytes.contains(&b'/') || bytes.contains(&0) { Err(Errno::ENOENT) }
  else { Ok(()) }
}

fn open_at(parent: i32, name: &OsStr, flags: i32) -> Result<File, Errno> {
  let name = CString::new(name.as_bytes()).map_err(|_| Errno::ENOENT)?;
  let fd = unsafe { libc::openat(parent, name.as_ptr(), flags | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
  if fd < 0 { Err(Errno::from(io::Error::last_os_error())) }
  else { Ok(unsafe { File::from_raw_fd(fd) }) }
}

fn visible_kind(metadata: &fs::Metadata) -> Result<FileType, Errno> {
  if metadata.is_dir() { Ok(FileType::Directory) }
  else if metadata.is_file() { Ok(FileType::RegularFile) }
  else { Err(Errno::ENOENT) }
}

fn timestamp(seconds: i64, nanos: i64) -> SystemTime {
  if seconds < 0 || nanos < 0 { return UNIX_EPOCH; }
  UNIX_EPOCH + Duration::new(seconds as u64, nanos as u32)
}

impl Filesystem for DirFs {
  fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
    let result = parent.0.checked_sub(1).ok_or(Errno::ENOENT)
      .and_then(|parent| self.child(parent as usize, name))
      .and_then(|(index, _)| self.attr(index));
    match result { Ok(attr) => reply.entry(&TTL, &attr, Generation(0)), Err(error) => reply.error(error) }
  }

  fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
    match ino.0.checked_sub(1).ok_or(Errno::ENOENT).and_then(|index| self.attr(index as usize)) {
      Ok(attr) => reply.attr(&TTL, &attr), Err(error) => reply.error(error),
    }
  }

  fn open(&self, _req: &Request, ino: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
    match ino.0.checked_sub(1).ok_or(Errno::ENOENT).and_then(|index| self.open_handle(index as usize, flags)) {
      Ok(handle) => reply.opened(FileHandle(handle), FopenFlags::empty()), Err(error) => reply.error(error),
    }
  }

  fn read(&self, _req: &Request, _ino: INodeNo, fh: FileHandle, offset: u64, size: u32, _flags: OpenFlags, _lock_owner: Option<LockOwner>, reply: ReplyData) {
    match self.read_handle(fh.0, offset, size) { Ok(bytes) => reply.data(&bytes), Err(error) => reply.error(error) }
  }

  fn release(&self, _req: &Request, _ino: INodeNo, fh: FileHandle, _flags: OpenFlags, _lock_owner: Option<LockOwner>, _flush: bool, reply: ReplyEmpty) {
    if let Ok(mut handles) = self.handles.lock() { handles.remove(&fh.0); }
    reply.ok();
  }

  fn readdir(&self, _req: &Request, ino: INodeNo, _fh: FileHandle, offset: u64, mut reply: ReplyDirectory) {
    let Some(index) = ino.0.checked_sub(1).map(|index| index as usize) else { return reply.error(Errno::ENOENT) };
    let entries = match self.entries(index) { Ok(entries) => entries, Err(error) => return reply.error(error) };
    let parent = self.nodes.lock().ok().and_then(|nodes| nodes.entries.get(index).map(|node| node.parent)).unwrap_or(0);
    let special = [(OsString::from("."), index, FileType::Directory), (OsString::from(".."), parent, FileType::Directory)];
    for (position, (name, child, kind)) in special.into_iter().chain(entries).enumerate().skip(offset as usize) {
      if reply.add(INodeNo(child as u64 + 1), position as u64 + 1, kind, &name) { break; }
    }
    reply.ok();
  }

  fn setattr(&self, _req: &Request, _ino: INodeNo, _mode: Option<u32>, _uid: Option<u32>, _gid: Option<u32>, _size: Option<u64>,
    _atime: Option<TimeOrNow>, _mtime: Option<TimeOrNow>, _ctime: Option<SystemTime>, _fh: Option<FileHandle>,
    _crtime: Option<SystemTime>, _chgtime: Option<SystemTime>, _bkuptime: Option<SystemTime>, _flags: Option<BsdFileFlags>, reply: ReplyAttr) {
    reply.error(Errno::EROFS);
  }

  fn mkdir(&self, _req: &Request, _parent: INodeNo, _name: &OsStr, _mode: u32, _umask: u32, reply: ReplyEntry) { reply.error(Errno::EROFS); }
  fn mknod(&self, _req: &Request, _parent: INodeNo, _name: &OsStr, _mode: u32, _umask: u32, _rdev: u32, reply: ReplyEntry) { reply.error(Errno::EROFS); }
  fn symlink(&self, _req: &Request, _parent: INodeNo, _name: &OsStr, _target: &Path, reply: ReplyEntry) { reply.error(Errno::EROFS); }
  fn link(&self, _req: &Request, _ino: INodeNo, _parent: INodeNo, _name: &OsStr, reply: ReplyEntry) { reply.error(Errno::EROFS); }
  fn unlink(&self, _req: &Request, _parent: INodeNo, _name: &OsStr, reply: ReplyEmpty) { reply.error(Errno::EROFS); }
  fn rmdir(&self, _req: &Request, _parent: INodeNo, _name: &OsStr, reply: ReplyEmpty) { reply.error(Errno::EROFS); }
  fn rename(&self, _req: &Request, _parent: INodeNo, _name: &OsStr, _newparent: INodeNo, _newname: &OsStr, _flags: RenameFlags, reply: ReplyEmpty) { reply.error(Errno::EROFS); }
  fn create(&self, _req: &Request, _parent: INodeNo, _name: &OsStr, _mode: u32, _umask: u32, _flags: i32, reply: ReplyCreate) { reply.error(Errno::EROFS); }
  fn write(&self, _req: &Request, _ino: INodeNo, _fh: FileHandle, _offset: u64, _data: &[u8], _write_flags: WriteFlags,
    _flags: OpenFlags, _lock_owner: Option<LockOwner>, reply: ReplyWrite) { reply.error(Errno::EROFS); }
}

/// Mounts as the current user only, without device, suid or executable access.
pub fn mount(filesystem: DirFs, mountpoint: &Path) -> Result<BackgroundSession, String> {
  let mut config = Config::default();
  config.mount_options = vec![
    MountOption::RO, MountOption::NoDev, MountOption::NoSuid, MountOption::NoExec,
    MountOption::DefaultPermissions, MountOption::FSName("docker-vm-dir".into()),
    MountOption::Subtype("dir".into()),
  ];
  fuser::spawn_mount(filesystem, mountpoint, &config)
    .map_err(|error| format!("mount folder at {}: {error}", mountpoint.display()))
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::os::unix::fs::symlink;

  struct Fixture(std::path::PathBuf);

  impl Fixture {
    fn new() -> Self {
      let base = std::env::temp_dir().join(format!("docker-vm-dirfs-{}", uuid::Uuid::new_v4().simple()));
      fs::create_dir(&base).unwrap();
      fs::create_dir(base.join("root")).unwrap();
      fs::create_dir(base.join("root/docs")).unwrap();
      fs::write(base.join("root/docs/readme.txt"), b"hello from host\n").unwrap();
      fs::write(base.join("outside.txt"), b"secret outside\n").unwrap();
      symlink(base.join("outside.txt"), base.join("root/escape")).unwrap();
      symlink("docs", base.join("root/dir-link")).unwrap();
      Self(base)
    }

    fn root(&self) -> std::path::PathBuf { self.0.join("root") }
  }

  impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }

  #[test]
  fn lists_and_streams_regular_files() {
    let fixture = Fixture::new();
    let filesystem = DirFs::new(&fixture.root(), false).unwrap();
    let entries = filesystem.entries(0).unwrap();
    assert_eq!(entries.iter().map(|entry| entry.0.to_string_lossy().into_owned()).collect::<Vec<_>>(), ["docs"]);
    let (docs, kind) = filesystem.child(0, OsStr::new("docs")).unwrap();
    assert_eq!(kind, FileType::Directory);
    let (readme, kind) = filesystem.child(docs, OsStr::new("readme.txt")).unwrap();
    assert_eq!(kind, FileType::RegularFile);
    let handle = filesystem.open_handle(readme, OpenFlags(libc::O_RDONLY)).unwrap();
    assert_eq!(filesystem.read_handle(handle, 0, 5).unwrap(), b"hello");
    assert_eq!(filesystem.read_handle(handle, 6, 100).unwrap(), b"from host\n");
    assert_eq!(filesystem.attr(readme).unwrap().perm & 0o222, 0);
  }

  #[test]
  fn symlinks_and_parent_escapes_are_invisible() {
    let fixture = Fixture::new();
    let filesystem = DirFs::new(&fixture.root(), false).unwrap();
    assert!(filesystem.child(0, OsStr::new("escape")).is_err());
    assert!(filesystem.child(0, OsStr::new("dir-link")).is_err());
    assert!(filesystem.child(0, OsStr::new("..")).is_err());
    assert!(filesystem.child(0, OsStr::new("../outside.txt")).is_err());
    let entries = filesystem.entries(0).unwrap();
    assert_eq!(entries.len(), 1);
  }

  #[test]
  fn writes_are_rejected_and_selected_root_cannot_be_a_symlink() {
    let fixture = Fixture::new();
    let filesystem = DirFs::new(&fixture.root(), false).unwrap();
    let (docs, _) = filesystem.child(0, OsStr::new("docs")).unwrap();
    let (readme, _) = filesystem.child(docs, OsStr::new("readme.txt")).unwrap();
    assert_eq!(filesystem.open_handle(readme, OpenFlags(libc::O_WRONLY)).unwrap_err(), Errno::EROFS);
    assert!(DirFs::new(&fixture.root(), true).is_err());
    symlink(fixture.root(), fixture.0.join("root-link")).unwrap();
    assert!(DirFs::new(&fixture.0.join("root-link"), false).is_err());
  }
}
