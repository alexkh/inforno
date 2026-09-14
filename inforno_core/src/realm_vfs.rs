#![cfg(target_os = "linux")]

use fuser::{
    FileAttr, FileType, Filesystem, ReplyAttr, ReplyCreate, ReplyData, ReplyDirectory, ReplyEmpty,
    ReplyEntry, ReplyOpen, ReplyWrite, Request, TimeOrNow,
};
use libc::{EACCES, EEXIST, ENOENT, O_RDWR, O_WRONLY};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::time::SystemTime;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};
use crate::realm::{ActiveRealm, Cap};

const TTL: Duration = Duration::from_secs(1);

#[derive(Debug, Clone)]
struct VNode {
    ino: u64,
    host_path: Option<PathBuf>,
    is_dir: bool,
}

pub struct RealmFuseFS {
    realm: ActiveRealm,
    /// The Role this mounted view is served on behalf of. Fixed for the
    /// lifetime of the mount — one FUSE session currently represents one
    /// Role's masked view, matching one `VfsMaskSession::spawn_vfs` call.
    role: String,
    inodes: HashMap<u64, VNode>,
    path_to_ino: HashMap<PathBuf, u64>,
    next_ino: u64,
}

impl RealmFuseFS {
    pub fn new(realm: ActiveRealm, role: String) -> Self {
        let mut fs = Self {
            realm,
            role,
            inodes: HashMap::new(),
            path_to_ino: HashMap::new(),
            next_ino: 1,
        };

        // Initialize Root inode (1)
        fs.get_or_create_ino(Path::new("/"), None, true);
        fs
    }

    fn get_or_create_ino(&mut self, vpath: &Path, host_path: Option<PathBuf>, is_dir: bool) -> u64 {
        if let Some(&ino) = self.path_to_ino.get(vpath) {
            return ino;
        }

        let ino = self.next_ino;
        self.next_ino += 1;

        let node = VNode {
            ino,
            host_path,
            is_dir,
        };

        self.inodes.insert(ino, node);
        self.path_to_ino.insert(vpath.to_path_buf(), ino);
        ino
    }

    fn stat_to_attr(&self, ino: u64, host_path: &Option<PathBuf>, is_dir: bool, is_protected: bool) -> FileAttr {
        let (size, perm) = if let Some(hp) = host_path {
            if let Ok(meta) = fs::metadata(hp) {
                // If protected, strip write bits completely (0o444). Otherwise, grant read/write (0o644).
                (meta.len(), if is_dir { 0o755 } else { if is_protected { 0o444 } else { 0o644 } })
            } else {
                (0, 0o555)
            }
        } else {
            (0, if is_dir { 0o755 } else { 0o444 })
        };

        FileAttr {
            ino,
            size,
            blocks: (size + 511) / 512,
            atime: UNIX_EPOCH,
            mtime: UNIX_EPOCH,
            ctime: UNIX_EPOCH,
            crtime: UNIX_EPOCH,
            kind: if is_dir { FileType::Directory } else { FileType::RegularFile },
            perm,
            nlink: 1,
            // MAGIC TRICK: If protected, claim UID/GID 0 (root). 
            // In our User Namespace mapping, 0 is unmapped, which renders to the jailed bash shell as `nobody`.
            uid: if is_protected { 0 } else { unsafe { libc::getuid() } },
            gid: if is_protected { 0 } else { unsafe { libc::getgid() } },
            rdev: 0,
            flags: 0,
            blksize: 512,
        }
    }
}

