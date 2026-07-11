// fetches all installed ollama models

use ollama_rs::Ollama;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let ollama = Ollama::default();

    let local_models = ollama.list_local_models().await?;

    println!("Local Models:");
    for model in local_models {
        println!("- {}", model.name);
    }

    Ok(())
}
