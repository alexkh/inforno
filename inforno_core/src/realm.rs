use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::collections::HashMap;
use indexmap::IndexMap;
use std::sync::Arc;
use globset::{Glob, GlobSet, GlobSetBuilder};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
pub struct Elevation(pub u32);

impl Elevation {
    pub const NONE: Elevation = Elevation(0);
    pub const ROLE_ONLY: Elevation = Elevation(1);
    pub const MIN_GENERAL: Elevation = Elevation(2);
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "snake_case")]
pub enum GlobExpr {
    Any(Vec<GlobExpr>),
    All(Vec<GlobExpr>),
    Not(Box<GlobExpr>),
    Match(Vec<String>),
    Ref(String),
}

pub enum CompiledExpr {
    Any(Vec<CompiledExpr>),
    All(Vec<CompiledExpr>),
    Not(Box<CompiledExpr>),
    Match(globset::GlobSet),
}

impl CompiledExpr {
    pub fn compile(expr: &GlobExpr, defs: &IndexMap<String, GlobExpr>) -> Result<Self, String> {
        match expr {
            GlobExpr::Any(exprs) => {
                let compiled = exprs.iter().map(|e| Self::compile(e, defs)).collect::<Result<Vec<_>, _>>()?;
                Ok(CompiledExpr::Any(compiled))
            }
            GlobExpr::All(exprs) => {
                let compiled = exprs.iter().map(|e| Self::compile(e, defs)).collect::<Result<Vec<_>, _>>()?;
                Ok(CompiledExpr::All(compiled))
            }
            GlobExpr::Not(expr) => {
                Ok(CompiledExpr::Not(Box::new(Self::compile(expr, defs)?)))
            }
            GlobExpr::Match(globs) => {
                let mut builder = globset::GlobSetBuilder::new();
                for g in globs {
                    builder.add(globset::Glob::new(g).map_err(|e| format!("Invalid glob '{}': {}", g, e))?);
                }
                Ok(CompiledExpr::Match(builder.build().map_err(|e| e.to_string())?))
            }
            GlobExpr::Ref(name) => {
                if let Some(resolved) = defs.get(name) {
                    Self::compile(resolved, defs)
                } else {
                    Err(format!("Undefined expression reference: '{}'", name))
                }
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

/// A single named rule governing whether a *new* file may be created at a path.
/// Each rule carries its own human-readable `description` so that the set of
/// active rules can be listed out (e.g. to an AI agent) independently of any
/// specific denial, since FUSE error replies cannot carry custom text.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct CreateRule {
    /// Glob(s), relative to the mount, scoping which paths this rule applies to.
    /// Empty means "applies everywhere under this mount".
    #[serde(default)]
    pub scope: Vec<String>,
    /// If set, paths in scope MUST match this expression to be creatable (whitelist).
    pub allow_if: Option<GlobExpr>,
    /// If set, paths in scope that match this expression are forbidden (blacklist).
    pub deny_if: Option<GlobExpr>,
    /// Human-readable explanation, e.g. "Only .rs files may be created under src/".
    pub description: String,
}

/// The kind of access a `Grant` confers.
#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GrantKind {
    Read,
    Write,
    Create,
    /// Special kind: unhides a path that would otherwise be hidden (e.g. by
    /// the built-in dotfile restriction). Only grants of this kind, and only
    /// when they explicitly name the role in `roles` (never via
    /// `min_elevation` alone), may unhide anything.
    Visibility,
}

/// A role definition: just a name-to-elevation mapping, plus an optional
/// description. Roles carry NO grants of their own — grants are declared
/// separately (`RealmConfig::grants`) and reference role names or an
/// elevation threshold. This keeps a role's containment guarantee intrinsic
/// to its elevation: nothing about which grants exist can change what
/// elevation tier a role sits in.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct RoleConfig {
    pub elevation: Elevation,
    pub description: Option<String>,
}

/// A single grant of access. Applies to a role if EITHER:
/// - `roles` names that role explicitly, OR
/// - `min_elevation` is set and the role's elevation is >= it —
///   UNLESS the role's elevation is `Elevation::ROLE_ONLY` (1), in which case
///   `min_elevation` is ALWAYS ignored and only explicit `roles` membership
///   counts. `Elevation::NONE` (0) never qualifies for any grant, ever.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Grant {
    pub expr: GlobExpr,
    pub kind: GrantKind,
    #[serde(default)]
    pub min_elevation: Option<Elevation>,
    #[serde(default)]
    pub roles: Vec<String>,
    /// If set, names a hidden/read-only/create-deny restriction this grant is
    /// permitted to override (currently only `"dotfiles"` is meaningful).
    /// Overrides ALWAYS require explicit `roles` membership — `min_elevation`
    /// alone can never satisfy an override, regardless of its value.
    #[serde(default)]
    pub overrides: Option<String>,
    pub description: String,
}

