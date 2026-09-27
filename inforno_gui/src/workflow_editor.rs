use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use eframe::egui;
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
        InfornoNode::OllamaPrompt { .. } | InfornoNode::OpenRouterPrompt { .. } => vec![],
    }
}

fn node_outputs(object_info: &ObjectInfo, node: &InfornoNode) -> Vec<(String, String)> {
    match node {
        InfornoNode::Comfy { class_type, .. } => comfy_outputs(object_info, class_type),
        InfornoNode::OllamaPrompt { .. } | InfornoNode::OpenRouterPrompt { .. } => {
            vec![("STRING".to_string(), "STRING".to_string())]
        }
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
        InfornoNode::Comfy { .. } => return Err("not an LLM node".to_string()),
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
    if !wf.default_graph_loaded {
        if let Some(info) = wf.object_info.lock().unwrap().clone() {
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
                                InfornoNode::Comfy { .. } => continue,
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
            if let Some(result) = &*state.workflow_editor_state.last_result.lock().unwrap() {
                match result {
                    Ok(body) => { ui.colored_label(egui::Color32::from_rgb(90, 220, 150), format!("Queued: {body}")); }
                    Err(err) => { ui.colored_label(ui.visuals().error_fg_color, err); }
                }
            }

            ui.separator();

            let object_info = state.workflow_editor_state.object_info.clone();
            state.workflow_editor_state.snarl.show(&mut ComfyViewer { object_info }, &egui_snarl::ui::SnarlStyle::default(), "workflow_snarl", ui);
        });

    state.show_workflow_editor = open;
}