impl Filesystem for RealmFuseFS {
    fn lookup(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEntry) {
        let name_str = match name.to_str() {
            Some(s) => s,
            None => {
                reply.error(ENOENT);
                return;
            }
        };

        let parent_vpath = self.inodes.get(&parent).and_then(|_node| {
            self.path_to_ino.iter().find_map(|(path, &ino)| if ino == parent { Some(path) } else { None })
        });

        let parent_vpath = match parent_vpath {
            Some(p) => p.clone(),
            None => {
                reply.error(ENOENT);
                return;
            }
        };

        let child_vpath = parent_vpath.join(name_str);

        // Security check via ActiveRealm
        if let Some(host_path) = self.realm.secure_resolve_path(&child_vpath, &self.role) {
            // `secure_resolve_path` only validates the virtual->host mapping and
            // visibility rules; it says nothing about whether anything actually
            // lives at that path. Without this check, `lookup` reports a
            // positive (phantom, zero-size) entry for ANY name under an
            // accessible mount, so the kernel later rejects a real `mkdir`/
            // `create` for that same name with EEXIST, believing the dentry
            // already exists.
            if !host_path.exists() {
                reply.error(ENOENT);
                return;
            }
            let is_dir = host_path.is_dir();
            let is_protected = self.realm.is_path_read_only(&child_vpath);
            let ino = self.get_or_create_ino(&child_vpath, Some(host_path.clone()), is_dir);
            let attr = self.stat_to_attr(ino, &Some(host_path), is_dir, is_protected);
            reply.entry(&TTL, &attr, 0);
            return;
        }

        let vpath_str = child_vpath.to_str().unwrap_or("");

        // Synthetic OS Directories (required for bash/tools to not crash inside the jail)
        let synthetic_dirs = [
            "/dev", "/proc", "/tmp", "/bin", "/usr", "/lib", "/lib64", "/etc",
            "/.cargo", "/.rustup",
        ];
        if synthetic_dirs.contains(&vpath_str) {
            let ino = self.get_or_create_ino(&child_vpath, None, true);
            let attr = self.stat_to_attr(ino, &None, true, false);
            reply.entry(&TTL, &attr, 0);
            return;
        }

        // Virtual Directory Lookup (for top-level configured mounts)
        let is_virtual_dir = self.realm.mounts.iter().any(|m| m.virtual_path.starts_with(vpath_str));
        if is_virtual_dir {
            let ino = self.get_or_create_ino(&child_vpath, None, true);
            let attr = self.stat_to_attr(ino, &None, true, false);
            reply.entry(&TTL, &attr, 0);
            return;
        }

        reply.error(ENOENT);
    }

    fn getattr(&mut self, _req: &Request<'_>, ino: u64, reply: ReplyAttr) {
        if let Some(node) = self.inodes.get(&ino).cloned() {
            // If this inode is backed by a real host path that no longer
            // exists (deleted via unlink/rmdir, or removed out-of-band), say
            // so honestly instead of fabricating a fake stat — otherwise the
            // kernel keeps treating a dead entry as alive forever, and later
            // `mkdir`/`create` calls for the same name spuriously fail with
            // EEXIST. Synthetic nodes (host_path == None) are unaffected.
            if let Some(ref hp) = node.host_path {
                if !hp.exists() {
                    reply.error(ENOENT);
                    return;
                }
            }

            let vpath = self.path_to_ino.iter().find_map(|(p, &i)| if i == ino { Some(p.clone()) } else { None });
            let is_protected = vpath.map(|p| self.realm.is_path_read_only(&p)).unwrap_or(false);
            
            let attr = self.stat_to_attr(ino, &node.host_path, node.is_dir, is_protected);
            reply.attr(&TTL, &attr);
        } else {
            reply.error(ENOENT);
        }
    }

    fn open(&mut self, _req: &Request<'_>, ino: u64, flags: i32, reply: ReplyOpen) {
        if self.inodes.contains_key(&ino) {
            let vpath = self.path_to_ino.iter().find_map(|(p, &i)| if i == ino { Some(p.clone()) } else { None });
            
            if let Some(p) = vpath {
                let is_write_access = (flags & O_WRONLY) != 0 || (flags & O_RDWR) != 0;
                
                if is_write_access {
                    let can_write = self.realm.can_access(&p, Cap::Write, &self.role).is_ok();
                    let can_append = self.realm.can_access(&p, Cap::Append, &self.role).is_ok();
                    
                    if !can_write && !can_append {
                        reply.error(EACCES);
                        return;
                    }
                    if !can_write && can_append && (flags & libc::O_TRUNC) != 0 {
                        reply.error(EACCES); // Append-only cannot truncate via open
                        return;
                    }
                } else if self.realm.can_access(&p, Cap::Read, &self.role).is_err() {
                    reply.error(EACCES);
                    return;
                }
            }
            // The second argument here is FUSE's own `FOPEN_*` reply
            // bitmask (FOPEN_DIRECT_IO, FOPEN_KEEP_CACHE, ...), not an echo
            // of the caller's open(2) flags — those are a different
            // bitfield entirely. Passing `flags` through directly used to
            // set FOPEN_DIRECT_IO on any write-mode open, since O_WRONLY
            // and FOPEN_DIRECT_IO share bit 0. That silently broke mmap()
            // on writable files (the kernel won't MAP_SHARED a direct_io
            // file), which never showed up on plain read()/write() but
            // breaks tools like rustc's archive writer that mmap their
            // output. We don't need any special behavior here — every
            // read/write re-resolves the host path fresh regardless.
            reply.opened(ino, 0);
        } else {
            reply.error(ENOENT);
        }
    }