/// An Actor presents zero or more role names when acting within a Realm.
/// Each held role's grants are evaluated strictly against that role's own
/// elevation — there is no blending of elevation across roles. An Actor's
/// net access is the union of what each individually held role independently
/// permits. An Actor holding no roles is equivalent to holding a single
/// implicit `Elevation::NONE` role: no filesystem access at all, and no
/// Realm lookup is required to determine that.
///
/// Named `Actor` rather than `Agent` to avoid collision with Inforno's
/// existing `Agent` concept (an LLM + optionally modified Preset options),
/// which is unrelated to Realm access control.
#[derive(Debug, Clone, Default)]
pub struct Actor {
    pub roles: Vec<String>,
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
    /// Rules constraining what new files may be created under this mount.
    #[serde(default)]
    pub create_rules: Vec<CreateRule>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct RealmConfig {
    pub default_workspace: Option<String>,
    pub hide_if: Option<GlobExpr>,
    pub read_only_if: Option<GlobExpr>,
    #[serde(default)]
    pub expressions: IndexMap<String, GlobExpr>,
    #[serde(default)]
    pub wildcards: IndexMap<String, Vec<String>>,
    #[serde(default)]
    pub mounts: IndexMap<String, RealmMountConfig>,
    /// Rules constraining what new files may be created anywhere in the Realm.
    #[serde(default)]
    pub create_rules: Vec<CreateRule>,
    /// Role name -> elevation/description. Roles are the only things an
    /// Agent presents; elevation is intrinsic to the role, not assignable
    /// per-Agent, so a role's containment guarantee holds everywhere it's used.
    #[serde(default)]
    pub roles: IndexMap<String, RoleConfig>,
    /// Grants of read/write/create/visibility access, referencing role names
    /// and/or an elevation threshold. Applies on top of (never loosens) the
    /// hard hide_if/read_only_if/create_rules restrictions above, EXCEPT for
    /// the specific case of a Grant with `overrides` naming a restriction.
    #[serde(default)]
    pub grants: Vec<Grant>,
}

/// A compiled `CreateRule`, ready to be matched against relative paths.
#[derive(Clone)]
pub struct CompiledCreateRule {
    scope_set: Arc<GlobSet>,
    allow_expr: Option<Arc<CompiledExpr>>,
    deny_expr: Option<Arc<CompiledExpr>>,
    pub description: Arc<str>,
}

impl CompiledCreateRule {
    fn compile(rule: &CreateRule, defs: &IndexMap<String, GlobExpr>) -> Result<Self, String> {
        let mut builder = GlobSetBuilder::new();
        if rule.scope.is_empty() {
            // No scope specified -> applies to every path under the mount.
            builder.add(Glob::new("**").map_err(|e| e.to_string())?);
        } else {
            for g in &rule.scope {
                builder.add(Glob::new(g).map_err(|e| format!("Invalid glob '{}': {}", g, e))?);
            }
        }
        let scope_set = builder.build().map_err(|e| e.to_string())?;

        let allow_expr = if let Some(ref expr) = rule.allow_if {
            Some(Arc::new(CompiledExpr::compile(expr, defs)?))
        } else {
            None
        };
        let deny_expr = if let Some(ref expr) = rule.deny_if {
            Some(Arc::new(CompiledExpr::compile(expr, defs)?))
        } else {
            None
        };

        Ok(Self {
            scope_set: scope_set.into(),
            allow_expr,
            deny_expr,
            description: rule.description.as_str().into(),
        })
    }

