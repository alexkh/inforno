//! Chat view: the pane tree host (`ui_chat`) and the message list of one
//! chat (`render_chat_messages`). Rendering is split across submodules:
//! - `message`: user/assistant frames, headers, reasoning, attachments
//! - `cell`:    welcome note, note cells, the appender, draft commit
//! - `content`: a single message body (markdown, math, Rhai, code blocks)
//! - `latex`:   LaTeX → SVG for math in markdown

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::Sender;

use egui_commonmark::CommonMarkCache;
use rust_i18n::t;

use inforno_core::common::{Chat, FileOpMsg, MsgRole};
use inforno_core::realm::ActiveRealm;

use crate::state::{ChatMsgUi, State};

mod cell;
mod content;
mod latex;
mod message;
mod table;

/// Role the GUI itself assumes when resolving Realm-mapped paths referenced
/// in chat (e.g. "open in editor" / autocorrect links on code blocks). This
/// must match a `gui:` role in the user's realm.yml. If they haven't defined
/// one, `resolve_filepath` treats the Realm as contributing nothing and we
/// fall back to `project_root` — no special-casing needed here, the GUI just
/// has no Realm access, which is the correct default.
pub const GUI_ROLE: &str = "gui";

/// Byte cache shared by compiled math SVGs and decoded image attachments.
type MathCache = std::rc::Rc<std::cell::RefCell<HashMap<String, std::sync::Arc<[u8]>>>>;

/// Everything the message renderers read (plus the markdown cache they
/// need mutably). Built once per `render_chat_messages` call.
struct RenderCtx<'a> {
    cache: &'a mut CommonMarkCache,
    math_cache: MathCache,
    project_root: &'a Option<PathBuf>,
    active_realm: &'a Option<ActiveRealm>,
    op_tx: &'a Sender<FileOpMsg>,
    /// Width of the pane the chat is drawn in.
    total_width: f32,
    /// User-adjustable cap on message width (Ctrl+[ / Ctrl+] / slider).
    max_msg_width: f32,
}

/// Everything the renderers want changed. The chat is borrowed immutably
/// while drawing, so mutations are queued here and applied afterwards by
/// `apply_actions`.
#[derive(Default)]
struct ChatActions {
    delete_requests: Vec<i64>,
    /// (msg_id, content, is_unsaved)
    content_updates: Vec<(i64, String, bool)>,
    /// (msg_id, content) to persist to the DB
    db_updates: Vec<(i64, String)>,
    /// (msg_id, output) from a Rhai ▶ Run
    rhai_updates: Vec<(i64, String)>,
    /// TODO: set by Rhai's `send_prompt()`, but nothing consumes it yet.
    #[allow(dead_code)]
    llm_prompt_request: Option<String>,
    /// "Open in Workflow Editor" was clicked on a ComfyUI JSON block.
    comfy_workflow_request: Option<String>,
    /// New appender text and whether it's now treated as Rhai.
    new_draft: Option<(String, bool)>,
    /// The appender lost focus (or Ctrl+Enter): commit it as a note cell.
    draft_lost_focus: bool,
}

