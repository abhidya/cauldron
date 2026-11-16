//! **Cauldrons live inside environments**.
//!
//! An ENVIRONMENT is a runtime: one supervisor, its services, its thread store
//! and account pool (`data/<env>/`, see [`crate::paths`]). A CAULDRON is a
//! directory of the user's data INSIDE an environment — its soup, its
//! clusters, bucket, dumps, ledger, settings and ripple runs:
//!
//! ```text
//!   data/<env>/cauldrons/<cauldron>/
//!       cauldron.toml        name, created_at, enabled, budget, seed
//!       soup/                its OWN soup: the spine `Cauldron` dir (spine.db + ws.db)
//!       events/log/          its own commit stream
//!       tui/                 bucket.json, dumps.jsonl, clusters.json, settings.toml, seed/, ...
//!       ripples/runs/        this cauldron's ripple runs
//!       ripples/blobs/       and their blobs
//! ```
//!
//! **THE IMPLICIT LEGACY CAULDRON.** Before cauldrons existed, the same
//! data lived directly in the environment: `data/<env>/tui/soup/`,
//! `data/<env>/soup/cauldron/`, `data/<env>/ripples/runs/`. An environment
//! holding ANY of those presents one implicit cauldron NAMED AFTER THE
//! ENVIRONMENT whose paths ARE those legacy locations. It is computed here and
//! NEVER WRITTEN: listing cauldrons creates no directory, writes no
//! `cauldron.toml`, moves no byte. The user's existing environments keep
//! working exactly as they did, each as one cauldron of the same name.
//! Creating a second cauldron in a legacy env makes `cauldrons/<new>/`; the
//! implicit one stays implicit beside it.
//!
//! **This module never writes.** Every function here computes a path or reads
//! a directory listing. Creating, renaming and removing cauldrons are the
//! CLI's and use these paths.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::paths::{Environment, SHARED_NAME};

/// `data/<env>/cauldrons` — the directory under which every explicit cauldron
/// of an environment lives. Spelled once.
pub const CAULDRONS_DIR: &str = "cauldrons";

/// A cauldron's own manifest, `cauldrons/<name>/cauldron.toml`.
pub const CAULDRON_MANIFEST: &str = "cauldron.toml";

/// The variable the CLI exports for `--cauldron <name>`, the pair of
/// `CAULDRON_ENV`, so a face it execs opens the same cauldron.
pub const CAULDRON_VAR: &str = "CAULDRON_CAULDRON";

/// **Names a cauldron may not have** (`cauldron` and `shared` are
/// reserved).
///
/// * `cauldron` — `data/<env>/cauldron/` is the SUPERVISOR's own directory
///   ([`Environment::supervisor_data`]) and a reserved service name; a cauldron
///   called `cauldron` would read, in every message and path, as the
///   supervisor's.
/// * `shared` — `data/shared/` is the root-wide, non-environment tree
///   ([`SHARED_NAME`]); the same name as a cauldron invites exactly the
///   confusion the env reservation exists to prevent.
///
/// The `.removed/` graveyard under `cauldrons/` (`remove`) needs no entry:
/// the grammar's first-character rule already refuses a leading `.`.
pub const RESERVED_CAULDRON_NAMES: &[&str] = &["cauldron", SHARED_NAME];

/// **The cauldron name grammar.** The same grammar as an environment name
/// (`[a-z0-9_-]{1,32}`, first character `[a-z0-9]`), plus
/// [`RESERVED_CAULDRON_NAMES`]. Nothing is guessed or sanitised.
pub fn validate_cauldron_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 32
        && name.chars().next().map(|c| c.is_ascii_lowercase() || c.is_ascii_digit()) == Some(true)
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if !ok {
        bail!(
            "REFUSED: cauldron name `{name}` is not usable. A cauldron name is 1-32 characters of \
             [a-z0-9_-], starting with [a-z0-9]. Nothing is guessed or sanitised, because a \
             silently rewritten name is a silently wrong directory."
        );
    }
    if RESERVED_CAULDRON_NAMES.contains(&name) {
        bail!(
            "REFUSED: `{name}` is a RESERVED name and cannot be a cauldron. `data/<env>/cauldron/` \
             is the supervisor's own directory and `data/shared/` is the root-wide shared tree. \
             Reserved: {}. Pick any other name.",
            RESERVED_CAULDRON_NAMES.join(", ")
        );
    }
    Ok(())
}

/// Which of the two shapes a cauldron has on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CauldronKind {
    /// `data/<env>/cauldrons/<name>/`.
    Dir,
    /// The implicit legacy cauldron: named after the environment, its
    /// paths are the pre-cauldron locations directly in `data/<env>/`.
    Legacy,
}

