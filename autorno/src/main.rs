use inforno_core::realm::{ActiveRealm, RealmConfig};
use inforno_core::realm_mount::VfsMaskSession;
use inforno_core::realm_spawn::spawn_masked_command;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::net::{UnixListener, UnixStream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Debug)]
pub enum DaemonCommand {
    Start { id: String, realm: String, role: String, cmd: String },
    Stop { id: String },
    Run { realm: String, role: String, cmd: String },
    Ping,
}

#[derive(Serialize, Debug)]
pub enum DaemonResponse {
    Ok(String),
    Error(String),
}

struct Harness {
    // Same declaration-order-is-drop-order reasoning as `VfsMaskSession`:
    // the child (and the mount namespace/bind-mounts it holds against
    // `vfs_session`'s FUSE tree) should be gone before the FUSE session
    // tears down. Defense-in-depth only — dropping a `Child` doesn't kill
    // or wait for it, so this doesn't substitute for the explicit
    // kill+wait in `Stop` above, just protects other drop paths.
    child: std::process::Child,
    vfs_session: VfsMaskSession,
}

/// Host directories scanned when resolving a Realm's `bin` cascade
/// (`ActiveRealm::allowed_binaries`). A binary needs to live in one of
/// these to ever be selectable via `bin:` in `realm2.yml`, regardless of
/// what the glob expression says. Each match is later bind-mounted into
/// the chroot at the corresponding path (`/bin`, `/usr/bin`, or
/// `/usr/local/bin`) rather than always into `/bin` — see
/// `build_masked_command`'s step 6b in `realm_spawn.rs`.
fn bin_search_dirs() -> Vec<std::path::PathBuf> {
    ["/usr/local/bin", "/usr/local/sbin", "/usr/bin", "/usr/sbin", "/bin", "/sbin"]
        .iter()
        .map(std::path::PathBuf::from)
        .collect()
}