pub fn ui_chat(ui: &mut egui::Ui, state: &mut State) {
    egui::CentralPanel::default()
    //.stick_to_the_bottom(true)
    .show(ui, |ui| {
        if state.is_modal_open {
            ui.disable();
        }

        // 1. Temporarily extract the tree from the state.
        // We replace it with a cheap, empty placeholder so the state remains valid.
        let mut tree = std::mem::replace(
            &mut state.pane_tree,
            egui_tiles::Tree::empty("temp_tree")
        );

        // 2. Create the behavior bridge WITH the new action queue
        let mut behavior = crate::panes::PaneBehavior {
            state,
            split_requests: Vec::new(),
            close_requests: Vec::new(),
            open_chat_requests: Vec::new(),
        };

        // 3. Render the layout
        tree.ui(&mut behavior, ui);

        // --- Layout Auto-Fix Pass ---
        // When a user drags the active tab out of a container to split the screen,
        // the old container still remembers that tab as "active", even though it's gone!
        // This sweep ensures every Tabs container points to a valid, existing child.
        let mut needs_repaint = false;
        for (_, tile) in tree.tiles.iter_mut() {
            if let egui_tiles::Tile::Container(egui_tiles::Container::Tabs(tabs)) = tile {
                if !tabs.children.is_empty() {
                    // Check if the currently active tab actually exists inside this container
                    let has_valid_active = tabs.active.is_some_and(|id| tabs.children.contains(&id));

                    if !has_valid_active {
                        // The active tab was dragged away. Reset focus to the first remaining tab.
                        tabs.active = Some(tabs.children[0]);
                        needs_repaint = true;
                    }
                }
            }
        }

        if needs_repaint {
            ui.ctx().request_repaint();
        }

        // Extract the queues to drop `behavior` and release the borrow on `state`
        let split_requests = behavior.split_requests;
        let close_requests = behavior.close_requests;
        let open_chat_requests = behavior.open_chat_requests;

        // 4. Process any requested splits safely
        for (new_pane, direction) in split_requests {
            let new_tile = tree.tiles.insert_pane(new_pane);
            let new_tabs = tree.tiles.insert_tab_tile(vec![new_tile]);

            if let Some(root_id) = tree.root {
                // Determine whether to split horizontally or vertically
                let new_root = match direction {
                    crate::panes::SplitAction::Right => {
                        tree.tiles.insert_horizontal_tile(vec![root_id, new_tabs])
                    }
                    crate::panes::SplitAction::Down => {
                        tree.tiles.insert_vertical_tile(vec![root_id, new_tabs])
                    }
                };
                tree.root = Some(new_root);
            } else {
                tree.root = Some(new_tabs);
            }
        }

        // 5. Process any requested closes safely
        for dead_id in close_requests {
            if state.active_tile_id == Some(dead_id) {
                state.active_tile_id = None;
                state.active_chat_id = None;
            }

            state.merge_apps.remove(&dead_id);

            // This safely removes the tab and cleans up its parent containers
            tree.remove_recursively(dead_id);
        }

        // 6. Put the updated tree back! (Safe because `behavior` is dead)
        state.pane_tree = tree;

        // 6.5 Process Tab Upgrades (e.g., Temporary Notebooks converting to saved Chats)
        let upgrades = ui.data_mut(|d| {
            let u = d.get_temp::<Vec<(i64, i64)>>(egui::Id::new("tab_upgrades")).unwrap_or_default();
            d.insert_temp(egui::Id::new("tab_upgrades"), Vec::<(i64, i64)>::new());
            u
        });

        for (old_id, new_id) in upgrades {
            for (_, tile) in state.pane_tree.tiles.iter_mut() {
                if let egui_tiles::Tile::Pane(crate::panes::Pane::Chat { chat_id: id }) = tile {
                    if *id == old_id {
                        *id = new_id;
                    }
                }
            }
            // Safely drop the old ghost chat from memory now that tabs are updated
            state.open_chats.remove(&old_id);
        }

        // 7. Process any requested chat opens
        for (chat_id, open_right) in open_chat_requests {
            // Guarantee the target chat is loaded into application memory
            if !state.open_chats.contains_key(&chat_id) {
                let loaded_chat = inforno_core::db::fetch_chat(&state.db_conn, chat_id, &state.presets).unwrap_or_default();
                state.open_chats.insert(chat_id, loaded_chat);
            }

            // Route the UI appropriately
            if open_right {
                crate::panes::open_chat_in_right_pane(state, chat_id);
            } else {
                crate::panes::open_chat_in_tab(state, chat_id);
            }
        }
    });
}

// --- Message list ---

