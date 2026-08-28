#![cfg(target_os = "linux")]

use nix::mount::{mount, MsFlags};
use nix::sched::{unshare, CloneFlags};
use nix::unistd::{chdir, chroot, getgid, getuid};
use std::fs;
use std::path::Path;
use std::process::Command;
use crate::realm::ActiveRealm;
use crate::realm_vfs::RealmFuseFS;

pub struct RealmJailSession {
    mount_dir: tempfile::TempDir,
    _session_handle: fuser::BackgroundSession,
}

impl RealmJailSession {
    /// Spawns the background FUSE mount representing the Realm VFS.
    pub fn spawn_vfs(realm: ActiveRealm) -> Result<Self, Box<dyn std::error::Error>> {
        let mount_dir = tempfile::TempDir::new()?;
        let fs = RealmFuseFS::new(realm);

        // Intentionally omitting MountOption::RO to allow FUSE to selectively handle write operations.
        let session = fuser::spawn_mount2(fs, mount_dir.path(), &[
            fuser::MountOption::FSName("inforno_realm".to_string()),
        ])?;

        Ok(Self {
            mount_dir,
            _session_handle: session,
        })
    }

    pub fn mount_path(&self) -> &Path {
        self.mount_dir.path()
    }

    /// Spawns a shell or child command jailed strictly inside the Realm VFS
    /// without requiring root privileges, safely bringing in OS dependencies.
    pub fn spawn_jailed_command(&self, cmd: &str) -> Result<std::process::Child, Box<dyn std::error::Error>> {
        let mount_path = self.mount_path().to_path_buf();
        let uid = getuid();
        let gid = getgid();
        let cmd_str = cmd.to_string();

        unsafe {
            use std::os::unix::process::CommandExt;

            let mut command = Command::new("/bin/sh");
            command.arg("-c").arg(cmd_str);

            command.pre_exec(move || {
                // 1. Unshare User and Mount namespaces (unprivileged isolation)
                unshare(CloneFlags::CLONE_NEWUSER | CloneFlags::CLONE_NEWNS)?;

                // 2. Map current UID/GID to the SAME UID/GID inside the new namespace.
                // This ensures FUSE-assigned UID 0 (root) maps to `nobody` for read-only files.
                fs::write("/proc/self/setgroups", "deny")?;
                fs::write("/proc/self/uid_map", format!("{} {} 1", uid, uid))?;
                fs::write("/proc/self/gid_map", format!("{} {} 1", gid, gid))?;

                let none: Option<&str> = None;

                // 3. Mount over synthetic OS directories provided by our FUSE layer
                mount(Some("proc"), &mount_path.join("proc"), Some("proc"), MsFlags::empty(), none)?;
                mount(Some("tmpfs"), &mount_path.join("tmp"), Some("tmpfs"), MsFlags::empty(), none)?;
                
                // Bind-mount host's /dev so tools have access to /dev/null, /dev/urandom, etc.
                mount(Some("/dev"), &mount_path.join("dev"), none, MsFlags::MS_BIND | MsFlags::MS_REC, none)?;

                // 4. Trap the process inside the FUSE environment
                chroot(&mount_path)?;
                chdir("/")?;

                Ok(())
            });

            Ok(command.spawn()?)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::realm::{ActiveRealm, RealmConfig, RealmMountConfig, GlobExpr};
    use indexmap::IndexMap;
    use std::fs::{self, File};
    use std::io::Write;

    #[test]
    fn test_jail_permissions() -> Result<(), Box<dyn std::error::Error>> {
        // 1. Setup host directory and files
        let host_dir = tempfile::tempdir()?;
        
        let normal_path = host_dir.path().join("normal.txt");
        let hidden_path = host_dir.path().join("hidden.txt");
        let ro_path = host_dir.path().join("readonly.txt");

        File::create(&normal_path)?.write_all(b"normal_data\n")?;
        File::create(&hidden_path)?.write_all(b"hidden_data\n")?;
        File::create(&ro_path)?.write_all(b"ro_data\n")?;

        // 2. Configure the Boolean AST Policy
        let mut mounts = IndexMap::new();
        mounts.insert("/workspace".to_string(), RealmMountConfig {
            host: host_dir.path().to_path_buf(),
            read_only: false,
            hide_if: Some(GlobExpr::Match(vec!["hidden.txt".to_string()])),
            read_only_if: Some(GlobExpr::Match(vec!["readonly.txt".to_string()])),
            wildcards: vec![],
            ignore: vec![],
            description: None,
            kind: None,
        });

        let config = RealmConfig {
            default_workspace: None,
            hide_if: None,
            read_only_if: None,
            wildcards: IndexMap::new(),
            mounts,
        };

        // 3. Compile Realm and Spawn FUSE Driver
        let active_realm = ActiveRealm::from_config("test_realm".to_string(), config)?;
        let jail = RealmJailSession::spawn_vfs(active_realm)?;

        // Give FUSE a moment to fully initialize in the background thread
        std::thread::sleep(std::time::Duration::from_millis(200));

        let fuse_workspace = jail.mount_path().join("workspace");
        let normal_vpath = fuse_workspace.join("normal.txt");
        let hidden_vpath = fuse_workspace.join("hidden.txt");
        let ro_vpath = fuse_workspace.join("readonly.txt");

        // --- Test 1: Hidden files return ENOENT to the host ---
        assert!(!hidden_vpath.exists(), "Hidden file should be completely invisible to VFS");

        // --- Test 2: Normal files can be read and written ---
        let data = fs::read_to_string(&normal_vpath)?;
        assert_eq!(data, "normal_data\n");
        fs::write(&normal_vpath, "modified\n").expect("Normal write failed");
        
        // Verify write propagated back to the underlying host filesystem
        let host_data = fs::read_to_string(&normal_path)?;
        assert_eq!(host_data, "modified\n");

        // --- Test 3: Read-only files are visible but reject writes via FUSE EACCES ---
        assert!(ro_vpath.exists(), "Read-only file should be visible to VFS");
        let read_ro_data = fs::read_to_string(&ro_vpath)?;
        assert_eq!(read_ro_data, "ro_data\n");
        
        let err = fs::write(&ro_vpath, "hack").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied, "Write should be denied by FUSE Layer");

        // Clean up FUSE session gracefully
        drop(jail);

        Ok(())
    }
}
