use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use indexmap::IndexMap;
use std::sync::Arc;
use globset::{Glob, GlobSet, GlobSetBuilder};

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "snake_case")]
pub enum GlobExpr {
    Any(Vec<GlobExpr>),
    All(Vec<GlobExpr>),
    Not(Box<GlobExpr>),
    Match(Vec<String>),
}

pub enum CompiledExpr {
    Any(Vec<CompiledExpr>),
    All(Vec<CompiledExpr>),
    Not(Box<CompiledExpr>),
    Match(globset::GlobSet),
}

impl CompiledExpr {
    pub fn compile(expr: &GlobExpr) -> Result<Self, String> {
        match expr {
            GlobExpr::Any(exprs) => {
                let compiled = exprs.iter().map(Self::compile).collect::<Result<Vec<_>, _>>()?;
                Ok(CompiledExpr::Any(compiled))
            }
            GlobExpr::All(exprs) => {
                let compiled = exprs.iter().map(Self::compile).collect::<Result<Vec<_>, _>>()?;
                Ok(CompiledExpr::All(compiled))
            }
            GlobExpr::Not(expr) => {
                Ok(CompiledExpr::Not(Box::new(Self::compile(expr)?)))
            }
            GlobExpr::Match(globs) => {
                let mut builder = globset::GlobSetBuilder::new();
                for g in globs {
                    builder.add(globset::Glob::new(g).map_err(|e| format!("Invalid glob '{}': {}", g, e))?);
                }
                Ok(CompiledExpr::Match(builder.build().map_err(|e| e.to_string())?))
            }
        }
    }

