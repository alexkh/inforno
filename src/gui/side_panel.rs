use crate::{common::Chat, db::{delete_chat, export_chat_to_markdown, fetch_chat}, gui::State};
use rust_i18n::t;

pub fn ui_side_panel(ctx: &egui::Context, state: &mut State) {
    egui::SidePanel::new(egui::panel::Side::Left, "panel").show(ctx, |ui| {
        // Disable main UI if a modal/rename is open to force focus
        if state.is_modal_open || state.chat_to_rename.is_some() {
            ui.disable();
        }
        egui::ScrollArea::vertical().show(ui, |ui| {

            ui.add(egui::Image::new(
                egui::include_image!("../../assets/inforno.webp"))
                .max_width(260.0)
                .corner_radius(5));

            ui.label(t!("chats_label"));

            // --- NEW: Horizontal Layout for New Chat actions ---
            ui.horizontal(|ui| {
                if ui.button(t!("new_chat_btn")).on_hover_text(
                    egui::RichText::new(t!("new_chat_tooltip"))
                        .strong()
                        .heading()
                    ).clicked() {
                    state.chat = Chat::default();
                }

                if ui.button(t!("new_chat_copying_agents_btn")).on_hover_text(
                    egui::RichText::new(t!("new_chat_copying_agents_tooltip"))
                        .strong()
                        .heading()
                    )
                    .clicked() {
                    let mut template = state.chat.clone();
                    template.id = 0;
                    template.title = "Unnamed Chat".to_string();
                    template.msg_pool.clear();
                    for agent in &mut template.agents {
                        agent.id = 0;
                        agent.msg_ids.clear();
                    }
                    state.chat = template;
                }

                if ui.button(t!("new_chat_copying_prompts_btn")).on_hover_text(
                    egui::RichText::new(t!("new_chat_copying_prompts_tooltip"))
                        .strong()
                        .heading()
                    ).clicked() {
                    extract_prompts(&state.chat, &mut state.bottom_panel_state, &state.project_root);
                    state.chat = Chat::default();
                }

                if ui.button(t!("new_chat_copying_agents_prompts_btn")).on_hover_text(
                    egui::RichText::new(t!("new_chat_copying_agents_prompts_tooltip"))
                        .strong()
                        .heading()
                    ).clicked() {
                    extract_prompts(&state.chat, &mut state.bottom_panel_state, &state.project_root);
                    let mut template = state.chat.clone();
                    template.id = 0;
                    template.title = "Unnamed Chat".to_string();
                    template.msg_pool.clear();
                    for agent in &mut template.agents {
                        agent.id = 0;
                        agent.msg_ids.clear();
                    }
                    state.chat = template;
                }
            });

            let mut to_delete_chat_id = 0;

            // Iterate through chats
            for db_chat in &mut state.db_chats {
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;

                    // --- WRENCH MENU ---
                    // We use menu_button. The label is the wrench icon.
                    ui.menu_button("🔧", |ui| {
                        ui.set_min_width(80.0);

                        // 1. Rename Option
                        if ui.button(egui::RichText::new(t!("rename_chat_btn")))
                            .on_hover_text(egui::RichText::new(
                                    t!("rename_chat_tooltip"))
                            .heading())
                        .clicked() {
                            // Prepare state for renaming
                            state.chat_to_rename = Some(db_chat.id);
                            state.chat_rename_buffer = db_chat.title.clone();
                            ui.close();
                        }

                        ui.separator();

                        // Export to Markdown
                        if ui.button(egui::RichText::new(t!("export_chat_btn")))
                            .on_hover_text(egui::RichText::new(
                                    t!("export_chat_tooltip"))
                            .heading())
                        .clicked() {
                            match export_chat_to_markdown(&state.db_conn, db_chat.id, &state.presets) {
                                Ok(markdown) => {
                                    let tx_clone = state.op_tx.clone();
                                    let title = db_chat.title.clone();
                                    let ctx_clone = ctx.clone();
                                    tokio::spawn(async move {
                                        let task = rfd::AsyncFileDialog::new()
                                            .add_filter("Markdown", &["md"])
                                            .set_file_name(format!("{}.md", title))
                                            .save_file()
                                            .await;

                                        if let Some(handle) = task {
                                            let path = handle.path().to_path_buf();
                                            if let Err(e) = std::fs::write(&path, markdown) {
                                                eprintln!("Failed to write markdown: {}", e);
                                            }
                                        }
                                        ctx_clone.request_repaint();
                                    });
                                }
                                Err(e) => {
                                    eprintln!("Failed to export chat: {}", e);
                                }
                            }
                            ui.close();
                        }

                        ui.separator();

                        // 2. Delete Option (Red)
                        if ui.button(egui::RichText::new(t!("delete_chat_btn"))
                            .color(ui.visuals().error_fg_color))
                            .on_hover_text(egui::RichText::new(
                                    t!("delete_chat_tooltip"))
                            .heading()
                            .color(ui.visuals().error_fg_color))
                        .clicked() {
                             // Try to delete the chat from the database:
                             if let Ok(_) = delete_chat(&state.db_conn,
                                    db_chat.id) {
                                println!("Chat {} deleted", db_chat.id);
                                if state.chat.id == db_chat.id {
                                    state.chat = Chat::default();
                                }
                                to_delete_chat_id = db_chat.id;
                            }
                            ui.close();
                        }
                    })
                    // Add the tooltip to the wrench button itself
                    .response.on_hover_text(t!("chat_options_tooltip"));

                    // --- CHAT SELECTION BUTTON ---
                    let is_selected = state.chat.id == db_chat.id;

                    let btn_response = clipped_button(ui, &db_chat.title, is_selected)
                        .on_hover_text(
                            egui::RichText::new(&db_chat.title)
                                .heading() // Makes it large
                                .strong()  // Makes it bold
                                .color(ui.visuals().strong_text_color()) // Ensures it's bright in both light/dark themes
                        );

                    if btn_response.clicked() {
                        println!("You selected chat {} {}",
                        db_chat.title, db_chat.id);
                        println!("Fetching chat...");
                        state.chat = fetch_chat(
                                &state.db_conn, db_chat.id, &state.presets)
                                .unwrap_or_else(|e| {
                            eprintln!("Could not load chat from database: {}", e);
                            Chat::default()
                        });
                    }
                });
            }

            // Cleanup deleted chats after the loop
            if to_delete_chat_id != 0 {
                state.db_chats.retain(|c| c.id != to_delete_chat_id);
            }
        });
    });

    // --- RENAME POPUP WINDOW ---
    // This draws a small window on top of everything if a chat is being renamed
    render_rename_window(ctx, state);
}

