// minimal egui openrouter stream example
use eframe::egui;
use futures_util::StreamExt;
use openrouter_rs::{
    api::chat::{ChatCompletionRequest, Message},
    types::{Effort, Role},
    OpenRouterClient,
};
use std::sync::mpsc::{channel, Receiver, Sender};
use tokio::runtime::Runtime;

// 1. Define Events
enum StreamEvent {
    Content(String),
    Reasoning(String),
    Finished,
    Error(String),
}

// 2. The Application State
struct InfornoApp {
    prompt: String,
    content_buffer: String,
    reasoning_buffer: String,
    is_streaming: bool,

    // Communication channels
    rx: Receiver<StreamEvent>,
    tx: Sender<StreamEvent>,

    // The Tokio Runtime is now owned by the App
    rt: Runtime,
}

impl InfornoApp {
    // We pass the runtime in during creation
    fn new(_cc: &eframe::CreationContext<'_>, rt: Runtime) -> Self {
        let (tx, rx) = channel();
        Self {
            prompt: "How to promote a desktop application in 2025? Think about this step by step.".to_owned(),
            content_buffer: String::new(),
            reasoning_buffer: String::new(),
            is_streaming: false,
            rx,
            tx,
            rt,
        }
    }

    fn start_streaming(&mut self) {
        let prompt = self.prompt.clone();
        let tx = self.tx.clone();

        self.is_streaming = true;
        self.content_buffer.clear();
        self.reasoning_buffer.clear();

        // KEY CHANGE: We use the existing runtime to spawn the async task.
        // No need to create a new thread manually!
        self.rt.spawn(async move {
            if let Err(e) = run_chat_stream(prompt, tx.clone()).await {
                let _ = tx.send(StreamEvent::Error(format!("Error: {}", e)));
            }
            let _ = tx.send(StreamEvent::Finished);
        });
    }
}

// 3. The Async Logic (Same as before)
async fn run_chat_stream(
    user_prompt: String,
    tx: Sender<StreamEvent>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    dotenv::dotenv().ok();

    let api_key = std::env::var("OPENROUTER_API_KEY")
        .map_err(|_| "OPENROUTER_API_KEY not found")?;

    let client = OpenRouterClient::builder()
        .api_key(api_key)
        .http_referer("https://github.com/your_repo")
        .x_title("inforno-stream")
        .build()?;

    let chat_request = ChatCompletionRequest::builder()
        .model("deepseek/deepseek-r1-0528:free")
        //.model("cognitivecomputations/dolphin-mistral-24b-venice-edition:free")
        .messages(vec![Message::new(Role::User, &user_prompt)])
        .reasoning_effort(Effort::High)
        .build()?;

    let mut stream = client.stream_chat_completion(&chat_request).await?;

    while let Some(event_result) = stream.next().await {
        match event_result {
            Ok(event) => {
                if let Some(choice) = event.choices.first() {
                    if let Some(reasoning) = choice.reasoning() {
                        if !reasoning.is_empty() {
                            let _ = tx.send(StreamEvent::Reasoning(reasoning.to_string()));
                        }
                    }
                    if let Some(content) = choice.content() {
                        if !content.is_empty() {
                            let _ = tx.send(StreamEvent::Content(content.to_string()));
                        }
                    }
                }
            }
            Err(e) => {
                let _ = tx.send(StreamEvent::Error(e.to_string()));
            }
        }
    }
    Ok(())
}

// 4. UI Update Loop
impl eframe::App for InfornoApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Poll for updates
        while let Ok(event) = self.rx.try_recv() {
            match event {
                StreamEvent::Content(t) => self.content_buffer.push_str(&t),
                StreamEvent::Reasoning(t) => self.reasoning_buffer.push_str(&t),
                StreamEvent::Error(e) => self.content_buffer.push_str(&format!("\n[ERROR]: {}", e)),
                StreamEvent::Finished => self.is_streaming = false,
            }
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Inforno: Tokio Integration");

            ui.horizontal(|ui| {
                ui.text_edit_singleline(&mut self.prompt);
                if ui.add_enabled(!self.is_streaming, egui::Button::new("Send")).clicked() {
                    self.start_streaming();
                }
            });

            ui.separator();

            // Reasoning Panel
            egui::TopBottomPanel::top("reasoning").resizable(true).min_height(100.0).show_inside(ui, |ui| {
                ui.colored_label(egui::Color32::LIGHT_BLUE, "Reasoning Stream:");
                egui::ScrollArea::vertical().stick_to_bottom(true).show(ui, |ui| {
                     ui.add_sized(ui.available_size(), egui::TextEdit::multiline(&mut self.reasoning_buffer).code_editor());
                });
            });

            ui.add_space(10.0);

            // Content Panel
            ui.colored_label(egui::Color32::LIGHT_GREEN, "Final Output:");
            egui::ScrollArea::vertical().stick_to_bottom(true).show(ui, |ui| {
                 ui.add_sized(ui.available_size(), egui::TextEdit::multiline(&mut self.content_buffer));
            });
        });

        if self.is_streaming {
            ctx.request_repaint();
        }
    }
}

// 5. The Main Entry Point
fn main() -> Result<(), eframe::Error> {
    // 1. Create the Tokio Runtime
    let rt = Runtime::new().expect("Unable to create Runtime");

    // Optional: Enter the runtime context if you use libraries that implicitly require it.
    // However, since we are explicitly passing `rt` to our App, we don't strictly need `_enter` here
    // unless we were doing async setup *before* running the app.
    let _enter = rt.enter();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([600.0, 800.0]),
        ..Default::default()
    };

    // 2. Pass the runtime into the App via the closure
    eframe::run_native(
        "Inforno Stream",
        options,
        Box::new(|cc| {
            // We move `rt` into the App struct here
            Ok(Box::new(InfornoApp::new(cc, rt)))
        }),
    )
}