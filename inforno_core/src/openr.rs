use futures_util::StreamExt;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}, mpsc::Sender};

use openrouter_rs::{OpenRouterClient, api::chat::*, types::{CompletionsResponse, Effort}};
use secrecy::ExposeSecret;

use crate::common::{ApiKey, ChatQue, ChatStreamEvent, DbOpenrModel, mask_key_secure};

fn build_openr_client(query: &ChatQue) -> Result<OpenRouterClient, String> {
    let mut builder = OpenRouterClient::builder();

    if let Some(nickname) = &query.preset.options.openrouter_custom_url {
        // A custom endpoint gets its own token from OPENROUTER_API_URLS, never
        // a saved key: the environment is attacker-controlled (vaultorno §13).
        let configs = crate::common::get_custom_openr_configs();
        let Some(cfg) = configs.iter().find(|c| c.nickname == *nickname) else {
            // Refuse to execute if the requested nickname isn't locally defined
            return Err(format!("Custom URL nickname '{}' not found in OPENROUTER_API_URLS environment settings.", nickname));
        };
        let token = if cfg.token.trim().is_empty() { "dummy_token" } else { cfg.token.as_str() };
        println!("using custom endpoint '{}' with key: {}", cfg.nickname, mask_key_secure(token));
        builder.base_url(cfg.url.as_str());
        builder.api_key(token);
    } else {
        // The saved key goes only to the hosts saved with it.
        let key = &query.preset.api_key;
        crate::api_keys::check_host(&key.hosts, crate::api_keys::OPENROUTER_API)?;
        // print the first two and last two characters of the key in case we are not
        // sure whether the right key is used
        println!("using key: {}", mask_key_secure(key.key.expose_secret()));
        builder.base_url(crate::api_keys::OPENROUTER_API);
        builder.api_key(key.key.expose_secret());
    }

    builder.build().map_err(|e| e.to_string())
}

/// Builds the chat request shared by every OpenRouter entry point.
fn build_chat_request(query: &ChatQue) -> Result<ChatCompletionRequest, String> {
    let opts = &query.preset.options;
    let mut request_builder = ChatCompletionRequest::builder();
    request_builder
        .model(query.preset.model.clone())
        .messages(query.chat.to_openrouter_messages(query.talker_ind));

    match opts.include_reasoning {
        // Explicit yes -> force high effort; explicit no -> disable;
        // unset -> the service provider decides.
        Some(true) => { request_builder.reasoning_effort(Effort::High); }
        Some(false) => { request_builder.include_reasoning(false); }
        None => {}
    }

    if let Some(seed) = opts.seed {
        request_builder.seed(seed as u32);
    }
    if let Some(temp) = opts.temperature {
        request_builder.temperature(temp);
    }
    if let Some(top_p) = opts.top_p {
        request_builder.top_p(top_p);
    }
    if let Some(max_tokens) = opts.max_tokens {
        request_builder.max_tokens(max_tokens as u32);
    }
    if let Some(top_k) = opts.top_k {
        request_builder.top_k(top_k.max(0) as u32);
    }
    if let Some(fp) = opts.frequency_penalty {
        request_builder.frequency_penalty(fp);
    }
    if let Some(pp) = opts.presence_penalty {
        request_builder.presence_penalty(pp);
    }
    if let Some(rp) = opts.repetition_penalty {
        request_builder.repetition_penalty(rp);
    }
    if let Some(min_p) = opts.min_p {
        request_builder.min_p(min_p);
    }
    if let Some(stop) = &opts.stop_sequences {
        request_builder.stop(stop.clone());
    }

    request_builder.build().map_err(|e| e.to_string())
}

// simple request without streaming or history
pub async fn do_openr_chat_que(query: ChatQue) ->
        Result<CompletionsResponse, Box<dyn std::error::Error>> {

    let client = build_openr_client(&query)?;

    let request = build_chat_request(&query)?;

    let response = client.send_chat_completion(&request).await?;
    Ok(response)
}

