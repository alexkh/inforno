use eframe::egui;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use tokio::runtime::Runtime;

struct MyApp {
    picked_path: Option<PathBuf>,
    // Receiver to get data from the async thread
    rx: Receiver<PathBuf>,
    // Sender to pass to the async thread
    tx: Sender<PathBuf>,
}

impl Default for MyApp {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self {
            picked_path: None,
            rx,
            tx,
        }
    }
}

impl eframe::App for MyApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 1. POLL THE CHANNEL
        // Check if we received a file path from the async task
        if let Ok(path) = self.rx.try_recv() {
            self.picked_path = Some(path);
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Async Tokio File Dialog");

            // Display the path if we have one
            if let Some(path) = &self.picked_path {
                ui.monospace(format!("Selected: {}", path.display()));
            } else {
                ui.label("No file selected.");
            }

            ui.separator();

            // 2. TRIGGER THE TASK
            if ui.button("Open File (Async)").clicked() {
                let tx_clone = self.tx.clone();
                let ctx_clone = ctx.clone(); // Clone context to repaint later

                // Spawn on the existing Tokio runtime
                tokio::spawn(async move {
                    // Use rfd::AsyncFileDialog
                    // This is non-blocking and awaitable
                    let task = rfd::AsyncFileDialog::new()
                        .set_directory("/")
                        .pick_file()
                        .await;

                    // If user picked a file (didn't cancel)
                    if let Some(handle) = task {
                        // Send the path back to the UI thread
                        let _ = tx_clone.send(handle.path().to_path_buf());

                        // IMPORTANT: Request a repaint so the UI updates immediately
                        ctx_clone.request_repaint();
                    }
                });
            }
        });
    }
}

fn main() -> eframe::Result {
    // Your existing Tokio setup
    let rt = Runtime::new().expect("Unable to create Runtime");
    let _enter = rt.enter();

    let options = eframe::NativeOptions::default();
    eframe::run_native(
        "Tokio Dialog App",
        options,
        Box::new(|_cc| Ok(Box::new(MyApp::default()))),
    )
}