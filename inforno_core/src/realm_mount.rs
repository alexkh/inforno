#![cfg(target_os = "linux")]

use std::path::Path;
use crate::realm::ActiveRealm;
use crate::realm_vfs::RealmFuseFS;

// real_mount.rs mounts the vfs, realm_process.rs runs a chrooted process in vfs

/// A `VfsMaskSession` is NOT an isolated sandbox or VM boundary — it is a thin
/// virtual filesystem layer over real host directories, in two senses at once:
/// it masks PATHS (presenting short, role-agnostic virtual paths in place of
/// long or developer-specific real ones) and it masks PERMISSIONS (hiding,
/// read-only, and create rules applied per access level/role). It does not
/// copy, snapshot, or isolate the underlying files. Those files are ordinary
/// files on disk, may be reachable through multiple concurrent VfsMasks (or
/// directly, outside any VfsMask) at the same time, and writes made through
/// one VfsMask are immediately visible everywhere else those same host paths
/// are reachable. Concurrent access from multiple VfsMasks, or from outside
/// all VfsMasks, can race exactly as with any two processes writing to the
/// same file without coordination.
pub struct VfsMaskSession {
    // Declaration order IS drop order for struct fields in Rust (top to
    // bottom — unlike local variables, which drop in reverse). The FUSE
    // session must be unmounted before we ever try to remove the directory
    // it's mounted on: removing a still-mounted directory just fails
    // (EBUSY), and `TempDir`'s Drop silently swallows that error, leaving
    // an orphaned mountpoint behind with no daemon left to answer it. Any
    // later access to it (even an `ls -la` statting it) then hangs forever
    // waiting for a FUSE reply that will never come.
    _session_handle: fuser::BackgroundSession,
    mount_dir: tempfile::TempDir,
}

impl VfsMaskSession {
    /// Spawns the background FUSE mount representing the Realm's masked view,
    /// as enforced for the given Role for the lifetime of the session — a
    /// different Role needs its own `spawn_vfs` call and its own mount.
    pub fn spawn_vfs(realm: ActiveRealm, role: String) -> Result<Self, Box<dyn std::error::Error>> {
        let base_dir = std::env::temp_dir().join("autorno_mounts");
        std::fs::create_dir_all(&base_dir)?;
        let mount_dir = tempfile::Builder::new().prefix("vfs_").tempdir_in(&base_dir)?;
        
        let fs = RealmFuseFS::new(realm, role);

        // Intentionally omitting MountOption::RO to allow FUSE to selectively handle write operations.
        let session = fuser::spawn_mount2(fs, mount_dir.path(), &[
            fuser::MountOption::FSName("inforno_vfsmask".to_string()),
        ])?;

        Ok(Self {
            _session_handle: session,
            mount_dir,
        })
    }

