#![cfg(target_os = "linux")]

use nix::mount::{mount, MsFlags};
use nix::sched::{unshare, CloneFlags};
use nix::unistd::{chdir, chroot, getgid, getuid};
use std::fs;
use std::path::{Path, PathBuf};

/// Builds a process trapped strictly inside the given VfsMask's
/// mounted view, without requiring root privileges, using unprivileged Linux
/// user + mount namespaces. `mount_path` should come from a live
/// `VfsMaskSession::mount_path()`.
pub fn build_masked_command(mount_path: &Path, cmd: &str, realm_name: &str, extra_binaries: &[PathBuf]) -> Result<std::process::Command, Box<dyn std::error::Error>> {
    let mount_path = mount_path.to_path_buf();
    let extra_binaries = extra_binaries.to_vec();
    let uid = getuid();
    let gid = getgid();
    let cmd_str = cmd.to_string();
    
    // Setup FHS-compliant paths on the host
    let mut global_usr_bin = std::path::PathBuf::new();
    let mut realm_etc = std::path::PathBuf::new();
    let mut realm_usr_local = std::path::PathBuf::new();
    if let Some(proj_dirs) = directories::ProjectDirs::from("", "", "inforno") {
        let data_dir = proj_dirs.data_dir();
        global_usr_bin = data_dir.join("mounts").join("usr").join("bin");
        let realm_vfs_dir = data_dir.join("realms").join(realm_name).join("vfs");
        realm_etc = realm_vfs_dir.join("etc");
        realm_usr_local = realm_vfs_dir.join("usr").join("local");
        
        let _ = std::fs::create_dir_all(&global_usr_bin);
        let _ = std::fs::create_dir_all(&realm_etc);
        let _ = std::fs::create_dir_all(&realm_usr_local);
    }

    unsafe {
        use std::os::unix::process::CommandExt;

        let mut command = if cmd_str.is_empty() {
            std::process::Command::new("/bin/bash")
        } else {
            let mut c = std::process::Command::new("/bin/bash");
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
        // Prioritize local realm tools, then global static tools, then cherry-picked host tools, then cargo
        command.env("PATH", "/usr/local/bin:/usr/local/sbin:/usr/bin:/usr/sbin:/bin:/.cargo/bin");

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
            fs::write("/proc/self/setgroups", "deny")?;
            fs::write("/proc/self/uid_map", format!("{} {} 1", uid, uid))?;
            fs::write("/proc/self/gid_map", format!("{} {} 1", gid, gid))?;

            let none: Option<&str> = None;

            // 2.5 Mark root as PRIVATE so unprivileged bind-mounts don't propagate 
            mount(Some("none"), Path::new("/"), none, MsFlags::MS_PRIVATE | MsFlags::MS_REC, none)?;

            // 3. Mount tmpfs over structural synthetic directories. 
            // This gives us kernel-memory scratchpads to build our FHS tree, completely bypassing FUSE!
            for dir in &["tmp", "bin", "usr"] {
                let target = mount_path.join(dir);
                mount(Some("tmpfs"), &target, Some("tmpfs"), MsFlags::empty(), none)?;
            }

            // 4. Bind-mount direct host directories (lib, lib64, dev, proc) over FUSE stubs
            for dir in &["/lib", "/lib64", "/dev", "/proc"] {
                let target = mount_path.join(dir.trim_start_matches('/'));
                if Path::new(dir).exists() {
                    mount(Some(*dir), &target, none, MsFlags::MS_BIND | MsFlags::MS_REC, none)?;
                    if dir.starts_with("/lib") {
                        mount(none, &target, none, MsFlags::MS_BIND | MsFlags::MS_REMOUNT | MsFlags::MS_REC | MsFlags::MS_RDONLY, none)?;
                    }
                }
            }

            // 5. Construct /usr inside the tmpfs
            let usr_target = mount_path.join("usr");
            for dir in &["/usr/lib", "/usr/lib64"] {
                let host_dir = Path::new(dir);
                let target = mount_path.join(dir.trim_start_matches('/'));
                if host_dir.exists() {
                    fs::create_dir_all(&target)?; // Creates safely inside tmpfs
                    mount(Some(host_dir), &target, none, MsFlags::MS_BIND | MsFlags::MS_REC, none)?;
                    mount(none, &target, none, MsFlags::MS_BIND | MsFlags::MS_REMOUNT | MsFlags::MS_REC | MsFlags::MS_RDONLY, none)?;
                }
            }

            // Global /usr/bin
            if global_usr_bin.exists() {
                let target = usr_target.join("bin");
                fs::create_dir_all(&target)?;
                mount(Some(&global_usr_bin), &target, none, MsFlags::MS_BIND | MsFlags::MS_REC, none)?;
                mount(none, &target, none, MsFlags::MS_BIND | MsFlags::MS_REMOUNT | MsFlags::MS_REC | MsFlags::MS_RDONLY, none)?;
            }

            // Realm /usr/local
            if realm_usr_local.exists() {
                let target = usr_target.join("local");
                fs::create_dir_all(&target)?;
                mount(Some(&realm_usr_local), &target, none, MsFlags::MS_BIND | MsFlags::MS_REC, none)?;
            }

            // Lock down /usr tmpfs
            mount(none, &usr_target, none, MsFlags::MS_BIND | MsFlags::MS_REMOUNT | MsFlags::MS_RDONLY, none)?;

            // 6. Construct /bin inside the tmpfs
            let bin_target = mount_path.join("bin");
            for bin in &["bash", "sh", "ls", "cat", "grep", "env", "pwd", "echo", "rm", "mkdir", "cp", "mv", "rmdir", "chmod", "true", "false"] {
                let host_bin = Path::new("/bin").join(bin);
                if host_bin.exists() {
                    let target_bin = bin_target.join(bin);
                    fs::File::create(&target_bin)?; // Safe inside tmpfs!
                    mount(Some(&host_bin), &target_bin, none, MsFlags::MS_BIND, none)?;
                    mount(none, &target_bin, none, MsFlags::MS_BIND | MsFlags::MS_REMOUNT | MsFlags::MS_RDONLY, none)?;
                }
            }

            // 6b. Realm-configured extra binaries (compilers, linkers, etc.),
            // given as absolute host paths via realm2.yml's `binaries:` list.
            // Mounted the same way, at the same time, as the coreutils above.
            for host_bin in &extra_binaries {
                if host_bin.exists() {
                    if let Some(bin_name) = host_bin.file_name() {
                        let target_bin = bin_target.join(bin_name);
                        fs::File::create(&target_bin)?; // Safe inside tmpfs!
                        mount(Some(host_bin), &target_bin, none, MsFlags::MS_BIND, none)?;
                        mount(none, &target_bin, none, MsFlags::MS_BIND | MsFlags::MS_REMOUNT | MsFlags::MS_RDONLY, none)?;
                    }
                }
            }

            mount(none, &bin_target, none, MsFlags::MS_BIND | MsFlags::MS_REMOUNT | MsFlags::MS_RDONLY, none)?;

            // 7. Rust toolchains
            if !host_home.is_empty() {
                for dir in &[".cargo", ".rustup"] {
                    let host_dir = Path::new(&host_home).join(dir);
                    let target = mount_path.join(dir);
                    if host_dir.exists() && target.exists() {
                        mount(Some(&host_dir), &target, none, MsFlags::MS_BIND | MsFlags::MS_REC, none)?;
                        mount(none, &target, none, MsFlags::MS_BIND | MsFlags::MS_REMOUNT | MsFlags::MS_REC | MsFlags::MS_RDONLY, none)?;
                    }
                }
            }

            // 8. Realm /etc (Read-Write base, Read-Only file overlays)
            let etc_target = mount_path.join("etc");
            if realm_etc.exists() {
                mount(Some(&realm_etc), &etc_target, none, MsFlags::MS_BIND | MsFlags::MS_REC, none)?;
            }
            for item in &["resolv.conf", "hosts", "localtime", "timezone", "ssl", "pki", "ca-certificates"] {
                let host_item = Path::new("/etc").join(item);
                if host_item.exists() {
                    let target_item = etc_target.join(item);
                    if host_item.is_dir() {
                        fs::create_dir_all(&target_item)?;
                    } else {
                        fs::File::create(&target_item)?;
                    }
                    mount(Some(&host_item), &target_item, none, MsFlags::MS_BIND | MsFlags::MS_REC, none)?;
                    mount(none, &target_item, none, MsFlags::MS_BIND | MsFlags::MS_REMOUNT | MsFlags::MS_REC | MsFlags::MS_RDONLY, none)?;
                }
            }

            let fake_passwd = mount_path.join("tmp/passwd");
            fs::write(&fake_passwd, format!("actor:x:{}:{}:Realm Actor:/:/bin/bash\nroot:x:0:0:root:/:/bin/bash\n", uid, gid))?;
            let passwd_target = etc_target.join("passwd");
            fs::File::create(&passwd_target)?;
            mount(Some(&fake_passwd), &passwd_target, none, MsFlags::MS_BIND, none)?;

            let fake_group = mount_path.join("tmp/group");
            fs::write(&fake_group, format!("actor:x:{}:\nroot:x:0:\n", gid))?;
            let group_target = etc_target.join("group");
            fs::File::create(&group_target)?;
            mount(Some(&fake_group), &group_target, none, MsFlags::MS_BIND, none)?;

            // 9. Chroot!
            chroot(&mount_path)?;
            chdir("/")?;

            Ok(())
        });

        Ok(command)
    }
}

pub fn spawn_masked_command(mount_path: &Path, cmd: &str, realm_name: &str, extra_binaries: &[PathBuf]) -> Result<std::process::Child, Box<dyn std::error::Error>> {
    Ok(build_masked_command(mount_path, cmd, realm_name, extra_binaries)?.spawn()?)
}
