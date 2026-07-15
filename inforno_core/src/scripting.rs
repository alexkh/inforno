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
