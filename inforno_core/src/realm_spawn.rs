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
pub fn build_masked_command(mount_path: &Path, cmd: &str, realm_name: &str, extra_binaries: &[PathBuf], allowed_envs: &[(String, String)]) -> Result<std::process::Command, Box<dyn std::error::Error>> {
    let mount_path = mount_path.to_path_buf();
    let extra_binaries = extra_binaries.to_vec();
    let allowed_envs = allowed_envs.to_vec();
    let uid = getuid();
    let gid = getgid();
    let cmd_str = cmd.to_string();
    
    // Setup FHS-compliant paths on the host.
    // `global_bin_dir` (~/.local/share/inforno/mounts/bin) and
    // `realm_bin_dir` (~/.local/share/inforno/realms/<realm>/mounts/bin)
    // are reserved for a future feature: staging/installing binaries
    // outside of whatever the realm's own `bin:` glob cascade selects from
    // the host — the former a global, read-only, inforno-managed bin dir
    // shared by all realms, the latter a per-realm, writable one for
    // installing binaries from inside the FUSE shell. Neither is mounted
    // yet; extra binaries are now placed by mirroring their real host
    // location instead (see step 6b below). Only the directories get
    // created here so the paths exist once that mounting lands.
    let mut global_bin_dir = std::path::PathBuf::new();
    let mut realm_etc = std::path::PathBuf::new();
    let mut realm_bin_dir = std::path::PathBuf::new();
    if let Some(proj_dirs) = directories::ProjectDirs::from("", "", "inforno") {
        let data_dir = proj_dirs.data_dir();
        global_bin_dir = data_dir.join("mounts").join("bin");
        let realm_vfs_dir = data_dir.join("realms").join(realm_name).join("vfs");
        realm_etc = realm_vfs_dir.join("etc");
        realm_bin_dir = data_dir.join("realms").join(realm_name).join("mounts").join("bin");
        
        let _ = std::fs::create_dir_all(&global_bin_dir);
        let _ = std::fs::create_dir_all(&realm_etc);
        let _ = std::fs::create_dir_all(&realm_bin_dir);
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
        
        // Clear all inherited environment variables first
        command.env_clear();

        // Inject the cherry-picked host environment variables
        for (k, v) in &allowed_envs {
            command.env(k, v);
        }

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
            for dir in &["/usr/lib", "/usr/lib64", "/usr/include", "/usr/share"] {
                let host_dir = Path::new(dir);
                let target = mount_path.join(dir.trim_start_matches('/'));
                if host_dir.exists() {
                    fs::create_dir_all(&target)?; // Creates safely inside tmpfs
                    mount(Some(host_dir), &target, none, MsFlags::MS_BIND | MsFlags::MS_REC, none)?;
                    mount(none, &target, none, MsFlags::MS_BIND | MsFlags::MS_REMOUNT | MsFlags::MS_REC | MsFlags::MS_RDONLY, none)?;
                }
            }

            // /usr/bin and /usr/local/bin as plain tmpfs directories.
            // Populated below (6b) by bind-mounting extra_binaries
            // individually according to where each was actually found on
            // the host, the same way the coreutils below populate /bin,
            // instead of bind-mounting a whole inforno-managed directory
            // over them (that's `global_bin_dir` / `realm_bin_dir` from
            // above -- disabled for now, see comment there). Must happen
            // before "Lock down /usr tmpfs" below, while the tmpfs is
            // still writable.
            let usr_bin_target = usr_target.join("bin");
            fs::create_dir_all(&usr_bin_target)?;
            let usr_local_bin_target = usr_target.join("local").join("bin");
            fs::create_dir_all(&usr_local_bin_target)?;

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
            // given as absolute host paths via realm2.yml's `binaries:` list
            // (resolved in `bin_search_dirs()`/`allowed_binaries` against
            // /usr/local/{bin,sbin}, /usr/{bin,sbin}, and /{bin,sbin}).
            // Each one is bind-mounted into whichever of /bin, /usr/bin, or
            // /usr/local/bin mirrors its *host* parent directory, so e.g. a
            // host-side /usr/local/bin tool lands in the chroot's
            // /usr/local/bin too, instead of always flattening into /bin.
            for host_bin in &extra_binaries {
                if !host_bin.exists() {
                    continue;
                }
                let Some(bin_name) = host_bin.file_name() else { continue };
                let target_dir = match host_bin.parent().and_then(|p| p.to_str()) {
                    Some("/usr/local/bin") | Some("/usr/local/sbin") => &usr_local_bin_target,
                    Some("/usr/bin") | Some("/usr/sbin") => &usr_bin_target,
                    _ => &bin_target, // /bin, /sbin, or anything unrecognized
                };
                let target_bin = target_dir.join(bin_name);
                fs::File::create(&target_bin)?; // Safe inside tmpfs!
                mount(Some(host_bin), &target_bin, none, MsFlags::MS_BIND, none)?;
                mount(none, &target_bin, none, MsFlags::MS_BIND | MsFlags::MS_REMOUNT | MsFlags::MS_RDONLY, none)?;
            }

            mount(none, &bin_target, none, MsFlags::MS_BIND | MsFlags::MS_REMOUNT | MsFlags::MS_RDONLY, none)?;

            // Lock down /usr tmpfs. Moved to here (after 6b) so it happens
            // only once /usr/bin and /usr/local/bin have been populated.
            // Non-recursive on purpose: /usr/bin and /usr/local/bin are
            // plain directories inside this same tmpfs (not separate
            // mounts), so this alone stops any new entries from being
            // created directly under /usr, /usr/bin, /usr/local, or
            // /usr/local/bin. It doesn't touch /usr/lib, /usr/include,
            // /usr/share, or the individual extra-binary bind-mounts placed
            // into /usr/bin and /usr/local/bin above -- those are their own
            // mounts and already carry the readonly mode they were given.
            mount(none, &usr_target, none, MsFlags::MS_BIND | MsFlags::MS_REMOUNT | MsFlags::MS_RDONLY, none)?;

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

pub fn spawn_masked_command(mount_path: &Path, cmd: &str, realm_name: &str, extra_binaries: &[PathBuf], allowed_envs: &[(String, String)]) -> Result<std::process::Child, Box<dyn std::error::Error>> {
    Ok(build_masked_command(mount_path, cmd, realm_name, extra_binaries, allowed_envs)?.spawn()?)
}