    pub fn mount_path(&self) -> &Path {
        self.mount_dir.path()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::realm::{ActiveRealm, RealmConfig, RealmMountConfig, GlobExpr, RoleConfig, Tier, Power, Cap};
    use indexmap::IndexMap;
    use std::fs::{self, File};
    use std::io::Write;
    use std::os::unix::fs::FileExt;

    #[test]
    fn test_vfsmask_permissions() -> Result<(), Box<dyn std::error::Error>> {
        // 1. Setup host directory and files
        let host_dir = tempfile::tempdir()?;

        let normal_path = host_dir.path().join("normal.txt");
        // Dotfile automatically tests the `is_builtin_dotfile_path` hiding mechanism
        let hidden_path = host_dir.path().join(".hidden.txt");
        let ro_path = host_dir.path().join("readonly.txt");
        let append_path = host_dir.path().join("append_only.txt");

        File::create(&normal_path)?.write_all(b"normal_data\n")?;
        File::create(&hidden_path)?.write_all(b"hidden_data\n")?;
        File::create(&ro_path)?.write_all(b"ro_data\n")?;
        File::create(&append_path)?.write_all(b"log_start\n")?;

        // 2. Configure the Powers Policy
        let mut mounts = IndexMap::new();
        mounts.insert("/workspace".to_string(), RealmMountConfig {
            host: host_dir.path().to_path_buf(),
            read_only: false,
            intro: None,
        });

        let mut roles = IndexMap::new();
        roles.insert("tester".to_string(), RoleConfig {
            tier: Tier(2),
            intro: "Test Role".to_string(),
            boss: None,
            powers: vec![],
            bin: None,
        });

        let mut tiers = std::collections::BTreeMap::new();
        tiers.insert(2, crate::realm::TierConfig { bin: None, powers: vec![
            Power {
                span: GlobExpr::Match { match_globs: vec!["**".to_string()] },
                caps: vec![Cap::Read],
                intro: Some("Read everything".to_string()),
                overrides: None,
            },
            Power {
                // Intentionally exclude readonly.txt and .hidden.txt from having Write power
                span: GlobExpr::Match { match_globs: vec!["normal.txt".to_string()] },
                caps: vec![Cap::Write],
                intro: Some("Write access to normal files".to_string()),
                overrides: None,
            },
            Power {
                span: GlobExpr::Match { match_globs: vec!["append_only.txt".to_string()] },
                caps: vec![Cap::Append],
                intro: Some("Append access to logs".to_string()),
                overrides: None,
            }
        ]});

        let config = RealmConfig {
            expressions: IndexMap::new(),
            mounts,
            places: IndexMap::new(),
            sandboxes: IndexMap::new(),
            roles,
            tiers,
            bin: None,
        };

        // 3. Compile Realm and Spawn FUSE Driver
        let active_realm = ActiveRealm::from_config("test_realm".to_string(), config)?;
        let vfsmask = VfsMaskSession::spawn_vfs(active_realm, "tester".to_string())?;

        // Give FUSE a moment to fully initialize in the background thread
        std::thread::sleep(std::time::Duration::from_millis(200));

        let fuse_workspace = vfsmask.mount_path().join("workspace");
        let normal_vpath = fuse_workspace.join("normal.txt");
        let hidden_vpath = fuse_workspace.join("hidden.txt");
        let ro_vpath = fuse_workspace.join("readonly.txt");
        let append_vpath = fuse_workspace.join("append_only.txt");

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
        // (Even though the Actor holds a Write grant for this file, the hard `read_only_if` on the mount overrides it)
        assert!(ro_vpath.exists(), "Read-only file should be visible to VFS");
        let read_ro_data = fs::read_to_string(&ro_vpath)?;
        assert_eq!(read_ro_data, "ro_data\n");

        let err = fs::write(&ro_vpath, "hack").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied, "Write should be denied by FUSE Layer");

        // --- Test 4: Append-only enforcement ---
        assert!(append_vpath.exists(), "Append file should be visible to VFS");
        
        // 4a. Read should work via the `**` global read grant
        let read_app_data = fs::read_to_string(&append_vpath)?;
        assert_eq!(read_app_data, "log_start\n");

        // 4b. Truncate via File::create (O_TRUNC) should fail because we lack Write
        let err_trunc = File::create(&append_vpath).unwrap_err();
        assert_eq!(err_trunc.kind(), std::io::ErrorKind::PermissionDenied, "Truncation should be denied by FUSE Layer");

        // 4c. Arbitrary offset write (e.g. overwriting the start of the file) should fail
        let mut file_no_append = fs::OpenOptions::new().write(true).open(&append_vpath)?;
        let err_over = file_no_append.write_at(b"hack", 0).unwrap_err();
        assert_eq!(err_over.raw_os_error(), Some(libc::EINVAL), "Non-EOF write should be denied with EINVAL");

        // 4d. Proper append (O_APPEND) should succeed
        let mut app_file = fs::OpenOptions::new().append(true).open(&append_vpath)?;
        app_file.write_all(b"new_log\n")?;

        let final_host_data = fs::read_to_string(&append_path)?;
        assert_eq!(final_host_data, "log_start\nnew_log\n");

        // Clean up FUSE session gracefully
        drop(vfsmask);

        Ok(())
    }
}
