//! A Folio in the older format (version 2): the offer to convert it, the
//! banner while no Folio is open, and afterwards the Realms that still link
//! the old path. Realm files are only shown here, never written.

use std::path::{Path, PathBuf};

use inforno_core::db::migrate::{
    conversion_destination, convert_legacy_folio, legacy_backup_path, same_file,
};

use crate::state::State;

/// A version 2 Folio that was asked for, and isn't open.
pub struct LegacyFolioPrompt {
    pub path: PathBuf,
    pub version: i32,
    /// It's the Home Folio (the one opened when no Folio is named).
    pub was_home: bool,
    /// Where "Move into folios" puts it; None when it stays where it is.
    pub move_dest: Option<PathBuf>,
    pub move_into_folios: bool,
    /// Where the original will be kept.
    pub backup: PathBuf,
    /// The window is showing. "Not now" hides it; the banner shows it again.
    pub open: bool,
}

impl LegacyFolioPrompt {
    pub fn new(path: PathBuf, version: i32, was_home: bool) -> Self {
        let move_dest = if was_home { None } else { conversion_destination(&path) };
        Self {
            backup: legacy_backup_path(&path),
            move_into_folios: move_dest.is_some(),
            move_dest,
            path,
            version,
            was_home,
            open: true,
        }
    }
}

/// What a finished conversion shows.
pub struct ConvertedFolioNotice {
    pub old_path: PathBuf,
    pub new_path: PathBuf,
    pub backup: PathBuf,
    /// (Realm, Folio key) entries that still link `old_path`.
    pub links: Vec<(String, String)>,
}

/// Strip under the menu bar while no Folio is open. A scratch Folio in
/// memory stands in, and nothing is saved.
pub fn ui_folio_banner(ui: &mut egui::Ui, state: &mut State) {
    let Some(reason) = state.folio_unavailable.clone() else { return };
    egui::Panel::top("folio_unavailable_banner").show(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.colored_label(
                ui.visuals().error_fg_color,
                format!("{} Nothing you do now is saved.", reason),
            );
            if let Some(prompt) = state.legacy_folio.as_mut() {
                if !prompt.open && ui.button("Convert…").clicked() {
                    prompt.open = true;
                }
            }
        });
    });
}

/// The offer to convert a version 2 Folio. Modal while it's showing.
pub fn ui_legacy_folio_prompt(ctx: &egui::Context, state: &mut State) {
    if !state.legacy_folio.as_ref().is_some_and(|p| p.open) {
        return;
    }
    state.is_modal_open = true;

    let mut convert = false;
    let mut not_now = false;
    let Some(prompt) = state.legacy_folio.as_mut() else { return };

    egui::Window::new("Convert Folio")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.set_max_width(520.0);
            ui.label(format!(
                "This Folio is in the older format (version {}), so Inforno can't open it as it is:",
                prompt.version
            ));
            ui.monospace(prompt.path.display().to_string());
            ui.add_space(8.0);
            ui.label("Converting works on a copy. Once the copy is converted, the original is kept, unchanged, as:");
            ui.monospace(prompt.backup.display().to_string());
            ui.add_space(8.0);

            match &prompt.move_dest {
                Some(dest) => {
                    ui.radio_value(&mut prompt.move_into_folios, true, "Move into folios (recommended)");
                    ui.indent("legacy_folio_dest", |ui| {
                        ui.monospace(dest.display().to_string());
                    });
                    ui.radio_value(&mut prompt.move_into_folios, false, "Convert where it is");
                    if prompt.move_into_folios {
                        ui.label(egui::RichText::new(
                            "Realms that link the old path are listed afterwards, for you to update by hand.",
                        ).weak());
                    }
                }
                None if prompt.was_home => {
                    ui.label("This is the Home Folio, so it's converted where it is.");
                }
                None => {
                    ui.label("It's already in the folios directory, so it's converted where it is.");
                }
            }

            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button("Convert").clicked() {
                    convert = true;
                }
                if ui.button("Not now").clicked() {
                    not_now = true;
                }
            });
        });

    if not_now {
        if let Some(prompt) = state.legacy_folio.as_mut() {
            prompt.open = false;
        }
        state.is_modal_open = state.show_realm_folio_init_modal;
    }
    if convert {
        convert_now(state);
    }
}

