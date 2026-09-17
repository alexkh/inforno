use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::collections::{BTreeMap, HashMap, HashSet};
use indexmap::IndexMap;
use std::sync::Arc;
use globset::{Glob, GlobSet, GlobSetBuilder};

/// A trust tier, 0-9. `0` and `1` are hardcoded and reserved:
/// - `0` (`NONE`): no power, of any kind, can ever apply. Provably zero access.
/// - `1` (`ROLE_ONLY`): only a role's OWN `powers` apply — tiers never cascade in.
/// Tiers `2..=9` are config-defined via `RealmConfig::tiers` and cascade
/// upward: a role at tier N holds every power defined at any tier `2..=N`,
/// plus its own. Numbers ascend from least to most trusted, so adding a
/// more-trusted tier later is just a bigger number — nothing existing needs
/// renumbering, and low-tier roles never need to know how many tiers exist
/// above them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
pub struct Tier(pub u32);

impl Tier {
    pub const NONE: Tier = Tier(0);
    pub const ROLE_ONLY: Tier = Tier(1);
    pub const MIN_CASCADING: Tier = Tier(2);
    pub const MAX: Tier = Tier(9);
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(untagged)]
pub enum GlobExpr {
    Any { any: Vec<GlobExpr> },
    All { all: Vec<GlobExpr> },
    Not { not: Box<GlobExpr> },
    Match { #[serde(rename = "match")] match_globs: Vec<String> },
    /// References a named expression in `RealmConfig::spans`, e.g.
    /// `{ ref: "rust_source" }`. Resolved and cycle-checked at compile time.
    Ref { #[serde(rename = "ref")] ref_name: String },
}

pub enum CompiledExpr {
    Any(Vec<CompiledExpr>),
    All(Vec<CompiledExpr>),
    Not(Box<CompiledExpr>),
    Match(globset::GlobSet),
}

impl CompiledExpr {
    /// Compiles a `GlobExpr`, resolving any `Ref(name)` against `defs`.
    /// Rejects references to undefined names and cyclic references.
    pub fn compile(expr: &GlobExpr, defs: &IndexMap<String, GlobExpr>) -> Result<Self, String> {
        Self::compile_inner(expr, defs, &mut Vec::new())
    }

    fn compile_inner(expr: &GlobExpr, defs: &IndexMap<String, GlobExpr>, stack: &mut Vec<String>) -> Result<Self, String> {
        match expr {
            GlobExpr::Any { any: exprs } => {
                let compiled = exprs.iter().map(|e| Self::compile_inner(e, defs, stack)).collect::<Result<Vec<_>, _>>()?;
                Ok(CompiledExpr::Any(compiled))
            }
            GlobExpr::All { all: exprs } => {
                let compiled = exprs.iter().map(|e| Self::compile_inner(e, defs, stack)).collect::<Result<Vec<_>, _>>()?;
                Ok(CompiledExpr::All(compiled))
            }
            GlobExpr::Not { not: inner } => {
                Ok(CompiledExpr::Not(Box::new(Self::compile_inner(inner, defs, stack)?)))
            }
            GlobExpr::Match { match_globs: globs } => {
                let mut builder = globset::GlobSetBuilder::new();
                for g in globs {
                    builder.add(globset::Glob::new(g).map_err(|e| format!("Invalid glob '{}': {}", g, e))?);
                }
                Ok(CompiledExpr::Match(builder.build().map_err(|e| e.to_string())?))
            }
            GlobExpr::Ref { ref_name: name } => {
                if stack.contains(name) {
                    let mut cycle = stack.clone();
                    cycle.push(name.clone());
                    return Err(format!("Cyclic expression reference: {}", cycle.join(" -> ")));
                }
                let target = defs.get(name).ok_or_else(|| format!("Undefined named expression '{}'", name))?;
                stack.push(name.clone());
                let compiled = Self::compile_inner(target, defs, stack)?;
                stack.pop();
                Ok(compiled)
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
/// active rules can be listed out (e.g. to an AI actor) independently of any
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

/// The kind of access a `Power` confers.
#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Cap {
    Read,
    Write,
    /// Write restricted to appending at end-of-file: no truncation, no
    /// arbitrary-offset writes. Enforced at the FUSE layer (blocking
    /// truncating `setattr` and non-EOF `write` offsets) for paths where a
    /// role holds only `Append`, not `Write`.
    Append,
    Create,
}

/// A single grant of access: a path pattern (`span`), the cap(s) it confers,
/// and optionally an `intro` — guidance shown to an actor regardless of
/// whether this power's outcome is permissive, e.g. steering it toward a
/// tool ("use `cargo add`") instead of a direct filesystem write, even when
/// direct writes are technically absent or present. `overrides` names a
/// restriction (currently only `"dotfiles"`) this power may override.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Power {
    pub span: GlobExpr,
    pub caps: Vec<Cap>,
    #[serde(default)]
    pub intro: Option<String>,
    #[serde(default)]
    pub overrides: Option<String>,
}

/// A role definition: a Tier plus any powers specific to this role alone
/// (on top of whatever the Tier cascades in). Elevation/tier is intrinsic to
/// the role, not assignable per-Actor, so a role's containment guarantee
/// holds everywhere it's used, regardless of what other roles the same
/// Actor simultaneously holds.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct RoleConfig {
    pub tier: Tier,
    #[serde(default)]
    pub powers: Vec<Power>,
    pub intro: String,
    #[serde(default)]
    pub boss: Option<String>,
    /// Glob expression selecting extra host binaries (matched by bare
    /// basename) to expose in `/bin`, on top of whatever the Realm-wide
    /// `bin` and this role's Tier(s) already contribute. See
    /// `RealmConfig::bin` for how the cascade combines.
    #[serde(default)]
    pub bin: Option<GlobExpr>,
    #[serde(default)]
    pub env: Vec<String>,
}

/// Declares that a mount contains more than one selectable root — e.g. a
/// Cargo workspace with several member crates, or a handful of sibling
/// checkouts a Sandbox might focus on one at a time. Each resolved root is
/// called a "Bucket". A mount with no `BucketConfig` has exactly one
/// implicit bucket: its own root, and the GUI shows no sub-selector.
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct BucketConfig {
    /// Explicit bucket roots, relative to the mount's host path.
    #[serde(default)]
    pub paths: Vec<String>,
    /// Auto-discover buckets from this mount's `Cargo.toml` `[workspace]
    /// members` (including `dir/*` glob members), same as before.
    #[serde(default)]
    pub cargo_workspace: bool,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct RealmMountConfig {
    pub host: PathBuf,
    #[serde(default)]
    pub read_only: bool,
    pub intro: Option<String>,
}

/// One Sandbox a Realm is willing to open. Exactly one of `study` or `path`
/// should be set:
/// - `study` names a directory under the managed studies root
///   (`~/.local/share/inforno/studies/<study>/`). A Study can hold more than
///   one sandbox file side by side (e.g. `info.rno` and `another.rno`),
///   plus whatever sidecar files a sandbox later accumulates (layout,
///   cache) — all sharing that one directory instead of inventing
///   parallel-named sidecar files per sandbox. `file` picks which sandbox
///   inside the Study; defaults to `info.rno` if omitted.
/// - `path` is an explicit host path to a sandbox file anywhere on the
///   filesystem — the escape hatch for sandboxes that predate Studies, or
///   are deliberately kept outside the managed studies root.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct SandboxRef {
    /// Absolute host path to the self-contained `.rno` sandbox file.
    /// Required on purpose: a Realm must name the exact sandbox file it
    /// authorizes, and must not rely on a resolver reconstructing a path
    /// from a short name.
    pub path: PathBuf,
    /// Realm roles this sandbox is allowed to use.
    /// Empty means the sandbox is known but not authorized to open
    /// this Realm under any role.
    #[serde(default)]
    pub roles: Vec<String>,
    #[serde(default)]
    pub description: Option<String>,
}

/// A single filesystem-path component: no separators, no `.`/`..`. Used to
/// keep `study`/`file` from ever being joined into a traversal outside
/// `studies_dir` (`study: "../../etc"` or `file: "../secrets"`). `pub(crate)`
/// so `db::init_study_sandbox` can apply the exact same rule when it creates
/// the directory/file this module later resolves.
pub(crate) fn is_safe_path_component(s: &str) -> bool {
    !s.is_empty() && s != "." && s != ".." && !s.contains('/') && !s.contains('\\')
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct TierConfig {
    #[serde(default)]
    pub powers: Vec<Power>,
    /// Glob expression selecting extra host binaries (matched by bare
    /// basename) to expose in `/bin` to every role at or above this tier —
    /// unioned with the Realm-wide `bin` and whatever the role adds itself.
    /// See `RealmConfig::bin` for how the cascade combines.
    #[serde(default)]
    pub bin: Option<GlobExpr>,
    #[serde(default)]
    pub env: Vec<String>,
}

fn validate_places<'de, D>(deserializer: D) -> Result<IndexMap<String, serde_saphyr::Commented<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let places = IndexMap::<String, serde_saphyr::Commented<String>>::deserialize(deserializer)?;
    for (name, commented) in &places {
        if commented.1.trim().is_empty() {
            return Err(serde::de::Error::custom(format!(
                "Place '{}' is missing a description. A YAML comment is required (e.g., `{} # My description`).",
                name, commented.0
            )));
        }
    }
    Ok(places)
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct RealmConfig {
    #[serde(default)]
    pub mounts: IndexMap<String, RealmMountConfig>,
    /// Global bookmarks resolving to virtual paths (or host paths) across the VFS.
    #[serde(default, deserialize_with = "validate_places")]
    pub places: IndexMap<String, serde_saphyr::Commented<String>>,
    /// Sandboxes permitted to open this Realm, keyed by local name. The key
    /// `"default"` is reserved and names the sandbox opened when the Realm
    /// itself is opened directly. Each sandbox must provide an absolute
    /// `path` to a self-contained `.rno` file.
    #[serde(default)]
    pub sandboxes: IndexMap<String, SandboxRef>,
    /// Role name -> Tier/description/role-specific powers.
    #[serde(default)]
    pub roles: IndexMap<String, RoleConfig>,
    /// Tier number (2-9) -> powers cascading to every role AT OR ABOVE that
    /// tier. Sparse: a tier number with no entry contributes nothing, no
    /// contiguity required. 0 and 1 are reserved and may not appear here.
    #[serde(default)]
    pub tiers: BTreeMap<u32, TierConfig>,
    /// Named, reusable `GlobExpr` definitions, referenced via
    /// `GlobExpr::Ref(name)`. May reference each other; cycles are rejected
    /// at load time. Purely a config-authoring convenience.
    #[serde(default, rename = "spans")]
    pub expressions: IndexMap<String, GlobExpr>,
    /// Glob expression selecting which host binaries (matched by bare
    /// basename, e.g. `cc`, `git`) are exposed in a spawned process's
    /// chroot `/bin`, on top of the fixed coreutils list (`bash`, `ls`,
    /// `cat`, ...) that's always present regardless of this cascade. This
    /// is the base of the `bin` cascade — applied first, then unioned with
    /// whatever the actor's Tier(s) (`TierConfig::bin`) and Role
    /// (`RoleConfig::bin`) add on top. A match at ANY level is enough to
    /// expose a binary — "everything except X" is expressed within one
    /// level's own expression (`all` + `not`), not by subtracting across
    /// levels. See `ActiveRealm::bin_is_selected`.
    #[serde(default)]
    pub bin: Option<GlobExpr>,
    #[serde(default)]
    pub env: Vec<String>,
}

/// Resolves the on-disk path for `key` in `config.sandboxes`. `studies_dir`
/// is the managed studies root (e.g. `~/.local/share/inforno/studies`),
/// supplied by the caller since this crate doesn't own XDG path resolution.
pub fn resolve_sandbox_path(
    key: &str,
    config: &RealmConfig,
) -> Result<PathBuf, String> {
    let sref = config.sandboxes.get(key)
        .ok_or_else(|| format!("No sandbox named '{}' declared in this Realm", key))?;

    if !sref.path.is_absolute() {
        return Err(format!(
            "Sandbox '{}' path must be an absolute host path, got '{}'",
            key, sref.path.display()
        ));
    }

    if sref.path.extension().and_then(|e| e.to_str()) != Some("rno") {
        return Err(format!(
            "Sandbox '{}' path must point to a `.rno` file, got '{}'",
            key, sref.path.display()
        ));
    }

    Ok(sref.path.clone())
}

/// Resolves the default sandbox, which is the first declared entry.
/// Returns `Err` if `sandboxes` is empty or resolution fails — the
/// caller treats that as "this Realm has no sandbox yet" and should prompt
/// the user to create one rather than silently materializing one.
pub fn resolve_default_sandbox_path(
    config: &RealmConfig,
) -> Result<PathBuf, String> {
    let (key, _) = config.sandboxes.first()
        .ok_or_else(|| "This Realm declares no sandboxes".to_string())?;
    resolve_sandbox_path(key, config)
}

/// Guards the Realm/Sandbox split: a sandbox must never live inside the
/// realms directory. `realms_dir` is `~/.config/inforno/realms`.
pub fn sandbox_path_is_valid(path: &Path, realms_dir: &Path) -> bool {
    let canon_realms = std::fs::canonicalize(realms_dir).unwrap_or_else(|_| realms_dir.to_path_buf());
    let parent = path.parent().unwrap_or(path);
    let canon_parent = std::fs::canonicalize(parent).unwrap_or_else(|_| parent.to_path_buf());
    !canon_parent.starts_with(&canon_realms)
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

/// A compiled `Power`, ready to be matched against relative paths.
#[derive(Clone)]
pub struct CompiledPower {
    /// Raw source, kept for display purposes (`describe_role_capabilities`
    /// via `describe_expr`) — matching always uses the compiled `span`.
    pub span_source: GlobExpr,
    span: Arc<CompiledExpr>,
    pub caps: Vec<Cap>,
    pub intro: Option<Arc<str>>,
    pub overrides: Option<String>,
}

impl CompiledPower {
    fn compile(power: &Power, defs: &IndexMap<String, GlobExpr>) -> Result<Self, String> {
        let span = Arc::new(CompiledExpr::compile(&power.span, defs)?);
        Ok(Self {
            span_source: power.span.clone(),
            span,
            caps: power.caps.clone(),
            intro: power.intro.as_deref().map(Into::into),
            overrides: power.overrides.clone(),
        })
    }

    fn matches_path(&self, rel_path: &Path) -> bool {
        self.span.is_match(rel_path)
    }

    fn has_cap(&self, cap: Cap) -> bool {
        self.caps.contains(&cap)
    }

    /// Whether this power grants `requested`. `Write` implies `Append` (a
    /// full write capability is strictly more permissive than append-only),
    /// but not the reverse — holding only `Append` never satisfies a
    /// request for `Write`.
    fn grants(&self, requested: Cap) -> bool {
        self.has_cap(requested) || (requested == Cap::Append && self.has_cap(Cap::Write))
    }
}

#[derive(Clone)]
pub struct CompiledMount {
    pub virtual_path: String,
    pub host_path: PathBuf,
    pub read_only: bool,
    pub intro: Option<String>,
}

#[derive(Clone)]
pub struct CompiledRole {
    pub tier: Tier,
    pub intro: Arc<str>,
    pub boss: Option<String>,
    pub powers: Vec<CompiledPower>,
    pub bin: Option<Arc<CompiledExpr>>,
    pub env: Option<Arc<globset::GlobSet>>,
    pub env_vars: HashMap<String, String>,
}

#[derive(Clone)]
pub struct ActiveRealm {
    pub name: String,
    pub mounts: Vec<CompiledMount>,
    pub raw_config: RealmConfig,
    pub roles: HashMap<String, CompiledRole>,
    /// Tier number -> powers cascading from that tier upward. `BTreeMap`
    /// keeps numeric order for `range()` queries during cascade resolution.
    pub tiers: BTreeMap<u32, Vec<CompiledPower>>,
    /// Tier number -> compiled `bin` expression, mirroring `tiers` above.
    /// Sparse: a tier with no `bin` declared contributes nothing to the cascade.
    pub tier_bins: BTreeMap<u32, Arc<CompiledExpr>>,
    pub tier_envs: BTreeMap<u32, Arc<globset::GlobSet>>,
    pub tier_env_vars: BTreeMap<u32, HashMap<String, String>>,
    /// The Realm-wide `bin` expression (from `RealmConfig::bin`), applied
    /// first in the cascade — see `bin_is_selected`.
    pub bin: Option<Arc<CompiledExpr>>,
    pub env: Option<Arc<globset::GlobSet>>,
    pub env_vars: HashMap<String, String>,
    /// Plain-English capability descriptions per role name, computed once at
    /// construction time. Correctness depends on `ActiveRealm` always being
    /// rebuilt fresh via `from_config` when realm2.yml changes, rather than
    /// mutated in place — if that assumption ever changes, this cache needs
    /// explicit invalidation.
    pub role_capabilities: HashMap<String, Arc<Vec<String>>>,
}

fn compile_env_list(envs: &[String]) -> Result<(Option<Arc<globset::GlobSet>>, HashMap<String, String>), String> {
    let mut builder = globset::GlobSetBuilder::new();
    let mut vars = HashMap::new();
    let mut has_globs = false;

    for item in envs {
        if let Some((k, v)) = item.split_once('=') {
            // Keep the value exactly as-is so trailing spaces (like in PS1) are preserved
            vars.insert(k.trim().to_string(), v.to_string());
        } else {
            builder.add(globset::Glob::new(item).map_err(|e| format!("Invalid env glob '{}': {}", item, e))?);
            has_globs = true;
        }
    }

    let globset = if has_globs {
        Some(Arc::new(builder.build().map_err(|e| e.to_string())?))
    } else {
        None
    };

    Ok((globset, vars))
}

impl ActiveRealm {
    pub fn from_config(name: String, config: RealmConfig) -> Result<Self, String> {
        let mut mounts = Vec::new();

        // Clone the config BEFORE the loop consumes it
        let raw_config = config.clone();

        // Fail fast on bad named expressions (undefined refs, cycles) even if
        // a definition happens not to be used anywhere else in this config.
        for (ename, expr) in &raw_config.expressions {
            CompiledExpr::compile(expr, &raw_config.expressions)
                .map_err(|e| format!("In named expression '{}': {}", ename, e))?;
        }

        for (v_path, mount_cfg) in config.mounts {
            mounts.push(CompiledMount {
                virtual_path: v_path,
                host_path: mount_cfg.host,
                read_only: mount_cfg.read_only,
                intro: mount_cfg.intro,
            });
        }

        mounts.sort_by(|a, b| b.virtual_path.len().cmp(&a.virtual_path.len()));

        // --- Tiers: validate range, compile, sanity-check `overrides`. ---
        for &k in raw_config.tiers.keys() {
            if k < 2 || k > 9 {
                return Err(format!(
                    "tiers key {} is out of range — tiers must be numbered 2-9 (0 and 1 are reserved, hardcoded tiers)",
                    k
                ));
            }
        }

        let mut tiers: BTreeMap<u32, Vec<CompiledPower>> = BTreeMap::new();
        let mut tier_bins: BTreeMap<u32, Arc<CompiledExpr>> = BTreeMap::new();
        let mut tier_envs: BTreeMap<u32, Arc<globset::GlobSet>> = BTreeMap::new();
        let mut tier_env_vars: BTreeMap<u32, HashMap<String, String>> = BTreeMap::new();
        for (&tier_num, tier_cfg) in &raw_config.tiers {
            let compiled = tier_cfg.powers
                .iter()
                .map(|p| CompiledPower::compile(p, &raw_config.expressions))
                .collect::<Result<Vec<_>, _>>()?;
            tiers.insert(tier_num, compiled);

            if let Some(expr) = &tier_cfg.bin {
                let compiled_bin = CompiledExpr::compile(expr, &raw_config.expressions)
                    .map_err(|e| format!("In tier {}'s `bin`: {}", tier_num, e))?;
                tier_bins.insert(tier_num, Arc::new(compiled_bin));
            }
            let (tier_env_set, tier_env_map) = compile_env_list(&tier_cfg.env)
                .map_err(|e| format!("In tier {}'s `env`: {}", tier_num, e))?;
            if let Some(set) = tier_env_set {
                tier_envs.insert(tier_num, set);
            }
            if !tier_env_map.is_empty() {
                tier_env_vars.insert(tier_num, tier_env_map);
            }
        }

        // --- Roles: validate tier 0 has no powers, tier <= 9, compile powers. ---
        let mut roles: HashMap<String, CompiledRole> = HashMap::new();
        for (rname, rcfg) in &raw_config.roles {
            if rcfg.tier.0 > Tier::MAX.0 {
                return Err(format!("Role '{}' has tier {}, which exceeds the maximum of {}", rname, rcfg.tier.0, Tier::MAX.0));
            }
            if rcfg.tier == Tier::NONE && (!rcfg.powers.is_empty() || rcfg.bin.is_some()) {
                return Err(format!(
                    "Role '{}' is at tier 0 (no access, hardcoded) but declares its own powers \
                     and/or a `bin` expression. Tier 0 can never receive any power or extra \
                     binary, of any kind — remove them or raise the tier.",
                    rname
                ));
            }

            let compiled_powers = rcfg
                .powers
                .iter()
                .map(|p| CompiledPower::compile(p, &raw_config.expressions))
                .collect::<Result<Vec<_>, _>>()?;

            let compiled_bin = match &rcfg.bin {
                Some(expr) => Some(Arc::new(
                    CompiledExpr::compile(expr, &raw_config.expressions)
                        .map_err(|e| format!("In role '{}'s `bin`: {}", rname, e))?,
                )),
                None => None,
            };

            let (compiled_env, env_vars) = compile_env_list(&rcfg.env)
                .map_err(|e| format!("In role '{}'s `env`: {}", rname, e))?;

            roles.insert(
                rname.clone(),
                CompiledRole {
                    tier: rcfg.tier,
                    intro: rcfg.intro.as_str().into(),
                    boss: rcfg.boss.clone(),
                    powers: compiled_powers,
                    bin: compiled_bin,
					env: compiled_env,
                    env_vars,
                },
            );
        }

        // --- Sandboxes: validate required absolute .rno paths and allowed role names. ---
        if let Some((sname, sref)) = raw_config.sandboxes.first() {
            if !sref.path.exists() {
                return Err(format!("Default sandbox '{}' points to a non-existent path: {}", sname, sref.path.display()));
            }
            if sref.roles.is_empty() {
                return Err(format!("The default sandbox '{}' must authorize at least one Realm role; otherwise the Realm cannot be opened through it.", sname));
            }
        }

        for (sname, sref) in &raw_config.sandboxes {
            if !sref.path.is_absolute() {
                return Err(format!(
                    "Sandbox '{}' path must be absolute, got '{}'",
                    sname, sref.path.display()
                ));
            }

            if sref.path.extension().and_then(|e| e.to_str()) != Some("rno") {
                return Err(format!(
                    "Sandbox '{}' path must point to a `.rno` file, got '{}'",
                    sname, sref.path.display()
                ));
            }

            for role_name in &sref.roles {
                if !roles.contains_key(role_name) {
                    return Err(format!(
                        "Sandbox '{}' lists role '{}', but that role is not defined in this Realm",
                        sname, role_name
                    ));
                }
            }
        }

        let bin = match &raw_config.bin {
            Some(expr) => Some(Arc::new(
                CompiledExpr::compile(expr, &raw_config.expressions)
                    .map_err(|e| format!("In the Realm's top-level `bin`: {}", e))?,
            )),
            None => None,
        };

        let (env, env_vars) = compile_env_list(&raw_config.env)
            .map_err(|e| format!("In the Realm's top-level `env`: {}", e))?;

        let mut realm = Self {
            name,
            mounts,
            raw_config,
            roles,
            tiers,
            tier_bins,
            tier_envs,
            tier_env_vars,
            bin,
            env,
            env_vars,
            role_capabilities: HashMap::new(),
        };

        let role_names: Vec<String> = realm.roles.keys().cloned().collect();
        realm.role_capabilities = role_names
            .into_iter()
            .map(|n| {
                let caps = realm.describe_role_capabilities(&n);
                (n, Arc::new(caps))
            })
            .collect();

        Ok(realm)
    }

    /// All powers effectively held by `role`: every power defined at any
    /// tier `2..=role.tier` (cascading; gaps in `tiers` simply contribute
    /// nothing — no contiguity is required), plus the role's own `powers`.
    /// Tier 0 never reaches here in practice (callers short-circuit it);
    /// tier 1 correctly yields only the role's own powers, since the range
    /// `2..=1` is empty.
    fn effective_powers<'a>(&'a self, role: &'a CompiledRole) -> Vec<&'a CompiledPower> {
        let mut out: Vec<&CompiledPower> = Vec::new();
        if role.tier.0 >= Tier::MIN_CASCADING.0 {
            for (_, powers) in self.tiers.range(Tier::MIN_CASCADING.0..=role.tier.0) {
                out.extend(powers.iter());
            }
        }
        out.extend(role.powers.iter());
        out
    }

    /// Whether `candidate` (a bare binary basename, e.g. `Path::new("cc")`)
    /// is selected by `role_name`'s effective `bin` expression. Resolution
    /// mirrors `effective_powers`: the Realm-wide `bin` (if any) is checked
    /// first, then every cascading tier's `bin` from `2..=role.tier`, then
    /// the role's own `bin` — a match at ANY level is sufficient (a union
    /// across levels). Excluding names ("everything except X") is
    /// expressed within a single level's own boolean expression
    /// (`all` + `not`), not by subtracting across levels. Tier 0 (`NONE`)
    /// is "provably zero access, of any kind" by design (see `Tier`), so it
    /// short-circuits to `false` even for the Realm-wide `bin`.
    pub fn bin_is_selected(&self, role_name: &str, candidate: &Path) -> bool {
        let Some(role) = self.roles.get(role_name) else { return false };
        if role.tier == Tier::NONE {
            return false;
        }

        if let Some(expr) = &self.bin {
            if expr.is_match(candidate) {
                return true;
            }
        }

        if role.tier.0 >= Tier::MIN_CASCADING.0 {
            for (_, expr) in self.tier_bins.range(Tier::MIN_CASCADING.0..=role.tier.0) {
                if expr.is_match(candidate) {
                    return true;
                }
            }
        }

        if let Some(expr) = &role.bin {
            if expr.is_match(candidate) {
                return true;
            }
        }

        false
    }

    /// Scans `search_dirs` (host directories, e.g. `/usr/bin`,
    /// `/usr/local/bin`) for entries whose basename is selected by
    /// `role_name`'s effective `bin` expression (`bin_is_selected`),
    /// returning their absolute host paths. Used to decide which extra
    /// binaries get bind-mounted into a spawned process's chroot `/bin`,
    /// on top of the fixed coreutils list `realm_spawn.rs` always mounts.
    /// A basename already seen in an earlier dir is skipped, so distros
    /// where e.g. `/bin` symlinks to `/usr/bin` don't produce duplicate
    /// mount attempts for the same name.
    pub fn env_is_selected(&self, role_name: &str, candidate: &str) -> bool {
        let Some(role) = self.roles.get(role_name) else { return false };
        if role.tier == Tier::NONE {
            return false;
        }

        let cand_path = Path::new(candidate);

        if let Some(set) = &self.env {
            if set.is_match(cand_path) {
                return true;
            }
        }

        if role.tier.0 >= Tier::MIN_CASCADING.0 {
            for (_, set) in self.tier_envs.range(Tier::MIN_CASCADING.0..=role.tier.0) {
                if set.is_match(cand_path) {
                    return true;
                }
            }
        }

        if let Some(set) = &role.env {
            if set.is_match(cand_path) {
                return true;
            }
        }

        false
    }

    pub fn allowed_envs(&self, role_name: &str) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for (k, v) in std::env::vars() {
            if self.env_is_selected(role_name, &k) {
                out.push((k, v));
            }
        }

        let Some(role) = self.roles.get(role_name) else { return out };
        
        if role.tier == Tier::NONE {
            return out;
        }

        // Apply Realm-wide custom env vars
        for (k, v) in &self.env_vars {
            out.push((k.clone(), v.clone()));
        }

        // Apply tier custom env vars
        if role.tier.0 >= Tier::MIN_CASCADING.0 {
            for (_, vars) in self.tier_env_vars.range(Tier::MIN_CASCADING.0..=role.tier.0) {
                for (k, v) in vars {
                    out.push((k.clone(), v.clone()));
                }
            }
        }

        // Apply role custom env vars
        for (k, v) in &role.env_vars {
            out.push((k.clone(), v.clone()));
        }

        out
    }

    pub fn allowed_binaries(&self, role_name: &str, search_dirs: &[PathBuf]) -> Vec<PathBuf> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for dir in search_dirs {
            let Ok(entries) = std::fs::read_dir(dir) else { continue };
            for entry in entries.flatten() {
                let path = entry.path();
                let Some(name) = path.file_name() else { continue };
                if !seen.insert(name.to_os_string()) {
                    continue;
                }
                if self.bin_is_selected(role_name, Path::new(name)) {
                    out.push(path);
                }
            }
        }
        out
    }

    /// Whether `rel_path` (relative to a mount root) is hidden purely by the
    /// built-in dotfile rule, i.e. any path component starts with `.`. This
    /// is unconditional by default — nothing but an explicit
    /// `Power { caps: [visibility], overrides: "dotfiles" }` matching the
    /// path can unhide it, whether that power is declared directly on a
    /// role or cascaded in from a tier. It exists so Realm definitions
    /// (e.g. under `.inforno/`) and other sensitive dotfiles stay invisible
    /// by default, unless a config author deliberately carves out an
    /// exception by name (tier-level exceptions are intentionally allowed —
    /// build tooling like Cargo relies heavily on dotfiles such as
    /// `target/.fingerprint/`, and gating that per-role only would make
    /// tiers like the `**/target/**` grant above unworkable).
    fn is_builtin_dotfile_path(rel_path: &Path) -> bool {
        rel_path
            .components()
            .any(|c| c.as_os_str().to_str().map(|s| s.starts_with('.')).unwrap_or(false))
    }

    /// True if `role_name`'s effective powers — its own `powers` plus
    /// whatever cascades in from its tier — include one overriding
    /// "dotfiles" for this path. Tier-level overrides are allowed
    /// deliberately: build tooling (Cargo, etc.) writes constantly to
    /// dotfiles/dot-directories under paths like `target/`, and requiring
    /// every role to redeclare that exception itself, rather than
    /// inheriting it from a shared tier grant (e.g. the `**/target/**`
    /// power), would make tiers largely useless for this case.
    fn dotfile_override_applies(&self, rel_path: &Path, role_name: &str) -> bool {
        let Some(role) = self.roles.get(role_name) else { return false };
        self.effective_powers(role).iter().any(|p| {
            p.overrides.as_deref() == Some("dotfiles")
                && p.matches_path(rel_path)
        })
    }

    /// Single source of truth for "is this path hidden from `actor`" — used
    /// by both visibility checks (lookup/readdir) and creation checks, so
    /// there is no blind-create gap where a hidden path can still be created
    /// just because it can't be seen first.
    pub fn is_path_hidden(&self, virtual_path: &Path, role_name: &str) -> bool {
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
                    return !self.dotfile_override_applies(rel_path, role_name);
                }

                return false;
            }
        }
        false // Outside all mounts: not this function's concern; secure_resolve_path returns None anyway.
    }

