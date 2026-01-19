use std::sync::{Arc, Mutex};

use egui::{Align, Key, Layout, Modifiers, RichText, Ui, Vec2b};
use egui_autocomplete::AutoCompleteTextEdit;
use ollama_rs::Ollama;
use rand::Rng;
use rusqlite::Connection;
use rust_i18n::t;
use tokio_stream::StreamExt;

use crate::{
    common::{
        ChatRouter, DbOllamaModel, DbOpenrModel, ModelOptions, OllamaDownloading, Preset, PresetSelection, Presets, cloud_color, format_bytes, load_presets, local_color, router_color, err_color, strong_color
    },
    db::{
        cache::{
            clear_ollama_cache, get_ollama_model_info, get_openr_model_info,
            save_ollama_model,
        },
        delete_preset, save_preset,
    },
    gui::State,
};

// --- Data Structures ---
#[derive(Default)]
pub struct PresetEditorState {
    pub selected_preset: PresetSelection,
    pub edited_preset: Preset,
    pub editing: bool,
    pub openr_model_info: Option<DbOpenrModel>,
    pub seed_entered: String,
    pub temperature_entered: String,
    pub router_changed: bool,
    pub is_model_valid: bool,
    pub is_seed_valid: bool,
    pub is_temperature_valid: bool,
    pub ollama_only_installed: bool,
    pub ollama_model_info: Option<DbOllamaModel>,
    pub ollama_downloading: Arc<Mutex<OllamaDownloading>>,
}

// --- Macros ---

macro_rules! validated_edit {
    ($ui:expr, $label:expr, $width:expr, $text_var:expr, $is_valid:expr,
            $logic:block, $revert_logic:block) => {
        $ui.horizontal(|ui| {
            let color = if $is_valid {
                strong_color()
            } else {
                err_color()
            };
            ui.label($label);
            // The Revert Button
            if ui.button("⟲").on_hover_text(t!("revert_to_initial_tooltip"))
                    .clicked() {
                $revert_logic
            }
            let _response = ui.add(
                egui::TextEdit::singleline($text_var)
                .text_color(color)
                .desired_width($width),
            );
            $logic
        });
    };
    (
        $ui:expr,
        $label:expr,
        $width:expr,
        $text_var:expr,
        $is_valid:expr,
        $logic:block,
        $button_text:expr,
        $button_logic:block,
        $revert_logic:block
    ) => {
        $ui.horizontal(|ui| {
            let color = if $is_valid {
                strong_color()
            } else {
                err_color()
            };
            ui.label($label);
            // The Revert Button
            if ui.button("⟲").on_hover_text(t!("revert_to_initial_tooltip"))
                    .clicked() {
                $revert_logic
            }
            let _response = ui.add(
                egui::TextEdit::singleline($text_var)
                .text_color(color)
                .desired_width($width),
            );
            $logic;
            if ui.button($button_text).clicked() {
                $button_logic
            }
        });
    };
}

// --- Main Entry Point ---

pub fn ui_preset_editor(ctx: &egui::Context, state: &mut State) {
    if !state.show_preset_editor {
        return;
    }

    // we need a pass a mutable variable to enable the window's "close" button
    let mut show_preset_editor = state.show_preset_editor;

    // Preset Editor Window
    egui::Window::new(t!("preset_editor"))
        .collapsible(false)
        .scroll(Vec2b { x: false, y: true })
        .open(&mut show_preset_editor)
        .default_width(500.)
        .show(ctx, |ui| {
            if state.is_modal_open {
                ui.disable();
            }

            // 1. Handle Background Logic (Shortcuts, Saves)
            handle_global_shortcuts(ui, state);

            // 2. Select View Mode
            if state.preset_editor_state.editing {
                render_edit_mode(ui, ctx, state);
            } else {
                render_view_mode(ui, state);
            }
        });

    // Update state based on local boolean
    if !show_preset_editor {
        state.show_preset_editor = show_preset_editor;
    }
}

