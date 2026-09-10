pub fn is_likely_rhai(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() { return false; }

    let engine = rhai::Engine::new();
    // It must compile into a valid AST AND contain at least one code-like structural element
    // This avoids identifying a single English word (like "Note") as code.
    engine.compile(text).is_ok() && (
        text.contains('=') || text.contains('(') || text.contains('{') ||
        text.contains("let ") || text.contains("fn ") || text.contains("print")
    )
}

pub fn run_rhai(script: &str) -> (String, Option<String>) {
    use rhai::Engine;
    use std::sync::{Arc, Mutex};

    let mut engine = Engine::new();
    engine.set_max_operations(1_000_000); // 1 million max ops to prevent infinite loops

    // Thread-safe buffer to capture stdout
    let output = Arc::new(Mutex::new(String::new()));
    let out_clone = output.clone();

    engine.on_print(move |s| {
        let mut out = out_clone.lock().unwrap();
        out.push_str(s);
        out.push('\n');
    });

    let out_clone2 = output.clone();
    engine.on_debug(move |s, _src, _pos| {
        let mut out = out_clone2.lock().unwrap();
        out.push_str(&format!("[DEBUG] {}\n", s));
    });

    // NEW: Bridge for LLM Prompting
    let prompt_request = Arc::new(Mutex::new(None));
    let pr_clone = prompt_request.clone();
    engine.register_fn("send_prompt", move |text: &str| {
        *pr_clone.lock().unwrap() = Some(text.to_string());
    });

    // IPC Bridge to Autorno Daemon
    engine.register_fn("autorno_ping", || -> String {
        #[cfg(target_os = "linux")]
        {
            send_ipc_command(DaemonCommand::Ping)
        }
        #[cfg(not(target_os = "linux"))]
        {
            "Error: Autorno daemon IPC is only supported on Linux.".to_string()
        }
    });

    engine.register_fn("autorno_start", |id: rhai::ImmutableString, realm: rhai::ImmutableString, role: rhai::ImmutableString, cmd: rhai::ImmutableString| -> String {
        #[cfg(target_os = "linux")]
        {
            send_ipc_command(DaemonCommand::Start {
                id: id.to_string(),
                realm: realm.to_string(),
                role: role.to_string(),
                cmd: cmd.to_string(),
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            "Error: Autorno daemon IPC is only supported on Linux.".to_string()
        }
    });

    engine.register_fn("autorno_stop", |id: rhai::ImmutableString| -> String {
        #[cfg(target_os = "linux")]
        {
            send_ipc_command(DaemonCommand::Stop {
                id: id.to_string(),
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            "Error: Autorno daemon IPC is only supported on Linux.".to_string()
        }
    });

    engine.register_fn("autorno_run", |realm: rhai::ImmutableString, role: rhai::ImmutableString, cmd: rhai::ImmutableString| -> String {
        #[cfg(target_os = "linux")]
        {
            send_ipc_command(DaemonCommand::Run {
                realm: realm.to_string(),
                role: role.to_string(),
                cmd: cmd.to_string(),
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            "Error: Autorno daemon IPC is only supported on Linux.".to_string()
        }
    });

    let result = engine.eval::<rhai::Dynamic>(script);
    let mut final_out = output.lock().unwrap().clone();

    match result {
        Ok(val) => {
            if !val.is_unit() {
                final_out.push_str(&format!("=> {}", val));
            }
        }
        Err(e) => {
            final_out.push_str(&format!("Error: {}", e));
        }
    }

    let final_str = if final_out.is_empty() {
        "Execution finished (no output)".to_string()
    } else {
        final_out
    };

    let requested = prompt_request.lock().unwrap().take();
    (final_str, requested)
}

#[cfg(target_os = "linux")]
#[derive(serde::Serialize)]
pub enum DaemonCommand {
    Start { id: String, realm: String, role: String, cmd: String },
    Stop { id: String },
    Run { realm: String, role: String, cmd: String },
    Ping,
}

#[cfg(target_os = "linux")]
#[derive(serde::Deserialize)]
pub enum DaemonResponse {
    Ok(String),
    Error(String),
}

#[cfg(target_os = "linux")]
fn send_ipc_command(cmd: DaemonCommand) -> String {
    let socket_path = match directories::ProjectDirs::from("", "", "inforno") {
        Some(d) => d.cache_dir().join("autorno.sock"),
        None => return "Error: Could not resolve cache directory".to_string(),
    };

    use std::os::unix::net::UnixStream;
    use std::io::{Write, Read};

    let mut stream = match UnixStream::connect(&socket_path) {
        Ok(s) => s,
        Err(e) => return format!("Error connecting to daemon: {}", e),
    };

    let payload = match serde_json::to_string(&cmd) {
        Ok(s) => s,
        Err(e) => return format!("Error serializing command: {}", e),
    };

    if let Err(e) = stream.write_all(payload.as_bytes()) {
        return format!("Error writing to socket: {}", e);
    }

    let mut response_buf = String::new();
    if let Err(e) = stream.read_to_string(&mut response_buf) {
        return format!("Error reading from socket: {}", e);
    }

    match serde_json::from_str::<DaemonResponse>(&response_buf) {
        Ok(DaemonResponse::Ok(msg)) => msg,
        Ok(DaemonResponse::Error(msg)) => format!("Daemon Error: {}", msg),
        Err(_) => format!("Raw daemon response: {}", response_buf),
    }
}