    /// Whether `role_name` is actually defined in this Realm's `roles:` map.
    /// Callers that assume a fixed role (e.g. the GUI's own "gui" role) use
    /// this to treat an undefined role as zero access, rather than letting
    /// `secure_resolve_path`'s visibility-only check silently let paths
    /// through for a role the Realm has never heard of.
    pub fn has_role(&self, role_name: &str) -> bool {
        self.roles.contains_key(role_name)
    }

    pub fn secure_resolve_path(&self, virtual_path: &Path, role_name: &str) -> Option<PathBuf> {
        let path_str = virtual_path.to_str()?;
        for mount in &self.mounts {
            if path_str.starts_with(&mount.virtual_path) {
                let relative = path_str
                    .strip_prefix(&mount.virtual_path)
                    .unwrap_or("")
                    .trim_start_matches('/');
                let host_target = mount.host_path.join(relative);

                if self.is_path_hidden(virtual_path, role_name) { return None; }

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
                // Check if the entire mount is read-only
                return mount.read_only;
            }
        }
        true // If it's outside all mounts, treat as read-only to be safe
    }

    /// The full actor-aware access check: hard restrictions (hidden, mount
    /// read-only) MUST pass, AND at least one role the actor holds must carry
    /// a power granting the requested `cap` for this path (via tier cascade or
    /// the role's own powers). An actor holding no roles (or none the Realm
    /// recognizes) is denied everything, without needing any power lookup.
    pub fn can_access(&self, virtual_path: &Path, cap: Cap, role_name: &str) -> Result<(), String> {
        match cap {
            Cap::Create | Cap::Write | Cap::Append => {
                if self.is_path_hidden(virtual_path, role_name) {
                    return Err("Path is hidden and cannot be modified".to_string());
                }
                if self.is_path_read_only(virtual_path) {
                    return Err("Path is read-only".to_string());
                }
            }
            Cap::Read => {
                if self.is_path_hidden(virtual_path, role_name) {
                    return Err("Path is hidden".to_string());
                }
            }
        }

        let Some(role) = self.roles.get(role_name) else {
            return Err(format!("Role '{}' is not defined in this Realm; no filesystem access is possible", role_name));
        };

        let path_str = virtual_path
            .to_str()
            .ok_or_else(|| "Path is not valid UTF-8".to_string())?;

        let rel_path_owned = self
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
        let rel_path = Path::new(&rel_path_owned);

        let granted = self.effective_powers(role)
            .iter()
            .any(|p| p.grants(cap) && p.matches_path(rel_path));

        if granted {
            Ok(())
        } else {
            Err(format!("Role '{}' does not grant {:?} access to '{}'", role_name, cap, virtual_path.display()))
        }
    }