// --- Logic Helpers ---
fn save_active_preset(
    conn: &Connection,
    edited_preset: &mut Preset,
    presets: &mut Presets,
    error_msg: &mut Option<String>,
) {
    // We now use the passed variables directly
    match save_preset(conn, edited_preset) {
        Ok(val) => {
            if val == 0 {
                eprintln!("Error updating Preset. No rows changed");
            } else {
                println!("Preset saved successfully! id = {}", val);
                edited_preset.id = val;
                // Reload the presets list
                load_presets(conn, presets);
            }
        }
        Err(error) => {
            *error_msg = Some(format!("Error updating Preset: {}", error));
            eprintln!("Error updating Preset: {}", error);
        }
    }
}

fn handle_global_shortcuts(ui: &mut egui::Ui, state: &mut State) {
}

// --- View Mode ---

fn render_view_mode(ui: &mut egui::Ui, state: &mut State) {
    let substate = &mut state.preset_editor_state;

    ui.colored_label(
        ui.visuals().hyperlink_color, t!("preset_editor_invitation"),
    );

    ui.horizontal(|ui| {
        // New Preset Button
        if ui.button(t!("preset_new_btn")).clicked() {
            substate.edited_preset = Preset::default();
            substate.editing = true;
            substate.router_changed = true;
        }

        let num_presets = state.presets.cache.len();
        if num_presets > 0 {
            ui.label(t!("preset_or_select_existing_one"));

            crate::gui::bottom_panel::preset_combo_box(ui, "pe_preset_select",
                &mut substate.selected_preset,
                &state.presets);

            let mut start_editing = |preset: Preset| {
                substate.edited_preset = preset;
                substate.seed_entered = substate
                    .edited_preset
                    .options
                    .seed
                    .map(|n| n.to_string())
                    .unwrap_or_default();
                substate.temperature_entered = substate
                    .edited_preset
                    .options
                    .temperature
                    .map(|n| n.to_string())
                    .unwrap_or_default();
                substate.editing = true;
                substate.router_changed = true;
            };

            if ui.button(t!("preset_edit_btn")).clicked() {
                if let Some(preset) = state.presets.get(
                            substate.selected_preset.id).cloned() {
                    start_editing(preset.clone());
                }
            }

            if ui.button(t!("preset_duplicate_btn")).clicked() {
                if let Some(mut preset) = state.presets.get(
                            substate.selected_preset.id).cloned() {
                    preset.id = 0;
                    preset.title.push_str(" Copy");
                    start_editing(preset);
                }
            }
        }
    });

    if state.presets.cache.len() > 0 &&
            let Some(preset) = state.presets.get(substate.selected_preset.id) {
        ui.add_space(10.0);

        let total_width = ui.available_width();
        let label_col_width = 100.0;
        let value_col_width = total_width - label_col_width - 20.0;

        egui::Grid::new("preset_view_grid")
            .num_columns(2)
            .striped(true)
            .min_col_width(label_col_width)
            .show(ui, |ui| {
            let mut row = |label: &str, value: String| {
                ui.label(label);
                ui.add_sized(
                    [value_col_width, 20.0], |ui: &mut egui::Ui| {
                    ui.with_layout(Layout::top_down_justified(Align::LEFT), |ui| {
                          ui.label(value);
                    }).response
                });
                ui.end_row();
            };

            // --- Rows ---
            row(&t!("router_label"), preset.chat_router.to_string());
            row(&t!("model_label"), preset.model.clone());
            row(&t!("tooltip_label"), preset.tooltip.clone());

            row(&t!("reasoning_label"), preset.options.include_reasoning
                .map_or(t!("unset").to_string(), |s| {
                if s { t!("yes").to_string() } else { t!("no").to_string() }
            }));

            row(&t!("seed_label"), preset.options.seed.map_or(
                    t!("unset").to_string(), |s| s.to_string()));

            row(&t!("temperature_label"), preset.options.temperature
                    .map_or(t!("unset").to_string(), |s| s.to_string()));
        });
    }
}

// --- Edit Mode ---

