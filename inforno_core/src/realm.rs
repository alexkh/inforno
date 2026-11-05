use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::collections::{BTreeMap, HashMap, HashSet};
use indexmap::IndexMap;
use std::sync::Arc;

/// A trust tier, 0-9. `0` and `1` are hardcoded and reserved:
/// - `0` (`NONE`): no power, of any kind, can ever apply. Provably zero access.
/// - `1` (`DUTY_ONLY`): only a duty's OWN `powers` apply — tiers never cascade in.
/// Tiers `2..=9` are config-defined via `RealmConfig::tiers` and cascade
/// upward: a duty at tier N holds every power defined at any tier `2..=N`,
/// plus its own. Numbers ascend from least to most trusted, so adding a
/// more-trusted tier later is just a bigger number — nothing existing needs
/// renumbering, and low-tier duties never need to know how many tiers exist
/// above them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
pub struct Tier(pub u32);

impl Tier {
    pub const NONE: Tier = Tier(0);
    pub const DUTY_ONLY: Tier = Tier(1);
    pub const MIN_CASCADING: Tier = Tier(2);
    pub const MAX: Tier = Tier(9);
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(untagged)]
pub enum GlobExpr {
    Any { any: Vec<GlobExpr> },
    All { all: Vec<GlobExpr> },
    Not { not: Box<GlobExpr> },
    /// A raw list of expressions acts as an implicit OR (Any)
    List(Vec<GlobExpr>),
    /// A raw string. If it starts with '@', it's a reference to a Span. Otherwise, a glob.
    Pattern(String),
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
            GlobExpr::Any { any: exprs } | GlobExpr::List(exprs) => {
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
            GlobExpr::Pattern(s) => {
                if let Some(ref_name) = s.strip_prefix('@') {
                    if stack.contains(&ref_name.to_string()) {
                        let mut cycle = stack.clone();
                        cycle.push(ref_name.to_string());
                        return Err(format!("Cyclic expression reference: {}", cycle.join(" -> ")));
                    }
                    let target = defs.get(ref_name).ok_or_else(|| format!("Undefined named expression '{}'", ref_name))?;
                    stack.push(ref_name.to_string());
                    let compiled = Self::compile_inner(target, defs, stack)?;
                    stack.pop();
                    Ok(compiled)
                } else {
                    let mut builder = globset::GlobSetBuilder::new();
                    builder.add(globset::Glob::new(s).map_err(|e| format!("Invalid glob '{}': {}", s, e))?);
                    Ok(CompiledExpr::Match(builder.build().map_err(|e| e.to_string())?))
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

/// The kind of access a `Power` confers. Only meaningful for `Power::Fs` —
/// `Power::Mic` and `Power::Speaker` carry no caps at all, since a
/// microphone is inherently capture-only and a speaker inherently
/// playback-only on every mainstream sound stack (ALSA, PipeWire): there is
/// no "write" to a mic or "read" from a speaker to represent.
#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Cap {
    Read,
    Write,
    /// Write restricted to appending at end-of-file: no truncation, no
    /// arbitrary-offset writes. Enforced at the FUSE layer (blocking
    /// truncating `setattr` and non-EOF `write` offsets) for paths where a
    /// duty holds only `Append`, not `Write`.
    Append,
    Create,
}

fn default_audio_devices() -> Vec<String> {
    vec!["sysdefault".to_string()]
}

/// A single grant of access. `Fs` grants filesystem access over a path
/// pattern (`span`) with the given cap(s), unchanged from before this enum
/// existed. `Mic`/`Speaker` grant audio capture/playback over a device
/// list, with no caps to set — the variant itself is the capability.
/// `"sysdefault"` is the conventional name for "whatever the host's
/// system-default device currently is", present under that exact name on
/// every mainstream Linux sound stack, and what an omitted `mic`/`speaker`
/// list defaults to.
///
/// Untagged on purpose: `Mic`'s only field is named `mic` and `Speaker`'s
/// `speaker`, so the YAML shape alone disambiguates every variant — no
/// internal tag, no risk of `Mic` and `Speaker` colliding despite otherwise
/// identical shapes, and `Fs` keeps its original flat `span`/`caps`/
/// `overrides` keys, so every config written before this enum existed still
/// parses unchanged.
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(untagged)]
pub enum Power {
    Fs {
        span: GlobExpr,
        caps: Vec<Cap>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        overrides: Option<String>,
    },
    Mic {
        #[serde(default = "default_audio_devices")]
        mic: Vec<String>,
    },
    Speaker {
        #[serde(default = "default_audio_devices")]
        speaker: Vec<String>,
    },
}

/// A duty definition: a Tier plus any powers specific to this duty alone
/// (on top of whatever the Tier cascades in). The tier belongs to the duty,
/// not to whatever process carries it out, so a duty's containment holds
/// everywhere it's used.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct DutyConfig {
    pub tier: Tier,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub powers: Vec<serde_saphyr::Commented<Power>>,
    // `intro` was removed: duties are entries in `RealmConfig::duties`,
    // already wrapped in `Commented<DutyConfig>`, so a description now
    // lives as a YAML comment on/above the duty's key instead of a
    // dedicated (and previously required) field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boss: Option<String>,
    /// Glob expression selecting extra host binaries (matched by bare
    /// basename) to expose in `/bin`, on top of whatever the Realm-wide
    /// `bin` and this duty's Tier(s) already contribute. See
    /// `RealmConfig::bin` for how the cascade combines.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bin: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<String>,
}

/// Declares that a mount contains more than one selectable root — e.g. a
/// Cargo workspace with several member crates, or a handful of sibling
/// checkouts a Folio might focus on one at a time. Each resolved root is
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
    // `intro` was removed: mounts are entries in `RealmConfig::mounts`,
    // already wrapped in `Commented<RealmMountConfig>`, so a description
    // now lives as a YAML comment on/above the mount's key instead of a
    // dedicated field -- same pattern `places` already used.
}

