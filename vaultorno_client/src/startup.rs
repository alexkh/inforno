//! What an app does first thing in `main`, while it is still its only
//! thread (design doc §13). In this order:
//!
//! 1. Record the names of the variables it was started with, which the
//!    client sends in `Hello` (§7.5). They come from `/proc/self/environ`,
//!    which a non-dumpable process can no longer open (it then belongs to
//!    root), so this comes before step 4.
//! 2. Keep the Vulkan loader to the system's drivers and away from implicit
//!    layers, which it would otherwise load from the home folder with no
//!    variable involved.
//! 3. Remove variables no library should see: ones that would send
//!    key-carrying requests through a proxy, trust other root certificates,
//!    log TLS secrets or make OpenSSL load code, plus the app's own.
//! 4. Make the process non-dumpable, so other programs running as the user
//!    can't attach to it or read its memory, even where `ptrace_scope`
//!    would let them.
//!
//! Steps 2 and 3 change the environment, which is only sound while no other
//! thread can read it, so `startup` first checks that the process has
//! exactly one thread, and otherwise does nothing.
//!
//! A program that doesn't call `startup` still works: the client then reads
//! `/proc/self/environ` when it opens a vault.

use std::sync::OnceLock;

use crate::Error;
use crate::protocol::MAX_ENVIRONMENT;

/// The starting environment's names, or why they couldn't be read.
static STARTED: OnceLock<Result<Vec<String>, String>> = OnceLock::new();

/// Variables that would make a TLS or HTTP library trust other roots, use a
/// proxy, log session secrets, or load OpenSSL configuration, engines or
/// providers (which is code).
pub const NETWORK_ENV: &[&str] = &[
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "SSLKEYLOGFILE",
    "OPENSSL_CONF",
    "OPENSSL_ENGINES",
    "OPENSSL_MODULES",
    "HTTP_PROXY",
    "http_proxy",
    "HTTPS_PROXY",
    "https_proxy",
    "ALL_PROXY",
    "all_proxy",
    "NO_PROXY",
    "no_proxy",
];

/// What [`startup`] does. Every step but the first is Linux only.
#[derive(Debug, Clone, Copy)]
pub struct Startup<'a> {
    /// Make the process non-dumpable.
    pub non_dumpable: bool,
    /// Pin the Vulkan loader to the system's drivers, without implicit layers.
    pub pin_vulkan: bool,
    /// Remove [`NETWORK_ENV`].
    pub remove_network_env: bool,
    /// Other variables to remove.
    pub remove_env: &'a [&'a str],
}

/// Runs the startup steps (see the module docs). Call it first thing in
/// `main`, before anything starts a thread, the async runtime included.
pub fn startup(opts: &Startup<'_>) -> Result<(), Error> {
    if STARTED.get().is_some() {
        return Err(Error::Startup("startup() was already called".into()));
    }
    #[cfg(target_os = "linux")]
    {
        linux::run(opts)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = opts;
        Ok(())
    }
}

/// The starting environment's names (§7.5): as `startup` recorded them, or
/// read now for a program that didn't call it.
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) fn started_environment() -> Result<Vec<String>, Error> {
    match STARTED.get() {
        Some(recorded) => recorded.clone(),
        None => read_proc_environ(),
    }
    .map_err(Error::Protocol)
}

/// Names of the variables this program was started with, from
/// `/proc/self/environ`: what it was launched with, not what it has set
/// since, so an app may still set `VK_LOADER_LAYERS_DISABLE` and the like
/// for itself (§13).
#[cfg_attr(not(unix), allow(dead_code))]
fn read_proc_environ() -> Result<Vec<String>, String> {
    let raw = std::fs::read("/proc/self/environ").map_err(|e| {
        format!("can't read this program's environment from /proc/self/environ: {e}")
    })?;
    let names: Vec<String> = raw
        .split(|&b| b == 0)
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            let name = entry.split(|&b| b == b'=').next().unwrap_or(entry);
            // Lossily, so a name that isn't UTF-8 can't slip past a prefix.
            String::from_utf8_lossy(name).into_owned()
        })
        .collect();
    if names.len() > MAX_ENVIRONMENT {
        return Err(format!(
            "this program has more than {MAX_ENVIRONMENT} environment variables"
        ));
    }
    Ok(names)
}