    fn read(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        size: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyData,
    ) {
        let vpath = self.path_to_ino.iter().find_map(|(p, &i)| if i == ino { Some(p.clone()) } else { None });
        if let Some(ref p) = vpath {
            if self.realm.can_access(p, Cap::Read, &self.role).is_err() {
                reply.error(EACCES);
                return;
            }
        }

        if let Some(node) = self.inodes.get(&ino) {
            if let Some(ref host_path) = node.host_path {
                if let Ok(mut file) = File::open(host_path) {
                    let mut buf = vec![0u8; size as usize];
                    if file.seek(SeekFrom::Start(offset as u64)).is_ok() {
                        if let Ok(read_bytes) = file.read(&mut buf) {
                            reply.data(&buf[..read_bytes]);
                            return;
                        }
                    }
                }
            }
        }
        reply.error(ENOENT);
    }

    fn write(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        data: &[u8],
        _write_flags: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyWrite,
    ) {
        let vpath = self.path_to_ino.iter().find_map(|(p, &i)| if i == ino { Some(p.clone()) } else { None });
        let mut can_write = false;
        let mut can_append = false;

        if let Some(ref p) = vpath {
            can_write = self.realm.can_access(p, Cap::Write, &self.role).is_ok();
            can_append = self.realm.can_access(p, Cap::Append, &self.role).is_ok();

            if !can_write && !can_append {
                reply.error(EACCES);
                return;
            }
        }

        if let Some(node) = self.inodes.get(&ino) {
            if let Some(ref host_path) = node.host_path {
                if can_write {
                    match OpenOptions::new().write(true).open(host_path) {
                        Ok(mut file) => {
                            if file.seek(SeekFrom::Start(offset as u64)).is_ok() {
                                if let Ok(written) = file.write(data) {
                                    reply.written(written as u32);
                                    return;
                                }
                            }
                        }
                        Err(_) => { reply.error(EACCES); return; }
                    }
                } else if can_append {
                    match OpenOptions::new().append(true).open(host_path) {
                        Ok(mut file) => {
                            // Enforce append: deny arbitrary backward seeks by apps ignoring O_APPEND
                            if let Ok(meta) = file.metadata() {
                                if offset as u64 != meta.len() {
                                    reply.error(libc::EINVAL);
                                    return;
                                }
                            }
                            if let Ok(written) = file.write(data) {
                                reply.written(written as u32);
                                return;
                            }
                        }
                        Err(_) => { reply.error(EACCES); return; }
                    }
                }
            }
        }
        reply.error(ENOENT);
    }

    #[allow(clippy::too_many_arguments)]
    fn setattr(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _mode: Option<u32>,
        _uid: Option<u32>,
        _gid: Option<u32>,
        size: Option<u64>,
        _atime: Option<TimeOrNow>,
        _mtime: Option<TimeOrNow>,
        _ctime: Option<SystemTime>,
        _fh: Option<u64>,
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        _flags: Option<u32>,
        reply: ReplyAttr,
    ) {
        let node = match self.inodes.get(&ino).cloned() {
            Some(n) => n,
            None => {
                reply.error(ENOENT);
                return;
            }
        };

        let vpath = self.path_to_ino.iter().find_map(|(p, &i)| if i == ino { Some(p.clone()) } else { None });
        let is_protected = vpath.as_ref().map(|p| self.realm.is_path_read_only(p)).unwrap_or(false);

        // Only actually mutate the host file if a size change (e.g. truncate) was requested.
        if let Some(new_size) = size {
            if let Some(ref p) = vpath {
                // Truncation STRICTLY requires Write. Append is intentionally insufficient.
                if self.realm.can_access(p, Cap::Write, &self.role).is_err() {
                    reply.error(EACCES);
                    return;
                }
            } else {
                reply.error(EACCES);
                return;
            }
            
            if let Some(ref host_path) = node.host_path {
                match OpenOptions::new().write(true).open(host_path) {
                    Ok(file) => {
                        if file.set_len(new_size).is_err() {
                            reply.error(EACCES);
                            return;
                        }
                    }
                    Err(_) => {
                        reply.error(EACCES);
                        return;
                    }
                }
            }
        }

        let attr = self.stat_to_attr(ino, &node.host_path, node.is_dir, is_protected);
        reply.attr(&TTL, &attr);
    }