fn render_edit_mode(ui: &mut egui::Ui, ctx: &egui::Context, state: &mut State) {
    render_edit_action_buttons(ui, state);

    let substate = &mut state.preset_editor_state;

    // determine Dynamic Title Color based on current selection
    let current_title_color = router_color(&substate.edited_preset.chat_router);

    // Title Edit
    ui.horizontal(|ui| {
        ui.label("Title of Preset:");
        ui.add(egui::TextEdit::singleline(&mut substate.edited_preset.title)
                .text_color(current_title_color));
    });

    // Router Selection
    ui.horizontal(|ui| {
        ui.label("Select a Router:");
        if ui
            .radio_value(
                &mut substate.edited_preset.chat_router,
                ChatRouter::Ollama,
                egui::RichText::new("Ollama").color(local_color()),
            )
            .changed()
        {
            substate.router_changed = true;
        }
        if ui
            .radio_value(
                &mut substate.edited_preset.chat_router,
                ChatRouter::Openrouter,
                egui::RichText::new("Openrouter").color(cloud_color()),
            )
            .changed()
        {
            substate.router_changed = true;
        }
    });

    ui.separator();

    match substate.edited_preset.chat_router {
        ChatRouter::Ollama => render_ollama_editor(ui, ctx, state),
        ChatRouter::Openrouter => render_openrouter_editor(ui, state),
    }
}

fn render_edit_action_buttons(ui: &mut egui::Ui, state: &mut State) {
    let substate = &mut state.preset_editor_state;
    ui.horizontal(|ui| {
        // Save and Exit Button
        if ui.button(t!("preset_save_and_exit_btn")).clicked() {
            save_active_preset(&state.db_conn, &mut substate.edited_preset,
                    &mut state.presets, &mut state.error_msg);
            state.show_preset_editor = false;
        }
        // Save and Go Back Button
        if ui.button(t!("preset_save_and_go_back_btn")).clicked() {
            save_active_preset(&state.db_conn, &mut substate.edited_preset,
                    &mut state.presets, &mut state.error_msg);
            substate.editing = false;
        }
        // Back Without Saving Button
        if ui.button(t!("preset_back_without_saving_btn")).clicked() {
            substate.editing = false;
        }
        // Save Button
        if ui.button(t!("preset_save_btn")).clicked() {
            save_active_preset(&state.db_conn, &mut substate.edited_preset,
                    &mut state.presets, &mut state.error_msg);
        }
        // Save a Copy Button
        if ui.button(t!("preset_save_a_copy_btn")).clicked() {
            substate.edited_preset.id = 0;
            save_active_preset(&state.db_conn, &mut substate.edited_preset,
                    &mut state.presets, &mut state.error_msg);
        }

        // Delete... menu
        // Prepare disjoint borrows for the closure to avoid conflicts
        let conn = &state.db_conn;
        let presets = &mut state.presets;

        ui.menu_button(egui::RichText::new(t!("preset_delete_menu")), |ui| {

            ui.set_min_width(80.0);

            // 1. Cancel Option
            if ui.button(t!("preset_delete_cancel")).clicked() {
                ui.close();
            }

            // 2. Confirm Delete Option
            if ui.button(egui::RichText::new(t!("preset_delete_btn"))
                .color(err_color())).clicked()
            {
                match delete_preset(conn, substate.edited_preset.id) {
                    Ok(_) => {
                        println!("preset {} deleted successfully",
                            substate.edited_preset.id);
                        substate.selected_preset = PresetSelection::default();
                    },
                    Err(error) => {
                        println!("error deleting preset {}: {}",
                            substate.edited_preset.id, error);
                    },
                };
                // Refresh list and exit editing
                let _ = load_presets(conn, presets);
                substate.editing = false;
                ui.close();
            }
        })
        .response
        .on_hover_text(t!("preset_delete_btn_tooltip"));
    });
    ui.separator();
}

// --- Ollama Editor Logic ---