#[tracing::instrument(skip_all)]
pub fn render_chat_messages(ui: &mut egui::Ui, state: &mut State, chat_id: i64, total_width: f32) {
    if std::env::var_os("NOCHAT").is_some() {
        return;
    }
    let _render_span = tracing::info_span!("render_chat_messages", chat_id).entered();

    let max_msg_width = update_chat_width(ui, state, chat_id);

    if !state.open_chats.contains_key(&chat_id) {
        return; // Chat not loaded yet
    }

    let mut actions = ChatActions::default();
    {
        let mut ctx = RenderCtx {
            cache: &mut state.common_mark_cache,
            math_cache: state.math_cache.clone(),
            project_root: &state.project_root,
            active_realm: &state.active_realm,
            op_tx: &state.op_tx,
            total_width,
            max_msg_width,
        };
        let msg_ui_map = &mut state.chat_msg_ui;
        let chat = &state.open_chats[&chat_id];

        render_chat_body(ui, &mut ctx, &mut actions, msg_ui_map, chat_id, chat);
    }

    apply_actions(ui, state, chat_id, actions);

    render_width_slider(ui, state, chat_id, total_width, max_msg_width);
}

/// Applies the Ctrl+[ / Ctrl+] width shortcuts and returns this chat's
/// current max message width (never below 400).
fn update_chat_width(ui: &mut egui::Ui, state: &mut State, chat_id: i64) -> f32 {
    let mut max_msg_width = *state.chat_widths.entry(chat_id).or_insert(800.0);

    ui.input_mut(|i| {
        if i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::OpenBracket)) {
            max_msg_width -= 50.0;
        }
        if i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::CloseBracket)) {
            max_msg_width += 50.0;
        }
    });

    // Only enforce the minimum bound so it doesn't break layout
    max_msg_width = max_msg_width.max(400.0);
    state.chat_widths.insert(chat_id, max_msg_width);
    max_msg_width
}

/// Draws the Omnis (agent 0) timeline: user/system messages one per row,
/// consecutive assistant replies side by side in a grid, note cells, and
/// the appender at the bottom.
fn render_chat_body(
    ui: &mut egui::Ui,
    ctx: &mut RenderCtx,
    actions: &mut ChatActions,
    msg_ui_map: &mut HashMap<i64, ChatMsgUi>,
    chat_id: i64,
    chat: &Chat,
) {
    let msg_pool = &chat.msg_pool;

    if msg_pool.is_empty() {
        // No early return: the appender still renders beneath the welcome note.
        cell::render_welcome(ui, ctx, actions, msg_ui_map);
    }

    let Some(agent) = chat.agents.first() else { return };
    let _agent_span = tracing::info_span!("process_agent_messages", agent_id = agent.id).entered();

    let mut assistant_batch: Vec<i64> = Vec::new();

    for &msg_id in &agent.msg_ids {
        let Some(msg) = msg_pool.get(&msg_id) else { continue };
        let _msg_span = tracing::info_span!("process_message", msg_id = msg_id, role = %msg.msg_role).entered();

        match msg.msg_role {
            MsgRole::User | MsgRole::System | MsgRole::Developer => {
                if !assistant_batch.is_empty() {
                    message::render_assistant_grid(ui, ctx, actions, msg_pool, msg_ui_map, &assistant_batch);
                    assistant_batch.clear();
                }

                let msg_ui = msg_ui_map.entry(msg_id).or_default();
                if msg.msg_role == MsgRole::Developer {
                    cell::render_note_cell(ui, ctx, actions, msg, msg_ui);
                } else {
                    message::render_user_msg(ui, ctx, actions, msg, msg_ui);
                }
            }
            _ => assistant_batch.push(msg_id),
        }
    }

    if !assistant_batch.is_empty() {
        let _assistant_grid_span = tracing::info_span!("render_assistant_grid", count = assistant_batch.len()).entered();
        message::render_assistant_grid(ui, ctx, actions, msg_pool, msg_ui_map, &assistant_batch);
    }

    cell::render_appender(ui, ctx, actions, chat_id, chat);
}

