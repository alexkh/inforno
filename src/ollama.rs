use ollama_rs::{Ollama, error::OllamaError, generation::{chat::{ChatMessageResponse, request::ChatMessageRequest}, completion::{GenerationResponse, request::GenerationRequest}}, models::ModelOptions};

use crate::common::{ChatQue, ChatStreamEvent, DbOllamaModel};
use std::sync::{Arc, atomic::{AtomicBool, Ordering}, mpsc::Sender};
use tokio_stream::StreamExt;

pub async fn do_ollama_chat_que(query: ChatQue) ->
        Result<ChatMessageResponse, OllamaError> {
    let ollama = Ollama::default();

    let mut options = ModelOptions::default();
        //.temperature(query.preset.options.temperature)
        //.repeat_penalty(1.5)
        //.top_k(0)
        //.top_p(1.0);
    if let Some(t) = query.preset.options.temperature {
        if t >= 0.0 && t <= 2.0 {
            options = options.temperature(t as f32);
        }
    }
    if let Some(seed) = query.preset.options.seed {
        options = options.seed(seed as i32);
    }

    // create the Request
    let request = ChatMessageRequest::new(
        query.preset.model,
        query.chat.to_ollama_messages(0),
    ).options(options);

    // send
    ollama.send_chat_messages(request).await
}

pub async fn do_ollama_chat_stream(
    query: ChatQue,
    tx: Sender<ChatStreamEvent>,
    ctx: &egui::Context,
    abort_flag: Arc<AtomicBool>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let ollama = Ollama::default();
    let model_name = query.preset.model.clone();
    let messages = query.chat.to_ollama_messages(query.agent_ind);

    // 1. Prepare the ModelOptions (Seed & Temperature)
    let mut options = ModelOptions::default();

    if let Some(seed) = query.preset.options.seed {
        // ollama_rs uses i32 for seeds
        options = options.seed(seed as i32);
    }

    if let Some(temp) = query.preset.options.temperature {
        // ollama_rs uses f32 for temperature
        options = options.temperature(temp as f32);
    }

    options = options.top_k(0).top_p(1.0);

    // 2. Create the Request and attach Options
    let mut request = ChatMessageRequest::new(model_name, messages)
        .options(options);

    // 3. Conditional: Apply "Thinking"
    // Assuming your version of ollama_rs has the .think() method as shown in your snippet
    match query.preset.options.include_reasoning {
        Some(true) => {
            request = request.think(true);
        }
        Some(false) => {
            // Explicitly disable if your library supports passing false
            // If .think() only enables, you might just skip calling it here.
            request = request.think(false);
        }
        None => {
            // Leave as default
        }
    }

    let mut stream = ollama.send_chat_messages_stream(request).await?;

    while let Some(res) = stream.next().await {
        if abort_flag.load(Ordering::Relaxed) {
            println!("Agent {} stream aborted by user.", query.agent_ind);
            break;
        }
        match res {
            Ok(response) => {
                let msg = response.message;
                if !msg.content.is_empty() {
                    let _ = tx.send(ChatStreamEvent::Content(
                        query.agent_ind,
                        msg.content
                    ));
                    ctx.request_repaint();
                }
                if let Some(thinking) = &msg.thinking {
                    if !thinking.is_empty() {
                        let _ = tx.send(ChatStreamEvent::Reasoning(
                            query.agent_ind,
                            thinking.to_string(),
                        ));
                        ctx.request_repaint();
                    }
                }
            }
            Err(_) => {
                let _ = tx.send(ChatStreamEvent::Error(
                    query.agent_ind,
                    "Error during stream from Ollama".to_string()
                ));
                ctx.request_repaint();
            }
        }
    }
    println!("Finished stream from Ollama");
    ctx.request_repaint();
    Ok(())
}

pub async fn ollama_fetch_models() -> Result<Vec<DbOllamaModel>,
        Box<dyn std::error::Error>> {
    let ollama = Ollama::default();
    let models = ollama.list_local_models().await?;

    let db_models = models
        .into_iter()
        .map(|item| {
            DbOllamaModel {
                id: 0,
                name: item.name,
                // size: item.size.min(i64::MAX as u64) as i64,
                ts_model: Some(item.modified_at),
                ..Default::default()
            }
        })
        .collect();

    Ok(db_models)
}