/// One Folio a Realm is willing to open: a self-contained `.rno` file at an
/// absolute host path. Folios can live anywhere; a Realm groups the ones it
/// works with by linking to them here.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct FolioRef {
    /// Absolute host path to the self-contained `.rno` Folio file.
    /// Required on purpose: a Realm must name the exact Folio file it
    /// authorizes, and must not rely on a resolver reconstructing a path
    /// from a short name.
    pub path: PathBuf,
    /// Realm duties this Folio is allowed to use.
    /// Empty means the Folio is known but not authorized to open
    /// this Realm under any duty. `roles` is the key's former name: still
    /// read, never written.
    #[serde(default, alias = "roles", skip_serializing_if = "Vec::is_empty")]
    pub duties: Vec<String>,
    // A description lives as a YAML comment on/above the Folio's key in
    // `RealmConfig::folios` (`Commented<FolioRef>`), not in a field.
}

/// A single filesystem-path component: no separators, no `.`/`..`.
/// `db::folio_path_for` uses it so a Folio name can never be joined into a
/// path outside the folios directory (`../secrets.rno`).
pub(crate) fn is_safe_path_component(s: &str) -> bool {
    !s.is_empty() && s != "." && s != ".." && !s.contains('/') && !s.contains('\\')
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct TierConfig {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub powers: Vec<serde_saphyr::Commented<Power>>,
    /// Glob expression selecting extra host binaries (matched by bare
    /// basename) to expose in `/bin` to every duty at or above this tier —
    /// unioned with the Realm-wide `bin` and whatever the duty adds itself.
    /// See `RealmConfig::bin` for how the cascade combines.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bin: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
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
    /// Keyed by virtual path. Each entry is wrapped in `Commented` so a
    /// user-written comment near the mount (e.g. `workspace: # scratch
    /// checkout` or a `#`-line above it) survives being read and rewritten
    /// by the Realm Configuration window's Visual Builder, instead of being
    /// silently dropped on the next save. See the `places` field below for
    /// the original instance of this pattern; note that surviving a save
    /// requires serializing with `CommentPosition::Above` (see
    /// `write_realm_config` / callers of `serde_saphyr::to_string`), since
    /// the default `Inline` position drops comments on non-scalar values.
    #[serde(default)]
    pub mounts: IndexMap<String, serde_saphyr::Commented<RealmMountConfig>>,
    /// Global bookmarks resolving to virtual paths (or host paths) across the VFS.
    #[serde(default, deserialize_with = "validate_places")]
    pub places: IndexMap<String, serde_saphyr::Commented<String>>,
    /// Named, reusable `GlobExpr` definitions, referenced via
    /// `GlobExpr::Ref(name)`. May reference each other; cycles are rejected
    /// at load time. Purely a config-authoring convenience.
    #[serde(default, rename = "spans", skip_serializing_if = "IndexMap::is_empty")]
    pub expressions: IndexMap<String, GlobExpr>,
    /// Tier number (2-9) -> powers cascading to every duty AT OR ABOVE that
    /// tier. Sparse: a tier number with no entry contributes nothing, no
    /// contiguity required. 0 and 1 are reserved and may not appear here.
    /// `Commented` wrapping is the same comment-preservation mechanism as
    /// `mounts` above.
    #[serde(default)]
    pub tiers: BTreeMap<u32, serde_saphyr::Commented<TierConfig>>,
    /// Duty name -> Tier/duty-specific powers. `Commented` wrapping is the
    /// same comment-preservation mechanism as `mounts` above. `roles` is the
    /// key's former name: still read, so existing Realm files load until
    /// they're updated by hand, and never written.
    #[serde(default, alias = "roles")]
    pub duties: IndexMap<String, serde_saphyr::Commented<DutyConfig>>,
    /// Folios permitted to open this Realm, keyed by local name. The first
    /// entry is the default Folio, opened when the Realm itself is opened
    /// directly. Each Folio must provide an absolute `path` to a
    /// self-contained `.rno` file. `Commented` wrapping is the same
    /// comment-preservation mechanism as `mounts` above. `sandboxes` is the
    /// key's former name: still read, so existing Realm files load until
    /// they're updated by hand, and never written.
    #[serde(default, alias = "sandboxes")]
    pub folios: IndexMap<String, serde_saphyr::Commented<FolioRef>>,
    /// Glob expression selecting which host binaries (matched by bare
    /// basename, e.g. `cc`, `git`) are exposed in a spawned process's
    /// chroot `/bin`, on top of the fixed coreutils list (`bash`, `ls`,
    /// `cat`, ...) that's always present regardless of this cascade. This
    /// is the base of the `bin` cascade — applied first, then unioned with
    /// whatever the duty's Tier(s) (`TierConfig::bin`) and the duty itself
    /// (`DutyConfig::bin`) add on top. A match at ANY level is enough to
    /// expose a binary — "everything except X" is expressed within one
    /// level's own expression (`all` + `not`), not by subtracting across
    /// levels. See `ActiveRealm::bin_is_selected`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bin: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<String>,
}

/// Resolves the on-disk path for `key` in `config.folios`.
pub fn resolve_folio_path(
    key: &str,
    config: &RealmConfig,
) -> Result<PathBuf, String> {
    let fref = &config.folios.get(key)
        .ok_or_else(|| format!("No Folio named '{}' declared in this Realm", key))?
        .0;

    if !fref.path.is_absolute() {
        return Err(format!(
            "Folio '{}' path must be an absolute host path, got '{}'",
            key, fref.path.display()
        ));
    }

    if fref.path.extension().and_then(|e| e.to_str()) != Some("rno") {
        return Err(format!(
            "Folio '{}' path must point to a `.rno` file, got '{}'",
            key, fref.path.display()
        ));
    }

    Ok(fref.path.clone())
}

/// Resolves the default Folio, which is the first declared entry.
/// Returns `Err` if `folios` is empty or resolution fails — the
/// caller treats that as "this Realm has no Folio yet" and should prompt
/// the user to create one rather than silently materializing one.
pub fn resolve_default_folio_path(
    config: &RealmConfig,
) -> Result<PathBuf, String> {
    let (key, _) = config.folios.first()
        .ok_or_else(|| "This Realm declares no Folios".to_string())?;
    resolve_folio_path(key, config)
}

/// Guards the Realm/Folio split: a Folio must never live inside the
/// realms directory. `realms_dir` is `~/.config/inforno/realms`.
pub fn folio_path_is_valid(path: &Path, realms_dir: &Path) -> bool {
    let canon_realms = std::fs::canonicalize(realms_dir).unwrap_or_else(|_| realms_dir.to_path_buf());
    let parent = path.parent().unwrap_or(path);
    let canon_parent = std::fs::canonicalize(parent).unwrap_or_else(|_| parent.to_path_buf());
    !canon_parent.starts_with(&canon_realms)
}

/// True for any key that is present, whatever its value (even empty).
fn key_present<'de, D: serde::Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    serde::de::IgnoredAny::deserialize(d).map(|_| true)
}