fn convert_now(state: &mut State) {
    let Some(prompt) = state.legacy_folio.take() else { return };
    let dest = match (&prompt.move_dest, prompt.move_into_folios) {
        (Some(dest), true) => dest.clone(),
        _ => prompt.path.clone(),
    };
    let moved = !same_file(&dest, &prompt.path);
    // Looked up first: the old path is gone afterwards.
    let links = if moved { realms_linking(&prompt.path) } else { Vec::new() };

    match convert_legacy_folio(&prompt.path, &dest) {
        Ok(backup) => {
            state.reload(if prompt.was_home { None } else { Some(dest.clone()) });
            if let Some((realm, _)) = links.first() {
                open_realm_editor(state, realm);
            }
            state.converted_folio_notice = Some(ConvertedFolioNotice {
                old_path: prompt.path,
                new_path: dest,
                backup,
                links,
            });
        }
        Err(e) => {
            state.error_msg = Some(format!("Couldn't convert {}: {}", prompt.path.display(), e));
            state.is_modal_open = true;
            state.legacy_folio = Some(LegacyFolioPrompt { open: false, ..prompt });
        }
    }
}

/// Where the converted Folio is, where the original was kept, and which
/// Realms still link the old path.
pub fn ui_converted_folio_notice(ctx: &egui::Context, state: &mut State) {
    let Some(notice) = &state.converted_folio_notice else { return };
    let mut open = true;
    let mut close = false;
    let mut edit_realm: Option<String> = None;

    egui::Window::new("Folio Converted")
        .collapsible(false)
        .open(&mut open)
        .show(ctx, |ui| {
            ui.label("The converted Folio is open:");
            ui.horizontal(|ui| {
                ui.monospace(notice.new_path.display().to_string());
                if ui.button("🗐").on_hover_text("Copy the path").clicked() {
                    ui.ctx().copy_text(notice.new_path.display().to_string());
                }
            });
            ui.label("The original is kept, unchanged, as:");
            ui.monospace(notice.backup.display().to_string());

            if !notice.links.is_empty() {
                ui.add_space(10.0);
                ui.colored_label(ui.visuals().warn_fg_color, "These Realms still link the old path:");
                ui.monospace(notice.old_path.display().to_string());
                ui.label("Change that `path:` to the new one by hand in each realm2.yml. The Realm editor shows the file and changes it only when you save.");
                for (realm, folio_key) in &notice.links {
                    ui.horizontal(|ui| {
                        ui.label(format!("Realm '{}', Folio '{}'", realm, folio_key));
                        if ui.button("Open Realm editor").clicked() {
                            edit_realm = Some(realm.clone());
                        }
                    });
                }
            }

            ui.add_space(10.0);
            if ui.button("Close").clicked() {
                close = true;
            }
        });

    if let Some(realm) = edit_realm {
        open_realm_editor(state, &realm);
    }
    if close || !open {
        state.converted_folio_notice = None;
    }
}

/// (Realm, Folio key) for every Realm whose `folios:` links `folio`. A
/// realm2.yml that doesn't parse is skipped.
fn realms_linking(folio: &Path) -> Vec<(String, String)> {
    let Some(realms_dir) = directories::ProjectDirs::from("", "", "inforno")
        .map(|d| d.config_dir().join("realms"))
    else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&realms_dir) else { return Vec::new() };

    let mut links = Vec::new();
    for entry in entries.flatten() {
        let realm = entry.file_name().to_string_lossy().into_owned();
        let Ok(yaml) = std::fs::read_to_string(entry.path().join("realm2.yml")) else { continue };
        let Ok(config) = serde_saphyr::from_str::<inforno_core::realm::RealmConfig>(&yaml) else { continue };
        for (key, folio_ref) in &config.folios {
            if same_file(&folio_ref.0.path, folio) {
                links.push((realm.clone(), key.clone()));
            }
        }
    }
    links.sort();
    links
}

/// Opens the Realm editor on `realm`'s realm2.yml. The editor writes the
/// file only when the user saves.
pub fn open_realm_editor(state: &mut State, realm: &str) {
    let Some(path) = directories::ProjectDirs::from("", "", "inforno")
        .map(|d| d.config_dir().join("realms").join(realm).join("realm2.yml"))
    else {
        return;
    };
    match std::fs::read_to_string(&path) {
        Ok(yaml) => {
            let editor = &mut state.realm_config_state;
            editor.yaml_buffer = yaml.clone();
            editor.original_yaml = yaml;
            editor.cached_config = None;
            editor.parse_error = None;
            editor.realm_name = Some(realm.to_string());
            state.show_realm_config = true;
        }
        Err(e) => {
            state.error_msg = Some(format!("Couldn't read {}: {}", path.display(), e));
        }
    }
}