    fn create(
        &mut self,
        _req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
        flags: i32,
        reply: ReplyCreate,
    ) {
        let name_str = match name.to_str() {
            Some(s) => s,
            None => {
                reply.error(ENOENT);
                return;
            }
        };

        let parent_vpath = self.path_to_ino.iter().find_map(|(path, &ino)| if ino == parent { Some(path.clone()) } else { None });
        let parent_vpath = match parent_vpath {
            Some(p) => p,
            None => {
                reply.error(ENOENT);
                return;
            }
        };

        let child_vpath = parent_vpath.join(name_str);

        // Full agent-aware check: hard restrictions (hidden, mount read-only,
        // create_rules) AND whether the agent's roles actually grant Create
        // here. We can't relay the reason through the FUSE reply (errno-only),
        // so it's logged here; `role_capabilities` is how the agent learns
        // its actual capabilities up front, to avoid retry loops.
        if let Err(reason) = self.realm.can_access(&child_vpath, Cap::Create, &self.role) {
            eprintln!("Realm VFS: denied creating '{}': {}", child_vpath.display(), reason);
            reply.error(EACCES);
            return;
        }

        let host_path = match self.realm.secure_resolve_path(&child_vpath, &self.role) {
            Some(p) => p,
            None => {
                reply.error(EACCES);
                return;
            }
        };

        match OpenOptions::new().write(true).create_new(true).open(&host_path) {
            Ok(_) => {
                let ino = self.get_or_create_ino(&child_vpath, Some(host_path.clone()), false);
                let attr = self.stat_to_attr(ino, &Some(host_path), false, false);
                // See the matching note in `open()`: this is FUSE's
                // FOPEN_* reply bitmask, not the caller's open flags.
                reply.created(&TTL, &attr, 0, ino, 0);
            }
            Err(_) => {
                reply.error(EACCES);
            }
        }
    }

    fn mkdir(
        &mut self,
        _req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
        reply: ReplyEntry,
    ) {
        let name_str = match name.to_str() {
            Some(s) => s,
            None => {
                reply.error(ENOENT);
                return;
            }
        };

        let parent_vpath = self.path_to_ino.iter().find_map(|(path, &ino)| if ino == parent { Some(path.clone()) } else { None });
        let parent_vpath = match parent_vpath {
            Some(p) => p,
            None => {
                reply.error(ENOENT);
                return;
            }
        };

        let child_vpath = parent_vpath.join(name_str);

        // Same Cap::Create gate as `create()` — directories are governed by
        // the same powers/create_rules as regular files.
        if let Err(reason) = self.realm.can_access(&child_vpath, Cap::Create, &self.role) {
            eprintln!("Realm VFS: denied mkdir '{}': {}", child_vpath.display(), reason);
            reply.error(EACCES);
            return;
        }

        let host_path = match self.realm.secure_resolve_path(&child_vpath, &self.role) {
            Some(p) => p,
            None => {
                reply.error(EACCES);
                return;
            }
        };

        match fs::create_dir(&host_path) {
            Ok(_) => {
                let ino = self.get_or_create_ino(&child_vpath, Some(host_path.clone()), true);
                let attr = self.stat_to_attr(ino, &Some(host_path), true, false);
                reply.entry(&TTL, &attr, 0);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                reply.error(EEXIST);
            }
            Err(_) => {
                reply.error(EACCES);
            }
        }
    }

    fn unlink(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        let name_str = match name.to_str() {
            Some(s) => s,
            None => {
                reply.error(ENOENT);
                return;
            }
        };

        let parent_vpath = self.path_to_ino.iter().find_map(|(path, &ino)| if ino == parent { Some(path.clone()) } else { None });
        let parent_vpath = match parent_vpath {
            Some(p) => p,
            None => {
                reply.error(ENOENT);
                return;
            }
        };

        let child_vpath = parent_vpath.join(name_str);

        // Deleting a file requires Write — Append alone (log-style grants)
        // intentionally does not permit removing the file outright.
        if let Err(reason) = self.realm.can_access(&child_vpath, Cap::Write, &self.role) {
            eprintln!("Realm VFS: denied unlink '{}': {}", child_vpath.display(), reason);
            reply.error(EACCES);
            return;
        }

        let host_path = match self.realm.secure_resolve_path(&child_vpath, &self.role) {
            Some(p) => p,
            None => {
                reply.error(EACCES);
                return;
            }
        };

        match fs::remove_file(&host_path) {
            Ok(_) => {
                if let Some(ino) = self.path_to_ino.remove(&child_vpath) {
                    self.inodes.remove(&ino);
                }
                reply.ok();
            }
            Err(e) => {
                reply.error(e.raw_os_error().unwrap_or(EACCES));
            }
        }
    }