    /// Renders a role's actual, resolved capabilities as plain-English,
    /// positively-framed statements — no mention of tier numbers or any
    /// other engine-internal mechanism, only concrete outcomes, plus any
    /// authored `memo` guidance attached to a power. Intended to be
    /// spliced into an Actor's system message so it knows up front exactly
    /// what it can do (and how it should prefer to do it) without wasting
    /// turns retrying blocked operations. Computed once and cached in
    /// `role_capabilities` at construction time — call that field directly
    /// rather than this method after construction.
    fn describe_role_capabilities(&self, role_name: &str) -> Vec<String> {
        let Some(role) = self.roles.get(role_name) else {
            return vec!["This role does not exist in this Realm and has no filesystem access.".to_string()];
        };

        if role.tier == Tier::NONE {
            return vec!["This role has no filesystem access of any kind. Do not attempt to read, write, or create any files — such attempts will always be denied.".to_string()];
        }

        let mut lines = Vec::new();
        for power in self.effective_powers(role) {
            let verbs: Vec<&str> = power
                .caps
                .iter()
                .map(|c| match c {
                    Cap::Read => "read",
                    Cap::Write => "write",
                    Cap::Append => "append to",
                    Cap::Create => "create",
                })
                .collect();

            if verbs.is_empty() {
                continue;
            }

            let mut line = format!(
                "You may {} files {}.",
                verbs.join(", "),
                describe_expr(&power.span_source, &self.raw_config.expressions)
            );
            if let Some(ref intro) = power.intro {
                line.push(' ');
                line.push_str(intro);
            }
            lines.push(line);
        }

        if lines.is_empty() {
            lines.push("This role has no filesystem access of any kind. Do not attempt to read, write, or create any files — such attempts will always be denied.".to_string());
        } else {
            lines.push("You have no other filesystem access beyond what's listed above — do not attempt anything else; such attempts will always be denied.".to_string());
        }

        lines
    }
}