    fn in_scope(&self, rel_path: &Path) -> bool {
        self.scope_set.is_match(rel_path)
    }

    /// Assumes `in_scope` was already checked. Returns true if creation should be denied.
    fn forbids(&self, rel_path: &Path) -> bool {
        if let Some(ref allow) = self.allow_expr {
            if !allow.is_match(rel_path) {
                return true;
            }
        }
        if let Some(ref deny) = self.deny_expr {
            if deny.is_match(rel_path) {
                return true;
            }
        }
        false
    }
}

#[derive(Clone)]
pub struct CompiledRole {
    pub elevation: Elevation,
    pub description: Option<Arc<str>>,
}

#[derive(Clone)]
pub struct CompiledGrant {
    /// Kept for display purposes (e.g. `describe_role_capabilities`) — the
    /// original, uncompiled expression, rendered via `describe_expr`.
    pub expr_source: GlobExpr,
    expr: Arc<CompiledExpr>,
    pub kind: GrantKind,
    pub min_elevation: Option<Elevation>,
    pub roles: Vec<String>,
    pub overrides: Option<String>,
    pub description: Arc<str>,
}

impl CompiledGrant {
    fn compile(grant: &Grant, defs: &IndexMap<String, GlobExpr>) -> Result<Self, String> {
        let expr = Arc::new(CompiledExpr::compile(&grant.expr, defs)?);

        Ok(Self {
            expr_source: grant.expr.clone(),
            expr,
            kind: grant.kind,
            min_elevation: grant.min_elevation,
            roles: grant.roles.clone(),
            overrides: grant.overrides.clone(),
            description: grant.description.as_str().into(),
        })
    }

    fn matches_path(&self, rel_path: &Path) -> bool {
        self.expr.is_match(rel_path)
    }

    /// Whether this grant applies to a role at the given elevation, holding
    /// this exact role name. `Elevation::NONE` never qualifies.
    /// `Elevation::ROLE_ONLY` qualifies ONLY via explicit `roles` membership
    /// — `min_elevation` is ignored entirely at that tier.
    fn qualifies_role(&self, role_name: &str, role_elevation: Elevation) -> bool {
        if role_elevation == Elevation::NONE {
            return false;
        }
        let by_role = self.roles.iter().any(|r| r == role_name);
        if role_elevation == Elevation::ROLE_ONLY {
            return by_role;
        }
        let by_elevation = self.min_elevation.map_or(false, |min| role_elevation >= min);
        by_elevation || by_role
    }