/// Reads only the keys whose names changed; every other key is skipped.
#[derive(Deserialize)]
struct LegacyKeyProbe {
    #[serde(default, deserialize_with = "key_present")]
    roles: bool,
    #[serde(default)]
    sandboxes: Option<IndexMap<String, FolioKeyProbe>>,
    #[serde(default)]
    folios: Option<IndexMap<String, FolioKeyProbe>>,
}

#[derive(Deserialize)]
struct FolioKeyProbe {
    #[serde(default, deserialize_with = "key_present")]
    roles: bool,
}

/// The old key names a realm2.yml still uses, each with its new name, for
/// the user to change by hand. They still load (`serde(alias)`), but
/// nothing rewrites a Realm file on its own. Empty when there are none, or
/// when the file doesn't parse (the real parse reports that).
pub fn legacy_realm_keys(yaml: &str) -> Vec<String> {
    let Ok(probe) = serde_saphyr::from_str::<LegacyKeyProbe>(yaml) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if probe.roles {
        out.push("`roles:` at the top level → `duties:`".to_string());
    }
    if probe.sandboxes.is_some() {
        out.push("`sandboxes:` at the top level → `folios:`".to_string());
    }
    for (name, folio) in probe.folios.iter().flatten().chain(probe.sandboxes.iter().flatten()) {
        if folio.roles {
            out.push(format!("`roles:` in Folio '{}' → `duties:`", name));
        }
    }
    out
}


