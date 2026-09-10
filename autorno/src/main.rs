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
    vfs_session: VfsMaskSession,
    child: std::process::Child,
}

// Thread-safe registry mapping harness IDs to their active processes and FUSE mounts
type Registry = Arc<Mutex<HashMap<String, Harness>>>;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
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

    loop {
        match listener.accept().await {
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

            match spawn_harness(&realm, &role, &cmd) {
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
                // 1. Kill the actual trapped process
                let _ = harness.child.kill();
                // 2. The `vfs_session` is dropped here, which cleanly unmounts the background FUSE driver
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

fn spawn_harness(realm_name: &str, role_name: &str, cmd: &str) -> Result<Harness, Box<dyn std::error::Error>> {
    let proj_dirs = directories::ProjectDirs::from("", "", "inforno")
        .ok_or("Could not find project directories")?;
    let yaml_path = proj_dirs.config_dir().join("realms").join(realm_name).join("realm2.yml");

    let config_str = std::fs::read_to_string(&yaml_path)?;
    let config = serde_saphyr::from_str::<RealmConfig>(&config_str)?;

    let active_realm = ActiveRealm::from_config(realm_name.to_string(), config)?;
    if !active_realm.has_role(role_name) {
        return Err(format!("Role '{}' is not defined in Realm '{}'", role_name, realm_name).into());
    }

    let vfs_session = VfsMaskSession::spawn_vfs(active_realm, role_name.to_string())?;
    let child = spawn_masked_command(vfs_session.mount_path(), cmd)?;

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

    // Spin up an ephemeral FUSE session for the duration of this single command
    let vfs_session = VfsMaskSession::spawn_vfs(active_realm, role_name.to_string())?;
    
    let mut command = inforno_core::realm_spawn::build_masked_command(vfs_session.mount_path(), cmd)?;
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