/// **One cauldron of one environment** — a set of paths, never a handle on
/// open state. Constructing one writes nothing and does not require it to
/// exist; [`Environment::cauldrons`] is what says which ones do.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Cauldron {
    /// The cauldron's name. For [`CauldronKind::Legacy`] it is the env's name.
    pub name: String,
    /// The environment it lives in.
    pub env: String,
    pub kind: CauldronKind,
    /// The environment's data root, `<root>/data/<env>`.
    pub data_root: PathBuf,
}

impl Cauldron {
    pub fn is_legacy(&self) -> bool {
        self.kind == CauldronKind::Legacy
    }

    /// `cauldrons/<name>/` for a [`CauldronKind::Dir`] cauldron; `None` for the
    /// legacy one, which has no directory of its own (its data is spread over
    /// three legacy locations in the env's data root).
    pub fn dir(&self) -> Option<PathBuf> {
        match self.kind {
            CauldronKind::Dir => Some(self.data_root.join(CAULDRONS_DIR).join(&self.name)),
            CauldronKind::Legacy => None,
        }
    }

    /// `cauldrons/<name>/cauldron.toml`; `None` for the legacy cauldron, which
    /// has no manifest and is never given one by a read.
    pub fn manifest(&self) -> Option<PathBuf> {
        self.dir().map(|d| d.join(CAULDRON_MANIFEST))
    }

    /// The face's per-cauldron files (bucket, dumps, clusters, layout,
    /// settings, tickets, seed): `cauldrons/<name>/tui/`, or the legacy
    /// `data/<env>/tui/soup/`.
    pub fn tui(&self) -> PathBuf {
        match self.kind {
            CauldronKind::Dir => self.data_root.join(CAULDRONS_DIR).join(&self.name).join("tui"),
            CauldronKind::Legacy => self.data_root.join("tui").join("soup"),
        }
    }

    /// The dump log the ripples engine tails: `<tui>/dumps.jsonl`.
    pub fn dumps(&self) -> PathBuf {
        self.tui().join("dumps.jsonl")
    }

    /// The per-cauldron face settings (including the `cauldron.accounts.*`
    /// toggles): `<tui>/settings.toml`.
    pub fn settings(&self) -> PathBuf {
        self.tui().join("settings.toml")
    }

    /// The spine `Cauldron` directory (`spine.db` + one pool file per grammar):
    /// `cauldrons/<name>/soup/`, or the legacy `data/<env>/soup/cauldron/`.
    pub fn soup(&self) -> PathBuf {
        match self.kind {
            CauldronKind::Dir => self.data_root.join(CAULDRONS_DIR).join(&self.name).join("soup"),
            CauldronKind::Legacy => self.data_root.join("soup").join("cauldron"),
        }
    }

    /// **This cauldron's own commit stream** (one soup per
    /// cauldron, so one stream per cauldron): `cauldrons/<name>/events/`, or for
    /// the legacy cauldron the env's `data/<env>/events/` (whose `log/` is read
    /// as that cauldron's stream; the bus daemon's own index and cursors
    /// also live there, as today).
    pub fn events(&self) -> PathBuf {
        match self.kind {
            CauldronKind::Dir => self.data_root.join(CAULDRONS_DIR).join(&self.name).join("events"),
            CauldronKind::Legacy => self.data_root.join("events"),
        }
    }

    /// `<events>/log` — the `<seq>.jsonl` segments of the commit stream.
    pub fn events_log(&self) -> PathBuf {
        self.events().join("log")
    }

    /// This cauldron's ripple DATA (never the engine's software, which stays
    /// per env at `data/<env>/ripples/{configs,active.json,bus}`):
    /// `cauldrons/<name>/ripples/`, or the legacy `data/<env>/ripples/`.
    pub fn ripples(&self) -> PathBuf {
        match self.kind {
            CauldronKind::Dir => self.data_root.join(CAULDRONS_DIR).join(&self.name).join("ripples"),
            CauldronKind::Legacy => self.data_root.join("ripples"),
        }
    }

    /// `<ripples>/runs` — one directory per ripple run.
    pub fn ripple_runs(&self) -> PathBuf {
        self.ripples().join("runs")
    }

    /// `<ripples>/blobs` — the runs' content-addressed blobs.
    pub fn ripple_blobs(&self) -> PathBuf {
        self.ripples().join("blobs")
    }

    /// **The per-cauldron open lock**, `<tui>/open.lock`: a face holds it
    /// SHARED while the cauldron is open; the CLI takes it EXCLUSIVE for
    /// `rename` and `remove`. Returns the PATH ONLY and never creates it: the
    /// taker creates it lazily, on open. For the legacy cauldron this is
    /// `data/<env>/tui/soup/open.lock`, beside the face's existing lock files.
    pub fn open_lock(&self) -> PathBuf {
        self.tui().join("open.lock")
    }