/// A compiled `Power`, ready to be matched against relative paths (`Fs`), or
/// simply held as a resolved device grant (`Mic`/`Speaker` — there's
/// nothing to compile for these beyond cloning the device list, but they
/// share the enum so `effective_powers`/`describe_duty_capabilities` keep
/// treating all three uniformly as "things a duty holds").
#[derive(Clone)]
pub enum CompiledPower {
    Fs {
        /// Raw source, kept for display purposes (`describe_duty_capabilities`
        /// via `describe_expr`) — matching always uses the compiled `span`.
        span_source: GlobExpr,
        span: Arc<CompiledExpr>,
        caps: Vec<Cap>,
        overrides: Option<String>,
        intro: Option<Arc<str>>,
    },
    Mic {
        devices: Vec<String>,
        intro: Option<Arc<str>>,
    },
    Speaker {
        devices: Vec<String>,
        intro: Option<Arc<str>>,
    },
}

impl CompiledPower {
    fn compile(power: &Power, defs: &IndexMap<String, GlobExpr>, intro: Option<String>) -> Result<Self, String> {
        let intro: Option<Arc<str>> = intro.map(Into::into);
        Ok(match power {
            Power::Fs { span, caps, overrides } => CompiledPower::Fs {
                span_source: span.clone(),
                span: Arc::new(CompiledExpr::compile(span, defs)?),
                caps: caps.clone(),
                overrides: overrides.clone(),
                intro,
            },
            Power::Mic { mic } => CompiledPower::Mic { devices: mic.clone(), intro },
            Power::Speaker { speaker } => CompiledPower::Speaker { devices: speaker.clone(), intro },
        })
    }

    /// Only an `Fs` power can ever match a filesystem path.
    fn matches_path(&self, rel_path: &Path) -> bool {
        match self {
            CompiledPower::Fs { span, .. } => span.is_match(rel_path),
            CompiledPower::Mic { .. } | CompiledPower::Speaker { .. } => false,
        }
    }

    /// Only an `Fs` power carries caps; `Mic`/`Speaker` are the capability
    /// itself and never satisfy a filesystem cap check.
    fn has_cap(&self, cap: Cap) -> bool {
        match self {
            CompiledPower::Fs { caps, .. } => caps.contains(&cap),
            CompiledPower::Mic { .. } | CompiledPower::Speaker { .. } => false,
        }
    }

    /// Whether this power grants `requested`. `Write` implies `Append` (a
    /// full write capability is strictly more permissive than append-only),
    /// but not the reverse — holding only `Append` never satisfies a
    /// request for `Write`.
    fn grants(&self, requested: Cap) -> bool {
        self.has_cap(requested) || (requested == Cap::Append && self.has_cap(Cap::Write))
    }

    fn overrides(&self) -> Option<&str> {
        match self {
            CompiledPower::Fs { overrides, .. } => overrides.as_deref(),
            CompiledPower::Mic { .. } | CompiledPower::Speaker { .. } => None,
        }
    }

    fn intro(&self) -> Option<&Arc<str>> {
        match self {
            CompiledPower::Fs { intro, .. } | CompiledPower::Mic { intro, .. } | CompiledPower::Speaker { intro, .. } => intro.as_ref(),
        }
    }
}

#[derive(Clone)]
pub struct CompiledMount {
    pub virtual_path: String,
    pub host_path: PathBuf,
    pub read_only: bool,
}

#[derive(Clone)]
pub struct CompiledDuty {
    pub tier: Tier,
    pub boss: Option<String>,
    pub powers: Vec<CompiledPower>,
    pub bin: Option<Arc<globset::GlobSet>>,
    pub env: Option<Arc<globset::GlobSet>>,
    pub env_vars: HashMap<String, String>,
}

