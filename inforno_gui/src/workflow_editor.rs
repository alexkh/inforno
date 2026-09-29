use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use eframe::egui;
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};
use egui_snarl::{ui::{PinInfo, SnarlPin, SnarlViewer}, InPin, InPinId, NodeId, OutPin, OutPinId, Snarl};
use fuzzy_matcher::{FuzzyMatcher, skim::SkimMatcherV2};

use inforno_core::common::{Agent, Chat, ChatMsg, ChatQue, ChatRouter, MsgRole, Preset};
use rucomfyui::object_info::{Object, ObjectInfo, ObjectInputMetaTyped, ObjectType};

use crate::state::State;

/// A node in the editor. Either a real ComfyUI node -- whose pins, widgets
/// and defaults all come from the live server's `/object_info`, not from
/// anything hardcoded here -- or one of Inforno's own node kinds, which
/// don't exist on the ComfyUI server at all. The LLM nodes are resolved to
/// plain string values *before* the JSON prompt is built (see
/// `resolve_and_queue`), so as far as ComfyUI is concerned they were never
/// part of the graph.
///
/// SECURITY INVARIANT: `OllamaPrompt`/`OpenRouterPrompt` store only a
/// `preset_id` (an integer) and plain prompt text -- never a `Preset` or
/// `ApiKey`. The OpenRouter key lives only in `State::openrouter_api_key`
/// (loaded once from keyring/env/`.env`) and is resolved into a `Preset`
/// fresh on every Play click, inside `ui_workflow_editor`, as a function-
/// local value that is never written back into this enum, into
/// `WorkflowEditorState`, or into the ComfyUI JSON payload. If this graph
/// is ever made `Serialize` (e.g. for a "save workflow" feature), that
/// stays safe only as long as no variant here grows a `Preset`/`ApiKey`
/// field -- don't add one.
#[derive(Clone)]
pub enum InfornoNode {
    /// A real ComfyUI node. `class_type` is the key into `/object_info`.
    /// `values` holds the current text for every *unlinked* input, keyed
    /// by input name; an input that has a wire coming in ignores its entry
    /// here entirely.
    Comfy { class_type: String, values: BTreeMap<String, String> },
    /// Runs one chat completion against a local Ollama model and exposes
    /// the reply as a single STRING output -- wire it straight into e.g. a
    /// CLIPTextEncode's `text` input.
    OllamaPrompt { preset_id: i64, system_prompt: String, user_prompt: String },
    /// Same idea, via OpenRouter.
    OpenRouterPrompt { preset_id: i64, system_prompt: String, user_prompt: String },
    /// A documentation/comment node with no backend equivalent -- imported
    /// from ComfyUI's `Note`/`MarkdownNote` widgets, which exist only in
    /// the web frontend's canvas. Carries display text only, and stays
    /// excluded from every ComfyUI JSON payload for the same reason the
    /// security note above holds for API keys: `build_pre_prompt` only
    /// ever matches `InfornoNode::Comfy`, so a node of this shape simply
    /// cannot reach `/prompt` -- there's no separate filter to keep in
    /// sync, and no way for this to regress silently.
    MarkdownNote { title: String, text: String },
}

impl InfornoNode {
    fn display_title(&self, object_info: &ObjectInfo) -> String {
        match self {
            InfornoNode::Comfy { class_type, .. } => object_info
                .get(class_type)
                .map(|o| o.display_name().to_string())
                .unwrap_or_else(|| class_type.clone()),
            InfornoNode::OllamaPrompt { .. } => "🦙 Ollama Prompt".to_string(),
            InfornoNode::OpenRouterPrompt { .. } => "🌐 OpenRouter Prompt".to_string(),
            InfornoNode::MarkdownNote { title, .. } => format!("📝 {}", if title.is_empty() { "Note" } else { title }),
        }
    }
}

/// A pin's data-type label, purely for coloring pins the way ComfyUI itself
/// color-codes sockets (yellow for CLIP, red for VAE, etc.) -- cosmetic
/// only, there's no runtime type-checking of connections here, same as the
/// version this replaces.
fn color_for_type(label: &str) -> egui::Color32 {
    match label {
        "MODEL" => egui::Color32::from_rgb(180, 70, 220),
        "CLIP" => egui::Color32::from_rgb(230, 220, 90),
        "VAE" => egui::Color32::from_rgb(220, 90, 90),
        "CONDITIONING" => egui::Color32::from_rgb(90, 170, 230),
        "LATENT" => egui::Color32::from_rgb(230, 150, 90),
        "IMAGE" => egui::Color32::from_rgb(90, 220, 150),
        "STRING" => egui::Color32::from_rgb(140, 210, 210),
        "INT" | "FLOAT" => egui::Color32::from_rgb(180, 200, 120),
        _ => egui::Color32::GRAY,
    }
}

/// Renders an [`ObjectType`] the way ComfyUI's own JSON spells it (e.g.
/// `"MODEL"`), by round-tripping it through its own `Serialize` impl rather
/// than re-declaring the type names here.
fn object_type_label(ty: &ObjectType) -> String {
    serde_json::to_value(ty)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "?".to_string())
}