    /// Whether this grant may act as an override for a named restriction.
    /// Overrides ALWAYS require explicit role membership; `min_elevation`
    /// can never satisfy an override, at any tier, regardless of value.
    fn qualifies_as_override(&self, restriction_name: &str, role_name: &str) -> bool {
        self.overrides.as_deref() == Some(restriction_name)
            && self.roles.iter().any(|r| r == role_name)
    }
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
    pub create_rules: Vec<CompiledCreateRule>,
}

#[derive(Clone)]
pub struct ActiveRealm {
    pub name: String,
    pub default_workspace: Option<String>,
    pub global_hide_expr: Option<Arc<CompiledExpr>>,
    pub global_ro_expr: Option<Arc<CompiledExpr>>,
    pub global_create_rules: Vec<CompiledCreateRule>,
    pub mounts: Vec<CompiledMount>,
    pub raw_config: RealmConfig,
    pub roles: HashMap<String, CompiledRole>,
    pub grants: Vec<CompiledGrant>,
    /// Plain-English capability descriptions per role name, computed once at
    /// construction time. Correctness depends on `ActiveRealm` always being
    /// rebuilt fresh via `from_config` when realm.yml changes, rather than
    /// mutated in place — if that assumption ever changes, this cache needs
    /// explicit invalidation.
    pub role_capabilities: HashMap<String, Arc<Vec<String>>>,
}

impl ActiveRealm {
    pub fn from_config(name: String, config: RealmConfig) -> Result<Self, String> {
        let mut mounts = Vec::new();

        // Clone the config BEFORE the loop consumes it
        let raw_config = config.clone();
        let defs = &raw_config.expressions;
        
        let global_hide_expr = if let Some(ref expr) = raw_config.hide_if {
            Some(Arc::new(CompiledExpr::compile(expr, defs)?))
        } else {
            None
        };
        
        let global_ro_expr = if let Some(ref expr) = raw_config.read_only_if {
            Some(Arc::new(CompiledExpr::compile(expr, defs)?))
        } else {
            None
        };

        let global_create_rules = raw_config
            .create_rules
            .iter()
            .map(|r| CompiledCreateRule::compile(r, defs))
            .collect::<Result<Vec<_>, _>>()?;

        for (v_path, mount_cfg) in config.mounts {
            let mut builder = GlobSetBuilder::new();
            
            let hide_expr = if let Some(ref expr) = mount_cfg.hide_if {
                Some(Arc::new(CompiledExpr::compile(expr, defs)?))
            } else {
                None
            };

            let ro_expr = if let Some(ref expr) = mount_cfg.read_only_if {
                Some(Arc::new(CompiledExpr::compile(expr, defs)?))
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

            let create_rules = mount_cfg
                .create_rules
                .iter()
                .map(|r| CompiledCreateRule::compile(r, defs))
                .collect::<Result<Vec<_>, _>>()?;

            mounts.push(CompiledMount {
                virtual_path: v_path,
                host_path: mount_cfg.host,
                read_only: mount_cfg.read_only,
                ignore_set: ignore_set.into(),
                hide_expr,
                ro_expr,
                description: mount_cfg.description,
                kind,
                create_rules,
            });
        }

        mounts.sort_by(|a, b| b.virtual_path.len().cmp(&a.virtual_path.len()));

        let roles: HashMap<String, CompiledRole> = raw_config
            .roles
            .iter()
            .map(|(name, cfg)| {
                (
                    name.clone(),
                    CompiledRole {
                        elevation: cfg.elevation,
                        description: cfg.description.as_deref().map(Into::into),
                    },
                )
            })
            .collect();

        let grants = raw_config
            .grants
            .iter()
            .map(|g| CompiledGrant::compile(g, defs))
            .collect::<Result<Vec<_>, _>>()?;

        // Guard against the exact footguns discussed when designing this:
        // a min_elevation grant can never target Elevation::NONE (it would
        // never fire, since NONE never qualifies for anything, but writing
        // it invites the false impression that elevation 0 can be granted
        // something), and ROLE_ONLY-only grants declared with min_elevation
        // but no roles are silently dead at that tier.
        for grant in &grants {
            if grant.min_elevation == Some(Elevation::NONE) {
                return Err(format!(
                    "Grant '{}' sets min_elevation: 0, but elevation 0 can never receive \
                     any grant (it is the hardcoded no-access floor). Remove min_elevation \
                     or raise it to 2 or above, or list explicit roles instead.",
                    grant.description
                ));
            }
            if grant.min_elevation == Some(Elevation::ROLE_ONLY) && grant.roles.is_empty() {
                return Err(format!(
                    "Grant '{}' sets min_elevation: 1 with no roles listed. Elevation 1 is \
                     a hardcoded whitelist tier — only role-matched grants apply there, so \
                     this grant would never actually fire. Set min_elevation: 2 or above, \
                     or list explicit roles.",
                    grant.description
                ));
            }
        }

        let mut realm = Self {
            name,
            default_workspace: raw_config.default_workspace.clone(),
            global_hide_expr,
            global_ro_expr,
            global_create_rules,
            mounts,
            raw_config,
            roles,
            grants,
            role_capabilities: HashMap::new(),
        };

        let role_names: Vec<String> = realm.roles.keys().cloned().collect();
        realm.role_capabilities = role_names
            .into_iter()
            .map(|name| {
                let caps = realm.describe_role_capabilities(&name);
                (name, Arc::new(caps))
            })
            .collect();

        Ok(realm)
    }

    /// Whether `rel_path` (relative to a mount root) is hidden purely by the
    /// built-in dotfile rule, i.e. any path component starts with `.`. This
    /// is unconditional — no elevation, no `exempt_roles`, nothing but an
    /// explicit `Grant { kind: Visibility, overrides: Some("dotfiles"), .. }`
    /// naming a role can unhide anything matched by it. It exists so Realm
    /// definitions (e.g. under `.inforno/`) can never be discovered or
    /// blindly created by any role, at any elevation, unless a config author
    /// deliberately carves out an exception by name.
    fn is_builtin_dotfile_path(rel_path: &Path) -> bool {
        rel_path
            .components()
            .any(|c| c.as_os_str().to_str().map(|s| s.starts_with('.')).unwrap_or(false))
    }

    /// True if a grant held by `actor` explicitly overrides the dotfile
    /// restriction for `rel_path`. Requires the grant to name one of the
    /// actor's roles directly in `roles` — `min_elevation` can never satisfy
    /// this, regardless of value or which role holds it.
    fn dotfile_override_applies(&self, rel_path: &Path, actor: &Actor) -> bool {
        self.grants.iter().any(|g| {
            g.kind == GrantKind::Visibility
                && g.matches_path(rel_path)
                && actor.roles.iter().any(|role_name| g.qualifies_as_override("dotfiles", role_name))
        })
    }

    /// Single source of truth for "is this path hidden from `actor`" — used
    /// by both visibility checks (lookup/readdir) and creation checks, so
    /// there is no blind-create gap where a hidden path can still be created
    /// just because it can't be seen first.
    pub fn is_path_hidden(&self, virtual_path: &Path, actor: &Actor) -> bool {
        let path_str = match virtual_path.to_str() {
            Some(s) => s,
            None => return true, // Safe fallback
        };

        for mount in &self.mounts {
            if path_str.starts_with(&mount.virtual_path) {
                let relative = path_str
                    .strip_prefix(&mount.virtual_path)
                    .unwrap_or("")
                    .trim_start_matches('/');
                let rel_path = Path::new(relative);

                if Self::is_builtin_dotfile_path(rel_path) {
                    return !self.dotfile_override_applies(rel_path, actor);
                }

                if let Some(ref expr) = self.global_hide_expr {
                    if expr.is_match(rel_path) { return true; }
                }
                if let Some(ref expr) = mount.hide_expr {
                    if expr.is_match(rel_path) { return true; }
                }

                return false;
            }
        }
        false // Outside all mounts: not this function's concern; secure_resolve_path returns None anyway.
    }

    pub fn secure_resolve_path(&self, virtual_path: &Path, actor: &Actor) -> Option<PathBuf> {
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

                if self.is_path_hidden(virtual_path, actor) { return None; }

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

    /// Checks whether a *new* file may be created at `virtual_path`, ignoring
    /// role/grant permission entirely — this only evaluates the hard,
    /// Realm-wide restrictions (dotfile hiding, mount read-only, create_rules).
    /// On denial, returns the description of the first rule that forbids it —
    /// note this description cannot be relayed through the FUSE errno reply itself;
    /// it's intended for logging and for `describe_create_rules` below.
    /// Use `can_access` for the full actor-aware check (hard restrictions AND
    /// whether the actor's roles actually grant Create here).
    pub fn check_create_allowed(&self, virtual_path: &Path, actor: &Actor) -> Result<(), String> {
        let path_str = virtual_path
            .to_str()
            .ok_or_else(|| "Path is not valid UTF-8".to_string())?;

        if self.is_path_hidden(virtual_path, actor) {
            return Err("Path is hidden (e.g. a dotfile/dot-directory) and cannot be created".to_string());
        }

        for mount in &self.mounts {
            if path_str.starts_with(&mount.virtual_path) {
                if mount.read_only {
                    return Err(format!("Mount '{}' is entirely read-only", mount.virtual_path));
                }

                let relative = path_str
                    .strip_prefix(&mount.virtual_path)
                    .unwrap_or("")
                    .trim_start_matches('/');
                let rel_path = Path::new(relative);

                for rule in self.global_create_rules.iter().chain(mount.create_rules.iter()) {
                    if rule.in_scope(rel_path) && rule.forbids(rel_path) {
                        return Err(rule.description.to_string());
                    }
                }
                return Ok(());
            }
        }

        Err("Path is outside all configured mounts".to_string())
    }

    /// The full actor-aware access check: hard restrictions (hidden, create
    /// rules, read-only for Write) MUST pass, AND at least one role the
    /// actor holds must carry a Grant of the requested `kind` covering this
    /// path. An actor holding no roles (or none the Realm recognizes) is
    /// denied everything, without needing any grant lookup — this mirrors
    /// `Elevation::NONE`'s guarantee even for actors that hold no roles at all.
    pub fn can_access(&self, virtual_path: &Path, kind: GrantKind, actor: &Actor) -> Result<(), String> {
        match kind {
            GrantKind::Create => self.check_create_allowed(virtual_path, actor)?,
            GrantKind::Write => {
                if self.is_path_hidden(virtual_path, actor) {
                    return Err("Path is hidden and cannot be written".to_string());
                }
                if self.is_path_read_only(virtual_path) {
                    return Err("Path is read-only".to_string());
                }
            }
            GrantKind::Read | GrantKind::Visibility => {
                if self.is_path_hidden(virtual_path, actor) {
                    return Err("Path is hidden".to_string());
                }
            }
        }

        if actor.roles.is_empty() {
            return Err("Actor holds no roles; no filesystem access is possible".to_string());
        }

        let path_str = virtual_path
            .to_str()
            .ok_or_else(|| "Path is not valid UTF-8".to_string())?;

        let rel_path = self
            .mounts
            .iter()
            .find(|m| path_str.starts_with(&m.virtual_path))
            .map(|m| {
                path_str
                    .strip_prefix(&m.virtual_path)
                    .unwrap_or("")
                    .trim_start_matches('/')
                    .to_string()
            })
            .ok_or_else(|| "Path is outside all configured mounts".to_string())?;
        let rel_path = Path::new(&rel_path);

        let granted = actor.roles.iter().any(|role_name| {
            let Some(role) = self.roles.get(role_name) else { return false };
            self.grants.iter().any(|g| {
                g.kind == kind && g.matches_path(rel_path) && g.qualifies_role(role_name, role.elevation)
            })
        });

        if granted {
            Ok(())
        } else {
            Err(format!("No role held by this actor grants {:?} access to '{}'", kind, virtual_path.display()))
        }
    }

    /// Lists the description of every create-rule active in this Realm, so they
    /// can be surfaced up front (e.g. injected into an agent's system prompt or
    /// shown in a Realm-config UI), independent of any specific denial.
    pub fn describe_create_rules(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .global_create_rules
            .iter()
            .map(|r| r.description.to_string())
            .collect();

        for mount in &self.mounts {
            for rule in &mount.create_rules {
                out.push(format!("[{}] {}", mount.virtual_path, rule.description));
            }
        }
        out
    }

    /// Renders a role's actual, resolved capabilities as plain-English,
    /// positively-framed statements — no mention of elevation numbers, tiers,
    /// or any other engine-internal mechanism, only concrete outcomes.
    /// Intended to be spliced into an Agent's system message so it knows up
    /// front exactly what it can do and doesn't waste turns retrying blocked
    /// operations (e.g. looping on a failed file creation). Computed once
    /// and cached in `role_capabilities` at construction time — call that
    /// field directly rather than this method after construction.
    fn describe_role_capabilities(&self, role_name: &str) -> Vec<String> {
        let Some(role) = self.roles.get(role_name) else {
            return vec!["This role does not exist in this Realm and has no filesystem access.".to_string()];
        };

        if role.elevation == Elevation::NONE {
            return vec!["This role has no filesystem access of any kind. Do not attempt to read, write, or create any files — such attempts will always be denied.".to_string()];
        }

        let mut lines = Vec::new();
        for grant in &self.grants {
            if grant.kind == GrantKind::Visibility { continue; } // internal-only, not a user-facing capability
            if grant.qualifies_role(role_name, role.elevation) {
                let verb = match grant.kind {
                    GrantKind::Read => "read",
                    GrantKind::Write => "write",
                    GrantKind::Create => "create",
                    GrantKind::Visibility => unreachable!(),
                };
                lines.push(format!("You may {} files {}. {}", verb, describe_expr(&grant.expr_source, &self.raw_config.expressions), grant.description));
            }
        }

        if lines.is_empty() {
            lines.push("This role has no filesystem access of any kind. Do not attempt to read, write, or create any files — such attempts will always be denied.".to_string());
        } else {
            lines.push("You have no other filesystem access beyond what's listed above — do not attempt anything else; such attempts will always be denied.".to_string());
        }

        lines
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

/// Renders a `GlobExpr` as plain English. `Ref(name)` is fully EXPANDED to
/// the globs it resolves to, rather than printed as a bare name — an agent
/// reading this shouldn't need to know a Realm author used named
/// expressions internally, only the concrete outcome. Assumes `expr` came
/// from an already-validated `RealmConfig` (i.e. no undefined refs or
/// cycles); the `None` fallback below is defensive, not expected in practice.
pub fn describe_expr(expr: &GlobExpr, defs: &IndexMap<String, GlobExpr>) -> String {
    match expr {
        GlobExpr::Match(globs) => {
            if globs.len() == 1 {
                format!("matching `{}`", globs[0])
            } else {
                format!("matching any of: {}", globs.iter().map(|g| format!("`{}`", g)).collect::<Vec<_>>().join(", "))
            }
        }
        GlobExpr::Any(exprs) => {
            let parts: Vec<String> = exprs.iter().map(|e| describe_expr(e, defs)).collect();
            format!("any of ({})", parts.join("; or "))
        }
        GlobExpr::All(exprs) => {
            let parts: Vec<String> = exprs.iter().map(|e| describe_expr(e, defs)).collect();
            format!("all of ({})", parts.join("; and "))
        }
        GlobExpr::Not(inner) => format!("anything except {}", describe_expr(inner, defs)),
        GlobExpr::Ref(name) => match defs.get(name) {
            Some(target) => describe_expr(target, defs),
            None => format!("matching an undefined pattern '{}'", name),
        },
    }
}

pub fn resolve_filepath(
    realm: &Option<ActiveRealm>,
    actor: &Actor,
    project_root: &Option<std::path::PathBuf>,
    requested_path: &str
) -> Option<(std::path::PathBuf, bool)> {
    let mut target_root = None;
    let mut relative_path_str = requested_path.trim();

    // 1. Attempt VFS Translation if we are in a Realm
    if let Some(active_realm) = realm {
        let req_path = std::path::Path::new(relative_path_str);

        if let Some(secure_host_path) = active_realm.secure_resolve_path(req_path, actor) {
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