pub fn render_ollama_editor(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    state: &mut State,
) {
    ui.horizontal(|ui| {
        ui.label("Select a Model:");
        let substate = &mut state.preset_editor_state;
        let model_color = if substate.is_model_valid {
            strong_color()
        } else {
            err_color()

        };

        let source_list = if substate.ollama_only_installed {
            &state.ollama_model_names_installed
        } else {
            &state.ollama_model_names
        };

        let mut response = ui.add(
            AutoCompleteTextEdit::new(
                &mut substate.edited_preset.model,
                source_list,
            )
            .max_suggestions(30)
            .highlight_matches(true)
            .set_text_edit_properties(move |t| t.text_color(model_color)),
        );

        // conditionally attach the tooltip, only when it's not empty
        if !substate.edited_preset.tooltip.is_empty() {
            response = response.on_hover_text(
                egui::RichText::new(&substate.edited_preset.tooltip)
                    .strong()
                    .heading()
                    .color(router_color(&substate.edited_preset.chat_router))
            );
        }

        if response.changed() || substate.router_changed {
            substate.router_changed = false;
            let is_valid = if substate.ollama_only_installed {
                state
                    .ollama_model_names_installed
                    .contains(&substate.edited_preset.model)
            } else {
                state
                    .ollama_model_names
                    .contains(&substate.edited_preset.model)
            };
            substate.is_model_valid = is_valid;

            substate.edited_preset.tooltip = "".to_string();
            if is_valid {
                if let Some(conn) = &state.cache_conn {
                    let search_name = substate
                        .edited_preset
                        .model
                        .split_once(':')
                        .map(|(p, _)| p)
                        .unwrap_or(&substate.edited_preset.model);
                    if let Ok(info) = get_ollama_model_info(conn, search_name) {
                        substate.ollama_model_info = info;
                        if let Some(inf) = &substate.ollama_model_info {
                            // 1. Extract the tag (e.g., "32b-instruct") from the selected model string.
                            // If there is no ':', it corresponds to the base model, which implies the "latest" tag.
                            let selected_tag = substate
                                .edited_preset
                                .model
                                .split_once(':')
                                .map(|(_, tag)| tag)
                                .unwrap_or("latest");

                            // 2. Find the size string in the variants vector where the first element matches the tag
                            let size_str = inf
                                .variants
                                .iter()
                                .find(|(tag, _)| tag == selected_tag)
                                .map(|(_, size)| size.as_str())
                                .unwrap_or("Unknown");

                            // 3. Set the tooltip
                            substate.edited_preset.tooltip = format!(
                                    "L: {}: {}", inf.name, size_str);
                        }
                    }
                }
            }
        }

        if ui
            .checkbox(&mut substate.ollama_only_installed, "Installed Only")
            .changed()
        {
            substate.router_changed = true;
        }

        render_ollama_download_button(ui, ctx, substate);
    });

    render_ollama_download_progress(ui, state);

    if let Some(original_preset) = state.presets.get(
                state.preset_editor_state.selected_preset.id) {
        render_common_options(ui, &mut state.preset_editor_state,
                &original_preset.options);
    } else {
        // if preset does not exist yet, create a temporary ModelOptions
        let default_model_options = ModelOptions::default();
        render_common_options(ui, &mut state.preset_editor_state,
                &default_model_options);
    }

    ui.separator();

    if let Some(info) = &state.preset_editor_state.ollama_model_info {
        ui.heading("Model Description:");
        egui::ScrollArea::vertical()
            .min_scrolled_height(100.0)
            .show(ui, |ui| {
                ui.label(info.summary.clone().unwrap_or_default());
            });
    }
}

fn render_ollama_download_button(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    substate: &mut PresetEditorState,
) {
    let is_dl = substate.ollama_downloading.lock().unwrap().is_downloading;

    if ui.add_enabled(!is_dl, egui::Button::new("Download Model")).clicked() {
        {
            let mut oll_dl = substate.ollama_downloading.lock().unwrap();
            oll_dl.is_downloading = true;
            oll_dl.progress = 0.0;
            oll_dl.error_msg = None;
            oll_dl.status_text = "Starting...".to_string();
        }

        let state_clone = substate.ollama_downloading.clone();
        let ctx_clone = ctx.clone();
        let model_name = substate.edited_preset.model.clone();

        tokio::spawn(async move {
            let ollama = Ollama::default();
            match ollama.pull_model_stream(model_name, false).await {
                Ok(mut stream) => {
                    while let Some(res) = stream.next().await {
                        let mut oll_dl = state_clone.lock().unwrap();
                        match res {
                            Ok(status) => {
                                oll_dl.status_text = status.message.clone();
                                if let Some(total) = status.total {
                                    if let Some(comp) = status.completed {
                                        oll_dl.progress =
                                            comp as f32 / total as f32;
                                        oll_dl.progress_text = format!(
                                            "{} / {}",
                                            format_bytes(comp),
                                            format_bytes(total)
                                        );
                                    }
                                }
                                ctx_clone.request_repaint();
                            }
                            Err(e) => {
                                oll_dl.error_msg = Some(e.to_string());
                                oll_dl.is_downloading = false;
                                break;
                            }
                        }
                    }
                    let mut oll_dl = state_clone.lock().unwrap();
                    oll_dl.is_downloading = false;
                    oll_dl.status_text = "Done!".to_string();
                    oll_dl.progress = 1.0;
                    ctx_clone.request_repaint();
                }
                Err(e) => {
                    let mut oll_dl = state_clone.lock().unwrap();
                    oll_dl.error_msg = Some(format!("Failed start: {}", e));
                    oll_dl.is_downloading = false;
                    ctx_clone.request_repaint();
                }
            }
        });
    }
}