    fn rmdir(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        let name_str = match name.to_str() {
            Some(s) => s,
            None => {
                reply.error(ENOENT);
                return;
            }
        };

        let parent_vpath = self.path_to_ino.iter().find_map(|(path, &ino)| if ino == parent { Some(path.clone()) } else { None });
        let parent_vpath = match parent_vpath {
            Some(p) => p,
            None => {
                reply.error(ENOENT);
                return;
            }
        };

        let child_vpath = parent_vpath.join(name_str);

        if let Err(reason) = self.realm.can_access(&child_vpath, Cap::Write, &self.role) {
            eprintln!("Realm VFS: denied rmdir '{}': {}", child_vpath.display(), reason);
            reply.error(EACCES);
            return;
        }

        let host_path = match self.realm.secure_resolve_path(&child_vpath, &self.role) {
            Some(p) => p,
            None => {
                reply.error(EACCES);
                return;
            }
        };

        // fs::remove_dir mirrors rmdir(2): non-recursive, fails with
        // ENOTEMPTY/EEXIST if the directory still has children. That's the
        // correct behavior here — recursive deletion (`rm -rf`) is driven by
        // the actor issuing a series of unlink/rmdir calls, not by us.
        match fs::remove_dir(&host_path) {
            Ok(_) => {
                if let Some(ino) = self.path_to_ino.remove(&child_vpath) {
                    self.inodes.remove(&ino);
                }
                reply.ok();
            }
            Err(e) => {
                reply.error(e.raw_os_error().unwrap_or(EACCES));
            }
        }
    }

    fn rename(
        &mut self,
        _req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        newparent: u64,
        newname: &OsStr,
        _flags: u32,
        reply: ReplyEmpty,
    ) {
        let (name_str, newname_str) = match (name.to_str(), newname.to_str()) {
            (Some(a), Some(b)) => (a, b),
            _ => {
                reply.error(ENOENT);
                return;
            }
        };

        let parent_vpath = self.path_to_ino.iter().find_map(|(path, &ino)| if ino == parent { Some(path.clone()) } else { None });
        let newparent_vpath = self.path_to_ino.iter().find_map(|(path, &ino)| if ino == newparent { Some(path.clone()) } else { None });

        let (parent_vpath, newparent_vpath) = match (parent_vpath, newparent_vpath) {
            (Some(a), Some(b)) => (a, b),
            _ => {
                reply.error(ENOENT);
                return;
            }
        };

        let old_vpath = parent_vpath.join(name_str);
        let new_vpath = newparent_vpath.join(newname_str);

        // Making the old name disappear is a removal (same gate as `unlink`);
        // establishing the new name is a creation (same gate as
        // `create`/`mkdir`). This is exactly what lets a build tool's
        // atomic write-then-rename pattern (e.g. rustc writing a temp file,
        // then renaming it onto the final `.rmeta` name) work anywhere
        // Create is already granted, with no separate "rename" capability.
        if let Err(reason) = self.realm.can_access(&old_vpath, Cap::Write, &self.role) {
            eprintln!("Realm VFS: denied rename source '{}': {}", old_vpath.display(), reason);
            reply.error(EACCES);
            return;
        }
        if let Err(reason) = self.realm.can_access(&new_vpath, Cap::Create, &self.role) {
            eprintln!("Realm VFS: denied rename destination '{}': {}", new_vpath.display(), reason);
            reply.error(EACCES);
            return;
        }

        let old_host = match self.realm.secure_resolve_path(&old_vpath, &self.role) {
            Some(p) => p,
            None => {
                reply.error(EACCES);
                return;
            }
        };
        let new_host = match self.realm.secure_resolve_path(&new_vpath, &self.role) {
            Some(p) => p,
            None => {
                reply.error(EACCES);
                return;
            }
        };

        match fs::rename(&old_host, &new_host) {
            Ok(_) => {
                // Drop stale bookkeeping for a pre-existing destination
                // (rename can silently overwrite), then move the source's
                // inode, if tracked, to the new virtual path/host location.
                if let Some(old_ino) = self.path_to_ino.remove(&new_vpath) {
                    self.inodes.remove(&old_ino);
                }
                if let Some(ino) = self.path_to_ino.remove(&old_vpath) {
                    if let Some(node) = self.inodes.get_mut(&ino) {
                        node.host_path = Some(new_host.clone());
                    }
                    self.path_to_ino.insert(new_vpath.clone(), ino);
                }
                reply.ok();
            }
            Err(e) => {
                reply.error(e.raw_os_error().unwrap_or(EACCES));
            }
        }
    }