// Helper function to extract prompts and re-attach files
fn extract_prompts(
    chat: &Chat,
    bottom_state: &mut crate::gui::bottom_panel::BottomPanelState,
    project_root: &Option<std::path::PathBuf>,
) {
    let mut first_sys = None;
    let mut first_usr = None;
    let mut details = None;

    // Omnis agent contains all the ordered messages
    if let Some(agent) = chat.agents.first() {
        for msg_id in &agent.msg_ids {
            if let Some(msg) = chat.msg_pool.get(msg_id) {
                if first_sys.is_none() && msg.msg_role == crate::common::MsgRole::System {
                    first_sys = Some(msg.content.clone());
                }
                if first_usr.is_none() && msg.msg_role == crate::common::MsgRole::User {
                    first_usr = Some(msg.content.clone());
                    details = msg.details.clone();
                }
                if first_sys.is_some() && first_usr.is_some() {
                    break;
                }
            }
        }
    }

    if let Some(sys) = first_sys {
        bottom_state.system_prompt_edited = sys;
        bottom_state.show_system_prompt = true;
    } else {
        bottom_state.system_prompt_edited.clear();
        bottom_state.show_system_prompt = false;
    }

    if let Some(usr) = first_usr {
        bottom_state.prompt_edited = usr;
    } else {
        bottom_state.prompt_edited.clear();
    }

    bottom_state.pending_attachments.clear();
    if let Some(det) = details {
        if let Ok(atts) = serde_json::from_str::<Vec<crate::common::Attachment>>(&det) {
            for mut att in atts {
                let mut reattached = false;

                // Try to re-attach from project root
                if let Some(root) = project_root {
                    let path = root.join(&att.filename);
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        att.content = content;
                        reattached = true;
                    }
                }

                // Try to re-attach if it's an absolute path
                if !reattached {
                    let path = std::path::PathBuf::from(&att.filename);
                    if path.is_absolute() {
                        if let Ok(content) = std::fs::read_to_string(&path) {
                            att.content = content;
                        }
                    }
                }
                bottom_state.pending_attachments.push(att);
            }
        }
    }
}

