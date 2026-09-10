#![cfg(target_os = "linux")]

use nix::mount::{mount, MsFlags};
use nix::sched::{unshare, CloneFlags};
use nix::unistd::{chdir, chroot, getgid, getuid};
use std::fs;
use std::path::Path;
use std::process::Command;

/// Builds a process trapped strictly inside the given VfsMask's
/// mounted view, without requiring root privileges, using unprivileged Linux
/// user + mount namespaces. `mount_path` should come from a live
/// `VfsMaskSession::mount_path()`.
pub fn build_masked_command(mount_path: &Path, cmd: &str) -> Result<std::process::Command, Box<dyn std::error::Error>> {
    let mount_path = mount_path.to_path_buf();
    let uid = getuid();
    let gid = getgid();
    let cmd_str = cmd.to_string();

    unsafe {
        use std::os::unix::process::CommandExt;

        let mut command = if cmd_str.is_empty() {
            Command::new("/bin/bash")
        } else {
            let mut c = Command::new("/bin/bash");
            c.arg("-c").arg(cmd_str);
            c
        };

                        // Override host environment variables to match our synthetic identity
                command.env("HOME", "/");
                command.env("USER", "actor");
                command.env("LOGNAME", "actor");
                command.env("CARGO_HOME", "/.cargo");
                command.env("RUSTUP_HOME", "/.rustup");

                let host_home = std::env::var("HOME").unwrap_or_default();
                let host_path = std::env::var("PATH").unwrap_or_default();
                command.env("PATH", format!("/.cargo/bin:{}", host_path));

                command.pre_exec(move || {
            // 1. Unshare User, Mount, and UTS namespaces (UTS allows hostname spoofing)
            unshare(CloneFlags::CLONE_NEWUSER | CloneFlags::CLONE_NEWNS | CloneFlags::CLONE_NEWUTS)?;
            
            let name = b"realm";
            unsafe {
                if libc::sethostname(name.as_ptr() as *const libc::c_char, name.len()) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }

            // 2. Map current UID/GID to the SAME UID/GID inside the new namespace.
            // This ensures FUSE-assigned UID 0 (root) maps to `nobody` for read-only files.
            fs::write("/proc/self/setgroups", "deny")?;
            fs::write("/proc/self/uid_map", format!("{} {} 1", uid, uid))?;
            fs::write("/proc/self/gid_map", format!("{} {} 1", gid, gid))?;

            let none: Option<&str> = None;

            // 2.5 Mark root as PRIVATE so unprivileged bind-mounts don't propagate 
            // back to the host, which the kernel otherwise blocks with EPERM.
            mount(Some("none"), Path::new("/"), none, MsFlags::MS_PRIVATE | MsFlags::MS_REC, none)?;

            // 3. Mount over synthetic OS directories provided by our FUSE layer
            mount(Some("tmpfs"), &mount_path.join("tmp"), Some("tmpfs"), MsFlags::empty(), none)?;

            // Bind-mount host OS binaries and state so shell and tools like `ls` exist in the jail
            for dir in &["/bin", "/usr", "/lib", "/lib64", "/etc", "/dev", "/proc"] {
                let target = mount_path.join(dir.trim_start_matches('/'));
                // Ignore errors here since some systems might not have /lib64
                let _ = mount(Some(*dir), &target, none, MsFlags::MS_BIND | MsFlags::MS_REC, none);
            }

            // Bind-mount Rust toolchains if they exist on the host (Read-Only)
            if !host_home.is_empty() {
                for dir in &[".cargo", ".rustup"] {
                    let host_dir = Path::new(&host_home).join(dir);
                    if host_dir.exists() {
                        let target = mount_path.join(dir);
                        let _ = fs::create_dir_all(&target);
                        
                        // Step 1: Bind mount the directory
                        let _ = mount(Some(&host_dir), &target, none, MsFlags::MS_BIND | MsFlags::MS_REC, none);
                        
                        // Step 2: Remount the bind as Read-Only to completely protect the host
                        let _ = mount(
                            none, 
                            &target, 
                            none, 
                            MsFlags::MS_BIND | MsFlags::MS_REMOUNT | MsFlags::MS_REC | MsFlags::MS_RDONLY, 
                            none
                        );
                    }
                }
            }

            // Fake the user identity by overlaying synthetic /etc/passwd and /etc/group
            let fake_passwd = mount_path.join("tmp/passwd");
            fs::write(&fake_passwd, format!("actor:x:{}:{}:Realm Actor:/:/bin/bash\nroot:x:0:0:root:/:/bin/bash\n", uid, gid))?;
            mount(Some(&fake_passwd), &mount_path.join("etc/passwd"), none, MsFlags::MS_BIND, none)?;

            let fake_group = mount_path.join("tmp/group");
            fs::write(&fake_group, format!("actor:x:{}:\nroot:x:0:\n", gid))?;
            mount(Some(&fake_group), &mount_path.join("etc/group"), none, MsFlags::MS_BIND, none)?;

            // 4. Trap the process inside the masked VFS view
            chroot(&mount_path)?;
            chdir("/")?;

            Ok(())
        });

        Ok(command)
    }
}

pub fn spawn_masked_command(mount_path: &Path, cmd: &str) -> Result<std::process::Child, Box<dyn std::error::Error>> {
    Ok(build_masked_command(mount_path, cmd)?.spawn()?)
}