    fn readdir(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        let node = match self.inodes.get(&ino) {
            Some(n) => n.clone(),
            None => {
                reply.error(ENOENT);
                return;
            }
        };

        let parent_vpath = self.path_to_ino.iter().find_map(|(path, &i)| if i == ino { Some(path.clone()) } else { None });
        let parent_vpath = match parent_vpath {
            Some(p) => p,
            None => {
                reply.error(ENOENT);
                return;
            }
        };

        let mut entries: Vec<(u64, FileType, String)> = vec![
            (ino, FileType::Directory, ".".to_string()),
            (ino, FileType::Directory, "..".to_string()),
        ];

        if let Some(ref host_path) = node.host_path {
            if let Ok(read_dir) = fs::read_dir(host_path) {
                for entry in read_dir.flatten() {
                    let file_name = entry.file_name().to_string_lossy().to_string();
                    let child_vpath = parent_vpath.join(&file_name);

                    if let Some(resolved_host) = self.realm.secure_resolve_path(&child_vpath, &self.role) {
                        let is_dir = resolved_host.is_dir();
                        let child_ino = self.get_or_create_ino(&child_vpath, Some(resolved_host), is_dir);
                        let ftype = if is_dir { FileType::Directory } else { FileType::RegularFile };
                        entries.push((child_ino, ftype, file_name));
                    }
                }
            }
        } else {
            // We are in a synthetic directory (e.g. FUSE Root or an intermediate folder like /workspace)
            let parent_str = parent_vpath.to_str().unwrap_or("");
            let parent_clean = parent_str.trim_matches('/');

            let mut virtual_dirs = std::collections::HashSet::new();

            for mount in &self.realm.mounts {
                let m_vpath = mount.virtual_path.trim_matches('/');
                
                if parent_clean.is_empty() {
                    // We are at root, add the first component of the mount
                    if !m_vpath.is_empty() {
                        let first_comp = m_vpath.split('/').next().unwrap_or(m_vpath);
                        virtual_dirs.insert(first_comp.to_string());
                    }
                } else if m_vpath.starts_with(parent_clean) {
                    // We are deeper in the tree
                    let remainder = m_vpath.strip_prefix(parent_clean).unwrap_or("").trim_start_matches('/');
                    if !remainder.is_empty() {
                        let next_comp = remainder.split('/').next().unwrap_or(remainder);
                        virtual_dirs.insert(next_comp.to_string());
                    }
                }
            }

            for child_name in virtual_dirs {
                let child_vpath = parent_vpath.join(&child_name);
                // For intermediate dirs, host_path is None. lookup() will handle resolving it when accessed
                let child_ino = self.get_or_create_ino(&child_vpath, None, true);
                entries.push((child_ino, FileType::Directory, child_name));
            }
            
            // Populate synthetic OS directories in root
            if parent_vpath == Path::new("/") {
                for &sys_dir in &[
                    "dev", "proc", "tmp", "bin", "usr", "lib", "lib64", "etc",
                    ".cargo", ".rustup",
                ] {
                    let child_vpath = PathBuf::from("/").join(sys_dir);
                    let child_ino = self.get_or_create_ino(&child_vpath, None, true);
                    entries.push((child_ino, FileType::Directory, sys_dir.to_string()));
                }
            }
        }

        for (i, (e_ino, ftype, name)) in entries.into_iter().enumerate().skip(offset as usize) {
            if reply.add(e_ino, (i + 1) as i64, ftype, name) {
                break;
            }
        }
        reply.ok();
    }
}
