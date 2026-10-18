//! Root-confined FUSE view of a host directory.
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
  writable: bool,
  nodes: Mutex<Nodes>,
  handles: Mutex<HashMap<u64, File>>,
  next_handle: AtomicU64,
  uid: u32,
  gid: u32,
}

impl DirFs {
  pub fn new(root: &Path, writable: bool) -> Result<Self, String> {
    let path = CString::new(root.as_os_str().as_bytes()).map_err(|_| "directory path contains NUL")?;
    let fd = unsafe { libc::open(path.as_ptr(), libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
    if fd < 0 { return Err(format!("open share directory {}: {}", root.display(), io::Error::last_os_error())); }
    Ok(Self {
      root: unsafe { File::from_raw_fd(fd) },
      writable,
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
    Ok((self.register_child(parent, name)?, kind))
  }

  fn register_child(&self, parent: usize, name: &OsStr) -> Result<usize, Errno> {
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
    Ok(index)
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
      perm: (metadata.mode() & match (self.writable, kind) {
        (true, FileType::Directory) => 0o777,
        (true, _) => 0o666,
        (false, FileType::Directory) => 0o555,
        (false, _) => 0o444,
      }) as u16,
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
    if !self.writable && (flags.0 & libc::O_ACCMODE != libc::O_RDONLY || flags.0 & libc::O_TRUNC != 0) { return Err(Errno::EROFS); }
    let path = self.open_node(index, libc::O_PATH)?;
    if !path.metadata().map_err(Errno::from)?.is_file() { return Err(Errno::EISDIR); }
    // Reopen the already-checked inode, not its name: a swap to a FIFO or
    // device between lookup and open cannot make this call block or touch it.
    let access = flags.0 & libc::O_ACCMODE;
    let file = File::options()
      .read(access != libc::O_WRONLY)
      .write(access != libc::O_RDONLY)
      .truncate(flags.0 & libc::O_TRUNC != 0)
      .open(format!("/proc/self/fd/{}", path.as_raw_fd())).map_err(Errno::from)?;
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

  fn write_handle(&self, handle: u64, offset: u64, bytes: &[u8]) -> Result<usize, Errno> {
    if !self.writable { return Err(Errno::EROFS); }
    let file = self.handles.lock().map_err(|_| Errno::EIO)?.get(&handle)
      .ok_or(Errno::EBADF)?.try_clone().map_err(Errno::from)?;
    file.write_at(bytes, offset).map_err(Errno::from)
  }

  fn writable_directory(&self, parent: usize, name: &OsStr) -> Result<File, Errno> {
    if !self.writable { return Err(Errno::EROFS); }
    valid_name(name)?;
    self.directory(parent)
  }

  fn create_file(&self, parent: usize, name: &OsStr, umask: u32) -> Result<(usize, u64), Errno> {
    let directory = self.writable_directory(parent, name)?;
    let name_c = CString::new(name.as_bytes()).map_err(|_| Errno::EINVAL)?;
    let fd = unsafe { libc::openat(directory.as_raw_fd(), name_c.as_ptr(),
      libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC, 0o644 & !umask) };
    if fd < 0 { return Err(Errno::from(io::Error::last_os_error())); }
    let file = unsafe { File::from_raw_fd(fd) };
    let index = self.register_child(parent, name)?;
    let handle = self.next_handle.fetch_add(1, Ordering::Relaxed);
    self.handles.lock().map_err(|_| Errno::EIO)?.insert(handle, file);
    Ok((index, handle))
  }

  fn make_directory(&self, parent: usize, name: &OsStr, umask: u32) -> Result<usize, Errno> {
    let directory = self.writable_directory(parent, name)?;
    let name_c = CString::new(name.as_bytes()).map_err(|_| Errno::EINVAL)?;
    let status = unsafe { libc::mkdirat(directory.as_raw_fd(), name_c.as_ptr(), 0o755 & !umask) };
    if status < 0 { return Err(Errno::from(io::Error::last_os_error())); }
    self.register_child(parent, name)
  }

  fn remove_entry(&self, parent: usize, name: &OsStr, directory: bool) -> Result<(), Errno> {
    let parent_fd = self.writable_directory(parent, name)?;
    let entry = open_at(parent_fd.as_raw_fd(), name, libc::O_PATH)?;
    let kind = visible_kind(&entry.metadata().map_err(Errno::from)?)?;
    if directory && kind != FileType::Directory { return Err(Errno::ENOTDIR); }
    if !directory && kind != FileType::RegularFile { return Err(Errno::EISDIR); }
    let name_c = CString::new(name.as_bytes()).map_err(|_| Errno::EINVAL)?;
    let status = unsafe { libc::unlinkat(parent_fd.as_raw_fd(), name_c.as_ptr(), if directory { libc::AT_REMOVEDIR } else { 0 }) };
    if status < 0 { return Err(Errno::from(io::Error::last_os_error())); }
    self.nodes.lock().map_err(|_| Errno::EIO)?.by_name.remove(&(parent, name.to_os_string()));
    Ok(())
  }

  fn rename_entry(&self, parent: usize, name: &OsStr, newparent: usize, newname: &OsStr, flags: RenameFlags) -> Result<(), Errno> {
    if !flags.difference(RenameFlags::RENAME_NOREPLACE).is_empty() { return Err(Errno::EINVAL); }
    let olddir = self.writable_directory(parent, name)?;
    let newdir = self.writable_directory(newparent, newname)?;
    let old = open_at(olddir.as_raw_fd(), name, libc::O_PATH)?;
    visible_kind(&old.metadata().map_err(Errno::from)?)?;
    // renameat2 never follows either final component. A symlink target is
    // refused as well, rather than silently replacing it.
    match open_at(newdir.as_raw_fd(), newname, libc::O_PATH) {
      Ok(target) => { visible_kind(&target.metadata().map_err(Errno::from)?)?; }
      Err(Errno::ENOENT) => {}
      Err(error) => return Err(error),
    }
    let old_name = CString::new(name.as_bytes()).map_err(|_| Errno::EINVAL)?;
    let new_name = CString::new(newname.as_bytes()).map_err(|_| Errno::EINVAL)?;
    let status = unsafe { libc::renameat2(olddir.as_raw_fd(), old_name.as_ptr(), newdir.as_raw_fd(), new_name.as_ptr(), flags.bits()) };
    if status < 0 { return Err(Errno::from(io::Error::last_os_error())); }
    let mut nodes = self.nodes.lock().map_err(|_| Errno::EIO)?;
    nodes.by_name.remove(&(newparent, newname.to_os_string()));
    if let Some(index) = nodes.by_name.remove(&(parent, name.to_os_string())) {
      nodes.entries[index].parent = newparent;
      nodes.entries[index].name = newname.to_os_string();
      nodes.by_name.insert((newparent, newname.to_os_string()), index);
    }
    Ok(())
  }

  fn change_attributes(&self, index: usize, mode: Option<u32>, size: Option<u64>,
    atime: Option<TimeOrNow>, mtime: Option<TimeOrNow>) -> Result<FileAttr, Errno> {
    if !self.writable { return Err(Errno::EROFS); }
    let path = self.open_node(index, libc::O_PATH)?;
    let metadata = path.metadata().map_err(Errno::from)?;
    let kind = visible_kind(&metadata)?;
    let proc_path = CString::new(format!("/proc/self/fd/{}", path.as_raw_fd())).map_err(|_| Errno::EIO)?;
    if let Some(mode) = mode {
      let mode = mode & if kind == FileType::Directory { 0o777 } else { 0o666 };
      let status = unsafe { libc::fchmodat(libc::AT_FDCWD, proc_path.as_ptr(), mode, 0) };
      if status < 0 { return Err(Errno::from(io::Error::last_os_error())); }
    }
    if let Some(size) = size {
      if kind != FileType::RegularFile { return Err(Errno::EISDIR); }
      File::options().write(true).open(proc_path.to_string_lossy().as_ref())
        .map_err(Errno::from)?.set_len(size).map_err(Errno::from)?;
    }
    if atime.is_some() || mtime.is_some() {
      let times = [to_timespec(atime), to_timespec(mtime)];
      let status = unsafe { libc::utimensat(libc::AT_FDCWD, proc_path.as_ptr(), times.as_ptr(), 0) };
      if status < 0 { return Err(Errno::from(io::Error::last_os_error())); }
    }
    self.attr(index)
  }

  fn sync_handle(&self, handle: u64, data_only: bool) -> Result<(), Errno> {
    let file = self.handles.lock().map_err(|_| Errno::EIO)?.get(&handle)
      .ok_or(Errno::EBADF)?.try_clone().map_err(Errno::from)?;
    if data_only { file.sync_data() } else { file.sync_all() }.map_err(Errno::from)
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

fn to_timespec(time: Option<TimeOrNow>) -> libc::timespec {
  match time {
    None => libc::timespec { tv_sec: 0, tv_nsec: libc::UTIME_OMIT as _ },
    Some(TimeOrNow::Now) => libc::timespec { tv_sec: 0, tv_nsec: libc::UTIME_NOW as _ },
    Some(TimeOrNow::SpecificTime(time)) => {
      let duration = time.duration_since(UNIX_EPOCH).unwrap_or_default();
      libc::timespec { tv_sec: duration.as_secs() as _, tv_nsec: duration.subsec_nanos() as _ }
    }
  }
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

  fn setattr(&self, _req: &Request, ino: INodeNo, mode: Option<u32>, uid: Option<u32>, gid: Option<u32>, size: Option<u64>,
    atime: Option<TimeOrNow>, mtime: Option<TimeOrNow>, _ctime: Option<SystemTime>, _fh: Option<FileHandle>,
    _crtime: Option<SystemTime>, _chgtime: Option<SystemTime>, _bkuptime: Option<SystemTime>, _flags: Option<BsdFileFlags>, reply: ReplyAttr) {
    if uid.is_some() || gid.is_some() { return reply.error(if self.writable { Errno::EPERM } else { Errno::EROFS }); }
    let result = ino.0.checked_sub(1).ok_or(Errno::ENOENT)
      .and_then(|index| self.change_attributes(index as usize, mode, size, atime, mtime));
    match result { Ok(attr) => reply.attr(&TTL, &attr), Err(error) => reply.error(error) }
  }

  fn mkdir(&self, _req: &Request, parent: INodeNo, name: &OsStr, _mode: u32, umask: u32, reply: ReplyEntry) {
    let result = parent.0.checked_sub(1).ok_or(Errno::ENOENT)
      .and_then(|parent| self.make_directory(parent as usize, name, umask))
      .and_then(|index| self.attr(index));
    match result { Ok(attr) => reply.entry(&TTL, &attr, Generation(0)), Err(error) => reply.error(error) }
  }
  fn mknod(&self, _req: &Request, _parent: INodeNo, _name: &OsStr, _mode: u32, _umask: u32, _rdev: u32, reply: ReplyEntry) { reply.error(Errno::EROFS); }
  fn symlink(&self, _req: &Request, _parent: INodeNo, _name: &OsStr, _target: &Path, reply: ReplyEntry) { reply.error(Errno::EROFS); }
  fn link(&self, _req: &Request, _ino: INodeNo, _parent: INodeNo, _name: &OsStr, reply: ReplyEntry) { reply.error(Errno::EROFS); }
  fn unlink(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
    let result = parent.0.checked_sub(1).ok_or(Errno::ENOENT).and_then(|parent| self.remove_entry(parent as usize, name, false));
    match result { Ok(()) => reply.ok(), Err(error) => reply.error(error) }
  }
  fn rmdir(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
    let result = parent.0.checked_sub(1).ok_or(Errno::ENOENT).and_then(|parent| self.remove_entry(parent as usize, name, true));
    match result { Ok(()) => reply.ok(), Err(error) => reply.error(error) }
  }
  fn rename(&self, _req: &Request, parent: INodeNo, name: &OsStr, newparent: INodeNo, newname: &OsStr, flags: RenameFlags, reply: ReplyEmpty) {
    let result = parent.0.checked_sub(1).zip(newparent.0.checked_sub(1)).ok_or(Errno::ENOENT)
      .and_then(|(parent, newparent)| self.rename_entry(parent as usize, name, newparent as usize, newname, flags));
    match result { Ok(()) => reply.ok(), Err(error) => reply.error(error) }
  }
  fn create(&self, _req: &Request, parent: INodeNo, name: &OsStr, _mode: u32, umask: u32, _flags: i32, reply: ReplyCreate) {
    let result = parent.0.checked_sub(1).ok_or(Errno::ENOENT)
      .and_then(|parent| self.create_file(parent as usize, name, umask))
      .and_then(|(index, handle)| self.attr(index).map(|attr| (attr, handle)));
    match result { Ok((attr, handle)) => reply.created(&TTL, &attr, Generation(0), FileHandle(handle), FopenFlags::empty()), Err(error) => reply.error(error) }
  }
  fn write(&self, _req: &Request, _ino: INodeNo, fh: FileHandle, offset: u64, data: &[u8], _write_flags: WriteFlags,
    _flags: OpenFlags, _lock_owner: Option<LockOwner>, reply: ReplyWrite) {
    match self.write_handle(fh.0, offset, data) { Ok(count) => reply.written(count as u32), Err(error) => reply.error(error) }
  }
  fn fsync(&self, _req: &Request, _ino: INodeNo, fh: FileHandle, data_only: bool, reply: ReplyEmpty) {
    match self.sync_handle(fh.0, data_only) { Ok(()) => reply.ok(), Err(error) => reply.error(error) }
  }
}

/// Mounts as the current user only, without device, suid or executable access.
pub fn mount(filesystem: DirFs, mountpoint: &Path) -> Result<BackgroundSession, String> {
  let mut config = Config::default();
  config.mount_options = vec![
    MountOption::NoDev, MountOption::NoSuid, MountOption::NoExec,
    MountOption::DefaultPermissions, MountOption::FSName("docker-vm-dir".into()),
    MountOption::Subtype("dir".into()),
  ];
  if !filesystem.writable { config.mount_options.push(MountOption::RO); }
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
    symlink(fixture.root(), fixture.0.join("root-link")).unwrap();
    assert!(DirFs::new(&fixture.0.join("root-link"), false).is_err());
  }

  #[test]
  fn creates_writes_truncates_renames_and_removes_with_safe_modes() {
    let fixture = Fixture::new();
    let filesystem = DirFs::new(&fixture.root(), true).unwrap();
    let directory = filesystem.make_directory(0, OsStr::new("made"), 0o022).unwrap();
    assert_eq!(fs::metadata(fixture.root().join("made")).unwrap().mode() & 0o777, 0o755);
    let (file, handle) = filesystem.create_file(directory, OsStr::new("program.sh"), 0o022).unwrap();
    assert_eq!(filesystem.write_handle(handle, 0, b"#!/bin/sh\nexit 0\n").unwrap(), 17);
    filesystem.sync_handle(handle, false).unwrap();
    assert_eq!(fs::metadata(fixture.root().join("made/program.sh")).unwrap().mode() & 0o777, 0o644);
    filesystem.change_attributes(file, Some(0o777), None, None, None).unwrap();
    assert_eq!(fs::metadata(fixture.root().join("made/program.sh")).unwrap().mode() & 0o111, 0);
    filesystem.change_attributes(file, None, Some(4), None, None).unwrap();
    assert_eq!(fs::read(fixture.root().join("made/program.sh")).unwrap(), b"#!/b");
    filesystem.rename_entry(directory, OsStr::new("program.sh"), 0, OsStr::new("renamed.txt"), RenameFlags::empty()).unwrap();
    assert!(fixture.root().join("renamed.txt").exists());
    filesystem.remove_entry(0, OsStr::new("renamed.txt"), false).unwrap();
    filesystem.remove_entry(0, OsStr::new("made"), true).unwrap();
    assert!(!fixture.root().join("made").exists());
  }

  #[test]
  fn writable_paths_reject_parent_and_symlink_escapes() {
    let fixture = Fixture::new();
    let filesystem = DirFs::new(&fixture.root(), true).unwrap();
    let (docs, _) = filesystem.child(0, OsStr::new("docs")).unwrap();
    assert!(filesystem.create_file(0, OsStr::new("../outside.txt"), 0).is_err());
    assert!(filesystem.make_directory(0, OsStr::new(".."), 0).is_err());
    assert!(filesystem.rename_entry(docs, OsStr::new("readme.txt"), 0, OsStr::new("../outside.txt"), RenameFlags::empty()).is_err());
    assert!(filesystem.rename_entry(docs, OsStr::new("readme.txt"), 0, OsStr::new("escape"), RenameFlags::empty()).is_err());
    assert!(filesystem.rename_entry(0, OsStr::new("escape"), 0, OsStr::new("moved"), RenameFlags::empty()).is_err());
    assert_eq!(fs::read(fixture.0.join("outside.txt")).unwrap(), b"secret outside\n");
  }
}
