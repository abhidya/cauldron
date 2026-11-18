//! **Automatic processing of dumps, per cauldron**: the owner's setting and
//! the record of what the `ripples` service did with each dump on its own.
//!
//! Off unless the owner turned it on for a cauldron (`cauldron ripples on`,
//! at a terminal). Three programs read what is here, so it lives in this
//! crate, which all three use: the `cauldron` CLI writes the setting and
//! shows the queue when the service is stopped, the `ripples` service acts
//! on it, and the `mcp` service says in a dump's receipt whether it is on.
//!
//! - **The setting**: `<data root>/ripples/auto/<cauldron>.json`, one JSON
//!   object ([`Settings`]), mode 0600 in 0700 directories, replaced whole by
//!   a write to a new file, synced, then renamed over the old one
//!   ([`store`]), so a reader sees the old setting or the new one, never a
//!   mix. No file means off.
//! - **Bound to one cauldron**: a setting records the cauldron it was
//!   written for ([`Binding`]: the environment and the cauldron's id, the
//!   identity its MCP grant is bound to too; a setting written before
//!   cauldrons had ids, the cauldron's `created_at` and its directory's
//!   inode, until [`adopt_id`] carries it onto the id).
//!   Before every automatic run, and before every model call of one, the
//!   setting is checked against the cauldron of that name there now
//!   ([`effective`]): a cauldron that is gone (removed, renamed), one made
//!   again under the name, a setting copied from another cauldron or
//!   environment, or one with no binding at all, makes the setting VOID:
//!   nothing runs, and `cauldron ripples status` says why and how to clear
//!   it. `cauldron remove` and `cauldron rename` turn it off themselves.
//! - **The `since` rule**: turning it on records the highest dump id the
//!   cauldron held at that moment ([`Settings::since_dump`]); only a dump
//!   with a higher id is processed automatically. Older dumps are processed
//!   only by hand (`cauldron ripples run`), so turning it on, or an upgrade,
//!   never spends on a backlog.
//! - **The automatic journal**: `<data root>/ripples/auto.jsonl`, one JSON
//!   line per step, appended and synced by the service: `attempt` (before a
//!   run starts; it carries when to try again should the attempt be cut
//!   short) and `outcome` (with its `class`: `done`, `deferred`, `retry`,
//!   `parked` or `skip`). [`dump_states`] reads what each dump's automatic
//!   attempts came to; [`due`] picks the next dump to run; [`queue`] is what
//!   `cauldron ripples status` shows. Each line carries the binding's key
//!   ([`Binding::key`]), and only lines of the setting's own binding count:
//!   a cauldron made again under an old name starts with a clean queue.
//!   Ids, codes, counts and times only, never text.

use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// The service's directory under a data root.
pub const SERVICE_DIR: &str = "ripples";
/// The settings' directory under the service's.
pub const DIR: &str = "auto";
/// The automatic journal, in the service's directory.
pub const JOURNAL: &str = "auto.jsonl";
/// A dump that failed this many automatic attempts is parked `failed`.
pub const MAX_ATTEMPTS: u32 = 3;

fn yes() -> bool {
    true
}

/// **The cauldron a setting was written for.** With an `id` (the
/// cauldron's id, `env::cauldron::CauldronMeta::id`), that id is what binds
/// it: a cauldron renamed, or moved with its whole data root (another
/// directory, another inode), keeps it, and one made again under the name
/// has another. Without one (written before cauldrons had ids), its
/// `created_at` and directory inode bind it, until the cauldron is given its
/// id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Binding {
    /// The environment's name.
    pub env: String,
    /// The cauldron's id; `None` in a setting written before cauldrons had
    /// ids.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The cauldron's `created_at` (`cauldron.toml`), the identity its MCP
    /// grant is bound to; `None` for a cauldron that has none (the implicit
    /// legacy cauldron).
    #[serde(default)]
    pub created_at: Option<String>,
    /// The inode of the cauldron's directory (`None` for the legacy
    /// cauldron): a cauldron made again within the same second, or a data
    /// root copied elsewhere, is not the same cauldron either.
    #[serde(default)]
    pub dir_ino: Option<u64>,
}

impl Binding {
    /// The binding of the cauldron `c` of environment `e`.
    pub fn of(e: &crate::Environment, c: &crate::Cauldron) -> Binding {
        use std::os::unix::fs::MetadataExt;
        let meta = c.meta().ok();
        Binding {
            env: e.name.clone(),
            id: meta.as_ref().and_then(|m| m.id.clone()),
            created_at: meta.and_then(|m| m.created_at),
            dir_ino: c.dir().and_then(|d| std::fs::metadata(d).ok()).map(|m| m.ino()),
        }
    }

    /// The key the automatic journal's lines carry (`bound`): the id's, or
    /// without one the `created_at` and inode's ([`Binding::legacy_key`]).
    pub fn key(&self) -> String {
        match &self.id {
            Some(id) => format!("{}|id:{id}", self.env),
            None => self.legacy_key(),
        }
    }

    /// The key a setting written before cauldrons had ids carried; the lines
    /// it wrote still count for it once it is bound to the id.
    pub fn legacy_key(&self) -> String {
        format!("{}|{}|{}", self.env, self.created_at.as_deref().unwrap_or("-"), self.dir_ino.map(|i| i.to_string()).unwrap_or_else(|| "-".into()))
    }
}

/// **One cauldron's setting.**
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub cauldron: String,
    /// Is automatic processing on?
    pub on: bool,
    /// When it was last turned on (UTC, RFC 3339).
    pub since_at: String,
    /// The highest dump serial (`DM-<n>`) the cauldron held when it was
    /// turned on: only higher ones are processed automatically.
    pub since_dump: u64,
    /// `threads` or `local`.
    pub backend: String,
    /// The model given at `on` (`--model`), if any.
    #[serde(default)]
    pub model: Option<String>,
    /// Session-transcript dumps too (`--include-sessions`); text dumps only
    /// otherwise.
    #[serde(default)]
    pub include_sessions: bool,
    /// Scrub what goes to a local model too (always, unless
    /// `--no-scrub-local` was given). What goes through threads is always
    /// scrubbed.
    #[serde(default = "yes")]
    pub scrub_local: bool,
    /// When this setting was last written (UTC, RFC 3339).
    pub changed_at: String,
    /// When it was last turned off, if it was.
    #[serde(default)]
    pub off_at: Option<String>,
    /// The cauldron it was written for. A setting without one is void.
    #[serde(default)]
    pub bound_to: Option<Binding>,
}