    /// **`cauldron.toml`, read.** Writes nothing. The legacy cauldron has no
    /// manifest and reads as the defaults (named after its env, enabled, no
    /// seed); so does a `Dir` cauldron whose `cauldron.toml` is absent. A
    /// manifest that exists but does not parse is an error, never defaults.
    pub fn meta(&self) -> Result<CauldronMeta> {
        let defaults = CauldronMeta { name: Some(self.name.clone()), ..CauldronMeta::default() };
        let Some(path) = self.manifest() else { return Ok(defaults) };
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(defaults),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    /// **Whether this cauldron holds a SEED** (`cauldron.toml` `seed` is
    /// set, e.g. `"cauldron-build"` for the curated demo). Writes nothing. A
    /// seeded cauldron is read-only demo data: nothing is dumped into it, its
    /// Ledger is its seed, and the ripples runner has nothing to run there.
    /// The legacy cauldron has no manifest and so is never seeded by this
    /// test (`soup::demo::is_demo` adds the one legacy case, an old
    /// `cauldron-build` ENV). A manifest that does not parse is an error.
    pub fn is_seeded(&self) -> Result<bool> {
        Ok(self.meta()?.seed.is_some())
    }

    /// **Renaming the LEGACY cauldron is REFUSED in V1** (even as a
    /// display-only rename): any display name would have to be written
    /// somewhere in an existing environment, and the rule is zero bytes
    /// written for an existing env. A `Dir` cauldron is renamable (the CLI checks
    /// the open lock and a held tranche on top of this).
    pub fn check_renamable(&self) -> Result<()> {
        if self.is_legacy() {
            bail!(
                "REFUSED: `{}` is the implicit legacy cauldron of environment `{}`: its data lives \
                 directly in the environment's data root and renaming it would write there. \
                 Create a new cauldron instead; the legacy one keeps its name.",
                self.name,
                self.env
            );
        }
        Ok(())
    }

    /// For a listing: `name` or `name (legacy)`.
    pub fn display_name(&self) -> String {
        match self.kind {
            CauldronKind::Dir => self.name.clone(),
            CauldronKind::Legacy => format!("{} (legacy)", self.name),
        }
    }
}

/// **`cauldron.toml`**. Every field optional on read; `enabled`
/// defaults to `true`. `budget` is carried opaquely (its shape is defined elsewhere).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct CauldronMeta {
    pub name: Option<String>,
    pub created_at: Option<String>,
    /// The "Cauldron enabled" setting.
    pub enabled: bool,
    pub budget: Option<toml::Value>,
    /// `Some("cauldron-build")` for the demo; absent for the user's own.
    pub seed: Option<String>,
}

impl Default for CauldronMeta {
    fn default() -> Self {
        Self { name: None, created_at: None, enabled: true, budget: None, seed: None }
    }
}

impl CauldronMeta {
    /// The TOML text a writer (`create`) puts in `cauldron.toml`.
    pub fn to_toml(&self) -> Result<String> {
        toml::to_string(self).context("rendering cauldron.toml")
    }
}

/// Where a cauldron selection came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum CauldronSource {
    /// `cauldron --cauldron <name>` / `-c <name>`.
    Flag,
    /// `$CAULDRON_CAULDRON`.
    Var,
    /// Not given: the cauldron the face last opened in this env
    /// (`data/<env>/tui/last_cauldron.json`, read by the caller).
    Remembered,
    /// Not given: the environment's only cauldron.
    Only,
    /// Not given, several, nothing remembered, [`Fallback::First`]: the first
    /// in listing order (the legacy cauldron, else the first by name).
    First,
}

/// What [`Environment::select_cauldron`] does when nothing names a cauldron and
/// the env has several.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fallback {
    /// Refuse and ask for `--cauldron` (the CLI: `ctl`, `rename`, `remove`).
    Strict,
    /// Take the first in listing order (the face, which must open something).
    First,
}

impl CauldronSource {
    pub fn as_str(self) -> &'static str {
        match self {
            CauldronSource::Flag => "--cauldron",
            CauldronSource::Var => "$CAULDRON_CAULDRON",
            CauldronSource::Remembered => "the last cauldron opened in this env",
            CauldronSource::Only => "the env's only cauldron",
            CauldronSource::First => "the env's first cauldron",
        }
    }
}