// Thread-safe registry mapping harness IDs to their active processes and FUSE mounts
type Registry = Arc<Mutex<HashMap<String, Harness>>>;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    
    // --- CONFIG SUBCOMMANDS ---
    // autorno config realm <realmname> add place <place_name> <vpath> <intro...>
    // autorno config realm <realmname> rm  place <place_name>
    if args.len() >= 2 && args[1] == "config" {
        if args.len() < 6 || args[2] != "realm" || args[5] != "place" {
            eprintln!("Usage: autorno config realm <realmname> <add|rm> place ...");
            std::process::exit(1);
        }
        let realm_name = &args[3];
        let action = &args[4];
        
        let proj_dirs = directories::ProjectDirs::from("", "", "inforno")
            .ok_or("Could not find project directories")?;
        let yaml_path = proj_dirs.config_dir().join("realms").join(realm_name).join("realm2.yml");
        
        if !yaml_path.exists() {
            eprintln!("Realm config not found at {:?}", yaml_path);
            std::process::exit(1);
        }

        let config_str = std::fs::read_to_string(&yaml_path)?;
        let mut config = serde_saphyr::from_str::<inforno_core::realm::RealmConfig>(&config_str)?;

        match action.as_str() {
            "add" => {
                if args.len() < 9 {
                    eprintln!("Usage: autorno config realm <realmname> add place <place_name> <vpath> <intro...>");
                    eprintln!("Note: An intro is strictly required to provide context to the LLM.");
                    std::process::exit(1);
                }
                let name = &args[6];
                let vpath = &args[7];
                // Capture all remaining arguments as the intro/description
                let description = args[8..].join(" ");
                
                // Inject the required Commented structure
                config.places.insert(
                    name.to_string(), 
                    serde_saphyr::Commented(vpath.to_string(), format!(" {}", description))
                );
                
                std::fs::write(&yaml_path, serde_saphyr::to_string(&config)?)?;
                println!("✔ Added place '{}' -> '{}' to realm '{}'", name, vpath, realm_name);
            }
            "rm" => {
                if args.len() < 7 {
                    eprintln!("Usage: autorno config realm <realmname> rm place <place_name>");
                    std::process::exit(1);
                }
                let name = &args[6];
                if config.places.shift_remove(name).is_some() {
                    std::fs::write(&yaml_path, serde_saphyr::to_string(&config)?)?;
                    println!("✔ Removed place '{}' from realm '{}'", name, realm_name);
                } else {
                    eprintln!("Place '{}' not found in realm '{}'.", name, realm_name);
                    std::process::exit(1);
                }
            }
            _ => {
                eprintln!("Unknown config action: {}", action);
                std::process::exit(1);
            }
        }
        return Ok(());
    }

    // --- EXEC SUBCOMMAND ---
    if args.len() >= 2 && args[1] == "exec" {
        inforno_core::realm_mount::cleanup_orphaned_mounts();

        let mut idx = 2;
        let mut work_dir = None;
        if args.len() > idx && args[idx] == "--workdir" {
            if idx + 1 < args.len() {
                work_dir = Some(args[idx + 1].clone());
                idx += 2;
            }
        }

        if args.len() < idx + 2 {
            eprintln!("Usage: autorno exec [--workdir <dir>] <realm> <role> [cmd...]");
            std::process::exit(1);
        }

        let realm_name = &args[idx];
        let role_name = &args[idx + 1];
        let cmd = if args.len() > idx + 2 { args[idx + 2..].join(" ") } else { "".to_string() };
        
        let exit_code = match spawn_harness(realm_name, role_name, &cmd, work_dir.as_deref()) {
            Ok(mut harness) => {
                let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
                let mut sighup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()).unwrap();
                let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()).unwrap();
                let mut sigquit = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::quit()).unwrap();

                let status = loop {
                    if let Ok(Some(status)) = harness.child.try_wait() {
                        break Some(status);
                    }
                    tokio::select! {
                        _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => {},
                        _ = sigterm.recv() => break None,
                        _ = sighup.recv() => break None,
                        _ = sigint.recv() => break None,
                        _ = sigquit.recv() => break None,
                    }
                };

                if let Some(st) = status {
                    // explicitly drop harness to unmount FUSE before sleeping
                    drop(harness);
                    println!("\nProcess exited with status: {}", st);
                    println!("Closing terminal in 10 seconds...");
                    std::thread::sleep(std::time::Duration::from_secs(10));
                    st.code().unwrap_or(1)
                } else {
                    let _ = harness.child.kill();
                    let _ = harness.child.wait();
                    drop(harness);
                    1
                }
            }
            Err(e) => {
                eprintln!("\nFailed to start harness:\n{}", e);
                println!("Closing terminal in 10 seconds...");
                std::thread::sleep(std::time::Duration::from_secs(10));
                1
            }
        };
        std::process::exit(exit_code);
    }

    let proj_dirs = directories::ProjectDirs::from("", "", "inforno")
        .ok_or("Could not find project directories")?;
    
    let cache_dir = proj_dirs.cache_dir();
    std::fs::create_dir_all(cache_dir)?;
    let socket_path = cache_dir.join("autorno.sock");

    // Singleton Lock: Try binding. If the socket exists, ensure it's not a dead file from a crash.
    if socket_path.exists() {
        if tokio::net::UnixStream::connect(&socket_path).await.is_ok() {
            eprintln!("Daemon is already running at {:?}", socket_path);
            std::process::exit(1);
        } else {
            std::fs::remove_file(&socket_path)?;
        }
    }

            let listener = UnixListener::bind(&socket_path)?;
        println!("Autorno daemon listening on {:?}", socket_path);

        let registry: Registry = Arc::new(Mutex::new(HashMap::new()));

        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
        let mut sighup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;
        let mut sigquit = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::quit())?;

        loop {
            tokio::select! {
                accept_res = listener.accept() => {
                    match accept_res {
                        Ok((stream, _)) => {
                            let reg_clone = registry.clone();
                            tokio::spawn(async move {
                                if let Err(e) = handle_client(stream, reg_clone).await {
                                    eprintln!("Client error: {}", e);
                                }
                            });
                        }
                        Err(e) => eprintln!("Accept error: {}", e),
                    }
                }
                _ = sigterm.recv() => break,
                _ = sigint.recv() => break,
                _ = sighup.recv() => break,
                _ = sigquit.recv() => break,
            }
        }

        let mut reg = registry.lock().unwrap();
        for (_, mut harness) in reg.drain() {
            let _ = harness.child.kill();
            let _ = harness.child.wait();
        }
        drop(reg);

        let _ = std::fs::remove_file(&socket_path);

        Ok(())
    }

async fn handle_client(mut stream: UnixStream, registry: Registry) -> Result<(), Box<dyn std::error::Error>> {
    let mut buf = vec![0; 4096];
    let n = stream.read(&mut buf).await?;
    if n == 0 { return Ok(()); }

    let response = match serde_json::from_slice::<DaemonCommand>(&buf[..n]) {
        Ok(cmd) => process_command(cmd, registry),
        Err(e) => DaemonResponse::Error(format!("Invalid payload: {}", e)),
    };

    let res_bytes = serde_json::to_vec(&response)?;
    stream.write_all(&res_bytes).await?;
    Ok(())
}