impl Settings {
    /// The key of its binding (empty without one: no journal line matches).
    pub fn bound_key(&self) -> String {
        self.bound_to.as_ref().map(Binding::key).unwrap_or_default()
    }

    /// Does a journal line's `bound` belong to this setting: its key, or for
    /// a setting bound to an id, the key it carried before ([`Binding::legacy_key`]).
    pub fn owns_line(&self, bound: &Value) -> bool {
        let Some(b) = &self.bound_to else { return false };
        bound == b.key().as_str() || (b.id.is_some() && b.created_at.is_some() && bound == b.legacy_key().as_str())
    }
}

/// **A setting as it stands now**: the stored setting, and, when it does not
/// belong to the cauldron of its name there now, why (void: nothing runs).
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// The cauldron name it is stored under (its file's name).
    pub name: String,
    pub settings: Settings,
    pub void: Option<String>,
}

impl Entry {
    /// On, and not void: automatic processing runs.
    pub fn runs(&self) -> bool {
        self.settings.on && self.void.is_none()
    }
}

/// **Why `s` (stored under the name `stem`) does not belong to the cauldron
/// of that name in `e` now**, or `None` when it does.
pub fn void_reason(e: &crate::Environment, stem: &str, s: &Settings) -> Option<String> {
    if s.cauldron != stem {
        return Some(format!("the setting stored for cauldron `{stem}` names cauldron `{}`: it was copied or edited by hand", s.cauldron));
    }
    let Some(b) = &s.bound_to else {
        return Some(format!("the setting for cauldron `{stem}` records no cauldron it was written for (written by hand, or by an earlier build)"));
    };
    if b.env != e.name {
        return Some(format!("the setting for cauldron `{stem}` was written in environment `{}`, not `{}`: it was copied", b.env, e.name));
    }
    let c = match e.find_cauldron(stem) {
        Ok(Some(c)) => c,
        Ok(None) => return Some(format!("the setting belonged to cauldron `{stem}`, which is gone (removed or renamed)")),
        Err(err) => return Some(format!("the cauldrons of environment `{}` cannot be read ({err:#}), so the setting for `{stem}` cannot be checked", e.name)),
    };
    let now = Binding::of(e, &c);
    match (&b.id, &now.id) {
        // Bound to an id: that id, wherever the cauldron's directory is now.
        (Some(want), Some(have)) if want == have => None,
        (Some(_), _) => Some(format!(
            "the setting belonged to a cauldron `{stem}` created {}, which is gone; the cauldron `{stem}` there now (created {}) is another one",
            b.created_at.as_deref().unwrap_or("at an unknown time"),
            now.created_at.as_deref().unwrap_or("at an unknown time")
        )),
        // Written before cauldrons had ids, and the cauldron has one now: it
        // was bound to that id when the id was given, if it was this
        // cauldron's then; it was not.
        (None, Some(_)) => Some(format!(
            "the setting for cauldron `{stem}` was written before cauldrons had ids, and when the cauldron `{stem}` there now was given its id the setting did not match it (another `created_at` or directory), so it was not carried over"
        )),
        (None, None) if now.created_at != b.created_at || now.dir_ino != b.dir_ino => Some(format!(
            "the setting belonged to a cauldron `{stem}` created {}, which is gone; the cauldron `{stem}` there now (created {}) is another one",
            b.created_at.as_deref().unwrap_or("at an unknown time"),
            now.created_at.as_deref().unwrap_or("at an unknown time")
        )),
        (None, None) => None,
    }
}

/// **Carry a setting written before cauldrons had ids onto the id `id`
/// that the cauldron `c` was just given** (`soup_door::grants::adopt`, the
/// one moment this happens): only when the setting was written for that
/// very cauldron (the same environment, `created_at` and directory inode).
/// Answers whether it was carried. Otherwise it is left as it is, and void
/// ([`void_reason`]): its owner turns it on again.
pub fn adopt_id(e: &crate::Environment, c: &crate::Cauldron, id: &str) -> std::io::Result<bool> {
    let Some(mut s) = load(&e.data_root, &c.name)? else { return Ok(false) };
    let Some(b) = s.bound_to.clone() else { return Ok(false) };
    if b.id.is_some() || s.cauldron != c.name {
        return Ok(false);
    }
    let now = Binding::of(e, c);
    if b.env != e.name || b.created_at.is_none() || b.created_at != now.created_at || b.dir_ino != now.dir_ino {
        return Ok(false);
    }
    s.bound_to = Some(Binding { id: Some(id.to_string()), ..b });
    store(&e.data_root, &s)?;
    Ok(true)
}

/// What to do about a void setting, in a sentence.
pub fn void_what_to_do(env: &str, cauldron: &str) -> String {
    format!(
        "nothing is processed automatically under it. Clear it with `cauldron ripples off {cauldron} --env {env}`; to process the dumps of the cauldron `{cauldron}` there now automatically, its owner turns it on at a terminal: `cauldron ripples on {cauldron} --env {env}`"
    )
}

/// **`cauldron`'s setting as it stands** in `e`: `None` when there is none
/// (off).
pub fn effective(e: &crate::Environment, cauldron: &str) -> std::io::Result<Option<Entry>> {
    Ok(load(&e.data_root, cauldron)?.map(|s| {
        let void = void_reason(e, cauldron, &s);
        Entry { name: cauldron.to_string(), settings: s, void }
    }))
}