/// `--cauldron <name>`, once per process — the pair of `paths::set_env_flag`.
static CAULDRON_FLAG: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Record `--cauldron <name>` for this process. Validated at the flag, so a bad
/// or reserved name is refused before any verb runs. A second call is refused.
///
/// The grammar check is the only one possible here: whether the cauldron
/// EXISTS depends on the environment, which may not be resolved yet.
pub fn set_cauldron_flag(name: &str) -> Result<()> {
    validate_selected_name(name, CauldronSource::Flag)?;
    CAULDRON_FLAG
        .set(name.to_string())
        .map_err(|_| anyhow::anyhow!("`--cauldron` was given twice; give it once"))
}

/// The cauldron NAME this process was asked for, if any: `--cauldron`, else
/// `$CAULDRON_CAULDRON`. Validated; reads only.
pub fn requested_cauldron() -> Result<Option<(String, CauldronSource)>> {
    if let Some(f) = CAULDRON_FLAG.get() {
        return Ok(Some((f.clone(), CauldronSource::Flag)));
    }
    match std::env::var(CAULDRON_VAR) {
        Ok(v) if !v.trim().is_empty() => {
            validate_selected_name(&v, CauldronSource::Var)?;
            Ok(Some((v, CauldronSource::Var)))
        }
        _ => Ok(None),
    }
}

/// A SELECTED name may also be a legacy cauldron's, which is the env's name and
/// is not bound by the reservation (an env may be called `cauldron`). So only
/// the character grammar is applied at the flag; the reservation applies when
/// a cauldron DIRECTORY is named ([`Environment::cauldron_named`]).
fn validate_selected_name(name: &str, source: CauldronSource) -> Result<()> {
    match validate_cauldron_name(name) {
        Ok(()) => Ok(()),
        Err(_) if RESERVED_CAULDRON_NAMES.contains(&name) => Ok(()),
        Err(e) => Err(e).with_context(|| format!("the cauldron name came from {}", source.as_str())),
    }
}

/// The legacy locations whose presence makes an env present an implicit
/// cauldron, relative to `data/<env>/`.
pub const LEGACY_MARKERS: &[&[&str]] = &[&["tui", "soup"], &["soup", "cauldron"], &["ripples", "runs"]];

fn is_dir(p: &Path) -> bool {
    // `metadata` follows a symlink, as every other reader of these paths does.
    std::fs::metadata(p).map(|m| m.is_dir()).unwrap_or(false)
}

impl Environment {
    /// `data/<env>/cauldrons` — computed, never created here.
    pub fn cauldrons_dir(&self) -> PathBuf {
        self.data_root.join(CAULDRONS_DIR)
    }

    /// Does this environment hold pre-cauldron data? True when any of
    /// `tui/soup/`, `soup/cauldron/`, `ripples/runs/` is a directory. Reads only.
    pub fn has_legacy_cauldron(&self) -> bool {
        LEGACY_MARKERS.iter().any(|parts| {
            let mut p = self.data_root.clone();
            for c in *parts {
                p.push(c);
            }
            is_dir(&p)
        })
    }

    /// The implicit legacy cauldron's paths, whether or not the legacy data
    /// exists. Use [`Environment::has_legacy_cauldron`] to ask whether it does.
    pub fn legacy_cauldron(&self) -> Cauldron {
        Cauldron {
            name: self.name.clone(),
            env: self.name.clone(),
            kind: CauldronKind::Legacy,
            data_root: self.data_root.clone(),
        }
    }