#[derive(Clone)]
pub struct ActiveRealm {
    pub name: String,
    pub mounts: Vec<CompiledMount>,
    pub raw_config: RealmConfig,
    pub duties: HashMap<String, CompiledDuty>,
    /// Tier number -> powers cascading from that tier upward. `BTreeMap`
    /// keeps numeric order for `range()` queries during cascade resolution.
    pub tiers: BTreeMap<u32, Vec<CompiledPower>>,
    /// Tier number -> compiled `bin` expression, mirroring `tiers` above.
    /// Sparse: a tier with no `bin` declared contributes nothing to the cascade.
    pub tier_bins: BTreeMap<u32, Arc<globset::GlobSet>>,
    pub tier_envs: BTreeMap<u32, Arc<globset::GlobSet>>,
    pub tier_env_vars: BTreeMap<u32, HashMap<String, String>>,
    /// The Realm-wide `bin` expression (from `RealmConfig::bin`), applied
    /// first in the cascade — see `bin_is_selected`.
    pub bin: Option<Arc<globset::GlobSet>>,
    pub env: Option<Arc<globset::GlobSet>>,
    pub env_vars: HashMap<String, String>,
    /// Plain-English capability descriptions per duty name, computed once at
    /// construction time. Correctness depends on `ActiveRealm` always being
    /// rebuilt fresh via `from_config` when realm2.yml changes, rather than
    /// mutated in place — if that assumption ever changes, this cache needs
    /// explicit invalidation.
    pub duty_capabilities: HashMap<String, Arc<Vec<String>>>,
}