fn process_command(cmd: DaemonCommand, registry: Registry) -> DaemonResponse {
    match cmd {
        DaemonCommand::Start { id, realm, role, cmd } => {
            let mut reg = registry.lock().unwrap();
            if reg.contains_key(&id) {
                return DaemonResponse::Error(format!("Harness '{}' already running", id));
            }

            match spawn_harness(&realm, &role, &cmd, None) {
                Ok(harness) => {
                    reg.insert(id.clone(), harness);
                    DaemonResponse::Ok(format!("Started harness '{}'", id))
                }
                Err(e) => DaemonResponse::Error(e.to_string()),
            }
        }
        DaemonCommand::Stop { id } => {
            let mut reg = registry.lock().unwrap();
            if let Some(mut harness) = reg.remove(&id) {
                // 1. Kill the actual trapped process...
                let _ = harness.child.kill();
                // ...and wait for it to actually be reaped. `kill()` only
                // sends the signal — without `wait()` the process may
                // still be mid-exit, still holding its own mount namespace
                // (built from bind-mounts of `vfs_session`'s FUSE tree)
                // open underneath it. Dropping `vfs_session` while that's
                // still true races the unmount, can fail silently, and
                // leaves a dead mount behind that hangs any later access.
                let _ = harness.child.wait();
                // 2. `vfs_session` is dropped here (child now fully gone),
                //    which cleanly unmounts the background FUSE driver.
                DaemonResponse::Ok(format!("Stopped harness '{}'", id))
            } else {
                DaemonResponse::Error(format!("Harness '{}' not found", id))
            }
        }
        DaemonCommand::Run { realm, role, cmd } => {
            match run_harness(&realm, &role, &cmd) {
                Ok(output) => DaemonResponse::Ok(output),
                Err(e) => DaemonResponse::Error(e.to_string()),
            }
        }
        DaemonCommand::Ping => DaemonResponse::Ok("Pong".to_string()),
    }
}

fn spawn_harness(realm_name: &str, role_name: &str, cmd: &str, work_dir: Option<&str>) -> Result<Harness, Box<dyn std::error::Error>> {
    let proj_dirs = directories::ProjectDirs::from("", "", "inforno")
        .ok_or("Could not find project directories")?;
    let yaml_path = proj_dirs.config_dir().join("realms").join(realm_name).join("realm2.yml");

    let config_str = std::fs::read_to_string(&yaml_path)?;
    let config = serde_saphyr::from_str::<RealmConfig>(&config_str)?;

    let active_realm = ActiveRealm::from_config(realm_name.to_string(), config)?;
    if !active_realm.has_role(role_name) {
        return Err(format!("Role '{}' is not defined in Realm '{}'", role_name, realm_name).into());
    }

    // Resolved before `active_realm` is moved into `spawn_vfs` below.
    let extra_binaries = active_realm.allowed_binaries(role_name, &bin_search_dirs());
    let allowed_envs = active_realm.allowed_envs(role_name);

    let vfs_session = VfsMaskSession::spawn_vfs(active_realm, role_name.to_string())?;

    // Wait for the FUSE mount to be fully ready before proceeding
    let mut retries = 50;
    while !vfs_session.mount_path().join("bin").exists() && retries > 0 {
        std::thread::sleep(std::time::Duration::from_millis(10));
        retries -= 1;
    }

    let child = spawn_masked_command(vfs_session.mount_path(), cmd, realm_name, &extra_binaries, &allowed_envs, work_dir)?;

    Ok(Harness { vfs_session, child })
}

fn run_harness(realm_name: &str, role_name: &str, cmd: &str) -> Result<String, Box<dyn std::error::Error>> {
    let proj_dirs = directories::ProjectDirs::from("", "", "inforno")
        .ok_or("Could not find project directories")?;
    let yaml_path = proj_dirs.config_dir().join("realms").join(realm_name).join("realm2.yml");

    let config_str = std::fs::read_to_string(&yaml_path)?;
    let config = serde_saphyr::from_str::<inforno_core::realm::RealmConfig>(&config_str)?;

    let active_realm = inforno_core::realm::ActiveRealm::from_config(realm_name.to_string(), config)?;
    if !active_realm.has_role(role_name) {
        return Err(format!("Role '{}' is not defined in Realm '{}'", role_name, realm_name).into());
    }

    // Resolved before `active_realm` is moved into `spawn_vfs` below.
    let extra_binaries = active_realm.allowed_binaries(role_name, &bin_search_dirs());
    let allowed_envs = active_realm.allowed_envs(role_name);

    // Spin up an ephemeral FUSE session for the duration of this single command
    let vfs_session = VfsMaskSession::spawn_vfs(active_realm, role_name.to_string())?;
    
    // Wait for the FUSE mount to be fully ready before proceeding
    let mut retries = 50;
    while !vfs_session.mount_path().join("bin").exists() && retries > 0 {
        std::thread::sleep(std::time::Duration::from_millis(10));
        retries -= 1;
    }

    let mut command = inforno_core::realm_spawn::build_masked_command(vfs_session.mount_path(), cmd, realm_name, &extra_binaries, &allowed_envs, None)?;
    let output = command.output()?;
    
    let mut result = String::new();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    
    if !stdout.is_empty() {
        result.push_str(&stdout);
    }
    if !stderr.is_empty() {
        if !result.is_empty() { result.push('\n'); }
        result.push_str("STDERR:\n");
        result.push_str(&stderr);
    }
    
    Ok(if result.is_empty() { "(Command executed successfully with no output)".to_string() } else { result })
}
