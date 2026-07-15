pub mod common;
pub mod db;
pub mod ollama;
pub mod openr;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use std::path::{Path, PathBuf};
use std::sync::Arc;
use globset::{Glob, GlobSet, GlobSetBuilder};


#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct RealmMountConfig {
    pub host: PathBuf,
    #[serde(default)]
    pub templates: Vec<String>,
    #[serde(default)]
    pub ignore: Vec<String>,
    pub description: Option<String>,
    // E.g., "project", "workspace", "docs", "static"
    pub kind: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct RealmConfig {
    pub default_workspace: Option<String>,
    #[serde(default)]
    pub ignore_templates: IndexMap<String, Vec<String>>,
    #[serde(default)]
    pub mounts: IndexMap<String, RealmMountConfig>,
}

#[derive(Clone)]
pub struct CompiledMount {
    pub virtual_path: String,
    pub host_path: PathBuf,
    pub ignore_set: Arc<GlobSet>,
    pub description: Option<String>,
    pub kind: String, // Defaults to "project", but can also be "workspace"
}

#[derive(Clone)]
pub struct ActiveRealm {
    pub name: String,
    pub default_workspace: Option<String>,
    pub mounts: Vec<CompiledMount>,
}

impl ActiveRealm {
    pub fn from_config(name: String, config: RealmConfig) -> Result<Self, String> {
        let mut mounts = Vec::new();

        for (v_path, mount_cfg) in config.mounts {
            let mut builder = GlobSetBuilder::new();

            for tpl_name in &mount_cfg.templates {
                if let Some(tpl_globs) = config.ignore_templates.get(tpl_name) {
                    for g in tpl_globs {
                        builder.add(Glob::new(g).map_err(|e| format!("Invalid glob '{}': {}", g, e))?);
                    }
                } else {
                    return Err(format!("Template '{}' not found for mount '{}'", tpl_name, v_path));
                }
            }

            for g in &mount_cfg.ignore {
                builder.add(Glob::new(g).map_err(|e| format!("Invalid glob '{}': {}", g, e))?);
            }

            let ignore_set = builder.build().map_err(|e| e.to_string())?;
            let kind = mount_cfg.kind.unwrap_or_else(|| "project".to_string());

            mounts.push(CompiledMount {
                virtual_path: v_path,
                host_path: mount_cfg.host,
                ignore_set: ignore_set.into(),
                description: mount_cfg.description,
                kind,
            });
        }

        mounts.sort_by(|a, b| b.virtual_path.len().cmp(&a.virtual_path.len()));

        Ok(Self {
            name,
            default_workspace: config.default_workspace,
            mounts,
        })
    }

    pub fn secure_resolve_path(&self, virtual_path: &Path) -> Option<PathBuf> {
        let path_str = virtual_path.to_str()?;
        for mount in &self.mounts {
            if path_str.starts_with(&mount.virtual_path) {
                let relative = path_str
                    .strip_prefix(&mount.virtual_path)
                    .unwrap_or("")
                    .trim_start_matches('/');
                let host_target = mount.host_path.join(relative);
                if mount.ignore_set.is_match(&host_target) { return None; }
                return Some(host_target);
            }
        }
        None
    }
}

pub fn core_test() {
    println!("Core is linked!");
}