    /// **Every cauldron of this environment, WRITING NOTHING.**
    ///
    /// The implicit legacy cauldron first (if the env holds legacy data), then
    /// each subdirectory of `cauldrons/` whose name passes
    /// [`validate_cauldron_name`], sorted. Anything else under `cauldrons/`
    /// (`.removed/`, a stray file, an unusable name) is skipped. No
    /// `cauldrons/` directory and no legacy data is an EMPTY list — the
    /// first-run state — not an error.
    ///
    /// Refused only when `cauldrons/<env>/` exists BESIDE legacy data: two
    /// cauldrons would carry the env's name. Nothing this code writes makes
    /// that shape ([`Environment::cauldron_named`] hands back the legacy
    /// cauldron for that name), so it is a hand-made collision, and silently
    /// hiding either half would hide the user's data.
    pub fn cauldrons(&self) -> Result<Vec<Cauldron>> {
        let mut out = Vec::new();
        let legacy = self.has_legacy_cauldron();
        if legacy {
            out.push(self.legacy_cauldron());
        }
        let dir = self.cauldrons_dir();
        let rd = match std::fs::read_dir(&dir) {
            Ok(rd) => Some(rd),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e).with_context(|| format!("listing {}", dir.display())),
        };
        let mut named = Vec::new();
        for e in rd.into_iter().flatten().flatten() {
            if !is_dir(&e.path()) {
                continue;
            }
            let Some(name) = e.file_name().to_str().map(str::to_string) else { continue };
            if validate_cauldron_name(&name).is_err() {
                continue;
            }
            if legacy && name == self.name {
                bail!(
                    "REFUSED: environment `{env}` holds legacy cauldron data (its implicit cauldron \
                     is named `{env}`) AND a directory {} of the same name. Two cauldrons cannot \
                     share a name; neither is hidden. Rename the directory.",
                    e.path().display(),
                    env = self.name
                );
            }
            named.push(Cauldron {
                name,
                env: self.name.clone(),
                kind: CauldronKind::Dir,
                data_root: self.data_root.clone(),
            });
        }
        named.sort_by(|a, b| a.name.cmp(&b.name));
        out.extend(named);
        Ok(out)
    }

    /// **The cauldron a name means in this env, existing or not** — for a verb
    /// about to create, open or look one up. Writes nothing.
    ///
    /// The env's own name, when the env holds legacy data, is the implicit
    /// legacy cauldron (so `create <env>` in a legacy env finds it already
    /// there). Every other name is `cauldrons/<name>/` and must pass
    /// [`validate_cauldron_name`].
    pub fn cauldron_named(&self, name: &str) -> Result<Cauldron> {
        if name == self.name && self.has_legacy_cauldron() {
            return Ok(self.legacy_cauldron());
        }
        validate_cauldron_name(name)?;
        let c = Cauldron {
            name: name.to_string(),
            env: self.name.clone(),
            kind: CauldronKind::Dir,
            data_root: self.data_root.clone(),
        };
        if let Some(d) = c.dir() {
            crate::guard::require_inside(&self.data_root, &d)?;
        }
        Ok(c)
    }

    /// **The EXISTING cauldron of this env with that name**, or `None`.
    pub fn find_cauldron(&self, name: &str) -> Result<Option<Cauldron>> {
        Ok(self.cauldrons()?.into_iter().find(|c| c.name == name))
    }

    /// `data/<env>/tui/last_cauldron.json` — where the face remembers the last
    /// cauldron opened in this env (UI memory, the same class
    /// as `last_env.json`). The path only, never created here; the face
    /// owns its format and writes.
    pub fn last_cauldron_file(&self) -> PathBuf {
        self.data_root.join("tui").join("last_cauldron.json")
    }

    /// **THE ONE SELECTION RULE** for the CLI and the face.
    /// Writes nothing. In order:
    ///
    /// 1. `requested` (`--cauldron` / `$CAULDRON_CAULDRON`, from
    ///    [`requested_cauldron`]) — it must EXIST, else an error naming it;
    /// 2. `remembered` (the face's `last_cauldron.json` name) if it still
    ///    exists — a stale name is ignored, never an error;
    /// 3. the env's ONLY cauldron;
    /// 4. several: [`Fallback::Strict`] refuses ("say which with
    ///    `--cauldron`"), [`Fallback::First`] takes the first listed.
    ///
    /// `Ok(None)` means the env has NO cauldron (and none was requested): the
    /// first-run state. The CLI turns that into its own message.
    pub fn select_cauldron(
        &self,
        requested: Option<(String, CauldronSource)>,
        remembered: Option<&str>,
        fallback: Fallback,
    ) -> Result<Option<(Cauldron, CauldronSource)>> {
        let all = self.cauldrons()?;
        if let Some((name, source)) = requested {
            return match all.into_iter().find(|c| c.name == name) {
                Some(c) => Ok(Some((c, source))),
                None => bail!(
                    "no cauldron `{name}` in environment `{}` (from {}). `cauldron list --env {}` \
                     shows its cauldrons.",
                    self.name,
                    source.as_str(),
                    self.name
                ),
            };
        }
        if let Some(r) = remembered {
            if let Some(c) = all.iter().find(|c| c.name == r) {
                return Ok(Some((c.clone(), CauldronSource::Remembered)));
            }
        }
        match (all.len(), fallback) {
            (0, _) => Ok(None),
            (1, _) => Ok(Some((all.into_iter().next().expect("len 1"), CauldronSource::Only))),
            (_, Fallback::First) => Ok(Some((all.into_iter().next().expect("non-empty"), CauldronSource::First))),
            (n, Fallback::Strict) => bail!(
                "environment `{}` has {n} cauldrons ({}); say which with `--cauldron <name>`.",
                self.name,
                all.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join(", ")
            ),
        }
    }

    /// [`Environment::select_cauldron`] with this process's `--cauldron` /
    /// `$CAULDRON_CAULDRON` as `requested`.
    pub fn resolve_cauldron(
        &self,
        remembered: Option<&str>,
        fallback: Fallback,
    ) -> Result<Option<(Cauldron, CauldronSource)>> {
        self.select_cauldron(requested_cauldron()?, remembered, fallback)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::Source;
    use std::collections::BTreeMap;
    use std::os::unix::fs::MetadataExt;

    /// A fresh scratch root under the system temp dir — never `~/.cauldron`.
    fn scratch(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "ce1-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        let p = p.canonicalize().unwrap();
        // SENTINEL: the scratch root is not the user's live root.
        if let Some(h) = std::env::var_os("HOME") {
            assert!(!p.starts_with(Path::new(&h).join(".cauldron")), "scratch root under ~/.cauldron: {}", p.display());
        }
        p
    }

    fn env_in(root: &Path, name: &str) -> Environment {
        Environment {
            root: root.to_path_buf(),
            root_source: Source::CauldronHome,
            name: name.to_string(),
            name_source: Source::Default,
            data_root: root.join("data").join(name),
        }
    }

    /// Every entry under `root`: relative path → (is_dir, len, mtime ns, ctime ns).
    type Snap = BTreeMap<PathBuf, (bool, u64, i128, i128)>;
    fn snapshot(root: &Path) -> Snap {
        fn walk(base: &Path, p: &Path, out: &mut Snap) {
            let m = std::fs::symlink_metadata(p).unwrap();
            let mt = m.mtime() as i128 * 1_000_000_000 + m.mtime_nsec() as i128;
            let ct = m.ctime() as i128 * 1_000_000_000 + m.ctime_nsec() as i128;
            out.insert(p.strip_prefix(base).unwrap().to_path_buf(), (m.is_dir(), m.len(), mt, ct));
            if m.is_dir() {
                for e in std::fs::read_dir(p).unwrap() {
                    walk(base, &e.unwrap().path(), out);
                }
            }
        }
        let mut out = Snap::new();
        walk(root, root, &mut out);
        out
    }

    /// **THE ZERO-WRITE GUARD.** Runs `f` and fails if anything under `root`
    /// was created, removed, resized or had its mtime/ctime changed.
    fn assert_writes_nothing<T>(root: &Path, f: impl FnOnce() -> T) -> T {
        let before = snapshot(root);
        let out = f();
        let after = snapshot(root);
        assert_eq!(
            before, after,
            "WROTE under the scratch root (full listing + mtime/ctime before vs after differ)"
        );
        out
    }

    /// Lay down the three legacy locations with a byte in each.
    fn legacy_env(root: &Path, env: &str) {
        let d = root.join("data").join(env);
        for (dir, file) in [("tui/soup", "dumps.jsonl"), ("soup/cauldron", "spine.db"), ("ripples/runs", "r1")] {
            std::fs::create_dir_all(d.join(dir)).unwrap();
            std::fs::write(d.join(dir).join(file), b"x").unwrap();
        }
        std::fs::write(d.join("env.toml"), b"").unwrap();
    }

    #[test]
    fn legacy_env_presents_one_implicit_cauldron_and_writes_nothing() {
        let root = scratch("legacy");
        legacy_env(&root, "prod");
        let e = env_in(&root, "prod");
        let list = assert_writes_nothing(&root, || e.cauldrons().unwrap());
        assert_eq!(list.len(), 1, "{list:?}");
        let c = &list[0];
        assert_eq!((c.name.as_str(), c.env.as_str(), c.kind), ("prod", "prod", CauldronKind::Legacy));
        assert_eq!(c.tui(), root.join("data/prod/tui/soup"));
        assert_eq!(c.dumps(), root.join("data/prod/tui/soup/dumps.jsonl"));
        assert_eq!(c.soup(), root.join("data/prod/soup/cauldron"));
        assert_eq!(c.ripple_runs(), root.join("data/prod/ripples/runs"));
        assert_eq!(c.ripple_blobs(), root.join("data/prod/ripples/blobs"));
        assert_eq!(c.events_log(), root.join("data/prod/events/log"));
        assert_eq!(c.dir(), None);
        assert_eq!(c.manifest(), None);
        assert!(!e.cauldrons_dir().exists(), "cauldrons/ must not be created");
        // Every other read is zero-write too.
        assert_writes_nothing(&root, || {
            assert!(e.has_legacy_cauldron());
            assert_eq!(e.cauldron_named("prod").unwrap().kind, CauldronKind::Legacy);
            assert_eq!(e.find_cauldron("prod").unwrap().unwrap().kind, CauldronKind::Legacy);
            assert_eq!(e.select_cauldron(None, None, Fallback::Strict).unwrap().unwrap().1, CauldronSource::Only);
            let m = e.legacy_cauldron().meta().unwrap();
            assert!(m.enabled && m.seed.is_none() && m.name.as_deref() == Some("prod"));
            assert!(e.legacy_cauldron().check_renamable().is_err());
            let _ = e.cauldron_named("other").unwrap();
        });
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn any_one_legacy_marker_is_enough() {
        for marker in LEGACY_MARKERS {
            let root = scratch("marker");
            let mut p = root.join("data/dev");
            for c in *marker {
                p.push(c);
            }
            std::fs::create_dir_all(&p).unwrap();
            let e = env_in(&root, "dev");
            let list = assert_writes_nothing(&root, || e.cauldrons().unwrap());
            assert_eq!(list, vec![e.legacy_cauldron()], "marker {marker:?}");
            std::fs::remove_dir_all(&root).unwrap();
        }
    }

    #[test]
    fn legacy_plus_cauldrons_dir_returns_both() {
        let root = scratch("both");
        legacy_env(&root, "prod");
        let e = env_in(&root, "prod");
        std::fs::create_dir_all(e.cauldrons_dir().join("a")).unwrap();
        // Not cauldrons: the graveyard, a stray file, a reserved and an unusable name.
        std::fs::create_dir_all(e.cauldrons_dir().join(".removed/x-1")).unwrap();
        std::fs::write(e.cauldrons_dir().join("stray"), b"").unwrap();
        std::fs::create_dir_all(e.cauldrons_dir().join("shared")).unwrap();
        std::fs::create_dir_all(e.cauldrons_dir().join("Bad")).unwrap();
        let list = assert_writes_nothing(&root, || e.cauldrons().unwrap());
        let names: Vec<_> = list.iter().map(|c| (c.name.as_str(), c.kind)).collect();
        assert_eq!(names, vec![("prod", CauldronKind::Legacy), ("a", CauldronKind::Dir)]);
        let a = &list[1];
        assert_eq!(a.dir().unwrap(), root.join("data/prod/cauldrons/a"));
        assert_eq!(a.manifest().unwrap(), root.join("data/prod/cauldrons/a/cauldron.toml"));
        assert_eq!(a.tui(), root.join("data/prod/cauldrons/a/tui"));
        assert_eq!(a.soup(), root.join("data/prod/cauldrons/a/soup"));
        assert_eq!(a.ripple_runs(), root.join("data/prod/cauldrons/a/ripples/runs"));
        assert_eq!(a.events_log(), root.join("data/prod/cauldrons/a/events/log"));
        assert_eq!(a.display_name(), "a");
        assert_eq!(list[0].display_name(), "prod (legacy)");
        assert_eq!(a.open_lock(), root.join("data/prod/cauldrons/a/tui/open.lock"));
        assert_eq!(list[0].open_lock(), root.join("data/prod/tui/soup/open.lock"));
        assert!(a.check_renamable().is_ok());
        // Two cauldrons: the strict selection must be told which.
        assert_writes_nothing(&root, || {
            let err = e.select_cauldron(None, None, Fallback::Strict).unwrap_err().to_string();
            assert!(err.contains("2 cauldrons"), "{err}");
            let (c, s) = e.select_cauldron(None, None, Fallback::First).unwrap().unwrap();
            assert_eq!((c.name.as_str(), s), ("prod", CauldronSource::First));
            let (c, s) = e.select_cauldron(None, Some("a"), Fallback::Strict).unwrap().unwrap();
            assert_eq!((c.name.as_str(), s), ("a", CauldronSource::Remembered));
            // A stale remembered name is ignored, not an error.
            assert!(e.select_cauldron(None, Some("gone"), Fallback::Strict).is_err());
            let (c, s) = e.select_cauldron(Some(("a".into(), CauldronSource::Flag)), Some("prod"), Fallback::Strict).unwrap().unwrap();
            assert_eq!((c.name.as_str(), s), ("a", CauldronSource::Flag));
            assert!(e.select_cauldron(Some(("zz".into(), CauldronSource::Var)), None, Fallback::First).is_err());
        });
        // cauldron.toml: absent reads as defaults; present is parsed.
        assert!(a.meta().unwrap().enabled);
        let m = CauldronMeta { name: Some("a".into()), enabled: false, seed: Some("cauldron-build".into()), ..Default::default() };
        std::fs::write(a.manifest().unwrap(), m.to_toml().unwrap()).unwrap();
        assert_eq!(a.meta().unwrap(), m);
        std::fs::write(a.manifest().unwrap(), "enabled = \"no\"").unwrap();
        assert!(a.meta().is_err(), "a malformed cauldron.toml must not read as defaults");
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// `is_seeded` is the manifest's `seed` field, never the name, and
    /// reading it writes nothing.
    #[test]
    fn is_seeded_reads_the_seed_field_not_the_name() {
        let root = scratch("seeded");
        legacy_env(&root, "cauldron-build");
        let e = env_in(&root, "cauldron-build");
        let mk = |name: &str, seed: Option<&str>| {
            let c = e.cauldron_named(name).unwrap();
            std::fs::create_dir_all(c.dir().unwrap()).unwrap();
            let m = CauldronMeta { name: Some(name.into()), seed: seed.map(String::from), ..CauldronMeta::default() };
            std::fs::write(c.manifest().unwrap(), m.to_toml().unwrap()).unwrap();
            c
        };
        let demo = mk("demo", Some("cauldron-build"));
        let named = mk("cauldron-build-2", None);
        let bare = e.cauldron_named("bare").unwrap();
        std::fs::create_dir_all(bare.dir().unwrap()).unwrap();
        let legacy = e.legacy_cauldron();
        let got = assert_writes_nothing(&root, || {
            [&demo, &named, &bare, &legacy].map(|c| c.is_seeded().unwrap())
        });
        assert_eq!(got, [true, false, false, false]);
        // A manifest that does not parse is an error, never "not seeded".
        std::fs::write(bare.manifest().unwrap(), "seed = [").unwrap();
        assert!(bare.is_seeded().is_err());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn last_cauldron_file_is_per_env() {
        let e = env_in(Path::new("/nonexistent-ce1"), "dev");
        assert_eq!(e.last_cauldron_file(), Path::new("/nonexistent-ce1/data/dev/tui/last_cauldron.json"));
    }

    #[test]
    fn empty_env_has_no_cauldrons() {
        let root = scratch("empty");
        std::fs::create_dir_all(root.join("data/prod")).unwrap();
        let e = env_in(&root, "prod");
        let list = assert_writes_nothing(&root, || e.cauldrons().unwrap());
        assert!(list.is_empty());
        assert!(e.select_cauldron(None, None, Fallback::Strict).unwrap().is_none());
        assert!(e.select_cauldron(None, Some("x"), Fallback::First).unwrap().is_none());
        // Not even a data root: still empty, still nothing written.
        let e2 = env_in(&root, "never");
        assert!(assert_writes_nothing(&root, || e2.cauldrons().unwrap()).is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn same_named_dir_beside_legacy_is_refused_not_hidden() {
        let root = scratch("clash");
        legacy_env(&root, "dev");
        let e = env_in(&root, "dev");
        std::fs::create_dir_all(e.cauldrons_dir().join("dev")).unwrap();
        let err = e.cauldrons().unwrap_err().to_string();
        assert!(err.contains("share a name"), "{err}");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn name_grammar_refusals() {
        let long = "x".repeat(33);
        let max = "x".repeat(32);
        for bad in ["cauldron", "shared", "", "A", "-x", "_x", ".removed", "a/b", "a.b", "a b", long.as_str()] {
            assert!(validate_cauldron_name(bad).is_err(), "accepted {bad:?}");
        }
        for good in ["a", "0", "my-project", "my_project", max.as_str()] {
            validate_cauldron_name(good).unwrap();
        }
        let root = scratch("grammar");
        let e = env_in(&root, "prod");
        for bad in ["cauldron", "shared", "..", "a/b"] {
            assert!(e.cauldron_named(bad).is_err(), "cauldron_named accepted {bad:?}");
        }
        // The flag accepts a reserved spelling (it may be a legacy cauldron's
        // name, i.e. an env called `cauldron`) but never a bad one.
        assert!(validate_selected_name("cauldron", CauldronSource::Flag).is_ok());
        assert!(validate_selected_name("a/b", CauldronSource::Flag).is_err());
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// **FORCED RED for the zero-write guard**: a "cauldrons()" that creates
    /// `cauldrons/` (the shape the guard exists to catch) MUST trip it.
    #[test]
    fn zero_write_guard_catches_a_create() {
        let root = scratch("forced-red");
        legacy_env(&root, "prod");
        let e = env_in(&root, "prod");
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            assert_writes_nothing(&root, || {
                std::fs::create_dir_all(e.cauldrons_dir()).unwrap();
                e.cauldrons().unwrap()
            })
        }));
        assert!(caught.is_err(), "the zero-write guard did NOT catch `cauldrons/` being created");
        // And a write INSIDE an existing legacy dir (mtime only, no new entry
        // above it) is caught too.
        let root2 = scratch("forced-red-2");
        legacy_env(&root2, "prod");
        let caught = std::panic::catch_unwind(|| {
            assert_writes_nothing(&root2, || {
                std::fs::write(root2.join("data/prod/tui/soup/dumps.jsonl"), b"yy").unwrap();
            })
        });
        assert!(caught.is_err(), "the zero-write guard did NOT catch a rewrite");
        std::fs::remove_dir_all(&root).unwrap();
        std::fs::remove_dir_all(&root2).unwrap();
    }
}