fn compile_bin_list(bins: &[String]) -> Result<Option<Arc<globset::GlobSet>>, String> {
    if bins.is_empty() {
        return Ok(None);
    }
    let mut builder = globset::GlobSetBuilder::new();
    for item in bins {
        builder.add(globset::Glob::new(item).map_err(|e| format!("Invalid bin glob '{}': {}", item, e))?);
    }
    Ok(Some(Arc::new(builder.build().map_err(|e| e.to_string())?)))
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
            let mount_cfg = mount_cfg.0;
            mounts.push(CompiledMount {
                virtual_path: v_path,
                host_path: mount_cfg.host,
                read_only: mount_cfg.read_only,
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
        let mut tier_bins: BTreeMap<u32, Arc<globset::GlobSet>> = BTreeMap::new();
        let mut tier_envs: BTreeMap<u32, Arc<globset::GlobSet>> = BTreeMap::new();
        let mut tier_env_vars: BTreeMap<u32, HashMap<String, String>> = BTreeMap::new();
        for (&tier_num, tier_cfg) in &raw_config.tiers {
            let tier_cfg = &tier_cfg.0;
            let compiled = tier_cfg.powers
                .iter()
                .map(|p| {
                    let intro = if p.1.trim().is_empty() { None } else { Some(p.1.trim().to_string()) };
                    CompiledPower::compile(&p.0, &raw_config.expressions, intro)
                })
                .collect::<Result<Vec<_>, _>>()?;
            tiers.insert(tier_num, compiled);

            if let Some(set) = compile_bin_list(&tier_cfg.bin).map_err(|e| format!("In tier {}'s `bin`: {}", tier_num, e))? {
                tier_bins.insert(tier_num, set);
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

        // --- Duties: validate tier 0 has no powers, tier <= 9, compile powers. ---
        let mut duties: HashMap<String, CompiledDuty> = HashMap::new();
        for (dname, dcfg) in &raw_config.duties {
            let dcfg = &dcfg.0;
            if dcfg.tier.0 > Tier::MAX.0 {
                return Err(format!("Duty '{}' has tier {}, which exceeds the maximum of {}", dname, dcfg.tier.0, Tier::MAX.0));
            }
            if dcfg.tier == Tier::NONE && (!dcfg.powers.is_empty() || !dcfg.bin.is_empty()) {
                return Err(format!(
                    "Duty '{}' is at tier 0 (no access, hardcoded) but declares its own powers \
                     and/or extra binaries. Tier 0 can never receive any power or extra \
                     binary, of any kind — remove them or raise the tier.",
                    dname
                ));
            }

            let compiled_powers = dcfg
                .powers
                .iter()
                .map(|p| {
                    let intro = if p.1.trim().is_empty() { None } else { Some(p.1.trim().to_string()) };
                    CompiledPower::compile(&p.0, &raw_config.expressions, intro)
                })
                .collect::<Result<Vec<_>, _>>()?;

            let compiled_bin = compile_bin_list(&dcfg.bin)
                .map_err(|e| format!("In duty '{}'s `bin`: {}", dname, e))?;

            let (compiled_env, env_vars) = compile_env_list(&dcfg.env)
                .map_err(|e| format!("In duty '{}'s `env`: {}", dname, e))?;

            duties.insert(
                dname.clone(),
                CompiledDuty {
                    tier: dcfg.tier,
                    boss: dcfg.boss.clone(),
                    powers: compiled_powers,
                    bin: compiled_bin,
                    env: compiled_env,
                    env_vars,
                },
            );
        }

        // --- Folios: validate required absolute .rno paths and allowed duty names. ---
        if let Some((fname, fref)) = raw_config.folios.first() {
            let fref = &fref.0;
            if !fref.path.exists() {
                return Err(format!("Default Folio '{}' points to a non-existent path: {}", fname, fref.path.display()));
            }
            if fref.duties.is_empty() {
                return Err(format!("The default Folio '{}' must authorize at least one Realm duty; otherwise the Realm cannot be opened through it.", fname));
            }
        }

        for (fname, fref) in &raw_config.folios {
            let fref = &fref.0;
            if !fref.path.is_absolute() {
                return Err(format!(
                    "Folio '{}' path must be absolute, got '{}'",
                    fname, fref.path.display()
                ));
            }

            if fref.path.extension().and_then(|e| e.to_str()) != Some("rno") {
                return Err(format!(
                    "Folio '{}' path must point to a `.rno` file, got '{}'",
                    fname, fref.path.display()
                ));
            }

            for duty_name in &fref.duties {
                if !duties.contains_key(duty_name) {
                    return Err(format!(
                        "Folio '{}' lists duty '{}', but that duty is not defined in this Realm",
                        fname, duty_name
                    ));
                }
            }
        }

        let bin = compile_bin_list(&raw_config.bin)
            .map_err(|e| format!("In the Realm's top-level `bin`: {}", e))?;

        let (env, env_vars) = compile_env_list(&raw_config.env)
            .map_err(|e| format!("In the Realm's top-level `env`: {}", e))?;

        let mut realm = Self {
            name,
            mounts,
            raw_config,
            duties,
            tiers,
            tier_bins,
            tier_envs,
            tier_env_vars,
            bin,
            env,
            env_vars,
            duty_capabilities: HashMap::new(),
        };

        let duty_names: Vec<String> = realm.duties.keys().cloned().collect();
        realm.duty_capabilities = duty_names
            .into_iter()
            .map(|n| {
                let caps = realm.describe_duty_capabilities(&n);
                (n, Arc::new(caps))
            })
            .collect();

        Ok(realm)
    }

    /// All powers effectively held by `duty`: every power defined at any
    /// tier `2..=duty.tier` (cascading; gaps in `tiers` simply contribute
    /// nothing — no contiguity is required), plus the duty's own `powers`.
    /// Tier 0 never reaches here in practice (callers short-circuit it);
    /// tier 1 correctly yields only the duty's own powers, since the range
    /// `2..=1` is empty.
    fn effective_powers<'a>(&'a self, duty: &'a CompiledDuty) -> Vec<&'a CompiledPower> {
        let mut out: Vec<&CompiledPower> = Vec::new();
        if duty.tier.0 >= Tier::MIN_CASCADING.0 {
            for (_, powers) in self.tiers.range(Tier::MIN_CASCADING.0..=duty.tier.0) {
                out.extend(powers.iter());
            }
        }
        out.extend(duty.powers.iter());
        out
    }

    /// Whether `candidate` (a bare binary basename, e.g. `Path::new("cc")`)
    /// is selected by `duty_name`'s effective `bin` expression. Resolution
    /// mirrors `effective_powers`: the Realm-wide `bin` (if any) is checked
    /// first, then every cascading tier's `bin` from `2..=duty.tier`, then
    /// the duty's own `bin` — a match at ANY level is sufficient (a union
    /// across levels). Excluding names ("everything except X") is
    /// expressed within a single level's own boolean expression
    /// (`all` + `not`), not by subtracting across levels. Tier 0 (`NONE`)
    /// is "provably zero access, of any kind" by design (see `Tier`), so it
    /// short-circuits to `false` even for the Realm-wide `bin`.
    pub fn bin_is_selected(&self, duty_name: &str, candidate: &Path) -> bool {
        let Some(duty) = self.duties.get(duty_name) else { return false };
        if duty.tier == Tier::NONE {
            return false;
        }

        if let Some(expr) = &self.bin {
            if expr.is_match(candidate) {
                return true;
            }
        }

        if duty.tier.0 >= Tier::MIN_CASCADING.0 {
            for (_, expr) in self.tier_bins.range(Tier::MIN_CASCADING.0..=duty.tier.0) {
                if expr.is_match(candidate) {
                    return true;
                }
            }
        }

        if let Some(expr) = &duty.bin {
            if expr.is_match(candidate) {
                return true;
            }
        }

        false
    }

    /// Whether host variable `candidate` passes through to `duty_name`'s
    /// processes. Same cascade as `bin_is_selected`.
    pub fn env_is_selected(&self, duty_name: &str, candidate: &str) -> bool {
        let Some(duty) = self.duties.get(duty_name) else { return false };
        if duty.tier == Tier::NONE {
            return false;
        }

        let cand_path = Path::new(candidate);

        if let Some(set) = &self.env {
            if set.is_match(cand_path) {
                return true;
            }
        }

        if duty.tier.0 >= Tier::MIN_CASCADING.0 {
            for (_, set) in self.tier_envs.range(Tier::MIN_CASCADING.0..=duty.tier.0) {
                if set.is_match(cand_path) {
                    return true;
                }
            }
        }

        if let Some(set) = &duty.env {
            if set.is_match(cand_path) {
                return true;
            }
        }

        false
    }

    pub fn allowed_envs(&self, duty_name: &str) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for (k, v) in std::env::vars() {
            if self.env_is_selected(duty_name, &k) {
                out.push((k, v));
            }
        }

        let Some(duty) = self.duties.get(duty_name) else { return out };

        if duty.tier == Tier::NONE {
            return out;
        }

        // Apply Realm-wide custom env vars
        for (k, v) in &self.env_vars {
            out.push((k.clone(), v.clone()));
        }

        // Apply tier custom env vars
        if duty.tier.0 >= Tier::MIN_CASCADING.0 {
            for (_, vars) in self.tier_env_vars.range(Tier::MIN_CASCADING.0..=duty.tier.0) {
                for (k, v) in vars {
                    out.push((k.clone(), v.clone()));
                }
            }
        }

        // Apply duty custom env vars
        for (k, v) in &duty.env_vars {
            out.push((k.clone(), v.clone()));
        }

        out
    }

    /// Scans `search_dirs` (host directories, e.g. `/usr/bin`,
    /// `/usr/local/bin`) for entries whose basename is selected by
    /// `duty_name`'s effective `bin` expression (`bin_is_selected`),
    /// returning their absolute host paths. Used to decide which extra
    /// binaries get bind-mounted into a spawned process's chroot `/bin`,
    /// on top of the fixed coreutils list `realm_spawn.rs` always mounts.
    /// A basename already seen in an earlier dir is skipped, so distros
    /// where e.g. `/bin` symlinks to `/usr/bin` don't produce duplicate
    /// mount attempts for the same name.
    pub fn allowed_binaries(&self, duty_name: &str, search_dirs: &[PathBuf]) -> Vec<PathBuf> {
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
                if self.bin_is_selected(duty_name, Path::new(name)) {
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
    /// duty or cascaded in from a tier. It exists so Realm definitions
    /// (e.g. under `.inforno/`) and other sensitive dotfiles stay invisible
    /// by default, unless a config author deliberately carves out an
    /// exception by name (tier-level exceptions are intentionally allowed —
    /// build tooling like Cargo relies heavily on dotfiles such as
    /// `target/.fingerprint/`, and gating that per-duty only would make
    /// tiers like the `**/target/**` grant above unworkable).
    fn is_builtin_dotfile_path(rel_path: &Path) -> bool {
        rel_path
            .components()
            .any(|c| c.as_os_str().to_str().map(|s| s.starts_with('.')).unwrap_or(false))
    }

    /// True if `duty_name`'s effective powers — its own `powers` plus
    /// whatever cascades in from its tier — include one overriding
    /// "dotfiles" for this path. Tier-level overrides are allowed
    /// deliberately: build tooling (Cargo, etc.) writes constantly to
    /// dotfiles/dot-directories under paths like `target/`, and requiring
    /// every duty to redeclare that exception itself, rather than
    /// inheriting it from a shared tier grant (e.g. the `**/target/**`
    /// power), would make tiers largely useless for this case.
    fn dotfile_override_applies(&self, rel_path: &Path, duty_name: &str) -> bool {
        let Some(duty) = self.duties.get(duty_name) else { return false };
        self.effective_powers(duty).iter().any(|p| {
            p.overrides() == Some("dotfiles")
                && p.matches_path(rel_path)
        })
    }

    /// Single source of truth for "is this path hidden from `duty_name`" —
    /// used by both visibility checks (lookup/readdir) and creation checks,
    /// so there is no blind-create gap where a hidden path can still be
    /// created just because it can't be seen first.
    pub fn is_path_hidden(&self, virtual_path: &Path, duty_name: &str) -> bool {
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
                    return !self.dotfile_override_applies(rel_path, duty_name);
                }

                return false;
            }
        }
        false // Outside all mounts: not this function's concern; secure_resolve_path returns None anyway.
    }

    /// Whether `duty_name` is actually defined in this Realm's `duties:` map.
    /// Callers that assume a fixed duty (e.g. the GUI's own "gui" duty) use
    /// this to treat an undefined duty as zero access, rather than letting
    /// `secure_resolve_path`'s visibility-only check silently let paths
    /// through for a duty the Realm has never heard of.
    pub fn has_duty(&self, duty_name: &str) -> bool {
        self.duties.contains_key(duty_name)
    }

    pub fn secure_resolve_path(&self, virtual_path: &Path, duty_name: &str) -> Option<PathBuf> {
        let path_str = virtual_path.to_str()?;
        for mount in &self.mounts {
            if path_str.starts_with(&mount.virtual_path) {
                let relative = path_str
                    .strip_prefix(&mount.virtual_path)
                    .unwrap_or("")
                    .trim_start_matches('/');
                let host_target = mount.host_path.join(relative);

                if self.is_path_hidden(virtual_path, duty_name) { return None; }

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

    /// The full access check for `duty_name`: hard restrictions (hidden,
    /// mount read-only) MUST pass, AND the duty must hold a power granting
    /// the requested `cap` for this path (cascaded in from its tier or its
    /// own). A duty the Realm doesn't define is denied everything, without
    /// needing any power lookup.
    pub fn can_access(&self, virtual_path: &Path, cap: Cap, duty_name: &str) -> Result<(), String> {
        match cap {
            Cap::Create | Cap::Write | Cap::Append => {
                if self.is_path_hidden(virtual_path, duty_name) {
                    return Err("Path is hidden and cannot be modified".to_string());
                }
                if self.is_path_read_only(virtual_path) {
                    return Err("Path is read-only".to_string());
                }
            }
            Cap::Read => {
                if self.is_path_hidden(virtual_path, duty_name) {
                    return Err("Path is hidden".to_string());
                }
            }
        }

        let Some(duty) = self.duties.get(duty_name) else {
            return Err(format!("Duty '{}' is not defined in this Realm; no filesystem access is possible", duty_name));
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

        let granted = self.effective_powers(duty)
            .iter()
            .any(|p| p.grants(cap) && p.matches_path(rel_path));

        if granted {
            Ok(())
        } else {
            Err(format!("Duty '{}' does not grant {:?} access to '{}'", duty_name, cap, virtual_path.display()))
        }
    }

    /// Renders a duty's actual, resolved capabilities as plain-English,
    /// positively-framed statements — no mention of tier numbers or any
    /// other engine-internal mechanism, only concrete outcomes, plus the
    /// comment authored on each power. Intended to be spliced into the
    /// system message of whoever carries out the duty, so it knows up front
    /// exactly what it can do (and how it should prefer to do it) without
    /// wasting turns retrying blocked operations. Computed once and cached
    /// in `duty_capabilities` at construction time — call that field
    /// directly rather than this method after construction.
    fn describe_duty_capabilities(&self, duty_name: &str) -> Vec<String> {
        let Some(duty) = self.duties.get(duty_name) else {
            return vec!["This duty does not exist in this Realm and has no filesystem access.".to_string()];
        };

        if duty.tier == Tier::NONE {
            return vec!["This duty has no filesystem access of any kind. Do not attempt to read, write, or create any files — such attempts will always be denied.".to_string()];
        }

        let mut lines = Vec::new();
        for power in self.effective_powers(duty) {
            let mut line = match power {
                CompiledPower::Fs { caps, span_source, .. } => {
                    let verbs: Vec<&str> = caps
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

                    format!(
                        "You may {} files {}.",
                        verbs.join(", "),
                        describe_expr(span_source, &self.raw_config.expressions)
                    )
                }
                CompiledPower::Mic { devices, .. } => {
                    format!("You may listen through the microphone (device: {}).", devices.join(", "))
                }
                CompiledPower::Speaker { devices, .. } => {
                    format!("You may speak through the speaker (device: {}).", devices.join(", "))
                }
            };
            if let Some(intro) = power.intro() {
                line.push(' ');
                line.push_str(intro);
            }
            lines.push(line);
        }

        if lines.is_empty() {
            lines.push("This duty has no filesystem access of any kind. Do not attempt to read, write, or create any files — such attempts will always be denied.".to_string());
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
        GlobExpr::Pattern(s) => {
            if let Some(ref_name) = s.strip_prefix('@') {
                match defs.get(ref_name) {
                    Some(target) => describe_expr(target, defs),
                    None => format!("matching an undefined pattern '{}'", ref_name),
                }
            } else {
                format!("matching `{}`", s)
            }
        }
        GlobExpr::List(exprs) | GlobExpr::Any { any: exprs } => {
            let parts: Vec<String> = exprs.iter().map(|e| describe_expr(e, defs)).collect();
            if parts.len() == 1 {
                parts[0].clone()
            } else {
                format!("any of ({})", parts.join("; or "))
            }
        }
        GlobExpr::All { all: exprs } => {
            let parts: Vec<String> = exprs.iter().map(|e| describe_expr(e, defs)).collect();
            format!("all of ({})", parts.join("; and "))
        }
        GlobExpr::Not { not: inner } => format!("anything except {}", describe_expr(inner, defs)),
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
    _duty_name: &str,
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