#[cfg(target_os = "linux")]
mod linux {
    use std::fs;
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;

    use super::{NETWORK_ENV, STARTED, Startup, read_proc_environ};
    use crate::Error;

    /// Where the system's Vulkan driver manifests live; the loader's other
    /// places include the home folder.
    const VULKAN_DRIVER_DIRS: &[&str] = &["/usr/share/vulkan/icd.d", "/etc/vulkan/icd.d"];

    pub(super) fn run(opts: &Startup<'_>) -> Result<(), Error> {
        let threads = fs::read_dir("/proc/self/task")
            .map_err(|e| Error::Startup(format!("can't count this process's threads: {e}")))?
            .count();
        if threads != 1 {
            return Err(Error::Startup(format!(
                "{threads} threads are running; call startup() first thing in main, \
                 before anything starts a thread"
            )));
        }
        // Before going non-dumpable, which closes /proc/self/environ.
        let _ = STARTED.set(read_proc_environ());

        if opts.pin_vulkan {
            let drivers = system_vulkan_drivers();
            set_env("VK_LOADER_LAYERS_DISABLE", "~implicit~");
            set_env("VK_DRIVER_FILES", &drivers);
            // The same, for loaders older than VK_DRIVER_FILES (1.3.207).
            set_env("VK_ICD_FILENAMES", &drivers);
        }
        let network: &[&str] = if opts.remove_network_env {
            NETWORK_ENV
        } else {
            &[]
        };
        for name in network.iter().chain(opts.remove_env) {
            remove_env(name);
        }
        if opts.non_dumpable {
            rustix::process::set_dumpable_behavior(rustix::process::DumpableBehavior::NotDumpable)
                .map_err(|e| {
                    Error::Startup(format!("can't make this process non-dumpable: {e}"))
                })?;
        }
        Ok(())
    }

    /// The system's driver manifests, joined for `VK_DRIVER_FILES`. With none,
    /// `/dev/null`: no Vulkan at all (wgpu falls back to GL) rather than
    /// drivers chosen from the home folder.
    fn system_vulkan_drivers() -> String {
        let mut files = Vec::new();
        for dir in VULKAN_DRIVER_DIRS {
            let dir = Path::new(dir);
            if !root_only(dir, false) {
                continue;
            }
            let Ok(entries) = fs::read_dir(dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "json") && root_only(&path, true) {
                    if let Some(s) = path.to_str().filter(|s| !s.contains(':')) {
                        files.push(s.to_owned());
                    }
                }
            }
        }
        files.sort();
        if files.is_empty() {
            "/dev/null".to_owned()
        } else {
            files.join(":")
        }
    }

    /// Owned by root and not writable by group or others, following links.
    fn root_only(path: &Path, file: bool) -> bool {
        fs::metadata(path).is_ok_and(|m| {
            (if file { m.is_file() } else { m.is_dir() }) && m.uid() == 0 && m.mode() & 0o022 == 0
        })
    }

    #[allow(unsafe_code)]
    fn set_env(name: &str, value: &str) {
        // SAFETY: `run` checked that this is the process's only thread, and
        // nothing here starts another, so nothing reads the environment
        // meanwhile.
        unsafe { std::env::set_var(name, value) }
    }

    #[allow(unsafe_code)]
    fn remove_env(name: &str) {
        if name.is_empty() || name.contains(&['=', '\0'][..]) {
            return;
        }
        // SAFETY: as in `set_env`.
        unsafe { std::env::remove_var(name) }
    }
}