/// Applies everything the renderers queued while the chat was borrowed.
fn apply_actions(ui: &mut egui::Ui, state: &mut State, chat_id: i64, actions: ChatActions) {
    let mut draft_to_commit = None;

    if let Some(chat) = state.open_chats.get_mut(&chat_id) {
        for id in &actions.delete_requests {
            chat.msg_pool.remove(id);
            for agent in chat.agents.iter_mut() {
                agent.msg_ids.retain(|x| x != id);
                let _ = inforno_core::db::mod_agent_msgs(&state.db_conn, agent.id, &agent.msg_ids);
            }
            let _ = inforno_core::db::delete_msg(&state.db_conn, *id);
        }

        for (id, content, unsaved) in actions.content_updates {
            if let Some(m) = chat.msg_pool.get_mut(&id) {
                m.content = content;
                m.is_unsaved = unsaved;
            }
        }

        for (id, out_text) in actions.rhai_updates {
            if let Some(m) = chat.msg_pool.get_mut(&id) {
                m.volatile_output = Some(out_text);
            }
        }

        if let Some((draft, is_rhai)) = actions.new_draft {
            chat.draft_note = draft;
            chat.draft_is_rhai = is_rhai;
        }

        if actions.draft_lost_focus && !chat.draft_note.trim().is_empty() {
            draft_to_commit = Some(std::mem::take(&mut chat.draft_note));
        }
    } // borrow on state.open_chats ends here

    if let Some(draft_content) = draft_to_commit {
        cell::commit_draft(ui, state, chat_id, draft_content);
    }

    if let Some(json_str) = actions.comfy_workflow_request {
        state.show_workflow_editor = true;
        state.workflow_editor_state.pending_import_json = Some(json_str);
        ui.ctx().request_repaint();
    }

    for (id, content) in actions.db_updates {
        let _ = inforno_core::db::mod_msg_content(&state.db_conn, id, &content);
    }
}

/// Minimal width slider floating at the top of the chat viewport. Only
/// visible while hovered or being dragged.
fn render_width_slider(ui: &mut egui::Ui, state: &mut State, chat_id: i64, total_width: f32, mut max_msg_width: f32) {
    let clip_rect = ui.clip_rect();
    let mut slider_rect = clip_rect;
    slider_rect.max.y = slider_rect.min.y + 12.0;

    let mut ui_top = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(slider_rect)
            .layout(egui::Layout::top_down(egui::Align::Center))
    );
    ui_top.scope(|ui| {
        // Determine if the mouse is inside the slider's bounding box
        let pointer_pos = ui.input(|i| i.pointer.hover_pos());
        let is_hovered = pointer_pos.map_or(false, |pos| slider_rect.contains(pos));

        // Fetch the drag state of THIS specific slider from the previous frame
        let state_id = ui.id().with("slider_drag_state");
        let was_dragged_last_frame = ui.data(|d| d.get_temp::<bool>(state_id).unwrap_or(false));

        let show_slider = is_hovered || was_dragged_last_frame;

        // Not hovered and not dragging: hide the rail and the grabber
        if !show_slider {
            let visuals = ui.visuals_mut();
            visuals.widgets.inactive.bg_fill = egui::Color32::TRANSPARENT;
            visuals.widgets.inactive.bg_stroke = egui::Stroke::NONE;
            visuals.widgets.hovered.bg_fill = egui::Color32::TRANSPARENT;
            visuals.widgets.hovered.bg_stroke = egui::Stroke::NONE;
            visuals.widgets.active.bg_fill = egui::Color32::TRANSPARENT;
            visuals.widgets.active.bg_stroke = egui::Stroke::NONE;
            visuals.widgets.inactive.fg_stroke = egui::Stroke::NONE;
        }

        let prev_width = max_msg_width;
        let resp = ui.add_sized(
            [ui.available_width(), 12.0],
            // Allow the slider to extend to the current window size OR the currently saved max
            egui::Slider::new(&mut max_msg_width, 400.0..=(total_width.max(400.0).max(prev_width)))
                .show_value(false)
                .text("")
        );

        if show_slider {
            resp.clone().on_hover_text(
                egui::RichText::new(t!("adjust_message_width_tooltip"))
                    .strong()
                    .heading()
            );
        }

        // Save the exact drag state of this slider for the NEXT frame
        ui.data_mut(|d| d.insert_temp(state_id, resp.dragged()));

        // Instantly save state if the slider was dragged this frame
        if prev_width != max_msg_width {
            state.chat_widths.insert(chat_id, max_msg_width);
            ui.ctx().request_repaint();
        }
    });
}