fn render_ollama_download_progress(ui: &mut egui::Ui, state: &State) {
    let dl_state =
        state.preset_editor_state.ollama_downloading.lock().unwrap();
    if dl_state.is_downloading || dl_state.progress > 0.0 {
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.label(format!("Status: {}", dl_state.status_text));
            ui.with_layout(
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| {
                    ui.label(&dl_state.progress_text);
                },
            );
        });
        ui.add(
            egui::ProgressBar::new(dl_state.progress)
                .show_percentage()
                .animate(dl_state.is_downloading),
        );
    }
    if let Some(err) = &dl_state.error_msg {
        ui.colored_label(err_color(), format!("Error: {}", err));
    }
}

// --- OpenRouter Editor Logic ---

pub fn render_openrouter_editor(ui: &mut egui::Ui, state: &mut State) {
    ui.horizontal(|ui| {
        ui.label(t!("select_a_model"));
        let substate = &mut state.preset_editor_state;
        let model_color = if substate.is_model_valid {
            strong_color()
        } else {
            err_color()
        };

        let mut response = ui.add(
            AutoCompleteTextEdit::new(
                &mut substate.edited_preset.model,
                &state.openr_model_names,
            )
            .max_suggestions(10)
            .highlight_matches(true)
            .set_text_edit_properties(move |t| {
                t.text_color(model_color)
                .desired_width(f32::INFINITY)
            }),
        );

        // conditionally attach the tooltip, only when it's not empty
        if !substate.edited_preset.tooltip.is_empty() {
            response = response.on_hover_text(
                egui::RichText::new(&substate.edited_preset.tooltip)
                    .strong()
                    .heading()
                    .color(router_color(&substate.edited_preset.chat_router))
            );
        }

        if response.changed() || substate.router_changed {
            substate.router_changed = false;
            let is_valid = state
                .openr_model_names
                .contains(&substate.edited_preset.model);
            substate.is_model_valid = is_valid;

            substate.edited_preset.tooltip = "".to_string();
            if is_valid {
                if let Some(conn) = &state.cache_conn {
                    if let Ok(info) = get_openr_model_info(
                        conn,
                        &substate.edited_preset.model,
                    ) {
                        substate.openr_model_info = info;
                        if let Some(inf) = &substate.openr_model_info {
                            substate.edited_preset.tooltip =
                            format!("R: {}, P:${:.2}/M, C:${:.2}/M",
                                    inf.name.clone(),
                                    inf.price_prompt.unwrap_or(0.0) * 1e6,
                                    inf.price_completion.unwrap_or(0.0) * 1e6);
                        }
                    }
                }
            }
        }
    });

    if let Some(original_preset) = state.presets.get(
                state.preset_editor_state.selected_preset.id) {
        render_common_options(ui, &mut state.preset_editor_state,
                &original_preset.options);
    } else {
        // if preset does not exist yet, create a temporary ModelOptions
        let default_model_options = ModelOptions::default();
        render_common_options(ui, &mut state.preset_editor_state,
                &default_model_options);
    }

    ui.separator();

    if let Some(info) = &state.preset_editor_state.openr_model_info {
        ui.label(format!("Model Name: {}", info.name));
        ui.horizontal(|ui| {
            ui.label(format!("Context Length: {}", info.context_length));
            ui.label(format!(
                "Date: {}",
                info.ts_model.clone().unwrap_or_default()
            ));
        });
        ui.label(format!(
            "Prompt: ${:.2}/M, Completion: ${:.2}/M",
            info.price_prompt.unwrap_or(0.0) * 1e6,
            info.price_completion.unwrap_or(0.0) * 1e6
        ));

        ui.heading("Description:");
        egui::ScrollArea::vertical()
            .min_scrolled_height(100.0)
            .show(ui, |ui| {
                ui.label(&info.description);
            });
    }
}