/// The inputs of a node, in pin-index order, as (name, type label). For a
/// `Comfy` node this comes straight from `/object_info`'s declared
/// `input_order`, so pin index N here always means the same input ComfyUI
/// itself thinks is input N -- unlike the fixed 4-pin KSampler this
/// replaces, every required *and* optional input gets a pin, matching how
/// ComfyUI's own editor treats every input as both a socket and (when
/// unlinked) an inline widget.
/// The declared inputs of a ComfyUI class, in pin-index order, as
/// (name, type label), read straight from `/object_info`'s `input_order`.
fn comfy_inputs(object_info: &ObjectInfo, class_type: &str) -> Vec<(String, String)> {
    object_info
        .get(class_type)
        .map(|obj| {
            obj.all_inputs()
                .map(|(name, input, _required)| {
                    let label = input.as_type().map(object_type_label).unwrap_or_else(|| "?".to_string());
                    (name.to_string(), label)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn comfy_outputs(object_info: &ObjectInfo, class_type: &str) -> Vec<(String, String)> {
    object_info
        .get(class_type)
        .map(|obj| {
            obj.output
                .iter()
                .zip(obj.output_name.iter())
                .map(|(ty, name)| (name.clone(), object_type_label(ty)))
                .collect()
        })
        .unwrap_or_default()
}

fn node_inputs(object_info: &ObjectInfo, node: &InfornoNode) -> Vec<(String, String)> {
    match node {
        InfornoNode::Comfy { class_type, .. } => comfy_inputs(object_info, class_type),
        InfornoNode::OllamaPrompt { .. } | InfornoNode::OpenRouterPrompt { .. } | InfornoNode::MarkdownNote { .. } => vec![],
    }
}

fn node_outputs(object_info: &ObjectInfo, node: &InfornoNode) -> Vec<(String, String)> {
    match node {
        InfornoNode::Comfy { class_type, .. } => comfy_outputs(object_info, class_type),
        InfornoNode::OllamaPrompt { .. } | InfornoNode::OpenRouterPrompt { .. } => {
            vec![("STRING".to_string(), "STRING".to_string())]
        }
        InfornoNode::MarkdownNote { .. } => vec![],
    }
}

/// Finds the pin index of a named input for a given class, so callers (like
/// `default_graph`) can wire connections by name instead of guessing a
/// numeric index that depends on the server's declared input order.
fn input_index(object_info: &ObjectInfo, class_type: &str, name: &str) -> Option<usize> {
    object_info
        .get(class_type)?
        .all_inputs()
        .position(|(n, _, _)| n == name)
}

/// Best-effort coercion of a raw text-box string back into the JSON shape
/// ComfyUI's `/prompt` endpoint expects for a given input, based on the
/// metadata the server itself declared (number vs. boolean vs. string).
/// Anything we don't recognize is sent through as a plain string.
fn coerce_value(object_info: &ObjectInfo, class_type: &str, input_name: &str, raw: &str) -> serde_json::Value {
    let typed = object_info
        .get(class_type)
        .and_then(|o| o.all_inputs().find(|(n, _, _)| *n == input_name))
        .and_then(|(_, input, _)| input.as_meta_typed());

    match typed {
        Some(ObjectInputMetaTyped::Number(_)) => {
            if let Ok(i) = raw.parse::<i64>() { serde_json::json!(i) }
            else if let Ok(f) = raw.parse::<f64>() { serde_json::json!(f) }
            else { serde_json::json!(raw) }
        }
        Some(ObjectInputMetaTyped::Boolean(_)) => serde_json::json!(raw.eq_ignore_ascii_case("true")),
        _ => serde_json::json!(raw),
    }
}

pub struct ComfyViewer {
    pub object_info: Arc<Mutex<Option<Arc<ObjectInfo>>>>,
    /// Shared across frames (not per-node) so re-rendering a note's text
    /// doesn't re-layout it from scratch every frame -- same reason
    /// `State::common_mark_cache` exists for chat messages.
    pub markdown_cache: std::rc::Rc<std::cell::RefCell<CommonMarkCache>>,
}

impl ComfyViewer {
    /// Snapshot of the object info, or an empty map before the first fetch
    /// completes (nodes just show zero pins until then). This clones an
    /// `Arc`, not the schema itself -- see the note on `WorkflowEditorState`.
    fn info(&self) -> Arc<ObjectInfo> {
        self.object_info.lock().unwrap().clone().unwrap_or_default()
    }
}

impl SnarlViewer<InfornoNode> for ComfyViewer {
    fn title(&mut self, node: &InfornoNode) -> String {
        node.display_title(&self.info())
    }

    fn inputs(&mut self, node: &InfornoNode) -> usize {
        node_inputs(&self.info(), node).len()
    }

    fn outputs(&mut self, node: &InfornoNode) -> usize {
        node_outputs(&self.info(), node).len()
    }

    fn show_input(&mut self, pin: &InPin, ui: &mut egui::Ui, snarl: &mut Snarl<InfornoNode>) -> impl SnarlPin + 'static {
        let info = self.info();
        let node = &mut snarl[pin.id.node];
        let (name, ty) = node_inputs(&info, node)[pin.id.input].clone();
        
        ui.horizontal(|ui| {
            ui.label(name.as_str());
            
            // If the pin is completely unconnected, we display the ComfyUI parameter widget. 
            // This natively aligns the widget vertically with the pin!
            if pin.remotes.is_empty() {
                if let InfornoNode::Comfy { class_type, values } = node {
                    let entry = values.entry(name.clone()).or_default();
                    
                    let mut combo_choices = None;
                    if let Some(obj) = info.get(class_type) {
                        if let Some((_, input, _)) = obj.all_inputs().find(|(n, _, _)| n == &name) {
                            if let Ok(val) = serde_json::to_value(input) {
                                if let Some(arr) = val.as_array() {
                                    if let Some(first_elem) = arr.get(0) {
                                        if let Some(choices) = first_elem.as_array() {
                                            combo_choices = Some(
                                                choices.iter()
                                                    .filter_map(|v| v.as_str().map(String::from))
                                                    .collect::<Vec<_>>(),
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }

                    if let Some(choices) = combo_choices {
                        if !choices.is_empty() && !choices.contains(entry) {
                            *entry = choices[0].clone();
                        }
                        egui::ComboBox::from_id_salt((pin.id.node, pin.id.input))
                            .width(110.0)
                            .selected_text(entry.as_str())
                            .show_ui(ui, |ui| {
                                for choice in choices {
                                    ui.selectable_value(entry, choice.clone(), choice);
                                }
                            });
                    } else {
                        ui.add_sized(
                            egui::vec2(90.0, ui.spacing().interact_size.y),
                            egui::TextEdit::singleline(entry),
                        );
                    }
                }
            }
        });
        
        PinInfo::circle().with_fill(color_for_type(&ty))
    }

    fn show_output(&mut self, pin: &OutPin, ui: &mut egui::Ui, snarl: &mut Snarl<InfornoNode>) -> impl SnarlPin + 'static {
        let info = self.info();
        let node = &mut snarl[pin.id.node];
        let outputs = node_outputs(&info, node);
        let (name, ty) = outputs[pin.id.output].clone();

        // egui-snarl sizes this node from what it measured *last* frame, and
        // clips this frame's output labels to that remembered width before
        // it's allowed to grow. If the node's width was ever locked in
        // narrower than an output name needs (e.g. on the first frames
        // after ComfyUI's /object_info populates a longer output list than
        // the node started with), it can get stuck permanently too narrow,
        // silently cutting names off against the pin circle instead of
        // wrapping or overflowing. Reserving room for the widest output
        // name on every row keeps the node from ever locking in that small.
        let widest = outputs
            .iter()
            .map(|(n, _)| {
                egui::WidgetText::from(n.as_str())
                    .into_galley(ui, None, f32::INFINITY, egui::TextStyle::Body)
                    .size()
                    .x + 35.0
            })
            .fold(0.0_f32, f32::max);

        ui.add_sized(
            egui::vec2(widest, ui.spacing().interact_size.y),
            egui::Label::new(name.as_str()),
        );
        PinInfo::circle().with_fill(color_for_type(&ty))
    }

    fn has_body(&mut self, node: &InfornoNode) -> bool {
        // Native ComfyUI nodes display parameters entirely within their input pins.
        // We only return true (have a body) for our custom LLM Prompt nodes!
        match node {
            InfornoNode::Comfy { .. } => false,
            _ => true,
        }
    }

    fn show_body(&mut self, node_id: NodeId, _inputs: &[InPin], _outputs: &[OutPin], ui: &mut egui::Ui, snarl: &mut Snarl<InfornoNode>) {
        const FIELD_WIDTH: f32 = 140.0;
        
        // Comfy nodes are completely skipped here since has_body returns false for them.
        if let InfornoNode::OllamaPrompt { preset_id, system_prompt, user_prompt }
            | InfornoNode::OpenRouterPrompt { preset_id, system_prompt, user_prompt } = &mut snarl[node_id] {
            ui.horizontal(|ui| { ui.label("preset id:"); ui.add(egui::DragValue::new(preset_id)); });
            ui.label("system:");
            ui.add_sized(
                egui::vec2(FIELD_WIDTH, ui.spacing().interact_size.y * 2.0),
                egui::TextEdit::multiline(system_prompt),
            );
            ui.label("user:");
            ui.add_sized(
                egui::vec2(FIELD_WIDTH, ui.spacing().interact_size.y * 3.0),
                egui::TextEdit::multiline(user_prompt),
            );
        } else if let InfornoNode::MarkdownNote { text, .. } = &snarl[node_id] {
            let text = text.clone();
            ui.push_id(node_id, |ui| {
                ui.set_max_width(360.0);
                let mut cache = self.markdown_cache.borrow_mut();
                CommonMarkViewer::new().show(ui, &mut cache, &text);
            });
        }
    }

    fn connect(&mut self, from: &OutPin, to: &InPin, snarl: &mut Snarl<InfornoNode>) {
        // No type-checking on connections yet -- accept anything. Cosmetic
        // pin colors are the only signal a user gets today that they've
        // wired a Latent output into an Image input; ComfyUI's own server
        // will reject an actually-invalid graph when Play is pressed.
        snarl.connect(from.id, to.id);
    }

    fn has_graph_menu(&mut self, _pos: egui::Pos2, _snarl: &mut Snarl<InfornoNode>) -> bool {
        true
    }

    fn show_graph_menu(&mut self, pos: egui::Pos2, ui: &mut egui::Ui, snarl: &mut Snarl<InfornoNode>) {
        let info = self.info();
        ui.set_min_width(220.0);

        ui.label("Inforno");
        if ui.button("🦙 Ollama Prompt").clicked() {
            snarl.insert_node(pos, InfornoNode::OllamaPrompt {
                preset_id: 0, system_prompt: String::new(), user_prompt: String::new(),
            });
            ui.close();
        }
        if ui.button("🌐 OpenRouter Prompt").clicked() {
            snarl.insert_node(pos, InfornoNode::OpenRouterPrompt {
                preset_id: 0, system_prompt: String::new(), user_prompt: String::new(),
            });
            ui.close();
        }

        ui.separator();
        ui.label("ComfyUI");
        let filter_id = ui.id().with("node_menu_filter");
        let mut filter = ui.ctx().data_mut(|d| d.get_temp::<String>(filter_id).unwrap_or_default());
        ui.text_edit_singleline(&mut filter);
        ui.ctx().data_mut(|d| d.insert_temp(filter_id, filter.clone()));

        let matcher = SkimMatcherV2::default();
        let mut matches: Vec<(i64, &String, &Object)> = info
            .iter()
            .filter_map(|(class_type, obj)| {
                if filter.is_empty() {
                    Some((0, class_type, obj))
                } else {
                    matcher.fuzzy_match(obj.display_name(), &filter)
                        .or_else(|| matcher.fuzzy_match(class_type, &filter))
                        .map(|score| (score, class_type, obj))
                }
            })
            .collect();
        matches.sort_by(|a, b| b.0.cmp(&a.0));

        egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
            for (_, class_type, obj) in matches.into_iter().take(60) {
                if ui.button(obj.display_name()).clicked() {
                    snarl.insert_node(pos, InfornoNode::Comfy { class_type: class_type.clone(), values: BTreeMap::new() });
                    ui.close();
                }
            }
        });
    }
}

/// Builds ComfyUI's default txt2img graph: checkpoint feeds two prompt
/// encoders (positive/negative) and an empty latent, all three feed a
/// sampler, which feeds VAE decode, which feeds save image. This is
/// exactly the graph a fresh ComfyUI install shows on first load.
///
/// Needs a loaded `object_info` because connections are now made by input
/// *name* (`input_index`) against the server's declared schema, not by a
/// guessed numeric pin index -- see the note on `input_index`.
fn default_graph(object_info: &ObjectInfo) -> Snarl<InfornoNode> {
    let mut snarl = Snarl::new();
    let values = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    };
    let comfy = |class_type: &str, vals: BTreeMap<String, String>| InfornoNode::Comfy {
        class_type: class_type.to_string(), values: vals,
    };
    let idx = |class_type: &str, name: &str| input_index(object_info, class_type, name).unwrap_or(0);

    let checkpoint = snarl.insert_node(egui::pos2(0.0, 150.0),
        comfy("CheckpointLoaderSimple", values(&[("ckpt_name", "sd_xl_base_1.0.safetensors")])));
    let positive = snarl.insert_node(egui::pos2(300.0, 0.0),
        comfy("CLIPTextEncode", values(&[("text", "a photo of a cat")])));
    let negative = snarl.insert_node(egui::pos2(300.0, 150.0),
        comfy("CLIPTextEncode", values(&[("text", "text, watermark")])));
    let latent = snarl.insert_node(egui::pos2(300.0, 320.0),
        comfy("EmptyLatentImage", values(&[("width", "1024"), ("height", "1024"), ("batch_size", "1")])));
    let sampler = snarl.insert_node(egui::pos2(620.0, 150.0),
        comfy("KSampler", values(&[
            ("seed", "0"), ("steps", "20"), ("cfg", "8.0"),
            ("sampler_name", "euler"), ("scheduler", "normal"), ("denoise", "1.0"),
        ])));
    let decode = snarl.insert_node(egui::pos2(900.0, 150.0), comfy("VAEDecode", BTreeMap::new()));
    let save = snarl.insert_node(egui::pos2(1150.0, 150.0),
        comfy("SaveImage", values(&[("filename_prefix", "Inforno")])));

    snarl.connect(OutPinId { node: checkpoint, output: 1 }, InPinId { node: positive, input: idx("CLIPTextEncode", "clip") });
    snarl.connect(OutPinId { node: checkpoint, output: 1 }, InPinId { node: negative, input: idx("CLIPTextEncode", "clip") });
    snarl.connect(OutPinId { node: checkpoint, output: 0 }, InPinId { node: sampler, input: idx("KSampler", "model") });
    snarl.connect(OutPinId { node: positive, output: 0 }, InPinId { node: sampler, input: idx("KSampler", "positive") });
    snarl.connect(OutPinId { node: negative, output: 0 }, InPinId { node: sampler, input: idx("KSampler", "negative") });
    snarl.connect(OutPinId { node: latent, output: 0 }, InPinId { node: sampler, input: idx("KSampler", "latent_image") });
    snarl.connect(OutPinId { node: sampler, output: 0 }, InPinId { node: decode, input: idx("VAEDecode", "samples") });
    snarl.connect(OutPinId { node: checkpoint, output: 2 }, InPinId { node: decode, input: idx("VAEDecode", "vae") });
    snarl.connect(OutPinId { node: decode, output: 0 }, InPinId { node: save, input: idx("SaveImage", "images") });

    snarl
}

/// Repairs "almost JSON" that has raw control characters (most commonly a
/// bare newline) sitting inside a string literal instead of properly
/// escaped as `\n`/`\r`/`\t`/`\u00XX`. Per the JSON spec these are illegal
/// unescaped, so `serde_json` correctly rejects them -- but some
/// ComfyUI exports/copy-paste paths produce exactly this (e.g. a
/// MarkdownNote's text widget saved with literal newlines instead of
/// escaped ones). Tracks string-open/close and existing backslash escapes
/// char-by-char so a raw `"` or `\` already inside the string isn't
/// mistaken for a boundary; a no-op on JSON that's already valid, so it's
/// safe to run unconditionally before every parse in this module.
pub fn sanitize_json_control_chars(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_string = false;
    let mut escape_next = false;

    for c in text.chars() {
        if in_string {
            if escape_next {
                out.push(c);
                escape_next = false;
                continue;
            }
            match c {
                '\\' => { out.push(c); escape_next = true; }
                '"' => { in_string = false; out.push(c); }
                _ if (c as u32) < 0x20 => {
                    match c {
                        '\n' => out.push_str("\\n"),
                        '\r' => out.push_str("\\r"),
                        '\t' => out.push_str("\\t"),
                        other => out.push_str(&format!("\\u{:04x}", other as u32)),
                    }
                }
                _ => out.push(c),
            }
        } else {
            if c == '"' {
                in_string = true;
            }
            out.push(c);
        }
    }

    out
}

/// Heuristic detection for ComfyUI's "API format" prompt JSON: an object
/// whose every value is itself an object carrying a string `class_type`
/// and an `inputs` object. This is exactly the shape `/prompt` expects and
/// the shape ComfyUI's "Save (API Format)" export produces -- distinct
/// enough from arbitrary JSON pasted into chat that it shouldn't false-
/// positive on unrelated code blocks.
pub fn is_comfy_api_workflow_json(value: &serde_json::Value) -> bool {
    let Some(map) = value.as_object() else { return false };
    if map.is_empty() {
        return false;
    }
    map.values().all(|node| {
        node.get("class_type").and_then(|v| v.as_str()).is_some()
            && node.get("inputs").map(|v| v.is_object()).unwrap_or(false)
    })
}

/// A ComfyUI API-format input value is a two-element `[node_id, output_index]`
/// array when it's a link to another node's output; anything else is a
/// plain literal. Mirrors the convention ComfyUI's own `/prompt` endpoint
/// uses to tell the two apart (the same convention `resolve_and_queue`
/// produces when it serializes a graph the other direction).
fn as_comfy_link(value: &serde_json::Value) -> Option<(String, usize)> {
    let arr = value.as_array()?;
    if arr.len() != 2 {
        return None;
    }
    let node_id = arr[0].as_str()?.to_string();
    let output = arr[1].as_u64()? as usize;
    Some((node_id, output))
}

fn comfy_value_to_string(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}

/// Builds a graph from a ComfyUI API-format prompt JSON (e.g. from
/// ComfyUI's "Save (API Format)" export, or a `/prompt` request body
/// pasted back into chat). Node positions aren't part of this format, so
/// nodes are laid out on a simple grid in ascending original-id order;
/// connections are re-wired using `input_index` against the live schema,
/// exactly like `default_graph` does, so a name -> pin-index mismatch
/// can't silently misconnect a wire.
fn build_graph_from_api_json(json_str: &str, object_info: &ObjectInfo) -> Result<Snarl<InfornoNode>, String> {
    let parsed: serde_json::Value = serde_json::from_str(json_str)
        .map_err(|e| format!("Invalid JSON: {e}"))?;
    let map = parsed.as_object().ok_or_else(|| "Expected a JSON object of nodes".to_string())?;
    if map.is_empty() {
        return Err("Workflow JSON has no nodes".to_string());
    }

    let mut ordered_ids: Vec<&String> = map.keys().collect();
    ordered_ids.sort_by_key(|k| k.parse::<i64>().unwrap_or(i64::MAX));

    let mut snarl = Snarl::new();
    let mut id_map: HashMap<String, NodeId> = HashMap::new();
    let cols = 5;

    for (idx, orig_id) in ordered_ids.iter().enumerate() {
        let node_val = &map[*orig_id];
        let class_type = node_val.get("class_type")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("Node '{orig_id}' is missing class_type"))?
            .to_string();

        let mut values = BTreeMap::new();
        if let Some(inputs) = node_val.get("inputs").and_then(|v| v.as_object()) {
            for (name, val) in inputs {
                if as_comfy_link(val).is_none() {
                    values.insert(name.clone(), comfy_value_to_string(val));
                }
            }
        }

        let pos = egui::pos2((idx % cols) as f32 * 280.0, (idx / cols) as f32 * 220.0);
        let node_id = snarl.insert_node(pos, InfornoNode::Comfy { class_type, values });
        id_map.insert((*orig_id).clone(), node_id);
    }

    for orig_id in &ordered_ids {
        let node_val = &map[*orig_id];
        let Some(class_type) = node_val.get("class_type").and_then(|v| v.as_str()) else { continue };
        let Some(inputs) = node_val.get("inputs").and_then(|v| v.as_object()) else { continue };
        let Some(&to_node) = id_map.get(*orig_id) else { continue };

        for (name, val) in inputs {
            let Some((from_orig_id, output)) = as_comfy_link(val) else { continue };
            let Some(&from_node) = id_map.get(&from_orig_id) else { continue };
            let Some(input) = input_index(object_info, class_type, name) else { continue };
            snarl.connect(OutPinId { node: from_node, output }, InPinId { node: to_node, input });
        }
    }

    Ok(snarl)
}

/// Heuristic detection for ComfyUI's "UI format" workflow JSON (what the
/// web frontend itself saves/loads, e.g. `workflow.json` or a graph copied
/// from the editor via Ctrl+A/Ctrl+C): a top-level object carrying a
/// `nodes` array and a `links` array. Unlike the API format, this one
/// *does* carry node positions/sizes -- see the doc comment on
/// `build_graph_from_ui_json` for why the two formats differ and how each
/// is parsed.
pub fn is_comfy_ui_workflow_json(value: &serde_json::Value) -> bool {
    let Some(map) = value.as_object() else { return false };
    matches!(map.get("nodes"), Some(v) if v.is_array())
        && matches!(map.get("links"), Some(v) if v.is_array())
}

/// True if `value` looks like either ComfyUI JSON format this module knows
/// how to import (see `is_comfy_api_workflow_json` and
/// `is_comfy_ui_workflow_json`).
pub fn is_comfy_workflow_json(value: &serde_json::Value) -> bool {
    is_comfy_api_workflow_json(value) || is_comfy_ui_workflow_json(value)
}

/// The input names of `class_type`, in the server's declared order,
/// restricted to ones that actually get an on-node widget when unlinked
/// (`ObjectInputMetaTyped::as_meta_typed()` returns `Some` for primitive/
/// combo types like INT, FLOAT, STRING, BOOLEAN, and `None` for pure
/// socket types like MODEL/CLIP/VAE/CONDITIONING). This is the same order
/// the ComfyUI frontend fills `widgets_values` in, which is what makes
/// reconstructing that array's meaning possible without a positional
/// schema of our own.
fn widget_eligible_input_names(object_info: &ObjectInfo, class_type: &str) -> Vec<String> {
    object_info
        .get(class_type)
        .map(|obj| {
            obj.all_inputs()
                .filter(|(_, input, _)| input.as_meta_typed().is_some())
                .map(|(name, _, _)| name.to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// A node's `pos` field, tolerating the two shapes litegraph has
/// serialized it as across ComfyUI frontend versions: a `[x, y]` array, or
/// an `{"0": x, "1": y}` object.
fn parse_ui_pos(node_val: &serde_json::Value) -> Option<egui::Pos2> {
    let pos = node_val.get("pos")?;
    if let Some(arr) = pos.as_array() {
        let x = arr.get(0)?.as_f64()?;
        let y = arr.get(1)?.as_f64()?;
        return Some(egui::pos2(x as f32, y as f32));
    }
    if let Some(obj) = pos.as_object() {
        let x = obj.get("0")?.as_f64()?;
        let y = obj.get("1")?.as_f64()?;
        return Some(egui::pos2(x as f32, y as f32));
    }
    None
}

/// Builds a graph from ComfyUI's "UI format" workflow JSON -- the format
/// the web frontend itself saves, distinct from the flattened "API
/// format" `build_graph_from_api_json` handles. The two differ because
/// the API format is only ever consumed by the Python backend's executor,
/// which has no concept of a canvas -- it just needs `class_type` plus
/// resolved input values/links, so that's all ComfyUI bothers to emit for
/// `/prompt`. The UI format is the frontend's own editor-state dump, so it
/// additionally carries `pos`/`size` per node, and (unlike the API format)
/// stores literal widget values *positionally* in a `widgets_values` array
/// rather than keyed by input name -- see `widget_eligible_input_names`
/// for how that position is reconstructed. Connections here live in a
/// single top-level `links` array of `[link_id, from_node, from_slot,
/// to_node, to_slot, type]` tuples; each node's own `inputs` entries only
/// carry a `link` id to look up in that array, not the endpoint directly.
fn build_graph_from_ui_json(json_str: &str, object_info: &ObjectInfo) -> Result<(Snarl<InfornoNode>, Option<String>), String> {
    let parsed: serde_json::Value = serde_json::from_str(json_str)
        .map_err(|e| format!("Invalid JSON: {e}"))?;
    let root = parsed.as_object().ok_or_else(|| "Expected a JSON object".to_string())?;
    let nodes = root.get("nodes").and_then(|v| v.as_array())
        .ok_or_else(|| "Missing 'nodes' array".to_string())?;
    if nodes.is_empty() {
        return Err("Workflow JSON has no nodes".to_string());
    }
    let links = root.get("links").and_then(|v| v.as_array()).cloned().unwrap_or_default();

    // link_id -> (origin_node_id_as_string, origin_output_slot)
    let mut link_origins: HashMap<i64, (String, usize)> = HashMap::new();
    for link in &links {
        let Some(arr) = link.as_array() else { continue };
        if arr.len() < 5 { continue; }
        let (Some(link_id), Some(origin_id), Some(origin_slot)) =
            (arr[0].as_i64(), arr[1].as_i64(), arr[2].as_u64()) else { continue };
        link_origins.insert(link_id, (origin_id.to_string(), origin_slot as usize));
    }

    // Pass 0: resolve every node's InfornoNode value and raw position up
    // front, without inserting anything yet, so the whole graph's
    // coordinates can be normalized afterward -- a real export's positions
    // are absolute canvas coordinates (sometimes placed far from the
    // origin, e.g. a documentation note parked off to the side), and the
    // editor's default viewport starts near (0, 0), so importing them
    // verbatim can leave every node scrolled out of view even though the
    // import itself succeeded.
    struct PendingNode {
        orig_id: String,
        node: InfornoNode,
        pos: egui::Pos2,
    }

    let mut pending = Vec::new();
    let mut note_count = 0usize;
    let mut grid_idx = 0usize;
    let cols = 5;

    for node_val in nodes {
        let orig_id = node_val.get("id").and_then(|v| v.as_i64())
            .ok_or_else(|| "Node is missing an integer 'id'".to_string())?
            .to_string();
        let class_type = node_val.get("type")
            .or_else(|| node_val.get("class_type"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("Node '{orig_id}' is missing type/class_type"))?
            .to_string();

        let pos = parse_ui_pos(node_val).unwrap_or_else(|| {
            let p = egui::pos2((grid_idx % cols) as f32 * 280.0, (grid_idx / cols) as f32 * 220.0);
            grid_idx += 1;
            p
        });

        // `Note`/`MarkdownNote` are frontend-only documentation widgets --
        // there's no backend class_type for either, so sending one to
        // /prompt always fails no matter what's installed. Rather than
        // drop them (and lose e.g. model-download instructions), import
        // them as a distinct, read-only `MarkdownNote` node: see the type
        // definition for why this guarantees it can never reach a ComfyUI
        // JSON payload.
        if class_type == "Note" || class_type == "MarkdownNote" {
            let title = node_val.get("title").and_then(|v| v.as_str()).unwrap_or("Note").to_string();
            // Prefer a named lookup if the source ever provides one;
            // otherwise fall back to the first positional widgets_values
            // entry, which is where both Note and MarkdownNote keep their
            // text in stock ComfyUI exports.
            let text = node_val.get("widgets_values_named")
                .and_then(|v| v.get("text"))
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .or_else(|| {
                    node_val.get("widgets_values")
                        .and_then(|v| v.as_array())
                        .and_then(|a| a.first())
                        .map(comfy_value_to_string)
                })
                .unwrap_or_default();

            note_count += 1;
            pending.push(PendingNode { orig_id, node: InfornoNode::MarkdownNote { title, text }, pos });
            continue;
        }

        // Names currently carrying a link, per this node's own declared
        // inputs -- used both to skip them when consuming widgets_values
        // and (in the second pass) to actually wire the connection.
        let declared_inputs = node_val.get("inputs").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        let linked_names: std::collections::HashSet<String> = declared_inputs.iter()
            .filter(|inp| inp.get("link").map(|l| !l.is_null()).unwrap_or(false))
            .filter_map(|inp| inp.get("name").and_then(|v| v.as_str()).map(str::to_string))
            .collect();

        let mut values = BTreeMap::new();
        match node_val.get("widgets_values") {
            Some(serde_json::Value::Array(arr)) => {
                let order = widget_eligible_input_names(object_info, &class_type);
                for (name, val) in order.into_iter().filter(|n| !linked_names.contains(n)).zip(arr.iter()) {
                    values.insert(name, comfy_value_to_string(val));
                }
            }
            Some(serde_json::Value::Object(map)) => {
                for (name, val) in map {
                    if !linked_names.contains(name) {
                        values.insert(name.clone(), comfy_value_to_string(val));
                    }
                }
            }
            _ => {}
        }

        pending.push(PendingNode { orig_id, node: InfornoNode::Comfy { class_type, values }, pos });
    }

    // Normalize so the top-left of the imported graph sits near the
    // origin, regardless of where the source canvas placed it -- fixes the
    // "editor window looks empty" symptom for exports whose nodes were
    // positioned far from (0, 0).
    let min_x = pending.iter().map(|n| n.pos.x).fold(f32::INFINITY, f32::min);
    let min_y = pending.iter().map(|n| n.pos.y).fold(f32::INFINITY, f32::min);
    let margin = 40.0;

    let mut snarl = Snarl::new();
    let mut id_map: HashMap<String, NodeId> = HashMap::new();

    for pn in pending {
        let adjusted = egui::pos2(pn.pos.x - min_x + margin, pn.pos.y - min_y + margin);
        let node_id = snarl.insert_node(adjusted, pn.node);
        id_map.insert(pn.orig_id, node_id);
    }

    for node_val in nodes {
        let Some(orig_id) = node_val.get("id").and_then(|v| v.as_i64()).map(|i| i.to_string()) else { continue };
        let Some(class_type) = node_val.get("type").or_else(|| node_val.get("class_type")).and_then(|v| v.as_str()) else { continue };
        // Notes have no declared inputs to wire in practice, but skip
        // explicitly rather than rely on that -- input_index() below would
        // simply return None for a Note's class_type anyway (it isn't a
        // real ComfyUI class), so this is belt-and-suspenders, not load-bearing.
        if class_type == "Note" || class_type == "MarkdownNote" { continue; }
        let Some(&to_node) = id_map.get(&orig_id) else { continue };
        let Some(declared_inputs) = node_val.get("inputs").and_then(|v| v.as_array()) else { continue };

        for inp in declared_inputs {
            let Some(name) = inp.get("name").and_then(|v| v.as_str()) else { continue };
            let Some(link_id) = inp.get("link").and_then(|v| v.as_i64()) else { continue };
            let Some((from_orig_id, from_slot)) = link_origins.get(&link_id) else { continue };
            let Some(&from_node) = id_map.get(from_orig_id) else { continue };
            let Some(input) = input_index(object_info, class_type, name) else { continue };
            snarl.connect(OutPinId { node: from_node, output: *from_slot }, InPinId { node: to_node, input });
        }
    }

    let info_msg = if note_count > 0 {
        Some(format!(
            "Imported {} documentation note(s) as read-only Note node(s) (kept visible, excluded from Play).",
            note_count
        ))
    } else {
        None
    };

    Ok((snarl, info_msg))
}

/// Parses whichever ComfyUI workflow JSON format `json_str` turns out to
/// be (API or UI -- see `is_comfy_workflow_json`), dispatching to the
/// matching builder.
fn build_graph_from_comfy_json(json_str: &str, object_info: &ObjectInfo) -> Result<(Snarl<InfornoNode>, Option<String>), String> {
    let sanitized = sanitize_json_control_chars(json_str);
    let parsed: serde_json::Value = serde_json::from_str(&sanitized)
        .map_err(|e| format!("Invalid JSON: {e}"))?;
    if is_comfy_ui_workflow_json(&parsed) {
        build_graph_from_ui_json(&sanitized, object_info)
    } else {
        build_graph_from_api_json(&sanitized, object_info).map(|snarl| (snarl, None))
    }
}

pub struct WorkflowEditorState {
    pub snarl: Snarl<InfornoNode>,
    pub comfy_url: String,
    /// Set by the background request thread, polled each frame. `Ok` holds
    /// whatever ComfyUI's `/prompt` response body was (its `prompt_id`,
    /// typically); `Err` holds a human-readable failure reason.
    pub last_result: Arc<Mutex<Option<Result<String, String>>>>,
    pub in_flight: Arc<Mutex<bool>>,
    /// The server's node schema, fetched once per session (or on manual
    /// refresh). `None` until the first successful fetch. Held behind an
    /// `Arc` so that every `.clone()` of this (there's one per viewer
    /// method call, per pin, per frame -- see `ComfyViewer::info`) is a
    /// refcount bump, not a deep copy of a multi-megabyte schema. Without
    /// this, the editor pegs a CPU core the whole time this window is open.
    pub object_info: Arc<Mutex<Option<Arc<ObjectInfo>>>>,
    pub object_info_error: Arc<Mutex<Option<String>>>,
    pub object_info_loading: Arc<Mutex<bool>>,
    /// The default txt2img graph needs a loaded schema to wire its
    /// connections by name (see `default_graph`), so it can't be built at
    /// `Default::default()` time -- `ui_workflow_editor` loads it in once,
    /// the first time a fetch succeeds.
    pub default_graph_loaded: bool,
    /// Set when a chat message's "Open in Workflow Editor" button is
    /// clicked: raw ComfyUI API-format JSON waiting to be turned into a
    /// graph. Consumed (and cleared) by `ui_workflow_editor` as soon as
    /// `object_info` is available, taking priority over loading the
    /// default graph -- see the loading block there.
    pub pending_import_json: Option<String>,
    pub import_error: Option<String>,
    /// Informational (not an error) note set alongside a *successful*
    /// import -- e.g. "N documentation notes were imported as read-only
    /// Note nodes."
    pub import_warning: Option<String>,
    /// Shared across every `MarkdownNote` node's rendering, so re-showing
    /// a note's text doesn't re-lay it out from scratch each frame.
    pub markdown_cache: std::rc::Rc<std::cell::RefCell<CommonMarkCache>>,
}

impl Default for WorkflowEditorState {
    fn default() -> Self {
        Self {
            snarl: Snarl::new(),
            comfy_url: "http://127.0.0.1:8188".to_string(),
            last_result: Arc::new(Mutex::new(None)),
            in_flight: Arc::new(Mutex::new(false)),
            object_info: Arc::new(Mutex::new(None)),
            object_info_error: Arc::new(Mutex::new(None)),
            object_info_loading: Arc::new(Mutex::new(false)),
            default_graph_loaded: false,
            pending_import_json: None,
            import_error: None,
            import_warning: None,
            markdown_cache: std::rc::Rc::new(std::cell::RefCell::new(CommonMarkCache::default())),
        }
    }
}

/// Fetches `/object_info` in the background. `rucomfyui::Client`'s exact
/// constructor is assumed here (`new(url)` + async `get_object_info()`) --
/// adjust to match your real client if it differs.
fn fetch_object_info(comfy_url: String, object_info: Arc<Mutex<Option<Arc<ObjectInfo>>>>,
        object_info_error: Arc<Mutex<Option<String>>>, loading: Arc<Mutex<bool>>) {
    *loading.lock().unwrap() = true;
    *object_info_error.lock().unwrap() = None;
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build();
        let result = match rt {
            Ok(rt) => rt.block_on(async {
                let client = rucomfyui::Client::new(comfy_url);
                client.get_object_info().await.map_err(|e| format!("{e:?}"))
            }),
            Err(e) => Err(format!("Failed to start async runtime: {e}")),
        };
        match result {
            Ok(info) => *object_info.lock().unwrap() = Some(Arc::new(info)),
            Err(e) => *object_info_error.lock().unwrap() = Some(e),
        }
        *loading.lock().unwrap() = false;
    });
}

/// One `Comfy` node's not-yet-finalized input value: a literal, a link to
/// another real ComfyUI node (ready as-is), or a placeholder standing in
/// for an Inforno-only node's text output, filled in once that node runs.
enum PendingInput {
    Value(serde_json::Value),
    Link { node_id: String, output: usize },
    PendingText(NodeId),
}

/// Phase 1: walks the graph and builds, for every real `Comfy` node, its
/// class type and inputs -- with any wire coming from an LLM node left as a
/// `PendingText` marker instead of a value, since ComfyUI has no idea those
/// nodes exist. Also returns the set of LLM node ids actually referenced,
/// so idle/unused LLM nodes in the graph don't get run for nothing.
fn build_pre_prompt(snarl: &Snarl<InfornoNode>, object_info: &ObjectInfo)
        -> (BTreeMap<String, (String, BTreeMap<String, PendingInput>)>, BTreeSet<NodeId>) {
    use std::collections::HashMap;
    let ids: HashMap<NodeId, String> = snarl.nodes_ids_data()
        .filter(|(_, n)| matches!(n.value, InfornoNode::Comfy { .. }))
        .enumerate()
        .map(|(i, (id, _))| (id, (i + 1).to_string()))
        .collect();

    let mut pre_prompt = BTreeMap::new();
    let mut used_llm_nodes = BTreeSet::new();

    for (id, node) in snarl.nodes_ids_data() {
        let InfornoNode::Comfy { class_type, values } = &node.value else { continue };
        let mut inputs = BTreeMap::new();

        for (input_idx, (name, _ty)) in node_inputs(object_info, &node.value).iter().enumerate() {
            let in_pin = snarl.in_pin(InPinId { node: id, input: input_idx });
            if let Some(remote) = in_pin.remotes.first() {
                match &snarl[remote.node] {
                    InfornoNode::Comfy { .. } => {
                        if let Some(remote_id) = ids.get(&remote.node) {
                            inputs.insert(name.clone(), PendingInput::Link { node_id: remote_id.clone(), output: remote.output });
                        }
                    }
                    InfornoNode::OllamaPrompt { .. } | InfornoNode::OpenRouterPrompt { .. } => {
                        used_llm_nodes.insert(remote.node);
                        inputs.insert(name.clone(), PendingInput::PendingText(remote.node));
                    }
                    InfornoNode::MarkdownNote { .. } => {
                        // A documentation node has no output, so this link
                        // shouldn't exist in a well-formed graph -- ignore
                        // it rather than error, since it can't affect the
                        // resulting /prompt payload either way.
                    }
                }
            } else if let Some(raw) = values.get(name) {
                inputs.insert(name.clone(), PendingInput::Value(coerce_value(object_info, class_type, name, raw)));
            }
        }

        pre_prompt.insert(ids[&id].clone(), (class_type.clone(), inputs));
    }

    (pre_prompt, used_llm_nodes)
}

/// Runs one Inforno LLM node synchronously (blocking) and returns its reply
/// text. `preset` must already have `chat_router` and (for OpenRouter)
/// `api_key` set -- see the resolution step in `ui_workflow_editor`'s Play
/// button, which builds one of these per LLM node before this ever runs on
/// the background thread.
async fn run_llm_node(node: &InfornoNode, preset: &Preset) -> Result<String, String> {
    let (router_hint, system_prompt, user_prompt) = match node {
        InfornoNode::OllamaPrompt { system_prompt, user_prompt, .. } => (ChatRouter::Ollama, system_prompt, user_prompt),
        InfornoNode::OpenRouterPrompt { system_prompt, user_prompt, .. } => (ChatRouter::Openrouter, system_prompt, user_prompt),
        InfornoNode::Comfy { .. } | InfornoNode::MarkdownNote { .. } => return Err("not an LLM node".to_string()),
    };

    let preset = preset.clone();
    let _ = &router_hint; // router is already baked into `preset.chat_router` by the caller

    let mut chat = Chat::default();
    let sys_id = 1;
    let user_id = 2;
    chat.msg_pool.insert(sys_id, ChatMsg { id: sys_id, msg_role: MsgRole::System, content: system_prompt.clone(), ..Default::default() });
    chat.msg_pool.insert(user_id, ChatMsg { id: user_id, msg_role: MsgRole::User, content: user_prompt.clone(), ..Default::default() });
    let mut agent = Agent::default();
    agent.msg_ids = vec![sys_id, user_id];
    chat.agents = vec![agent];

    let query = ChatQue { agent_ind: 0, preset, chat: Arc::new(chat) };

    match router_hint {
        ChatRouter::Ollama => inforno_core::ollama::do_ollama_chat_que(query).await
            .map(|resp| resp.message.content)
            .map_err(|e| format!("Ollama error: {e:?}")),
        ChatRouter::Openrouter => inforno_core::openr::do_openr_chat_que(query).await
            .map(|resp| resp.choices.first().and_then(|c| c.content()).unwrap_or_default().to_string())
            .map_err(|e| format!("OpenRouter error: {e}")),
    }
}

/// Phase 2 + submission: runs every referenced LLM node, fills in the
/// `PendingText` markers, serializes the final ComfyUI `/prompt` payload,
/// and posts it -- all on a background thread so the UI never blocks.
fn resolve_and_queue(comfy_url: String, snarl: &Snarl<InfornoNode>, object_info: &ObjectInfo, resolved_presets: HashMap<NodeId, Preset>,
        last_result: Arc<Mutex<Option<Result<String, String>>>>, in_flight: Arc<Mutex<bool>>) {
    let (pre_prompt, used_llm_nodes) = build_pre_prompt(snarl, object_info);
    let llm_nodes: Vec<(NodeId, InfornoNode)> = used_llm_nodes.into_iter()
        .map(|id| (id, snarl[id].clone()))
        .collect();

    *in_flight.lock().unwrap() = true;
    std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
            Ok(rt) => rt,
            Err(e) => { *last_result.lock().unwrap() = Some(Err(format!("Failed to start async runtime: {e}"))); *in_flight.lock().unwrap() = false; return; }
        };

        let mut resolved: BTreeMap<NodeId, String> = BTreeMap::new();
        for (id, node) in &llm_nodes {
            let Some(preset) = resolved_presets.get(id) else {
                *last_result.lock().unwrap() = Some(Err(format!("No resolved preset for node {id:?}")));
                *in_flight.lock().unwrap() = false;
                return;
            };
            match rt.block_on(run_llm_node(node, preset)) {
                Ok(text) => { resolved.insert(*id, text); }
                Err(e) => { *last_result.lock().unwrap() = Some(Err(e)); *in_flight.lock().unwrap() = false; return; }
            }
        }

        let mut prompt = serde_json::Map::new();
        for (id, (class_type, inputs)) in pre_prompt {
            let mut json_inputs = serde_json::Map::new();
            for (name, val) in inputs {
                let json_val = match val {
                    PendingInput::Value(v) => v,
                    PendingInput::Link { node_id, output } => serde_json::json!([node_id, output]),
                    PendingInput::PendingText(llm_id) => serde_json::json!(resolved.get(&llm_id).cloned().unwrap_or_default()),
                };
                json_inputs.insert(name, json_val);
            }
            prompt.insert(id, serde_json::json!({ "class_type": class_type, "inputs": json_inputs }));
        }

        let client_id = format!("inforno-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos());
        let body = serde_json::json!({ "prompt": serde_json::Value::Object(prompt), "client_id": client_id });
        let url = format!("{}/prompt", comfy_url.trim_end_matches('/'));

        let result = reqwest::blocking::Client::new()
            .post(&url)
            .json(&body)
            .send()
            .map_err(|e| format!("Request failed: {e}"))
            .and_then(|resp| {
                if resp.status().is_success() {
                    resp.text().map_err(|e| format!("Failed to read response: {e}"))
                } else {
                    let status = resp.status();
                    let text = resp.text().unwrap_or_default();
                    Err(format!("ComfyUI returned {status}: {text}"))
                }
            });

        *last_result.lock().unwrap() = Some(result);
        *in_flight.lock().unwrap() = false;
    });
}

pub fn ui_workflow_editor(ui: &mut egui::Ui, state: &mut State) {
    if !state.show_workflow_editor {
        return;
    }
    let ctx = ui.ctx().clone();

    let wf = &mut state.workflow_editor_state;
    let loading = *wf.object_info_loading.lock().unwrap();
    let have_info = wf.object_info.lock().unwrap().is_some();
    if !have_info && !loading {
        fetch_object_info(wf.comfy_url.clone(), wf.object_info.clone(), wf.object_info_error.clone(), wf.object_info_loading.clone());
    }
    if let Some(info) = wf.object_info.lock().unwrap().clone() {
        if let Some(json_str) = wf.pending_import_json.take() {
            match build_graph_from_comfy_json(&json_str, &info) {
                Ok((snarl, warning)) => {
                    wf.snarl = snarl;
                    wf.import_error = None;
                    wf.import_warning = warning;
                }
                Err(e) => {
                    wf.import_error = Some(e);
                    wf.import_warning = None;
                }
            }
            wf.default_graph_loaded = true;
        } else if !wf.default_graph_loaded {
            wf.snarl = default_graph(&info);
            wf.default_graph_loaded = true;
        }
    }

    let mut open = state.show_workflow_editor;
    egui::Window::new("🧩 Workflow Editor")
        .default_size([1100.0, 650.0])
        .open(&mut open)
        .show(&ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("ComfyUI server:");
                ui.text_edit_singleline(&mut state.workflow_editor_state.comfy_url);

                if ui.button("🔃 Refresh Nodes").clicked() {
                    let wf = &state.workflow_editor_state;
                    fetch_object_info(wf.comfy_url.clone(), wf.object_info.clone(), wf.object_info_error.clone(), wf.object_info_loading.clone());
                }

                let busy = *state.workflow_editor_state.in_flight.lock().unwrap();
                let info = state.workflow_editor_state.object_info.lock().unwrap().clone();
                if ui.add_enabled(!busy && info.is_some(), egui::Button::new(if busy { "⏳ Queuing..." } else { "▶ Play" })).clicked() {
                    if let Some(info) = &info {
                        // Resolve one Preset per LLM node up front, on the UI
                        // thread, where `state.presets` and the global
                        // OpenRouter key both live. Ollama needs no key;
                        // OpenRouter uses the single app-wide key, same as
                        // every other OpenRouter request in Inforno.
                        //
                        // SECURITY: `resolved_presets` is a function-local
                        // value, rebuilt from `state.openrouter_api_key` on
                        // every Play click, and moved (not copied) into the
                        // background thread by `resolve_and_queue`. It is
                        // never assigned into `state.workflow_editor_state`
                        // or any other field that outlives this click or
                        // gets persisted/exported -- the key must never be
                        // reachable from anything saved to disk or sent to
                        // ComfyUI. Don't hoist this out of the click handler.
                        let mut resolved_presets = HashMap::new();
                        for (id, node_data) in state.workflow_editor_state.snarl.nodes_ids_data() {
                            let (preset_id, router) = match &node_data.value {
                                InfornoNode::OllamaPrompt { preset_id, .. } => (*preset_id, ChatRouter::Ollama),
                                InfornoNode::OpenRouterPrompt { preset_id, .. } => (*preset_id, ChatRouter::Openrouter),
                                InfornoNode::Comfy { .. } | InfornoNode::MarkdownNote { .. } => continue,
                            };
                            let mut preset = state.presets.get(preset_id).cloned().unwrap_or_default();
                            preset.chat_router = router.clone();
                            if router == ChatRouter::Openrouter {
                                preset.api_key = state.openrouter_api_key.clone();
                            }
                            resolved_presets.insert(id, preset);
                        }

                        resolve_and_queue(
                            state.workflow_editor_state.comfy_url.clone(),
                            &state.workflow_editor_state.snarl,
                            info,
                            resolved_presets,
                            state.workflow_editor_state.last_result.clone(),
                            state.workflow_editor_state.in_flight.clone(),
                        );
                    }
                }
            });

            if *state.workflow_editor_state.object_info_loading.lock().unwrap() {
                ui.colored_label(ui.visuals().weak_text_color(), "Loading node schema from ComfyUI...");
            }
            if let Some(err) = &*state.workflow_editor_state.object_info_error.lock().unwrap() {
                ui.colored_label(ui.visuals().error_fg_color, format!("Failed to load node schema: {err}"));
            }
            if let Some(err) = &state.workflow_editor_state.import_error {
                ui.colored_label(ui.visuals().error_fg_color, format!("Failed to import workflow: {err}"));
            }
            if let Some(note) = &state.workflow_editor_state.import_warning {
                ui.colored_label(ui.visuals().weak_text_color(), note);
            }
            if let Some(result) = &*state.workflow_editor_state.last_result.lock().unwrap() {
                match result {
                    Ok(body) => { ui.colored_label(egui::Color32::from_rgb(90, 220, 150), format!("Queued: {body}")); }
                    Err(err) => { ui.colored_label(ui.visuals().error_fg_color, err); }
                }
            }

            ui.separator();

            let object_info = state.workflow_editor_state.object_info.clone();
            let markdown_cache = state.workflow_editor_state.markdown_cache.clone();
            state.workflow_editor_state.snarl.show(&mut ComfyViewer { object_info, markdown_cache }, &egui_snarl::ui::SnarlStyle::default(), "workflow_snarl", ui);
        });

    state.show_workflow_editor = open;
}
