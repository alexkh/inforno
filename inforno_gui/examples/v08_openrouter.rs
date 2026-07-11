// exploring advantages or openrouter_rs vs openrouter_api crate
/*
[dependencies]
openrouter-rs = "0.4.5"
tokio = { version = "1", features = ["full"] }
*/

use dotenv::dotenv;
use std::env;

use openrouter_rs::{OpenRouterClient, api::chat::*, types::Role};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenv().ok();
    let openrouter_api_key = env::var("OPENROUTER_API_KEY").unwrap();

    // Create client
    let client = OpenRouterClient::builder()
        .api_key(openrouter_api_key)
        .build()?;

    // Send chat completion
    let request = ChatCompletionRequest::builder()
        .model("anthropic/claude-sonnet-4")
        .messages(vec![
            Message::new(Role::User, "Explain Rust ownership in simple terms")
        ])
        .build()?;

    let response = client.send_chat_completion(&request).await?;
    println!("{}", response.choices[0].content().unwrap_or(""));

    Ok(())
}