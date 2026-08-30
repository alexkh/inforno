#![cfg(target_os = "linux")]

use nix::mount::{mount, MsFlags};
use nix::sched::{unshare, CloneFlags};
use nix::unistd::{chdir, chroot, getgid, getuid};
use std::fs;
use std::path::Path;
use std::process::Command;

/// Spawns a shell or child command trapped strictly inside the given VfsMask's
/// mounted view, without requiring root privileges, using unprivileged Linux
/// user + mount namespaces. `mount_path` should come from a live
/// `VfsMaskSession::mount_path()`.
pub fn spawn_masked_command(mount_path: &Path, cmd: &str) -> Result<std::process::Child, Box<dyn std::error::Error>> {
    let mount_path = mount_path.to_path_buf();
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

            // 4. Trap the process inside the masked VFS view
            chroot(&mount_path)?;
            chdir("/")?;

            Ok(())
        });

        Ok(command.spawn()?)
    }
}