fn clipped_button(ui: &mut egui::Ui, text: &str, is_selected: bool)
        -> egui::Response {
    // 1. Calculate the height based on current font style
    let font_id = egui::TextStyle::Button.resolve(ui.style());
    let height = ui.text_style_height(&egui::TextStyle::Button)
            + ui.spacing().button_padding.y * 2.0;

    // 2. Allocate space strictly based on AVAILABLE width, ignoring text length.
    //    We use allocate_rect to tell the layout: "I only exist in this box."
    let desired_size = egui::vec2(ui.available_width(), height);
    let (rect, response) = ui.allocate_exact_size(desired_size,
            egui::Sense::click());

    // 3. Draw the Interaction (Hover/Click/Selected effects)
    if ui.is_rect_visible(rect) {
        // Decide visual style (selected vs normal)
        let visuals = if is_selected {
            &ui.style().visuals.widgets.active // or open
        } else {
            ui.style().interact(&response)
        };

        // Draw background
        ui.painter().rect(
            rect,
            visuals.corner_radius,
            visuals.bg_fill,
            visuals.bg_stroke,
            egui::StrokeKind::Inside,
        );

        // 4. Draw Text with Hard Clipping
        //    We create a temporary painter that ONLY draws inside the button rect.
        //    Any text spilling out is strictly invisible.
        let painter = ui.painter().with_clip_rect(rect);

        let text_pos = rect.min + ui.spacing().button_padding;
        painter.text(
            text_pos,
            egui::Align2::LEFT_TOP,
            text,
            font_id,
            visuals.text_color(),
        );
    }

    response
}

// Helper function to handle the popup logic
fn render_rename_window(ctx: &egui::Context, state: &mut State) {
    if let Some(chat_id) = state.chat_to_rename.clone() {
        let mut open = true;

        // Center the window and fix the size
        egui::Window::new("Rename Chat")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {

                ui.label("Enter new name:");

                // Text input
                let response = ui.text_edit_singleline(
                        &mut state.chat_rename_buffer);

                // Auto-focus the input box when window opens
                if response.lost_focus() && ui.input(
                        |i| i.key_pressed(egui::Key::Enter)) {
                    // Trigger save on Enter key
                    save_rename(state, chat_id);
                }

                ui.add_space(10.0);

                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        state.chat_to_rename = None;
                    }
                    if ui.button("Save").clicked() {
                        save_rename(state, chat_id);
                    }
                });
            });

        // Handle window close via 'X' button
        if !open {
            state.chat_to_rename = None;
        }
    }
}

// Helper to save changes to DB and State
fn save_rename(state: &mut State, chat_id: i64) {
    if let Some(target_db_chat) =
            state.db_chats.iter_mut().find(|c| c.id == chat_id) {

        // Update DB (You need to implement rename_chat in your db module)
        if let Err(error) = crate::db::mod_chat_title(&state.db_conn,
                chat_id, &state.chat_rename_buffer) {
            println!("Error: could not rename chat in the Sandbox: {}", error);
            return;
        }

        // Update the chat object locally
        target_db_chat.title = state.chat_rename_buffer.clone();

        // Update current chat if it's the one open
        if state.chat.id == chat_id {
            state.chat.title = target_db_chat.title.clone();
        }

        println!("Renamed chat {} to {}", chat_id, target_db_chat.title);
        state.chat_to_rename = None;
    }
}