// --- Deduplicated Options ---

fn show_original_value(ui: &mut Ui, text: String) {
    ui.label(RichText::new(format!("({}: {})", t!("currently"), text)));
}

pub fn render_common_options(
    ui: &mut egui::Ui,
    substate: &mut PresetEditorState,
    original_options: &ModelOptions,
) {
    // --- Reasoning ---
    ui.horizontal(|ui| {
        ui.label(t!("include_reasoning"));

        // Visualizing the original value
        let orig_text = match original_options.include_reasoning {
            Some(true) => t!("yes"),
            Some(false) => t!("no"),
            None => t!("unset"),
        };
        show_original_value(ui, orig_text.to_string());

        // Manual Revert Button for Radio Group
        if ui.button("⟲").on_hover_text(t!("revert_to_initial_tooltip"))
                .clicked() {
            substate.edited_preset.options.include_reasoning =
                    original_options.include_reasoning;
        }

        ui.radio_value(
            &mut substate.edited_preset.options.include_reasoning,
            None,
            t!("unset"),
        );
        ui.radio_value(
            &mut substate.edited_preset.options.include_reasoning,
            Some(true),
            t!("yes"),
        );
        ui.radio_value(
            &mut substate.edited_preset.options.include_reasoning,
            Some(false),
            t!("no"),
        );
    });

    let seed_label = if let Some(s) = original_options.seed {
        format!("{} ({}: {}):", t!("seed"), t!("currently"), s)
    } else {
        format!("{} ({}: {}):", t!("seed"), t!("currently"), t!("unset"))
    };

    // --- Seed using the Macro ---
    validated_edit!(
        ui,
        &seed_label,
        150.0,
        &mut substate.seed_entered,
        substate.is_seed_valid,
        // Validation Logic
        {
            if substate.seed_entered.is_empty() {
                substate.edited_preset.options.seed = None;
                substate.is_seed_valid = true;
            } else {
                match substate.seed_entered.parse::<i32>() {
                    Ok(val) => {
                        substate.edited_preset.options.seed = Some(val.abs());
                        substate.is_seed_valid = true;
                    }
                    Err(_) => substate.is_seed_valid = false,
                }
            }
        },
        t!("generate_random_seed_btn"),
        // Generate Button Logic
        {
            let new_seed: i32 = rand::rng().random_range(0..=i32::MAX);
            substate.seed_entered = new_seed.to_string();
            substate.edited_preset.options.seed = Some(new_seed);
            substate.is_seed_valid = true;
        },
        // Revert Logic
        {
            substate.seed_entered = original_options.seed.map(|s| s.to_string())
                    .unwrap_or_default();
        }
    );

    let temp_label = if let Some(t) = original_options.temperature {
        format!("{} ({}: {:.2}):", t!("temperature_range"), t!("currently"), t)
    } else {
        format!("{} ({}: {}):", t!("temperature_range"), t!("currently"),
            t!("unset"))
    };

    // --- Temperature using the Macro ---
    validated_edit!(
        ui,
        &temp_label,
        40.0,
        &mut substate.temperature_entered,
        substate.is_temperature_valid,
        // Validation Logic
        {
            if substate.temperature_entered.is_empty() {
                substate.edited_preset.options.temperature = None;
                substate.is_temperature_valid = false;
            } else {
                substate.edited_preset.options.temperature =
                    substate.temperature_entered.parse::<f64>().ok();
                if let Some(t) = substate.edited_preset.options.temperature {
                    substate.is_temperature_valid = (0.0..=2.0).contains(&t);
                } else {
                    substate.is_temperature_valid = false;
                }
            }
        },
        // Revert Logic (Note: Pattern 1 used here as there is no extra button)
        {
            substate.temperature_entered = original_options.temperature
                    .map(|t| t.to_string()).unwrap_or_default();
        }
    );
}
