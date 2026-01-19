/*
minimal program using openrouter api
[dependencies]
dotenv = "0.15.0"
openrouter_api = { version = "0.1.6", features = ["tracing"] }
tokio = "1.47.1"
*/

use futures_util::StreamExt;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}, mpsc::Sender};

use openrouter_rs::{OpenRouterClient, api::chat::*, types::{CompletionsResponse, Effort}};
use secrecy::ExposeSecret;

use crate::common::{ApiKey, ChatQue, ChatStreamEvent, DbOpenrModel, mask_key_secure};

// simple request without streaming or history
pub async fn do_openr_chat_que(query: ChatQue) ->
        Result<CompletionsResponse, Box<dyn std::error::Error>> {

    // print the frist two and last two characters of the key in case we are not
    // sure whether the right key is used
    println!("using key: {}", mask_key_secure(
        query.preset.api_key.key.expose_secret()));

    // Create client
    let client = OpenRouterClient::builder()
        .api_key(query.preset.api_key.key.expose_secret())
        .build()?;

    // Send chat completion
    let request = ChatCompletionRequest::builder()
    .model(query.preset.model)
    // Pass '0' or a variable like 'current_hist_id' here
    .messages(query.chat.to_openrouter_messages(0))
    .build()?;

    let response = client.send_chat_completion(&request).await?;
    Ok(response)
}

pub async fn do_openr_chat_stream(
    query: ChatQue,
    tx: Sender<ChatStreamEvent>,
    ctx: &egui::Context,
    abort_flag: Arc<AtomicBool>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    println!("using key: {}", mask_key_secure(
        query.preset.api_key.key.expose_secret()));

    let client = OpenRouterClient::builder()
        .api_key(query.preset.api_key.key.expose_secret())
        .build()?;

    // 1. Start the builder with mandatory fields
    let mut request_builder = ChatCompletionRequest::builder();
    request_builder
        .model(query.preset.model)
        .messages(query.chat.to_openrouter_messages(query.agent_ind));

    // 2. Conditional: Apply Reasoning
    match query.preset.options.include_reasoning {
        Some(true) => {
            // User explicitly wants reasoning -> Force High Effort
            request_builder.reasoning_effort(Effort::High);
        }
        Some(false) => {
            // User explicitly wants NO reasoning -> Disable it
            // request_builder = request_builder.reasoning(false);
        }
        None => {
            // User explicitly set "Unset" -> Do not call any method.
            // The service provider determines the default behavior.
        }
    }

    // 3. Conditional: Apply Seed
    if let Some(seed) = query.preset.options.seed {
        request_builder.seed(seed as u32);
    }

    // 4. Conditional: Apply Temperature
    if let Some(temp) = query.preset.options.temperature {
        // APIs usually expect f32, so we cast the stored f64
        request_builder.temperature(temp);
    }

    // 5. Finalize build
    let chat_request = request_builder.build()?;

    let mut stream = client.stream_chat_completion(&chat_request).await?;

    while let Some(event_result) = stream.next().await {
        // 1. CHECK SIGNAL: Stop immediately if flag is true
        if abort_flag.load(Ordering::Relaxed) {
            println!("OpenRouter stream aborted by user.");
            break; // Breaks the loop, dropping 'stream' and closing connection
        }

        match event_result {
            Ok(event) => {
                if let Some(choice) = event.choices.first() {
                    if let Some(reasoning) = choice.reasoning() {
                        if !reasoning.is_empty() {
                            let _ = tx.send(ChatStreamEvent::Reasoning(
                                    query.agent_ind, reasoning.to_string()));
                            ctx.request_repaint();
                        }
                    }
                    if let Some(content) = choice.content() {
                        if !content.is_empty() {
                            let _ = tx.send(ChatStreamEvent::Content(
                                    query.agent_ind, content.to_string()));
                            ctx.request_repaint();
                        }
                    }
                }
            }
            Err(e) => {
                let _ = tx.send(ChatStreamEvent::Error(
                            query.agent_ind, e.to_string()));
                ctx.request_repaint();
            }
        }
    }
    println!("Finished stream from OpenRouter");
    ctx.request_repaint();
    Ok(())
}

pub async fn openr_fetch_models(api_key: &ApiKey) -> Result<Vec<DbOpenrModel>,
        openrouter_rs::error::OpenRouterError> {
    // Create an OpenRouter client.
    // Create client
    let client = OpenRouterClient::builder()
        .api_key(api_key.key.expose_secret())
        .build()?;

    // Call the list_models method to get all available models.
    let models = client.list_models().await?;

    let db_models: Vec<DbOpenrModel> = models
        .into_iter()
        .map(|item| {
            if let Some((id1, id2)) = item.id.split_once('/') {
                DbOpenrModel {
                    id: 0,
                    provider: id1.into(),
                    model_id: item.id.into(),
                    name: item.name,
                    description: item.description,
                    context_length: item.context_length,
                    price_prompt: item.pricing.prompt.parse().ok(),
                    price_completion: item.pricing.completion.parse().ok(),
                    price_image: item.pricing.image.and_then(
                            |s| s.parse::<f64>().ok()),
                    details: None,
                    ts_model: Some(item.created.to_string()),
                }
            } else {
                println!("Error parsing an Openrouter model info");
                DbOpenrModel::default()
            }
        })
        .collect();

    Ok(db_models)
}
