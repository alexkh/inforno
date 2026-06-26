use crate::{common::Chat, db::{delete_chat, export_chat_to_markdown, fetch_chat}, gui::{State, split_button}};
use rust_i18n::t;
use split_button::SplitButton;

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

            ui.horizontal(|ui| {
                let response = ui.add(
                    egui::TextEdit::singleline(&mut state.search_query)
                        .hint_text("Search chats...")
                        .desired_width(ui.available_width() - 80.0)
                );

                // Allow triggering search on button click OR pressing Enter
                let enter_pressed = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));

                if (ui.button("🔍").clicked() || enter_pressed) && !state.search_query.trim().is_empty() {
                    if let Ok(results) = crate::db::search_chats(&state.db_conn, &state.search_query) {
                        crate::gui::panes::open_search_results_in_tab(state, state.search_query.clone(), results);
                    }
                }
            });

            // --- NEW: Horizontal Layout for New Chat actions ---
            ui.horizontal(|ui| {
                // Helper to find the next available temporary ID (0, -1, -2...)
                let get_temp_id = |state: &State| {
                    let mut id = 0;
                    while state.open_chats.contains_key(&id) { id -= 1; }
                    id
                };

                let new_chat_text = t!("new_chat_btn").to_string();
                let new_chat_tooltip = t!("new_chat_tooltip").to_string();

                // Calculate exactly how much width the button needs
                let font_id = egui::TextStyle::Button.resolve(ui.style());
                let text_width = ui.painter().layout_no_wrap(
                    new_chat_text.clone(), font_id,
                    egui::Color32::TRANSPARENT).size().x;
                let button_padding = ui.spacing().button_padding;
                let text_height = ui.text_style_height(&egui::TextStyle::Button);
                let height = text_height + button_padding.y * 2.0;

                // Base text width + extra allocated space for the double-wide arrow
                let desired_width = text_width + button_padding.x * 2.0;

                let (main_clicked, arrow_clicked) = SplitButton::new(&new_chat_text)
                    .id_salt("new_chat_btn")
                    .main_tooltip(&new_chat_tooltip)
                    .arrow_tooltip(t!("right_button_tooltip"))
                    .desired_width(desired_width)
                    .show(ui);

                if main_clicked {
                    let temp_id = get_temp_id(state);
                    let mut new_chat = Chat::default();
                    new_chat.id = temp_id;
                    state.open_chats.insert(temp_id, new_chat);
                    crate::gui::panes::open_chat_in_tab(state, temp_id);
                }

                if arrow_clicked {
                    let temp_id = get_temp_id(state);
                    let mut new_chat = Chat::default();
                    new_chat.id = temp_id;
                    state.open_chats.insert(temp_id, new_chat);
                    crate::gui::panes::open_chat_in_right_pane(state, temp_id);
                }

                if ui.button(t!("new_chat_copying_agents_btn")).on_hover_text(egui::RichText::new(t!("new_chat_copying_agents_tooltip")).strong().heading()).clicked() {
                    let active_id = state.active_chat_id.unwrap_or(0);
                    let temp_id = get_temp_id(state);
                    let mut template = state.open_chats.get(&active_id).cloned().unwrap_or_default();
                    template.id = temp_id;
                    template.title = "Unnamed Chat".to_string();
                    template.msg_pool.clear();
                    for agent in &mut template.agents {
                        agent.id = 0;
                        agent.msg_ids.clear();
                    }
                    state.open_chats.insert(temp_id, template);
                    crate::gui::panes::open_chat_in_tab(state, temp_id);
                }

                if ui.button(t!("new_chat_copying_prompts_btn")).on_hover_text(egui::RichText::new(t!("new_chat_copying_prompts_tooltip")).strong().heading()).clicked() {
                    let active_id = state.active_chat_id.unwrap_or(0);
                    if let Some(chat) = state.open_chats.get(&active_id) {
                        extract_prompts(chat, &mut state.bottom_panel_state, &state.project_root);
                    }
                    let temp_id = get_temp_id(state);
                    let mut new_chat = Chat::default();
                    new_chat.id = temp_id;
                    state.open_chats.insert(temp_id, new_chat);
                    crate::gui::panes::open_chat_in_tab(state, temp_id);
                }

                if ui.button(t!("new_chat_copying_agents_prompts_btn")).on_hover_text(egui::RichText::new(t!("new_chat_copying_agents_prompts_tooltip")).strong().heading()).clicked() {
                    let active_id = state.active_chat_id.unwrap_or(0);
                    let temp_id = get_temp_id(state);
                    let mut template = state.open_chats.get(&active_id).cloned().unwrap_or_default();
                    extract_prompts(&template, &mut state.bottom_panel_state, &state.project_root);
                    template.id = temp_id;
                    template.title = "Unnamed Chat".to_string();
                    template.msg_pool.clear();
                    for agent in &mut template.agents {
                        agent.id = 0;
                        agent.msg_ids.clear();
                    }
                    state.open_chats.insert(temp_id, template);
                    crate::gui::panes::open_chat_in_tab(state, temp_id);
                }
            });

            let mut to_delete_chat_id = 0;
            let mut clicked_chat_id = None; // 1. Create a temporary holder
            let mut right_clicked_chat_id: Option<i64> = None; // track right arrow clicks

            // Iterate through chats
            for db_chat in &mut state.db_chats {
                ui.horizontal_top(|ui| {
                    ui.set_max_height(20.0);
                    ui.spacing_mut().item_spacing.x = 2.0;

                    let is_selected = state.active_chat_id == Some(db_chat.id);

                    // ONE layout. Right-to-Left.
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {

                        // 1. The Wrench Menu (Rendered first, placed on far right)
                        ui.menu_button("🔧", |ui| {
                            ui.set_min_width(80.0);

                            if ui.button(egui::RichText::new(t!("rename_chat_btn"))).on_hover_text(egui::RichText::new(t!("rename_chat_tooltip")).heading()).clicked() {
                                state.chat_to_rename = Some(db_chat.id);
                                state.chat_rename_buffer = db_chat.title.split('\n').next().unwrap_or(&db_chat.title).trim().to_string();
                                ui.close();
                            }

                            ui.separator();

                            if ui.button(egui::RichText::new(t!("export_chat_btn"))).on_hover_text(egui::RichText::new(t!("export_chat_tooltip")).heading()).clicked() {
                                if let Ok(markdown) = export_chat_to_markdown(&state.db_conn, db_chat.id, &state.presets) {
                                    // Trigger the native egui dialog for saving
                                    state.pending_file_dialog_op = Some(crate::common::FileOp::ExportChat);
                                    state.pending_export_content = Some(markdown);

                                    // Create a safe default filename based on the chat's title
                                    let safe_title = db_chat.title.replace(|c: char| !c.is_alphanumeric() && c != ' ' && c != '-', "_");
                                    let default_name = format!("{}.md", safe_title); // <--- Create the String

                                    state.file_dialog = egui_file_dialog::FileDialog::new()
                                        .default_file_name(&default_name) // <--- Pass it as a reference (&str)
                                        .add_file_filter("Markdown", std::sync::Arc::new(|p: &std::path::Path| p.extension().is_some_and(|ext| ext == "md")));
                                    state.file_dialog.save_file();
                                }
                                ui.close();
                            }

                            ui.separator();

                            if ui.button(egui::RichText::new(t!("delete_chat_btn")).color(ui.visuals().error_fg_color)).on_hover_text(egui::RichText::new(t!("delete_chat_tooltip")).heading().color(ui.visuals().error_fg_color)).clicked() {
                                if let Ok(_) = delete_chat(&state.db_conn, db_chat.id) {
                                    if state.active_chat_id == Some(db_chat.id) {
                                        state.open_chats.remove(&db_chat.id);
                                        state.open_chats.insert(0, Chat::default());
                                        state.active_chat_id = Some(0);
                                    }
                                    to_delete_chat_id = db_chat.id;
                                }
                                ui.close();
                            }
                        }).response.on_hover_text(t!("chat_options_tooltip"));

                        // 2. The Pane Badges (Rendered second, placed to the left of the wrench)
                        if let Some(locations) = state.chat_locations.get(&db_chat.id) {
                            for loc in locations.iter().rev() {
                                ui.label(
                                    egui::RichText::new(loc)
                                        .strong()
                                        .background_color(ui.visuals().code_bg_color)
                                ).on_hover_text(format!("Open in Pane {}", loc));
                            }
                        }

                        // 3. The Unified Split Button
                        // We pass the full available width to our custom component, which handles the hover split automatically.
                        let available_width = ui.available_width();
                        let display_title = db_chat.title.split('\n').next().unwrap_or(&db_chat.title).trim();
                        let (main_clicked, arrow_clicked) = SplitButton::new(display_title)
                            .id_salt(db_chat.id)
                            .selected(is_selected)
                            .transparent(true) // Transparent for sidebar!
                            .main_tooltip(&db_chat.title)
                            .arrow_tooltip(t!("right_button_tooltip"))
                            .desired_width(available_width)
                            .arrow_width(35.0)
                            .show(ui);

                        if main_clicked {
                            clicked_chat_id = Some(db_chat.id);
                        }
                        if arrow_clicked {
                            right_clicked_chat_id = Some(db_chat.id);
                        }
                    });
                });
            }

            // Cleanup deleted chats after the loop
            if to_delete_chat_id != 0 {
                state.db_chats.retain(|c| c.id != to_delete_chat_id);
            }

            // 3. Handle the click outside the loop safely!
            if let Some(chat_id) = clicked_chat_id {
                if !state.open_chats.contains_key(&chat_id) {
                    let loaded_chat = fetch_chat(&state.db_conn, chat_id, &state.presets).unwrap_or_default();
                    state.open_chats.insert(chat_id, loaded_chat);
                }
                crate::gui::panes::open_chat_in_tab(state, chat_id);
            }

            // Handle opening in the right pane!
            if let Some(chat_id) = right_clicked_chat_id {
                if !state.open_chats.contains_key(&chat_id) {
                    let loaded_chat = fetch_chat(&state.db_conn, chat_id, &state.presets).unwrap_or_default();
                    state.open_chats.insert(chat_id, loaded_chat);
                }
                crate::gui::panes::open_chat_in_right_pane(state, chat_id);
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

        let clean_title = state.chat_rename_buffer.split('\n').next().unwrap_or(&state.chat_rename_buffer).trim().to_string();

        // Update DB
        if let Err(error) = crate::db::mod_chat_title(&state.db_conn,
                chat_id, &clean_title) {
            println!("Error: could not rename chat in the Sandbox: {}", error);
            return;
        }

        // Update the sidebar chat list locally
        target_db_chat.title = clean_title;

        // --- NEW: Unconditionally update the chat object if it's loaded in memory ---
        if let Some(chat) = state.open_chats.get_mut(&chat_id) {
            chat.title = target_db_chat.title.clone();
        }

        println!("Renamed chat {} to {}", chat_id, target_db_chat.title);
        state.chat_to_rename = None;
    }
}