/// Is `n` a cauldron name a setting can be kept under?
pub fn name_ok(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 32
        && n.chars().next().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && n.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// The settings' directory of the environment whose data root is `data_root`.
pub fn dir(data_root: &Path) -> PathBuf {
    data_root.join(SERVICE_DIR).join(DIR)
}

/// The automatic journal's path.
pub fn journal_path(data_root: &Path) -> PathBuf {
    data_root.join(SERVICE_DIR).join(JOURNAL)
}

/// The setting file of `cauldron`.
pub fn path(data_root: &Path, cauldron: &str) -> PathBuf {
    dir(data_root).join(format!("{cauldron}.json"))
}

/// `cauldron`'s setting; `None` when it was never turned on (off).
pub fn load(data_root: &Path, cauldron: &str) -> std::io::Result<Option<Settings>> {
    if !name_ok(cauldron) {
        return Ok(None);
    }
    match std::fs::read_to_string(path(data_root, cauldron)) {
        Ok(t) => serde_json::from_str(&t).map(Some).map_err(|e| std::io::Error::other(format!("{}: {e}", path(data_root, cauldron).display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Does automatic processing run for `cauldron` in `e`: on, and bound to
/// the cauldron of that name there now? A setting that cannot be read is
/// off.
pub fn is_on(e: &crate::Environment, cauldron: &str) -> bool {
    matches!(effective(e, cauldron), Ok(Some(x)) if x.runs())
}

/// Every setting there is, by the name it is stored under, each checked
/// against the cauldron of that name ([`effective`]; one that cannot be
/// read is left out).
pub fn all(e: &crate::Environment) -> Vec<Entry> {
    let mut names: Vec<String> = std::fs::read_dir(dir(&e.data_root))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|x| x.file_name().to_str().and_then(|n| n.strip_suffix(".json")).filter(|n| !n.starts_with('.')).map(str::to_string))
        .collect();
    names.sort();
    names.into_iter().filter_map(|n| effective(e, &n).ok().flatten()).collect()
}

/// **Turn it off** for `cauldron` (`cauldron remove`, `cauldron rename`,
/// `cauldron ripples off`): the setting kept, `on` false, `off_at` now.
/// `Ok(Some(the setting as it was))` when it was on; `Ok(None)` when there
/// was nothing to turn off.
pub fn turn_off(data_root: &Path, cauldron: &str, now: &str) -> std::io::Result<Option<Settings>> {
    match load(data_root, cauldron)? {
        Some(s) if s.on => {
            let was = s.clone();
            store(data_root, &Settings { on: false, off_at: Some(now.to_string()), changed_at: now.to_string(), ..s })?;
            Ok(Some(was))
        }
        _ => Ok(None),
    }
}

/// The `cauldron ripples on` command that turns `s` on again, under the
/// name `cauldron`.
pub fn on_command(s: &Settings, cauldron: &str, env: &str) -> String {
    let mut cmd = format!("cauldron ripples on {cauldron} --env {env}");
    if s.backend != "threads" {
        cmd.push_str(&format!(" --backend {}", s.backend));
    }
    if let Some(m) = &s.model {
        cmd.push_str(&format!(" --model {m}"));
    }
    if s.include_sessions {
        cmd.push_str(" --include-sessions");
    }
    if !s.scrub_local && s.backend == "local" {
        cmd.push_str(" --no-scrub-local");
    }
    cmd
}

/// **Write a setting**: the directories made owner-only, the file written
/// new (0600), synced, renamed over the old one, and the directory synced.
pub fn store(data_root: &Path, s: &Settings) -> std::io::Result<()> {
    if !name_ok(&s.cauldron) {
        return Err(std::io::Error::other(format!("`{}` is not a cauldron name", s.cauldron)));
    }
    crate::private::make_dir(&data_root.join(SERVICE_DIR))?;
    let d = dir(data_root);
    crate::private::make_dir(&d)?;
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|x| x.as_nanos()).unwrap_or(0);
    let tmp = d.join(format!(".{}.json.{}-{nanos}", s.cauldron, std::process::id()));
    {
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(crate::private::FILE_MODE).open(&tmp)?;
        let text = serde_json::to_string_pretty(s).map_err(std::io::Error::other)?;
        f.write_all(text.as_bytes())?;
        f.write_all(b"\n")?;
        f.sync_all()?;
    }
    if let Err(e) = std::fs::rename(&tmp, path(data_root, &s.cauldron)) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::File::open(&d)?.sync_all()
}

/// The serial of a dump id (`DM-12` is 12; anything else 0).
pub fn serial(dump: &str) -> u64 {
    dump.strip_prefix("DM-").and_then(|n| n.parse().ok()).unwrap_or(0)
}

/// Does `s` take a dump of this kind (`text`, or `session` with
/// `--include-sessions`) received after it was turned on?
pub fn takes(s: &Settings, dump: &str, kind: &str) -> bool {
    s.on && serial(dump) > s.since_dump && (kind == "text" || (kind == "session" && s.include_sessions))
}

/// Append one line to the automatic journal, synced. A last line a crash
/// cut short (no newline) is ended first, so it costs only itself and
/// never the line appended after it.
pub fn append(data_root: &Path, line: &Value) -> std::io::Result<()> {
    use std::io::{Read, Seek, SeekFrom};
    crate::private::make_dir(&data_root.join(SERVICE_DIR))?;
    let mut f = std::fs::OpenOptions::new().create(true).read(true).append(true).mode(crate::private::FILE_MODE).open(journal_path(data_root))?;
    let len = f.metadata()?.len();
    let mut text = format!("{line}\n");
    if len > 0 {
        let mut last = [0u8; 1];
        f.seek(SeekFrom::Start(len - 1))?;
        f.read_exact(&mut last)?;
        if last[0] != b'\n' {
            text.insert(0, '\n');
        }
    }
    f.write_all(text.as_bytes())?;
    f.sync_all()
}

/// Every line of the automatic journal (one that does not parse is skipped).
pub fn read(data_root: &Path) -> Vec<Value> {
    std::fs::read_to_string(journal_path(data_root)).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

/// What one dump's automatic attempts came to.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DumpAuto {
    pub cauldron: String,
    pub dump: String,
    /// Attempts that failed, or were cut short (killed, stopped).
    pub failed_attempts: u32,
    /// The last outcome line.
    pub last: Option<Value>,
    /// An attempt with no outcome: running now, or cut short.
    pub open: Option<Value>,
    /// Not to be tried before this (UTC, RFC 3339), if set.
    pub retry_after: Option<String>,
}

impl DumpAuto {
    /// `done`, `deferred`, `retry`, `parked`, `skip`, `cut_short` (an attempt
    /// with no outcome) or `none`.
    pub fn class(&self) -> &str {
        if self.open.is_some() {
            return "cut_short";
        }
        self.last.as_ref().and_then(|l| l["class"].as_str()).unwrap_or("none")
    }
}

/// **Each dump's automatic state**, from the journal's lines, by `(cauldron,
/// dump)`, counting only the lines of the setting's own binding (`bound`):
/// what an earlier cauldron of the same name came to never touches the one
/// there now. An attempt with no outcome after it counts as a failed
/// attempt, tried again after the time its line carries
/// (`retry_after_if_cut`).
pub fn dump_states(lines: &[Value], s: &Settings) -> BTreeMap<(String, String), DumpAuto> {
    let mine: Vec<&Value> = lines.iter().filter(|l| l["cauldron"] == s.cauldron.as_str() && s.owns_line(&l["bound"])).collect();
    states_of(mine)
}

fn states_of<'a>(lines: impl IntoIterator<Item = &'a Value>) -> BTreeMap<(String, String), DumpAuto> {
    let mut out: BTreeMap<(String, String), DumpAuto> = BTreeMap::new();
    let s = |v: &Value| v.as_str().unwrap_or("").to_string();
    for l in lines {
        let key = (s(&l["cauldron"]), s(&l["dump"]));
        if key.0.is_empty() || key.1.is_empty() {
            continue;
        }
        let d = out.entry(key.clone()).or_insert_with(|| DumpAuto { cauldron: key.0.clone(), dump: key.1.clone(), ..DumpAuto::default() });
        match l["event"].as_str() {
            Some("attempt") => {
                // An earlier attempt never closed: it was cut short.
                if let Some(o) = d.open.take() {
                    d.failed_attempts += 1;
                    d.retry_after = o["retry_after_if_cut"].as_str().map(str::to_string);
                }
                d.open = Some(l.clone());
            }
            Some("outcome") => {
                d.open = None;
                if l["class"] == "retry" {
                    d.failed_attempts += 1;
                }
                d.retry_after = l["retry_after"].as_str().map(str::to_string);
                d.last = Some(l.clone());
            }
            _ => {}
        }
    }
    for d in out.values_mut() {
        if let Some(o) = &d.open {
            d.failed_attempts += 1;
            d.retry_after = o["retry_after_if_cut"].as_str().map(str::to_string);
        }
    }
    out
}

/// **The next dump to run automatically** in `s`'s cauldron: of `received`
/// (the cauldron's dumps in state `received`, each `{id, kind}`, any
/// order), the oldest that `s` takes ([`takes`]), that is not done or
/// parked, and whose wait (if any) is over at `now` (UTC, RFC 3339, the
/// same form as the journal's times). A dump whose failed attempts reached
/// [`MAX_ATTEMPTS`] is never due: it is parked.
pub fn due(s: &Settings, received: &[Value], states: &BTreeMap<(String, String), DumpAuto>, now: &str) -> Option<String> {
    let mut ids: Vec<(u64, String)> = received
        .iter()
        .filter(|d| d["state"].as_str().is_none_or(|x| x == "received"))
        .filter_map(|d| {
            let id = d["id"].as_str()?;
            takes(s, id, d["kind"].as_str().unwrap_or("text")).then(|| (serial(id), id.to_string()))
        })
        .collect();
    ids.sort();
    ids.into_iter().map(|(_, id)| id).find(|id| match states.get(&(s.cauldron.clone(), id.clone())) {
        None => true,
        Some(d) => {
            !matches!(d.class(), "done" | "parked" | "skip")
                && d.failed_attempts < MAX_ATTEMPTS
                && d.retry_after.as_deref().is_none_or(|t| t <= now)
        }
    })
}

/// **What `cauldron ripples status` shows of one cauldron's automatic
/// processing**: the setting, and the queue: `waiting` (received after it
/// was turned on, to be run), `deferred` (with the reason: `budget`,
/// `threads_parked` and the park's cause, or `busy`, and when it is tried
/// again), `retrying` (failed attempts so far, the last
/// failure, when it is tried again), `failed` (parked after its attempts,
/// with the reason), `not_taken` (session dumps, without
/// `--include-sessions`), and the last ten it `processed`. `received` is
/// `None` when the dumps could not be read. `running` is the dump the
/// service is running for this cauldron now, if any: its open attempt is
/// shown as `running`, not as a failed one. A void setting (`automatic:
/// void`) shows why, and how to clear it, and no queue: nothing runs under
/// it.
pub fn queue(entry: &Entry, env: &str, received: Option<&[Value]>, states: &BTreeMap<(String, String), DumpAuto>, now: &str, running: Option<&str>) -> Value {
    let s = &entry.settings;
    if let Some(why) = &entry.void {
        return json!({
            "cauldron": entry.name,
            "automatic": if s.on { "void" } else { "off" },
            "why": why,
            "what_to_do": void_what_to_do(env, &entry.name),
            "changed_at": s.changed_at,
            "off_at": s.off_at,
            "now": now,
        });
    }
    let mut waiting = vec![];
    let mut in_flight = vec![];
    let mut deferred = vec![];
    let mut retrying = vec![];
    let mut not_taken = vec![];
    if let Some(rs) = received {
        let mut rs: Vec<&Value> = rs.iter().filter(|d| d["state"].as_str().is_none_or(|x| x == "received")).collect();
        rs.sort_by_key(|d| serial(d["id"].as_str().unwrap_or("")));
        for d in rs {
            let id = d["id"].as_str().unwrap_or("").to_string();
            let kind = d["kind"].as_str().unwrap_or("text");
            if serial(&id) <= s.since_dump {
                continue;
            }
            if !takes(s, &id, kind) {
                if s.on {
                    not_taken.push(json!({"dump": id, "kind": kind, "why": "a session dump; automatic processing takes text dumps only (`--include-sessions` was not given)"}));
                }
                continue;
            }
            match states.get(&(s.cauldron.clone(), id.clone())) {
                // Its attempt is open and the service is running it now.
                Some(st) if st.open.is_some() && running == Some(id.as_str()) => {
                    let o = st.open.clone().unwrap_or(Value::Null);
                    in_flight.push(json!({"dump": id, "attempt": o["n"], "since": o["at"], "failed_attempts_before": st.failed_attempts.saturating_sub(1)}));
                }
                Some(st) if st.class() == "deferred" => {
                    let l = st.last.clone().unwrap_or(Value::Null);
                    if l["held_by"] == "threads" {
                        deferred.push(json!({"dump": id, "reason": "threads_parked", "park": l["park"], "message": l["message"], "retry_after": st.retry_after}));
                    } else if l["reason"] == "busy" {
                        deferred.push(json!({"dump": id, "reason": "busy", "message": l["message"], "retry_after": st.retry_after}));
                    } else {
                        deferred.push(json!({"dump": id, "reason": "budget", "detail": l["reason"], "message": l["message"], "retry_after": st.retry_after}));
                    }
                }
                Some(st) if st.failed_attempts > 0 && st.failed_attempts < MAX_ATTEMPTS && !matches!(st.class(), "parked" | "done") => {
                    let l = st.last.clone().unwrap_or(Value::Null);
                    retrying.push(json!({"dump": id, "failed_attempts": st.failed_attempts, "of": MAX_ATTEMPTS, "last_code": if st.class() == "cut_short" { json!("cut_short") } else { l["code"].clone() }, "retry_after": st.retry_after}));
                }
                Some(st) if matches!(st.class(), "parked" | "done" | "skip") => {}
                _ => waiting.push(json!(id)),
            }
        }
    }
    let mut failed = vec![];
    let mut processed = vec![];
    for st in states.values().filter(|d| d.cauldron == s.cauldron) {
        let l = st.last.clone().unwrap_or(Value::Null);
        match st.class() {
            "parked" => failed.push(json!({"dump": st.dump, "code": l["code"], "message": l["message"], "attempts": st.failed_attempts, "run": l["run"], "at": l["at"], "what_to_do": l["what_to_do"]})),
            "done" => processed.push(json!({"dump": st.dump, "run": l["run"], "records": l["records"], "at": l["at"]})),
            _ => {}
        }
    }
    processed.sort_by(|a, b| a["at"].as_str().cmp(&b["at"].as_str()));
    let keep = processed.len().saturating_sub(10);
    processed.drain(..keep);
    let mut v = json!({
        "cauldron": s.cauldron,
        "automatic": if s.on { "on" } else { "off" },
        "since": {"at": s.since_at, "after_dump": format!("DM-{}", s.since_dump)},
        "backend": s.backend,
        "model": s.model,
        "include_sessions": s.include_sessions,
        "scrub_local": s.scrub_local,
        "changed_at": s.changed_at,
        "off_at": s.off_at,
        "waiting": waiting,
        "running": in_flight,
        "deferred": deferred,
        "retrying": retrying,
        "failed": failed,
        "not_taken": not_taken,
        "processed": processed,
        "now": now,
    });
    if received.is_none() {
        v["queue_unknown"] = json!("the cauldron's dumps could not be read, so `waiting` is not known");
    }
    v
}

/// **The threads hold**, read from the automatic journal: after threads
/// parks an automatic call, no automatic run on the threads backend, of any
/// cauldron of the environment, starts until `until` (the newest park's
/// `retry_after`). Any other outcome of a threads run ends it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ThreadsHold {
    /// When the next automatic run on threads may start; `None`: no hold.
    pub until: Option<String>,
    /// Parks in a row (each wait four times the one before).
    pub parks: u32,
    /// When the first park of the streak was recorded.
    pub since: Option<String>,
    /// The newest park's cause (`threshold`, `usage_limit`, …).
    pub park: Option<String>,
    /// The cauldron and dump whose call was parked last.
    pub cauldron: Option<String>,
    pub dump: Option<String>,
}

/// [`ThreadsHold`] from the journal's lines.
pub fn threads_hold(lines: &[Value]) -> ThreadsHold {
    let mut h = ThreadsHold::default();
    let s = |v: &Value| v.as_str().map(str::to_string);
    for l in lines.iter().filter(|l| l["event"] == "outcome" && l["backend"] == "threads") {
        if l["held_by"] == "threads" {
            if h.parks == 0 {
                h.since = s(&l["at"]);
            }
            h.parks += 1;
            h.until = s(&l["retry_after"]);
            h.park = s(&l["park"]);
            h.cauldron = s(&l["cauldron"]);
            h.dump = s(&l["dump"]);
        } else if l["class"] != "skip" {
            h = ThreadsHold::default();
        }
    }
    h
}

impl ThreadsHold {
    /// Is it holding at `now` (UTC, the journal's form)?
    pub fn holding(&self, now: &str) -> bool {
        self.until.as_deref().is_some_and(|u| u > now)
    }

    /// What `cauldron ripples status` shows of it: `null` when there is no
    /// hold; else since when, why, the next attempt, and what it holds.
    pub fn view(&self, now: &str) -> Value {
        let Some(until) = &self.until else { return Value::Null };
        let holding = self.holding(now);
        json!({
            "state": if holding { "holding" } else { "over" },
            "since": self.since,
            "reason": "threads_parked",
            "park": self.park,
            "parks_in_a_row": self.parks,
            "set_by": {"cauldron": self.cauldron, "dump": self.dump},
            "next_attempt": until,
            "holds": "every cauldron of this environment whose automatic processing uses the threads backend; local runs are not held",
            "note": if holding {
                format!("threads parked the last automatic call (`{}`), so no automatic call on threads starts before {until}; then one due dump is tried, the cauldron served least recently first, and a park again waits four times as long (at most an hour)", self.park.as_deref().unwrap_or("parked"))
            } else {
                "the wait is over: the next scan tries one due dump on threads, the cauldron served least recently first".to_string()
            },
        })
    }

    /// Mark `q` (one cauldron's [`queue`]) as held when the hold holds its
    /// dumps: `held: {by: threads_hold, until, dumps}`, the dumps waiting.
    pub fn mark(&self, q: &mut Value, s: &Settings, now: &str) {
        if s.backend != "local" && q["automatic"] == "on" && self.holding(now) {
            let mut dumps: Vec<Value> = q["waiting"].as_array().cloned().unwrap_or_default();
            dumps.extend(q["deferred"].as_array().into_iter().flatten().map(|d| d["dump"].clone()));
            dumps.extend(q["retrying"].as_array().into_iter().flatten().map(|d| d["dump"].clone()));
            q["held"] = json!({"by": "threads_hold", "until": self.until, "dumps": dumps, "why": "threads parked an automatic call; see `automatic.threads_hold`"});
        }
    }
}

/// **The order the service serves cauldrons in**: the one served least
/// recently first (its newest automatic attempt under its own binding; never
/// served, first of all), then by name. So a cauldron whose calls keep
/// being parked does not make every probe after a hold, and another one
/// waiting behind it is tried next.
pub fn serve_order(entries: &mut [Entry], lines: &[Value]) {
    let last = |e: &Entry| -> String {
        lines
            .iter()
            .filter(|l| l["event"] == "attempt" && l["cauldron"] == e.settings.cauldron.as_str() && e.settings.owns_line(&l["bound"]))
            .filter_map(|l| l["at"].as_str())
            .max()
            .unwrap_or("")
            .to_string()
    };
    entries.sort_by_cached_key(|e| (last(e), e.name.clone()));
}

/// **What a dump's receipt says of automatic processing** (`soup_dump`,
/// `soup_dump_session`, `cauldron soup dump`): whether it is on for the
/// cauldron, whether it takes this dump, and what happens next.
pub fn receipt(e: &crate::Environment, cauldron: &str, dump: &str, kind: &str) -> Value {
    let env = &e.name;
    let by_hand = format!("cauldron ripples run {dump} --env {env} --cauldron {cauldron}");
    match effective(e, cauldron) {
        Ok(Some(x)) if x.settings.on && x.void.is_some() => json!({
            "on": false,
            "takes_this_dump": false,
            "void": x.void,
            "note": format!("automatic processing is not on for this cauldron `{cauldron}`: a setting stored under its name belonged to another cauldron ({}), so it is void and takes nothing. This dump waits `received` until its owner runs `{by_hand}`; {}", x.void.as_deref().unwrap_or(""), void_what_to_do(env, cauldron)),
        }),
        Ok(Some(Entry { settings: s, .. })) if s.on && takes(&s, dump, kind) => json!({
            "on": true,
            "takes_this_dump": true,
            "backend": s.backend,
            "since": s.since_at,
            "note": format!(
                "automatic processing is on for cauldron `{cauldron}`: the ripples service processes this dump with no further command once it is running (records land marked `unreviewed` until the owner approves them at a terminal); `cauldron ripples status --env {env} --cauldron {cauldron}` and this dump's listing follow it"
            ),
        }),
        Ok(Some(Entry { settings: s, .. })) if s.on => json!({
            "on": true,
            "takes_this_dump": false,
            "backend": s.backend,
            "note": format!("automatic processing is on for cauldron `{cauldron}` but takes text dumps only (it was turned on without `--include-sessions`): this dump waits `received` until its owner runs `{by_hand}`"),
        }),
        _ => json!({
            "on": false,
            "takes_this_dump": false,
            "note": format!("automatic processing is off for cauldron `{cauldron}`: this dump waits `received` until its owner runs `{by_hand}`, or turns automatic processing on at a terminal (`cauldron ripples on {cauldron} --env {env}`) for dumps received after that"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setting(since: u64, sessions: bool) -> Settings {
        Settings {
            cauldron: "kb".into(),
            on: true,
            since_at: "2026-01-01T00:00:00.000Z".into(),
            since_dump: since,
            backend: "threads".into(),
            model: None,
            include_sessions: sessions,
            scrub_local: true,
            changed_at: "2026-01-01T00:00:00.000Z".into(),
            off_at: None,
            bound_to: Some(Binding { env: "dev".into(), id: None, created_at: Some("2026-01-01T00:00:00Z".into()), dir_ino: Some(7) }),
        }
    }

    fn scratch(tag: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("env-auto-{tag}-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn a_setting_is_written_whole_and_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let root = scratch("store");
        assert_eq!(load(&root, "kb").unwrap(), None);
        let s = setting(4, false);
        store(&root, &s).unwrap();
        assert_eq!(load(&root, "kb").unwrap(), Some(s.clone()));
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path(&root, "kb")), 0o600);
        assert_eq!(mode(&dir(&root)), 0o700);
        assert_eq!(mode(&root.join(SERVICE_DIR)), 0o700);
        // Replaced, never mixed; no temporary file left.
        store(&root, &Settings { on: false, ..s }).unwrap();
        assert!(!load(&root, "kb").unwrap().unwrap().on);
        assert_eq!(std::fs::read_dir(dir(&root)).unwrap().count(), 1);
        // A journal whose last line a crash cut short: the next line is
        // whole and read.
        std::fs::write(journal_path(&root), "{\"event\": \"attempt\", \"cau").unwrap();
        append(&root, &json!({"event": "outcome", "cauldron": "kb", "dump": "DM-1"})).unwrap();
        assert_eq!(read(&root), vec![json!({"event": "outcome", "cauldron": "kb", "dump": "DM-1"})]);
        assert!(store(&root, &Settings { cauldron: "../x".into(), ..setting(0, false) }).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn only_dumps_after_since_of_the_kinds_taken_are_due_and_attempts_are_bounded() {
        let s = setting(2, false);
        let received = vec![
            json!({"id": "DM-1", "kind": "text"}),
            json!({"id": "DM-3", "kind": "session"}),
            json!({"id": "DM-4", "kind": "text"}),
            json!({"id": "DM-5", "kind": "text"}),
        ];
        let now = "2026-01-02T00:00:00.000Z";
        let none = BTreeMap::new();
        assert_eq!(due(&s, &received, &none, now).as_deref(), Some("DM-4"));
        assert_eq!(due(&setting(2, true), &received, &none, now).as_deref(), Some("DM-3"));
        // Cut short, waiting: then due once the wait is over.
        let bound = s.bound_key();
        let att = |n: u32, cut: &str| json!({"event": "attempt", "cauldron": "kb", "bound": bound, "dump": "DM-4", "attempt": n, "retry_after_if_cut": cut});
        let out = |class: &str, after: &str| json!({"event": "outcome", "cauldron": "kb", "bound": bound, "dump": "DM-4", "class": class, "retry_after": after, "code": "inference_failed"});
        let lines = vec![att(1, "2026-01-03T00:00:00.000Z")];
        let st = dump_states(&lines, &s);
        assert_eq!(st[&("kb".into(), "DM-4".into())].failed_attempts, 1);
        assert_eq!(due(&s, &received, &st, now).as_deref(), Some("DM-5"));
        assert_eq!(due(&s, &received, &st, "2026-01-03T00:00:00.000Z").as_deref(), Some("DM-4"));
        // Three failures: parked, never due.
        let lines = vec![att(1, "x"), out("retry", "2026-01-01T00:00:00.000Z"), att(2, "x"), out("retry", "2026-01-01T00:00:00.000Z"), att(3, "x"), out("retry", "2026-01-01T00:00:00.000Z")];
        let st = dump_states(&lines, &s);
        assert_eq!(st[&("kb".into(), "DM-4".into())].failed_attempts, 3);
        assert_eq!(due(&s, &received, &st, now).as_deref(), Some("DM-5"));
        // Done: never again.
        let st = dump_states(&[att(1, "x"), out("done", "")], &s);
        assert_eq!(due(&s, &received, &st, now).as_deref(), Some("DM-5"));
        // Lines of another binding (an earlier cauldron of the same name)
        // count for nothing: DM-4 is due again.
        let other = Settings { bound_to: Some(Binding { created_at: Some("2025-01-01T00:00:00Z".into()), ..s.bound_to.clone().unwrap() }), ..s.clone() };
        assert_eq!(due(&other, &received, &dump_states(&[att(1, "x"), out("done", "")], &other), now).as_deref(), Some("DM-4"));
        // A setting with no binding matches no line.
        let unbound = Settings { bound_to: None, ..s.clone() };
        assert!(dump_states(&[att(1, "x"), out("done", "")], &unbound).is_empty());
        // Off: nothing.
        assert_eq!(due(&Settings { on: false, ..s.clone() }, &received, &none, now), None);
        let e = Entry { name: "kb".into(), settings: s.clone(), void: None };
        let q = queue(&e, "dev", Some(&received), &dump_states(&[att(1, "2026-01-03T00:00:00.000Z")], &s), now, None);
        assert_eq!(q["waiting"], json!(["DM-5"]));
        assert_eq!(q["retrying"][0]["dump"], "DM-4");
        assert_eq!(q["not_taken"][0]["dump"], "DM-3");
        // The same open attempt, while the service runs it: running.
        let q = queue(&e, "dev", Some(&received), &dump_states(&[att(1, "2026-01-03T00:00:00.000Z")], &s), now, Some("DM-4"));
        assert_eq!((q["running"][0]["dump"].clone(), q["retrying"].clone()), (json!("DM-4"), json!([])), "{q}");
    }

    /// **The threads hold, as status shows it, and the order cauldrons are
    /// served in**: since the first park of a streak, the newest park's cause
    /// and wait, which dump set it; over once its wait is; ended by any other
    /// outcome on threads. The cauldron served least recently goes first.
    #[test]
    fn the_hold_is_shown_and_the_least_recently_served_cauldron_goes_first() {
        let park = |c: &str, at: &str, until: &str| json!({"event": "outcome", "backend": "threads", "held_by": "threads", "class": "deferred", "park": "threshold", "cauldron": c, "dump": "DM-1", "at": at, "retry_after": until});
        let lines = vec![park("aa", "2026-01-01T00:00:00.000Z", "2026-01-01T00:02:00.000Z"), park("kb", "2026-01-01T00:02:01.000Z", "2026-01-01T00:10:00.000Z")];
        let h = threads_hold(&lines);
        assert_eq!((h.parks, h.since.as_deref(), h.until.as_deref(), h.cauldron.as_deref()), (2, Some("2026-01-01T00:00:00.000Z"), Some("2026-01-01T00:10:00.000Z"), Some("kb")));
        let v = h.view("2026-01-01T00:05:00.000Z");
        assert_eq!((v["state"].clone(), v["park"].clone(), v["next_attempt"].clone(), v["set_by"]["cauldron"].clone()), (json!("holding"), json!("threshold"), json!("2026-01-01T00:10:00.000Z"), json!("kb")), "{v}");
        assert_eq!(h.view("2026-01-01T00:11:00.000Z")["state"], "over");
        // Marked on a threads cauldron's queue while it holds; never on a local one.
        let s = setting(0, false);
        let mut q = json!({"automatic": "on", "waiting": ["DM-3"], "deferred": [{"dump": "DM-2"}], "retrying": []});
        h.mark(&mut q, &s, "2026-01-01T00:05:00.000Z");
        assert_eq!((q["held"]["by"].clone(), q["held"]["dumps"].clone()), (json!("threads_hold"), json!(["DM-3", "DM-2"])), "{q}");
        let mut q2 = json!({"automatic": "on", "waiting": ["DM-3"]});
        h.mark(&mut q2, &Settings { backend: "local".into(), ..s.clone() }, "2026-01-01T00:05:00.000Z");
        assert!(q2.get("held").is_none());
        // Any other outcome on threads ends it.
        let mut more = lines.clone();
        more.push(json!({"event": "outcome", "backend": "threads", "class": "done", "cauldron": "aa", "dump": "DM-1"}));
        assert_eq!(threads_hold(&more), ThreadsHold::default());
        assert!(threads_hold(&more).view("now").is_null());
        // Served least recently first: never served, then the oldest attempt.
        let entry = |name: &str| Entry { name: name.into(), settings: Settings { cauldron: name.into(), ..s.clone() }, void: None };
        let att = |c: &str, at: &str| json!({"event": "attempt", "cauldron": c, "bound": s.bound_key(), "at": at});
        let mut es = vec![entry("aa"), entry("kb"), entry("zz")];
        serve_order(&mut es, &[att("aa", "2026-01-01T00:03:00.000Z"), att("kb", "2026-01-01T00:01:00.000Z")]);
        assert_eq!(es.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["zz", "kb", "aa"]);
        serve_order(&mut es, &[]);
        assert_eq!(es.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["aa", "kb", "zz"]);
    }

    /// **A setting belongs to one cauldron**: void when the cauldron is
    /// gone, made again, renamed away, or the file was copied from another
    /// cauldron or environment, or carries no binding.
    #[test]
    fn a_setting_is_void_unless_it_belongs_to_the_cauldron_there_now() {
        let root = scratch("bind");
        let data = root.join("data").join("dev");
        let e = crate::Environment {
            root: root.clone(),
            root_source: crate::paths::Source::CauldronHome,
            name: "dev".into(),
            name_source: crate::paths::Source::CauldronEnv,
            data_root: data.clone(),
        };
        let make = |name: &str, at: &str| {
            let d = data.join("cauldrons").join(name);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("cauldron.toml"), format!("name = \"{name}\"\ncreated_at = \"{at}\"\nenabled = true\n")).unwrap();
        };
        make("kb", "2026-01-01T00:00:00Z");
        let c = e.find_cauldron("kb").unwrap().unwrap();
        let s = Settings { bound_to: Some(Binding::of(&e, &c)), ..setting(0, false) };
        store(&data, &s).unwrap();
        assert!(is_on(&e, "kb"));
        assert_eq!(all(&e).len(), 1);
        assert!(all(&e)[0].runs());
        // Copied to another name: void (the file's own name and its content
        // disagree), and not on for that name.
        make("other", "2026-01-01T00:00:00Z");
        std::fs::copy(path(&data, "kb"), path(&data, "other")).unwrap();
        assert!(!is_on(&e, "other"));
        assert!(effective(&e, "other").unwrap().unwrap().void.unwrap().contains("copied or edited"));
        // Copied from another environment: void.
        store(&data, &Settings { bound_to: Some(Binding { env: "prod".into(), ..s.bound_to.clone().unwrap() }), ..s.clone() }).unwrap();
        assert!(!is_on(&e, "kb"));
        // No binding: void.
        store(&data, &Settings { bound_to: None, ..s.clone() }).unwrap();
        assert!(!is_on(&e, "kb"));
        store(&data, &s).unwrap();
        assert!(is_on(&e, "kb"));
        // Moved away (removed or renamed): void, "gone"; moved back: the same
        // cauldron, so it is its setting again.
        let away = data.join("cauldrons").join(".removed");
        std::fs::create_dir_all(&away).unwrap();
        std::fs::rename(data.join("cauldrons").join("kb"), away.join("kb-1")).unwrap();
        assert!(!is_on(&e, "kb"));
        assert!(effective(&e, "kb").unwrap().unwrap().void.unwrap().contains("gone"));
        let q = queue(&effective(&e, "kb").unwrap().unwrap(), "dev", None, &BTreeMap::new(), "now", None);
        assert_eq!(q["automatic"], "void");
        assert!(q["what_to_do"].as_str().unwrap().contains("cauldron ripples off kb --env dev"));
        assert_eq!(receipt(&e, "kb", "DM-1", "text")["takes_this_dump"], false);
        std::fs::rename(away.join("kb-1"), data.join("cauldrons").join("kb")).unwrap();
        assert!(is_on(&e, "kb"));
        // Made again under the same name, even in the same second (same
        // `created_at`): another directory, so void.
        std::fs::rename(data.join("cauldrons").join("kb"), away.join("kb-2")).unwrap();
        make("kb", "2026-01-01T00:00:00Z");
        assert!(!is_on(&e, "kb"));
        assert_eq!(receipt(&e, "kb", "DM-1", "text")["takes_this_dump"], false);
        // Turned off: kept, off.
        assert!(turn_off(&data, "kb", "2026-02-01T00:00:00.000Z").unwrap().is_some());
        assert!(!load(&data, "kb").unwrap().unwrap().on);
        assert!(turn_off(&data, "kb", "2026-02-01T00:00:00.000Z").unwrap().is_none());
        let _ = std::fs::remove_dir_all(&root);
    }
}

/// **A cauldron's spend identities**: what the ripples journals' `call`
/// lines carry as `cauldron_id` for it (`id:` and its [`Binding::key`];
/// for a cauldron given its id after calls were journaled, also the
/// identity those calls carried, [`Binding::legacy_key`]). A cauldron with
/// no binding at all is `name:<name>`.
pub fn spend_identities(e: &crate::Environment, c: &crate::Cauldron) -> Vec<String> {
    let b = Binding::of(e, c);
    if b.id.is_none() && b.created_at.is_none() && b.dir_ino.is_none() {
        return vec![format!("name:{}", c.name)];
    }
    let mut out = vec![format!("id:{}", b.key())];
    if b.id.is_some() && b.created_at.is_some() {
        out.push(format!("id:{}", b.legacy_key()));
    }
    out
}

/// Whose spend a journal line is, as the spend views group it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Owner {
    /// The group's key in a view: the cauldron's current name; for spend of
    /// a cauldron that is no longer in the environment, the name it was
    /// journaled under followed by ` (gone)`.
    pub key: String,
    /// The cauldron's current name (the journaled one when it is gone).
    pub name: String,
    /// The cauldron's id (`cauldron.toml`), when it has one.
    pub id: Option<String>,
    pub gone: bool,
}

/// **The environment's cauldrons by spend identity**, so the spend views
/// group by the cauldron and not by the name a line was journaled under: a
/// renamed cauldron's earlier spend shows under its current name, and a
/// cauldron removed and made again under its name shows apart from it.
pub struct Owners(Vec<(Vec<String>, String, Option<String>)>);

impl Owners {
    pub fn of(e: &crate::Environment) -> Owners {
        Owners(e.cauldrons().unwrap_or_default().into_iter().map(|c| (spend_identities(e, &c), c.name.clone(), c.id())).collect())
    }

    /// The owner of a line journaled under `name`, carrying `identity`
    /// (`None` on a line an earlier build wrote, which is the cauldron of
    /// that name now).
    pub fn of_line(&self, identity: Option<&str>, name: &str) -> Owner {
        let found = match identity.filter(|i| !i.starts_with("name:")) {
            Some(i) => self.0.iter().find(|(ids, _, _)| ids.iter().any(|x| x == i)),
            None => self.0.iter().find(|(_, n, _)| n == name),
        };
        match found {
            Some((_, n, id)) => Owner { key: n.clone(), name: n.clone(), id: id.clone(), gone: false },
            None => Owner { key: format!("{name} (gone)"), name: name.to_string(), id: None, gone: true },
        }
    }
}