/// Renders a `GlobExpr` as plain English. `Ref(name)` is fully EXPANDED to
/// the globs it resolves to, rather than printed as a bare name — an actor
/// reading this shouldn't need to know a Realm author used named
/// expressions internally, only the concrete outcome. Assumes `expr` came
/// from an already-validated `RealmConfig` (i.e. no undefined refs or
/// cycles); the fallback below is defensive, not expected in practice.
pub fn describe_expr(expr: &GlobExpr, defs: &IndexMap<String, GlobExpr>) -> String {
    match expr {
        GlobExpr::Match { match_globs: globs } => {
            if globs.len() == 1 {
                format!("matching `{}`", globs[0])
            } else {
                format!("matching any of: {}", globs.iter().map(|g| format!("`{}`", g)).collect::<Vec<_>>().join(", "))
            }
        }
        GlobExpr::Any { any: exprs } => {
            let parts: Vec<String> = exprs.iter().map(|e| describe_expr(e, defs)).collect();
            format!("any of ({})", parts.join("; or "))
        }
        GlobExpr::All { all: exprs } => {
            let parts: Vec<String> = exprs.iter().map(|e| describe_expr(e, defs)).collect();
            format!("all of ({})", parts.join("; and "))
        }
        GlobExpr::Not { not: inner } => format!("anything except {}", describe_expr(inner, defs)),
        GlobExpr::Ref { ref_name: name } => match defs.get(name) {
            Some(target) => describe_expr(target, defs),
            None => format!("matching an undefined pattern '{}'", name),
        },
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
    role_name: &str,
    project_root: &Option<std::path::PathBuf>,
    requested_path: &str
) -> Option<(std::path::PathBuf, bool)> {
    let mut target_root = None;
    let mut relative_path_str = requested_path.trim();

    // 0. Direct Absolute Path Match (e.g. LLM hallucinates full host path)
    let raw_path = std::path::Path::new(relative_path_str);
    if raw_path.is_absolute() && raw_path.exists() && raw_path.is_file() {
        return Some((raw_path.to_path_buf(), false));
    }

    // 1. Attempt VFS Translation if we are in a Realm
    if let Some(active_realm) = realm.as_ref() {
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

    // 2. Fallback to standard project_root if no valid Realm VFS match was found
    let root_to_search = target_root.or_else(|| project_root.clone())?;

    // Strip leading slashes so `Path::join` doesn't discard the root_to_search!
    let safe_rel_path = relative_path_str.trim_start_matches('/');
    let req_path = std::path::Path::new(safe_rel_path);

    // 3. Standard Exact Match Check
    let full_path = root_to_search.join(req_path);
    
    // We return the path even if it doesn't exist yet, because the LLM might be creating a new file.
    // The `is_file()` check is removed because a non-existent path isn't a file or a directory yet.
    if !full_path.is_dir() {
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