#[tracing::instrument(skip_all)]
pub async fn do_openr_chat_sync(
    query: ChatQue,
    tx: Sender<ChatStreamEvent>,
    wakeup: Arc<dyn Fn() + Send + Sync>,
    abort_flag: Arc<AtomicBool>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let client = build_openr_client(&query)?;

    let chat_request = build_chat_request(&query)?;

    // Wait for the ENTIRE response to generate synchronously
    match client.send_chat_completion(&chat_request).await {
        Ok(response) => {
            // Check if the user aborted while we were waiting
            if abort_flag.load(Ordering::Relaxed) {
                println!("OpenRouter sync aborted by user.");
                return Ok(()); 
            }

            if let Some(choice) = response.choices.first() {
                if let Some(reasoning) = choice.reasoning() {
                    crate::common::emit_text(&tx, &*wakeup, ChatStreamEvent::Reasoning, query.talker_ind, reasoning);
                }
                if let Some(content) = choice.content() {
                    crate::common::emit_text(&tx, &*wakeup, ChatStreamEvent::Content, query.talker_ind, content);
                }
            }
        }
        Err(e) => {
            let _ = tx.send(ChatStreamEvent::Error(
                query.talker_ind,
                format!("OpenRouter sync error: {:?}", e)
            ));
            wakeup();
        }
    }

    println!("Finished sync request from OpenRouter");
    wakeup();
    Ok(())
}

#[tracing::instrument(skip_all)]
pub async fn do_openr_chat_stream(
    query: ChatQue,
    tx: Sender<ChatStreamEvent>,
    wakeup: Arc<dyn Fn() + Send + Sync>,
    abort_flag: Arc<AtomicBool>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let client = build_openr_client(&query)?;

    // 1. Start the builder with mandatory fields
    let mut request_builder = ChatCompletionRequest::builder();
    request_builder
        .model(query.preset.model)
        .messages(query.chat.to_openrouter_messages(query.talker_ind));

    // 2. Conditional: Apply Reasoning
    match query.preset.options.include_reasoning {
        Some(true) => {
            // User explicitly wants reasoning -> Force High Effort
            request_builder.reasoning_effort(Effort::High);
        }
        Some(false) => {
            // User explicitly wants NO reasoning -> Disable it
            request_builder.include_reasoning(false);
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

    if let Some(top_p) = query.preset.options.top_p {
        request_builder.top_p(top_p);
    }

    if let Some(max_tokens) = query.preset.options.max_tokens {
        request_builder.max_tokens(max_tokens as u32);
    }

    if let Some(top_k) = query.preset.options.top_k {
        request_builder.top_k(top_k.max(0) as u32);
    }

    if let Some(fp) = query.preset.options.frequency_penalty {
        request_builder.frequency_penalty(fp);
    }

    if let Some(pp) = query.preset.options.presence_penalty {
        request_builder.presence_penalty(pp);
    }

    if let Some(rp) = query.preset.options.repetition_penalty {
        request_builder.repetition_penalty(rp);
    }

    if let Some(min_p) = query.preset.options.min_p {
        request_builder.min_p(min_p);
    }

    if let Some(stop) = &query.preset.options.stop_sequences {
        request_builder.stop(stop.clone());
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
                        crate::common::emit_text(&tx, &*wakeup, ChatStreamEvent::Reasoning, query.talker_ind, reasoning);
                    }
                    if let Some(content) = choice.content() {
                        crate::common::emit_text(&tx, &*wakeup, ChatStreamEvent::Content, query.talker_ind, content);
                    }
                }
            }
            Err(e) => {
                let _ = tx.send(ChatStreamEvent::Error(
                            query.talker_ind, e.to_string()));
                wakeup();
            }
        }
    }
    println!("Finished stream from OpenRouter");
    wakeup();
    Ok(())
}

pub async fn openr_fetch_models(api_key: &ApiKey) -> Result<Vec<DbOpenrModel>,
        Box<dyn std::error::Error + Send + Sync>> {
    // The key goes only to the hosts saved with it.
    crate::api_keys::check_host(&api_key.hosts, crate::api_keys::OPENROUTER_API)?;
    let client = OpenRouterClient::builder()
        .base_url(crate::api_keys::OPENROUTER_API)
        .api_key(api_key.key.expose_secret())
        .build()?;

    // Call the list_models method to get all available models.
    let models = client.list_models().await?;

    let db_models: Vec<DbOpenrModel> = models
        .into_iter()
        .map(|item| {
            if let Some((id1, _)) = item.id.split_once('/') {
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