    pub fn is_match(&self, path: &std::path::Path) -> bool {
        match self {
            CompiledExpr::Any(exprs) => exprs.iter().any(|e| e.is_match(path)),
            CompiledExpr::All(exprs) => exprs.iter().all(|e| e.is_match(path)),
            CompiledExpr::Not(expr) => !expr.is_match(path),
            CompiledExpr::Match(set) => set.is_match(path),
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct RealmMountConfig {
    pub host: PathBuf,
    #[serde(default)]
    pub read_only: bool,
    pub hide_if: Option<GlobExpr>,
    pub read_only_if: Option<GlobExpr>,
    #[serde(default)]
    pub wildcards: Vec<String>,
    #[serde(default)]
    pub ignore: Vec<String>,
    pub description: Option<String>,
    // E.g., "project", "workspace", "docs", "static"
    pub kind: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct RealmConfig {
    pub default_workspace: Option<String>,
    pub hide_if: Option<GlobExpr>,
    pub read_only_if: Option<GlobExpr>,
    #[serde(default)]
    pub wildcards: IndexMap<String, Vec<String>>,
    #[serde(default)]
    pub mounts: IndexMap<String, RealmMountConfig>,
}

#[derive(Clone)]
pub struct CompiledMount {
    pub virtual_path: String,
    pub host_path: PathBuf,
    pub read_only: bool,
    pub ignore_set: Arc<GlobSet>,
    pub hide_expr: Option<Arc<CompiledExpr>>,
    pub ro_expr: Option<Arc<CompiledExpr>>,
    pub description: Option<String>,
    pub kind: String, // Defaults to "project", but can also be "workspace"
}

#[derive(Clone)]
pub struct ActiveRealm {
    pub name: String,
    pub default_workspace: Option<String>,
    pub global_hide_expr: Option<Arc<CompiledExpr>>,
    pub global_ro_expr: Option<Arc<CompiledExpr>>,
    pub mounts: Vec<CompiledMount>,
    pub raw_config: RealmConfig,
}

impl ActiveRealm {
    pub fn from_config(name: String, config: RealmConfig) -> Result<Self, String> {
        let mut mounts = Vec::new();

        // Clone the config BEFORE the loop consumes it
        let raw_config = config.clone();
        
        let global_hide_expr = if let Some(ref expr) = raw_config.hide_if {
            Some(Arc::new(CompiledExpr::compile(expr)?))
        } else {
            None
        };
        
        let global_ro_expr = if let Some(ref expr) = raw_config.read_only_if {
            Some(Arc::new(CompiledExpr::compile(expr)?))
        } else {
            None
        };

        for (v_path, mount_cfg) in config.mounts {
            let mut builder = GlobSetBuilder::new();
            
            let hide_expr = if let Some(ref expr) = mount_cfg.hide_if {
                Some(Arc::new(CompiledExpr::compile(expr)?))
            } else {
                None
            };

            let ro_expr = if let Some(ref expr) = mount_cfg.read_only_if {
                Some(Arc::new(CompiledExpr::compile(expr)?))
            } else {
                None
            };

            // Apply the reusable wildcard rules
            for wc_name in &mount_cfg.wildcards {
                if let Some(wc_globs) = raw_config.wildcards.get(wc_name) {
                    for g in wc_globs {
                        builder.add(Glob::new(g).map_err(|e| format!("Invalid glob '{}': {}", g, e))?);
                    }
                } else {
                    return Err(format!("Wildcard '{}' not found for mount '{}'", wc_name, v_path));
                }
            }

            // Apply mount-specific ignores
            for g in &mount_cfg.ignore {
                builder.add(Glob::new(g).map_err(|e| format!("Invalid glob '{}': {}", g, e))?);
            }

            let ignore_set = builder.build().map_err(|e| e.to_string())?;
            let kind = mount_cfg.kind.unwrap_or_else(|| "project".to_string());

            mounts.push(CompiledMount {
                virtual_path: v_path,
                host_path: mount_cfg.host,
                read_only: mount_cfg.read_only,
                ignore_set: ignore_set.into(),
                hide_expr,
                ro_expr,
                description: mount_cfg.description,
                kind,
            });
        }

        mounts.sort_by(|a, b| b.virtual_path.len().cmp(&a.virtual_path.len()));

        Ok(Self {
            name,
            default_workspace: raw_config.default_workspace.clone(),
            global_hide_expr,
            global_ro_expr,
            mounts,
            raw_config, 
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
                
                // Legacy ignore set matching
                if mount.ignore_set.is_match(&host_target) { return None; }
                
                // Evaluate AST hide expressions against the relative path
                let rel_path = Path::new(relative);
                if let Some(ref expr) = self.global_hide_expr {
                    if expr.is_match(rel_path) { return None; }
                }
                if let Some(ref expr) = mount.hide_expr {
                    if expr.is_match(rel_path) { return None; }
                }
                
                return Some(host_target);
            }
        }
        None
    }

    pub fn is_path_read_only(&self, virtual_path: &Path) -> bool {
        let path_str = match virtual_path.to_str() {
            Some(s) => s,
            None => return true, // Safe fallback
        };
        
        for mount in &self.mounts {
            if path_str.starts_with(&mount.virtual_path) {
                // 1. Check if the entire mount is read-only
                if mount.read_only { return true; }
                
                let relative = path_str
                    .strip_prefix(&mount.virtual_path)
                    .unwrap_or("")
                    .trim_start_matches('/');
                let rel_path = Path::new(relative);
                    
                // 2. Check global read-only rules
                if let Some(ref expr) = self.global_ro_expr {
                    if expr.is_match(rel_path) { return true; }
                }
                
                // 3. Check mount-specific read-only rules
                if let Some(ref expr) = mount.ro_expr {
                    if expr.is_match(rel_path) { return true; }
                }
                
                return false;
            }
        }
        true // If it's outside all mounts, treat as read-only to be safe
    }
}

pub fn get_relative_path(
    realm: &Option<ActiveRealm>,
    project_root: &Option<std::path::PathBuf>,
    file_path: &std::path::Path,
) -> String {
    let canonical_target = std::fs::canonicalize(file_path).unwrap_or_else(|_| file_path.to_path_buf());

    // 1. Try to map back to a Realm virtual path
    if let Some(active_realm) = realm {
        for mount in &active_realm.mounts {
            let canonical_root = std::fs::canonicalize(&mount.host_path).unwrap_or_else(|_| mount.host_path.clone());
            
            if let Ok(stripped) = canonical_target.strip_prefix(&canonical_root) {
                // Ensure Windows slashes are converted to standard virtual path slashes
                let stripped_str = stripped.to_string_lossy().replace('\\', "/");
                let v_path = mount.virtual_path.trim_end_matches('/');
                
                if stripped_str.is_empty() {
                    return if v_path.is_empty() { "/".to_string() } else { v_path.to_string() };
                }
                
                return format!("{}/{}", v_path, stripped_str);
            }
        }
    }

    // 2. Fallback to Project Root
    if let Some(root) = project_root {
        let canonical_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.clone());
        if let Ok(stripped) = canonical_target.strip_prefix(&canonical_root) {
            return stripped.display().to_string();
        }
    }

    // 3. Fallback to the absolute path if it is completely external
    file_path.display().to_string()
}

pub fn resolve_filepath(
    realm: &Option<ActiveRealm>,
    project_root: &Option<std::path::PathBuf>,
    requested_path: &str
) -> Option<(std::path::PathBuf, bool)> {
    let mut target_root = None;
    let mut relative_path_str = requested_path.trim();

    // 1. Attempt VFS Translation if we are in a Realm
    if let Some(active_realm) = realm {
        let req_path = std::path::Path::new(relative_path_str);

        if let Some(secure_host_path) = active_realm.secure_resolve_path(req_path) {
            // Perfect match found and permitted by the ignore list
            if secure_host_path.exists() && secure_host_path.is_file() {
                return Some((secure_host_path, false));
            }

            // If the exact match fails (e.g., a typo in the file name), prepare for the fuzzy fallback.
            // We need to extract the specific mount root this path belonged to.
            for mount in &active_realm.mounts {
                if relative_path_str.starts_with(&mount.virtual_path) {
                    target_root = Some(mount.host_path.clone());
                    relative_path_str = relative_path_str
                        .strip_prefix(&mount.virtual_path)
                        .unwrap_or(relative_path_str)
                        .trim_start_matches('/');
                    break;
                }
            }
        }
    }

    // 2. Fallback to standard project_root if no valid Realm VFS match was found
    let root_to_search = target_root.or_else(|| project_root.clone())?;

    // 3. Standard Exact Match Check
    let req_path = std::path::Path::new(relative_path_str);
    let full_path = root_to_search.join(req_path);
    if full_path.exists() && full_path.is_file() {
        return Some((full_path, false));
    }

    let target_name = req_path.file_name()?;

    let mut best_match = None;
    let mut best_score = -1;

    let mut dirs_to_visit = vec![root_to_search.clone()];
    let req_components: Vec<_> = req_path.components().rev().collect();

    while let Some(dir) = dirs_to_visit.pop() {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    let name = path.file_name().unwrap_or_default();
                    if name != "target" && name != ".git" && name != ".inforno" {
                        dirs_to_visit.push(path);
                    }
                } else if path.is_file() {
                    let is_match = {
                        let path_name = path.file_name().unwrap_or_default();
                        if path_name == target_name {
                            true
                        } else {
                            // fuzzy fallback: if the stem matches exactly!
                            let path_stem = path.file_stem().unwrap_or_default();
                            let target_stem = req_path.file_stem().unwrap_or_default();
                            path_stem == target_stem && !path_stem.is_empty()
                        }
                    };

                    if is_match {
                        let path_components: Vec<_> = path.components().rev().collect();
                        let mut score = 0;
                        for (a, b) in req_components.iter().zip(path_components.iter()) {
                            if a == b {
                                score += 1;
                            } else {
                                break;
                            }
                        }
                        if score > best_score {
                            best_score = score;
                            best_match = Some(path);
                        }
                    }
                }
            }
        }
    }

    if let Some(matched) = best_match {
        return Some((matched, true));
    }

    None
}
