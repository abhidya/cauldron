//! **Grants: which cauldrons the owner opened to the MCP door.**
//!
//! Every cauldron is closed to the MCP door until the owner grants it, from
//! a terminal, with `cauldron mcp grant`. One file per environment holds
//! every grant:
//!
//! ```json
//! {"version":1,"grants":[{"cauldron":"kb","scopes":["read","write","dump"],
//!   "projects":["/home/u/code/proj"],"granted_at":"2026-01-01T00:00:00Z","by":"tty"}]}
//! ```
//!
//! at `<root>/data/<env>/mcp/grants.json`, mode 0600, in a directory of mode
//! 0700. Every change is a read-modify-write under an exclusive `flock` on
//! `grants.lock` beside it, and lands by writing a new file and renaming it
//! over the old one, so a reader sees the old store or the new one, never
//! half of either, and two writers never lose each other's change.
//!
//! A grant names the cauldron, the scopes it is open for (`read`, `write`,
//! `dump`) and the project directories (canonical paths) whose sessions are
//! pointed at it by default. [`Grants::resolve`] picks the cauldron for a
//! call; [`Grants::authorize`] then checks that call's scope.
//!
//! A grant is bound to the cauldron it was made for, not only to its name:
//! it records that cauldron's id (`cauldron.toml` `id`, 128 random bits
//! minted when the cauldron is made, kept across a rename and a move of the
//! data root, never copied to another cauldron), and its `created_at`. A
//! cauldron removed and made again under the same name has another id, even
//! when its `cauldron.toml` names the same `created_at`, and its
//! predecessor's grant does not open it ([`Code::CauldronGone`]); the owner
//! grants the new one.
//!
//! A cauldron made before cauldrons had ids is given its id by the first
//! writer that opens it ([`adopt`]); at that moment, and only then, a grant
//! that records its `created_at` is carried onto the id. A grant that does
//! not match then is refused ([`Code::NotGranted`]), naming the `cauldron
//! mcp grant` that makes it again. Anyone who can write the data root can
//! write this store too: the terminal check of `cauldron mcp grant` guards
//! the command, not the file.
//!
//! A grant that records no identity (written before grants carried this
//! field, which every release that made grants has done) is NOT honoured
//! when the cauldron of its name has an identity: it is refused as
//! [`Code::NotGranted`], naming the `cauldron mcp grant` that makes it
//! again with its scopes and projects. It is never bound on first use,
//! because a name-only grant cannot tell the cauldron it was made for from
//! one made since under the same name, and binding it then would adopt the
//! newcomer for good; only the owner, at a terminal, binds a grant. A
//! cauldron that has no identity itself (the implicit legacy cauldron, or a
//! `cauldron.toml` without `created_at`) has nothing to bind to, and its
//! grant is honoured by name.
//!
//! The command line's project marker (`.cauldron`) plays no part here: a
//! marker never opens a cauldron to the MCP door, and is never read by it.

use std::fs::File;
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use env::Environment;
use serde::{Deserialize, Serialize};

use crate::marker::name_ok;
use crate::{refuse, Code, Error, Result};

/// The store's directory under an environment's data root.
pub const DIR: &str = "mcp";
/// The store's file name.
pub const FILE: &str = "grants.json";
/// The lock file every writer of the store holds.
pub const LOCK: &str = "grants.lock";
/// The store's format version.
pub const VERSION: u32 = 1;
/// The variable that names a cauldron for a session.
pub const ENV_VAR: &str = "CAULDRON_CAULDRON";

/// What a grant opens a cauldron for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Read,
    Write,
    Dump,
}

impl Scope {
    /// Every scope, in order; the default of a grant.
    pub const ALL: [Scope; 3] = [Scope::Read, Scope::Write, Scope::Dump];

    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Read => "read",
            Scope::Write => "write",
            Scope::Dump => "dump",
        }
    }

    pub fn parse(s: &str) -> Option<Scope> {
        Scope::ALL.into_iter().find(|x| x.as_str() == s)
    }

    /// `read,write,dump` into scopes, in order and without repeats.
    pub fn parse_list(s: &str) -> Result<Vec<Scope>> {
        let mut out = Vec::new();
        for w in s.split(',').map(str::trim).filter(|w| !w.is_empty()) {
            let Some(x) = Scope::parse(w) else {
                refuse!(Code::Usage, "REFUSED: `{w}` is not a scope; the scopes are read, write and dump");
            };
            out.push(x);
        }
        if out.is_empty() {
            refuse!(Code::Usage, "REFUSED: no scope was named; the scopes are read, write and dump");
        }
        Ok(sorted(out))
    }
}

fn sorted(mut v: Vec<Scope>) -> Vec<Scope> {
    v.sort();
    v.dedup();
    v
}

/// `read,write` for a list of scopes.
pub fn scopes_text(scopes: &[Scope]) -> String {
    scopes.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(",")
}

/// One cauldron's grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    pub cauldron: String,
    pub scopes: Vec<Scope>,
    /// Canonical project directories.
    #[serde(default)]
    pub projects: Vec<String>,
    pub granted_at: String,
    /// How the grant was made (`tty`: by a person at a terminal).
    pub by: String,
    /// The id of the cauldron this grant was made for (`cauldron.toml`
    /// `id`; see the module docs). Absent in a grant written before
    /// cauldrons had ids.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cauldron_id: Option<String>,
    /// The `created_at` of the cauldron this grant was made for (see the
    /// module docs). Absent in a grant written before this field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cauldron_created_at: Option<String>,
}

impl Grant {
    pub fn allows(&self, scope: Scope) -> bool {
        self.scopes.contains(&scope)
    }

    /// Was this grant made for `c` (the cauldron that has this name now)?
    /// A grant that recorded no identity is, only when `c` has none either
    /// (module docs).
    pub fn is_for(&self, c: &env::Cauldron) -> bool {
        match (&self.cauldron_id, c.id()) {
            (Some(want), Some(have)) => *want == have,
            // Bound to an id: never a cauldron without that id.
            (Some(_), None) => false,
            // Written before ids, and the cauldron has one: bound to it only
            // when it was given (`adopt`), so this one was not carried.
            (None, Some(_)) => false,
            // Neither has an id yet (before the cauldron's first writer):
            // the creation time, as before.
            (None, None) => match (&self.cauldron_created_at, identity(c)) {
                (None, None) => true,
                (None, Some(_)) => false,
                (Some(want), have) => have.as_deref() == Some(want.as_str()),
            },
        }
    }

    /// Does this grant name its cauldron only by name, while that cauldron
    /// has an identity (a grant to make again)?
    pub fn name_only_for(&self, c: &env::Cauldron) -> bool {
        self.cauldron_id.is_none() && self.cauldron_created_at.is_none() && identity(c).is_some()
    }

    /// Was this grant written before cauldrons had ids, and not carried onto
    /// the id its cauldron has now (a grant to make again)?
    pub fn not_carried_for(&self, c: &env::Cauldron) -> bool {
        self.cauldron_id.is_none() && self.cauldron_created_at.is_some() && c.id().is_some()
    }
}

/// What a grant records of the cauldron it is for: its `created_at`.
pub fn identity(c: &env::Cauldron) -> Option<String> {
    c.meta().ok().and_then(|m| m.created_at)
}

/// The whole store of one environment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grants {
    pub version: u32,
    #[serde(default)]
    pub grants: Vec<Grant>,
}

impl Default for Grants {
    fn default() -> Grants {
        Grants { version: VERSION, grants: Vec::new() }
    }
}

/// Where a call's cauldron came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedFrom {
    /// Named by the call itself.
    Arg,
    /// `$CAULDRON_CAULDRON` in the session's environment.
    EnvVar,
    /// The grant whose project contains the working directory: that project.
    Project(String),
}

impl ResolvedFrom {
    /// `arg`, `CAULDRON_CAULDRON` or `project:<granted path>`.
    pub fn as_text(&self) -> String {
        match self {
            ResolvedFrom::Arg => "arg".to_string(),
            ResolvedFrom::EnvVar => ENV_VAR.to_string(),
            ResolvedFrom::Project(p) => format!("project:{p}"),
        }
    }
}

/// The cauldron a call is about, and where that came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    pub cauldron: String,
    pub from: ResolvedFrom,
}

/// `<data root>/mcp`.
pub fn store_dir(e: &Environment) -> PathBuf {
    e.data_root.join(DIR)
}

/// `<data root>/mcp/grants.json`.
pub fn store_path(e: &Environment) -> PathBuf {
    store_dir(e).join(FILE)
}

fn store_err(what: String, err: &dyn std::error::Error) -> Error {
    Error::caused(Code::GrantStore, what, err)
}

impl Grants {
    /// The environment's store; an absent store is an empty one (every
    /// cauldron closed). A store that cannot be read or parsed is an error,
    /// never an empty store.
    pub fn load(e: &Environment) -> Result<Grants> {
        Grants::load_from(&store_path(e))
    }

    /// The store at `path` (see [`Grants::load`]).
    pub fn load_from(path: &Path) -> Result<Grants> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Grants::default()),
            Err(err) => return Err(store_err(format!("reading the grant store {}", path.display()), &err)),
        };
        let g: Grants = serde_json::from_str(&text).map_err(|err| store_err(format!("the grant store {} is not readable", path.display()), &err))?;
        if g.version != VERSION {
            refuse!(
                Code::GrantStore,
                "the grant store {} is version {}, and this build reads version {VERSION}; nothing was opened",
                path.display(),
                g.version
            );
        }
        Ok(g)
    }

    /// The grant of `cauldron`, if any.
    pub fn get(&self, cauldron: &str) -> Option<&Grant> {
        self.grants.iter().find(|g| g.cauldron == cauldron)
    }

    /// **Which cauldron a call is about.** In order: `explicit` (the call
    /// named it), else `env_var` (`$CAULDRON_CAULDRON` in the session), else
    /// the grant one of whose projects contains `cwd`, compared as canonical
    /// paths, the longest project winning. No match refuses
    /// ([`Code::NoCauldronForProject`]), and two different cauldrons granted
    /// for equally long projects refuse ([`Code::AmbiguousProject`]); there
    /// is never a fallback. A name that is resolved is not yet authorized:
    /// see [`Grants::authorize`]. `env` is only for the messages.
    pub fn resolve(&self, env: &str, cwd: Option<&Path>, explicit: Option<&str>, env_var: Option<&str>) -> Result<Resolution> {
        for (named, from) in [(explicit, ResolvedFrom::Arg), (env_var, ResolvedFrom::EnvVar)] {
            if let Some(n) = named.map(str::trim).filter(|n| !n.is_empty()) {
                if !name_ok(n) {
                    refuse!(
                        Code::Usage,
                        "`{n}` (from {}) is not a usable cauldron name ([a-z0-9_-], 1-32 characters, starting with [a-z0-9]). Nothing was read or written.",
                        from.as_text()
                    );
                }
                return Ok(Resolution { cauldron: n.to_string(), from });
            }
        }
        let Some(cwd) = cwd else {
            refuse!(
                Code::NoCauldronForProject,
                "no cauldron was named and no project directory is known, so no cauldron in environment `{env}` can be \
                 chosen. Name one (the `cauldron` argument), or the owner runs `cauldron mcp grant <cauldron> --env {env} \
                 --project <dir>` at a terminal on this machine. Nothing was read or written."
            );
        };
        let here = cwd.canonicalize().map_err(|err| {
            Error::new(
                Code::NoCauldronForProject,
                format!(
                    "the project directory {} cannot be resolved ({err}), so no grant in environment `{env}` can be \
                     matched to it. Name a cauldron (the `cauldron` argument). Nothing was read or written.",
                    cwd.display()
                ),
            )
        })?;
        // (depth, cauldron, project) of every grant project containing `here`.
        let mut best: Option<(usize, Vec<(String, String)>)> = None;
        for g in &self.grants {
            for p in &g.projects {
                let pp = Path::new(p);
                if !here.starts_with(pp) {
                    continue;
                }
                let depth = pp.components().count();
                match &mut best {
                    Some((d, v)) if *d == depth => v.push((g.cauldron.clone(), p.clone())),
                    Some((d, _)) if *d > depth => {}
                    _ => best = Some((depth, vec![(g.cauldron.clone(), p.clone())])),
                }
            }
        }
        let Some((_, mut hits)) = best else {
            refuse!(
                Code::NoCauldronForProject,
                "no cauldron was named, and no grant in environment `{env}` names {} as its project. Name one (the \
                 `cauldron` argument), or the owner runs `cauldron mcp grant <cauldron> --env {env} --project {}` at a \
                 terminal on this machine to make one this directory's default. Nothing was read or written.",
                here.display(),
                here.display()
            );
        };
        hits.sort();
        hits.dedup_by(|a, b| a.0 == b.0);
        if hits.len() > 1 {
            refuse!(
                Code::AmbiguousProject,
                "{} is inside projects granted to more than one cauldron in environment `{env}`, at the same depth: {}. \
                 Name the cauldron (the `cauldron` argument), or the owner narrows a grant with `cauldron mcp revoke \
                 <cauldron> --env {env} --project <dir>`. Nothing was read or written.",
                here.display(),
                hits.iter().map(|(c, p)| format!("`{c}` ({p})")).collect::<Vec<_>>().join(", ")
            );
        }
        let (cauldron, project) = hits.remove(0);
        Ok(Resolution { cauldron, from: ResolvedFrom::Project(project) })
    }

    /// **May a call with `scope` reach `cauldron`?** In order: a grant
    /// exists, it holds the scope ([`Code::NotGranted`] for either), the
    /// cauldron still exists ([`Code::CauldronGone`]), and it has a soup
    /// ([`Code::NoSoup`]).
    pub fn authorize(&self, e: &Environment, cauldron: &str, scope: Scope) -> Result<&Grant> {
        let env = &e.name;
        let Some(g) = self.get(cauldron) else {
            refuse!(
                Code::NotGranted,
                "Cauldron `{cauldron}` in environment `{env}` is closed to the MCP door: it has no grant. Nothing was \
                 read or written. The owner opens it from a terminal on this machine with `cauldron mcp grant \
                 {cauldron} --env {env}`."
            );
        };
        if !g.allows(scope) {
            let mut want = g.scopes.clone();
            want.push(scope);
            refuse!(
                Code::NotGranted,
                "Cauldron `{cauldron}` in environment `{env}` is closed to the MCP door: it has no `{}` grant. Nothing \
                 was read or written. The owner opens it from a terminal on this machine with `cauldron mcp grant \
                 {cauldron} --env {env} --scopes {}`.",
                scope.as_str(),
                scopes_text(&sorted(want))
            );
        }
        let Some(c) = e.find_cauldron(cauldron).map_err(|err| Error::from_anyhow(Code::CauldronGone, err))? else {
            refuse!(
                Code::CauldronGone,
                "Cauldron `{cauldron}` in environment `{env}` is granted to the MCP door but no longer exists. Nothing \
                 was read or written. The owner drops the grant with `cauldron mcp revoke {cauldron} --env {env}`."
            );
        };
        if g.name_only_for(&c) {
            refuse!(
                Code::NotGranted,
                "Cauldron `{cauldron}` in environment `{env}` has a grant that names it only by name: it was written by \
                 an older build, before a grant recorded which cauldron it was made for, so it cannot tell this cauldron \
                 from one made since under the same name, and it is not honoured. Nothing was read or written. The owner \
                 grants it again from a terminal on this machine with `{}`.",
                grant_command(env, cauldron, &g.scopes, &g.projects)
            );
        }
        if g.not_carried_for(&c) {
            refuse!(
                Code::NotGranted,
                "Cauldron `{cauldron}` in environment `{env}` has a grant written before cauldrons had ids, and when \
                 this cauldron was given its id the grant did not match it (it records another creation time), so it \
                 was not carried onto it and is not honoured. Nothing was read or written. The owner grants it again \
                 from a terminal on this machine with `{}`.",
                grant_command(env, cauldron, &g.scopes, &g.projects)
            );
        }
        if !g.is_for(&c) {
            refuse!(
                Code::CauldronGone,
                "Cauldron `{cauldron}` in environment `{env}` is not the cauldron that was granted to the MCP door: the \
                 granted one (created {}) is gone, and this one was made since (created {}). A grant never passes to a \
                 new cauldron that takes an old one's name. Nothing was read or written. The owner opens this one from a \
                 terminal on this machine with `cauldron mcp grant {cauldron} --env {env}`, or drops the old grant with \
                 `cauldron mcp revoke {cauldron} --env {env}`.",
                g.cauldron_created_at.as_deref().unwrap_or("?"),
                identity(&c).unwrap_or_else(|| "at an unknown time".into())
            );
        }
        if !c.soup().join(spine::spine_db::SPINE_FILE).is_file() {
            refuse!(Code::NoSoup, "Cauldron `{cauldron}` in environment `{env}` has no soup yet. Nothing was read or written.");
        }
        Ok(g)
    }
}

/// **Change the store under its lock**: take `grants.lock` exclusive
/// (waiting for another writer), read the store, let `f` change it, and
/// write it back as a new file renamed into place, mode 0600. Nothing is
/// written when `f` fails.
pub fn update<T>(e: &Environment, f: impl FnOnce(&mut Grants) -> Result<T>) -> Result<T> {
    let dir = store_dir(e);
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
        .map_err(|err| store_err(format!("creating {}", dir.display()), &err))?;
    let lock_path = dir.join(LOCK);
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(&lock_path)
        .map_err(|err| store_err(format!("opening {}", lock_path.display()), &err))?;
    lock.lock().map_err(|err| store_err(format!("locking {}", lock_path.display()), &err))?;
    let path = dir.join(FILE);
    let mut g = Grants::load_from(&path)?;
    let out = f(&mut g)?;
    write_store(&dir, &path, &g)?;
    drop(lock);
    Ok(out)
}

fn write_store(dir: &Path, path: &Path, g: &Grants) -> Result<()> {
    let text = serde_json::to_string_pretty(g).map_err(|err| store_err("writing the grant store".to_string(), &err))? + "\n";
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let tmp = dir.join(format!("{FILE}.tmp-{}-{nanos}", std::process::id()));
    let written = (|| -> std::io::Result<()> {
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)?;
        File::open(dir)?.sync_all()
    })();
    if let Err(err) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(store_err(format!("writing {}", path.display()), &err));
    }
    Ok(())
}

/// A project directory as a grant stores it: canonical, an existing
/// directory, and UTF-8.
pub fn canonical_project(p: &Path) -> Result<String> {
    let c = p
        .canonicalize()
        .map_err(|err| Error::caused(Code::Usage, format!("REFUSED: the project directory {} cannot be resolved", p.display()), &err))?;
    if !c.is_dir() {
        refuse!(Code::Usage, "REFUSED: the project {} is not a directory", c.display());
    }
    match c.to_str() {
        Some(s) => Ok(s.to_string()),
        None => refuse!(Code::Usage, "REFUSED: the project directory {} is not UTF-8, and a grant stores it as text", c.display()),
    }
}

/// The cauldron must exist in the environment and have a soup.
pub fn require_cauldron(e: &Environment, cauldron: &str) -> Result<()> {
    if !name_ok(cauldron) {
        refuse!(Code::Usage, "REFUSED: `{cauldron}` is not a usable cauldron name ([a-z0-9_-], 1-32 characters, starting with [a-z0-9])");
    }
    let Some(c) = e.find_cauldron(cauldron).map_err(|err| Error::from_anyhow(Code::NoCauldron, err))? else {
        refuse!(
            Code::NoCauldron,
            "REFUSED: there is no cauldron `{cauldron}` in environment `{}`, so it cannot be granted. `cauldron list --env {}` \
             shows its cauldrons. Nothing was granted.",
            e.name,
            e.name
        );
    };
    if !c.soup().join(spine::spine_db::SPINE_FILE).is_file() {
        refuse!(
            Code::NoSoup,
            "REFUSED: cauldron `{cauldron}` in environment `{}` has no soup yet, so it cannot be granted. Nothing was granted.",
            e.name
        );
    }
    Ok(())
}

/// **The grants that name their cauldron only by name**, while that
/// cauldron has an identity: each is refused on its first call
/// ([`Grant::name_only_for`]). Answers `[{cauldron, note, regrant}]`, one
/// per such grant, so `cauldron mcp status`, `cauldron mcp grants` and the
/// installer can say so before that first refusal. A grant whose cauldron
/// is gone is not listed here (its call is refused as gone).
pub fn name_only(e: &env::Environment, g: &Grants) -> Vec<serde_json::Value> {
    g.grants
        .iter()
        .filter_map(|gr| {
            let c = e.find_cauldron(&gr.cauldron).ok().flatten()?;
            (gr.name_only_for(&c) || gr.not_carried_for(&c)).then(|| {
                let regrant = grant_command(&e.name, &gr.cauldron, &gr.scopes, &gr.projects);
                let note = if gr.name_only_for(&c) {
                    name_only_note(&gr.cauldron, &e.name, &regrant)
                } else {
                    format!(
                        "the grant of cauldron `{}` in environment `{}` was written before cauldrons had ids and was not carried onto the id the cauldron was given (it records another creation time), so every call to it through the MCP door is refused until it is granted again, in a terminal: {regrant}",
                        gr.cauldron, e.name
                    )
                };
                serde_json::json!({
                    "cauldron": gr.cauldron,
                    "note": note,
                    "regrant": regrant,
                })
            })
        })
        .collect()
}

/// What is said of a grant that names its cauldron only by name.
pub fn name_only_note(cauldron: &str, env: &str, regrant: &str) -> String {
    format!(
        "the grant of cauldron `{cauldron}` in environment `{env}` names it only by name (it was written by a \
         pre-release build), so every call to it through the MCP door is refused until it is granted again, in a \
         terminal: {regrant}"
    )
}

/// The command that makes a grant, as a person types it.
pub fn grant_command(env: &str, cauldron: &str, scopes: &[Scope], projects: &[String]) -> String {
    let mut s = format!("cauldron mcp grant {cauldron} --env {env} --scopes {}", scopes_text(scopes));
    for p in projects {
        s.push_str(" --project ");
        s.push_str(&shell_word(p));
    }
    s
}

/// `p` quoted for a POSIX shell when it needs it.
fn shell_word(p: &str) -> String {
    if !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+,:@".contains(c)) {
        p.to_string()
    } else {
        format!("'{}'", p.replace('\'', "'\\''"))
    }
}

/// **Grant** `cauldron` (which must exist, with a soup) for `scopes`, and
/// add `projects` (already canonical, see [`canonical_project`]). An
/// existing grant keeps its projects, gains the new ones, and takes the new
/// scopes. Answers the grant as stored and whether it is new.
pub fn grant(e: &Environment, cauldron: &str, scopes: &[Scope], projects: &[String], by: &str) -> Result<(Grant, bool)> {
    require_cauldron(e, cauldron)?;
    let c = e.find_cauldron(cauldron).ok().flatten();
    // The cauldron's id, minted now if it has none yet (a grant is a writer).
    let id = match &c {
        Some(c) => adopt(e, c)?,
        None => None,
    };
    let created_at = c.as_ref().and_then(identity);
    let scopes = sorted(scopes.to_vec());
    if scopes.is_empty() {
        refuse!(Code::Usage, "REFUSED: a grant needs at least one scope (read, write, dump)");
    }
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    update(e, |g| {
        let created = g.get(cauldron).is_none();
        if created {
            g.grants.push(Grant {
                cauldron: cauldron.to_string(),
                scopes: Vec::new(),
                projects: Vec::new(),
                granted_at: String::new(),
                by: String::new(),
                cauldron_id: None,
                cauldron_created_at: None,
            });
        }
        let entry = g.grants.iter_mut().find(|x| x.cauldron == cauldron).expect("present");
        entry.scopes = scopes.clone();
        for p in projects {
            if !entry.projects.contains(p) {
                entry.projects.push(p.clone());
            }
        }
        entry.granted_at = now.clone();
        entry.by = by.to_string();
        // Granting again binds the grant to the cauldron that has the name now.
        entry.cauldron_id = id.clone();
        entry.cauldron_created_at = created_at.clone();
        g.grants.sort_by(|a, b| a.cauldron.cmp(&b.cauldron));
        Ok((g.get(cauldron).expect("present").clone(), created))
    })
}

/// What a revoke did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Revoked {
    /// The whole grant was removed.
    Grant(Grant),
    /// One project was removed; the grant as it now stands.
    Project { project: String, grant: Grant },
    /// The cauldron had no grant; nothing changed.
    NotGranted,
    /// The grant has no such project; nothing changed.
    NoSuchProject(Grant),
}

/// **Revoke** `cauldron`'s grant, or with `project` only that project's
/// binding (matched as given and as its canonical path, so a project
/// directory that no longer exists can still be unbound).
pub fn revoke(e: &Environment, cauldron: &str, project: Option<&Path>) -> Result<Revoked> {
    let wanted: Option<Vec<String>> = project.map(|p| {
        let mut v = vec![p.display().to_string()];
        if let Ok(c) = p.canonicalize() {
            v.push(c.display().to_string());
        }
        v
    });
    update(e, |g| {
        let Some(i) = g.grants.iter().position(|x| x.cauldron == cauldron) else { return Ok(Revoked::NotGranted) };
        match &wanted {
            None => Ok(Revoked::Grant(g.grants.remove(i))),
            Some(names) => {
                let entry = &mut g.grants[i];
                let Some(j) = entry.projects.iter().position(|p| names.contains(p)) else {
                    return Ok(Revoked::NoSuchProject(entry.clone()));
                };
                let project = entry.projects.remove(j);
                Ok(Revoked::Project { project, grant: entry.clone() })
            }
        }
    })
}

/// Follow a cauldron's rename: its grant, if any, now names `new`. Answers
/// whether a grant was changed. Writes nothing when there is no store.
///
/// `created_at` is the renamed cauldron's identity ([`identity`]; a rename
/// keeps its `cauldron.toml`, so it is the same before and after). The grant
/// keeps the identity it records, so it still opens the renamed cauldron and
/// never one made later under either name; a grant written before grants
/// carried one is bound to `created_at` now. A grant that already named
/// `new` belonged to a cauldron that is gone (the caller has checked no
/// cauldron is called `new`), and is dropped: left, it would answer for the
/// renamed one, and one that records no identity would open it.
pub fn rename(e: &Environment, old: &str, new: &str, created_at: Option<&str>) -> Result<bool> {
    if !store_path(e).exists() {
        return Ok(false);
    }
    update(e, |g| {
        let before = g.grants.len();
        g.grants.retain(|x| x.cauldron != new);
        let mut changed = g.grants.len() != before;
        for x in g.grants.iter_mut().filter(|x| x.cauldron == old) {
            x.cauldron = new.to_string();
            if x.cauldron_created_at.is_none() {
                x.cauldron_created_at = created_at.map(str::to_string);
            }
            changed = true;
        }
        g.grants.sort_by(|a, b| a.cauldron.cmp(&b.cauldron));
        Ok(changed)
    })
}

/// **Give cauldron `c` its id, if it has none yet** (a cauldron made before
/// cauldrons had ids), and answer it; `None` for a cauldron with no
/// manifest (the legacy cauldron, or a directory without `cauldron.toml`),
/// which has nothing to hold one. Called by every writer that opens a
/// cauldron: the `mcp` service before a call reaches a granted one, and
/// `cauldron mcp grant`, `cauldron ripples on` and `cauldron rename`.
///
/// At that moment, and only then, what was bound to the cauldron before
/// ids is carried onto the new id: its grant, when the grant records the
/// cauldron's `created_at`; its automatic ripples setting, when the setting
/// records its `created_at` and directory inode
/// (`env::ripples_auto::adopt_id`). Whatever does not match stays unbound
/// and is refused, naming the command that makes it again. Done under the
/// grant store's lock, so two writers mint one id. The manifest is written
/// whole (a new file renamed over the old one, owner-only).
pub fn adopt(e: &Environment, c: &env::Cauldron) -> Result<Option<String>> {
    let Some(manifest) = c.manifest().filter(|m| m.is_file()) else { return Ok(None) };
    if let Some(id) = c.id() {
        return Ok(Some(id));
    }
    let dir = store_dir(e);
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
        .map_err(|err| store_err(format!("creating {}", dir.display()), &err))?;
    let lock_path = dir.join(LOCK);
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(&lock_path)
        .map_err(|err| store_err(format!("opening {}", lock_path.display()), &err))?;
    lock.lock().map_err(|err| store_err(format!("locking {}", lock_path.display()), &err))?;
    let mut meta = c.meta().map_err(|err| Error::from_anyhow(Code::Store, err))?;
    if let Some(id) = meta.id.clone() {
        return Ok(Some(id));
    }
    let id = env::cauldron::new_cauldron_id().map_err(|err| store_err("minting a cauldron id".to_string(), &err))?;
    meta.id = Some(id.clone());
    let text = meta.to_toml().map_err(|err| Error::from_anyhow(Code::Store, err))?;
    let tmp = manifest.with_file_name(format!(".{}.id-{}", env::cauldron::CAULDRON_MANIFEST, std::process::id()));
    let written = (|| -> std::io::Result<()> {
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, &manifest)?;
        if let Some(d) = manifest.parent() {
            File::open(d)?.sync_all()?;
        }
        Ok(())
    })();
    if let Err(err) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(store_err(format!("writing the cauldron's id into {}", manifest.display()), &err));
    }
    // The grant and the setting written for this very cauldron, onto it.
    let path = dir.join(FILE);
    if path.exists() {
        let mut g = Grants::load_from(&path)?;
        let mut changed = false;
        for x in g.grants.iter_mut().filter(|x| x.cauldron == c.name && x.cauldron_id.is_none()) {
            if x.cauldron_created_at.is_some() && x.cauldron_created_at == meta.created_at {
                x.cauldron_id = Some(id.clone());
                changed = true;
            }
        }
        if changed {
            write_store(&dir, &path, &g)?;
        }
    }
    env::ripples_auto::adopt_id(e, c, &id).map_err(|err| store_err("carrying the automatic ripples setting onto the cauldron's id".to_string(), &err))?;
    drop(lock);
    Ok(Some(id))
}

/// Follow a cauldron's removal: drop its grant. Answers whether one was
/// dropped. Writes nothing when there is no store.
pub fn forget(e: &Environment, cauldron: &str) -> Result<bool> {
    if !store_path(e).exists() {
        return Ok(false);
    }
    update(e, |g| {
        let before = g.grants.len();
        g.grants.retain(|x| x.cauldron != cauldron);
        Ok(g.grants.len() != before)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(cauldron: &str, projects: &[&str]) -> Grant {
        Grant {
            cauldron: cauldron.into(),
            scopes: Scope::ALL.to_vec(),
            projects: projects.iter().map(|p| p.to_string()).collect(),
            granted_at: "2026-01-01T00:00:00Z".into(),
            by: "tty".into(),
            cauldron_id: None,
            cauldron_created_at: None,
        }
    }

    fn scratch(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "soup-door-grants-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p.canonicalize().unwrap()
    }

    fn env_at(root: &Path) -> Environment {
        Environment {
            root: root.to_path_buf(),
            root_source: env::paths::Source::CauldronHome,
            name: "dev".into(),
            name_source: env::paths::Source::Default,
            data_root: root.join("data").join("dev"),
        }
    }

    #[test]
    fn nothing_is_granted_by_default() {
        let root = scratch("closed");
        let e = env_at(&root);
        let store = Grants::load(&e).unwrap();
        assert!(store.grants.is_empty());
        let err = store.resolve("dev", Some(&root), None, None).unwrap_err();
        assert_eq!(err.code, Code::NoCauldronForProject, "{err}");
        assert!(err.message.contains("cauldron mcp grant <cauldron> --env dev --project"), "{err}");
        let err = store.authorize(&e, "kb", Scope::Read).unwrap_err();
        assert_eq!(err.code, Code::NotGranted);
        assert!(err.message.contains("closed to the MCP door") && err.message.contains("cauldron mcp grant kb --env dev"), "{err}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_explicit_name_then_the_variable_then_the_longest_project() {
        let root = scratch("order");
        let outer = root.join("code");
        let inner = outer.join("proj");
        let deep = inner.join("src").join("deep");
        std::fs::create_dir_all(&deep).unwrap();
        let store = Grants {
            version: 1,
            grants: vec![g("wide", &[outer.to_str().unwrap()]), g("narrow", &[inner.to_str().unwrap()])],
        };
        let r = store.resolve("dev", Some(&deep), Some("kb"), Some("other")).unwrap();
        assert_eq!((r.cauldron.as_str(), r.from.as_text()), ("kb", "arg".to_string()));
        let r = store.resolve("dev", Some(&deep), None, Some("other")).unwrap();
        assert_eq!((r.cauldron.as_str(), r.from.as_text()), ("other", "CAULDRON_CAULDRON".to_string()));
        let r = store.resolve("dev", Some(&deep), Some(" "), Some("")).unwrap();
        assert_eq!(r.cauldron, "narrow");
        assert_eq!(r.from, ResolvedFrom::Project(inner.to_str().unwrap().to_string()));
        let r = store.resolve("dev", Some(&outer), None, None).unwrap();
        assert_eq!(r.cauldron, "wide");
        // A sibling whose name only shares a prefix is not inside.
        let sibling = outer.join("proj-two");
        std::fs::create_dir_all(&sibling).unwrap();
        assert_eq!(store.resolve("dev", Some(&sibling), None, None).unwrap().cauldron, "wide");
        assert_eq!(store.resolve("dev", Some(&root), None, None).unwrap_err().code, Code::NoCauldronForProject);
        let err = store.resolve("dev", Some(&deep), Some("../x"), None).unwrap_err();
        assert_eq!(err.code, Code::Usage);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_symlinked_working_directory_is_matched_by_its_real_path() {
        let root = scratch("symlink");
        let proj = root.join("proj");
        std::fs::create_dir_all(proj.join("sub")).unwrap();
        std::os::unix::fs::symlink(&proj, root.join("alias")).unwrap();
        let store = Grants { version: 1, grants: vec![g("kb", &[proj.to_str().unwrap()])] };
        let r = store.resolve("dev", Some(&root.join("alias").join("sub")), None, None).unwrap();
        assert_eq!(r.cauldron, "kb");
        assert_eq!(r.from.as_text(), format!("project:{}", proj.display()));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn two_cauldrons_at_the_same_depth_refuse_as_ambiguous() {
        let root = scratch("ambiguous");
        let proj = root.join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        let p = proj.to_str().unwrap();
        let store = Grants { version: 1, grants: vec![g("a", &[p]), g("b", &[p])] };
        let err = store.resolve("dev", Some(&proj), None, None).unwrap_err();
        assert_eq!(err.code, Code::AmbiguousProject);
        assert!(err.message.contains("`a`") && err.message.contains("`b`"), "{err}");
        // The same cauldron twice is not ambiguous.
        let store = Grants { version: 1, grants: vec![g("a", &[p, p])] };
        assert_eq!(store.resolve("dev", Some(&proj), None, None).unwrap().cauldron, "a");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_scope_the_grant_lacks_is_refused_naming_the_command_that_adds_it() {
        let root = scratch("scope");
        let e = env_at(&root);
        let mut read_only = g("kb", &[]);
        read_only.scopes = vec![Scope::Read];
        let store = Grants { version: 1, grants: vec![read_only] };
        let err = store.authorize(&e, "kb", Scope::Write).unwrap_err();
        assert_eq!(err.code, Code::NotGranted);
        assert!(err.message.contains("no `write` grant") && err.message.contains("--scopes read,write"), "{err}");
        // Granted, but the cauldron is gone.
        let err = store.authorize(&e, "kb", Scope::Read).unwrap_err();
        assert_eq!(err.code, Code::CauldronGone, "{err}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_store_is_written_whole_with_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let root = scratch("store");
        let e = env_at(&root);
        update(&e, |g| {
            g.grants.push(super::tests::g("kb", &["/p"]));
            Ok(())
        })
        .unwrap();
        let mode = std::fs::metadata(store_path(&e)).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let dir_mode = std::fs::metadata(store_dir(&e)).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700);
        assert_eq!(Grants::load(&e).unwrap().grants.len(), 1);
        // A failed change writes nothing.
        let err = update(&e, |g| -> Result<()> {
            g.grants.clear();
            Err(Error::new(Code::Usage, "no"))
        });
        assert!(err.is_err());
        assert_eq!(Grants::load(&e).unwrap().grants.len(), 1);
        // No temporary file is left behind.
        let names: Vec<String> = std::fs::read_dir(store_dir(&e)).unwrap().map(|x| x.unwrap().file_name().into_string().unwrap()).collect();
        assert!(names.iter().all(|n| n == FILE || n == LOCK), "{names:?}");
        // A store that does not parse is an error, never an empty store.
        std::fs::write(store_path(&e), "{not json").unwrap();
        assert_eq!(Grants::load(&e).unwrap_err().code, Code::GrantStore);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_rename_or_a_removal_carries_the_grant() {
        let root = scratch("follow");
        let e = env_at(&root);
        // No store: nothing is written.
        assert!(!rename(&e, "kb", "kb2", None).unwrap());
        assert!(!forget(&e, "kb").unwrap());
        assert!(!store_path(&e).exists());
        update(&e, |g| {
            g.grants.push(super::tests::g("kb", &["/p"]));
            g.grants.push(super::tests::g("other", &[]));
            // A stale grant under the new name (its cauldron is gone).
            g.grants.push(super::tests::g("kb2", &["/stale"]));
            Ok(())
        })
        .unwrap();
        assert!(rename(&e, "kb", "kb2", Some("2026-01-02T00:00:00Z")).unwrap());
        let after = Grants::load(&e).unwrap();
        assert_eq!(after.grants.iter().filter(|x| x.cauldron == "kb2").count(), 1, "the stale grant was kept");
        let moved = after.get("kb2").unwrap();
        assert_eq!(moved.projects, ["/p"]);
        // A grant with no identity is bound to the renamed cauldron's.
        assert_eq!(moved.cauldron_created_at.as_deref(), Some("2026-01-02T00:00:00Z"));
        // One with an identity keeps it.
        update(&e, |g| {
            g.grants.iter_mut().find(|x| x.cauldron == "kb2").unwrap().cauldron_created_at = Some("2026-01-03T00:00:00Z".into());
            Ok(())
        })
        .unwrap();
        assert!(rename(&e, "kb2", "kb3", Some("2026-01-04T00:00:00Z")).unwrap());
        assert_eq!(Grants::load(&e).unwrap().get("kb3").unwrap().cauldron_created_at.as_deref(), Some("2026-01-03T00:00:00Z"));
        assert!(rename(&e, "kb3", "kb2", None).unwrap());
        assert!(forget(&e, "kb2").unwrap());
        assert!(!forget(&e, "kb2").unwrap());
        let left: Vec<String> = Grants::load(&e).unwrap().grants.into_iter().map(|g| g.cauldron).collect();
        assert_eq!(left, ["other"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn scopes_parse_in_order_without_repeats() {
        assert_eq!(Scope::parse_list("dump,read,read").unwrap(), vec![Scope::Read, Scope::Dump]);
        assert!(Scope::parse_list("read,admin").is_err());
        assert!(Scope::parse_list("").is_err());
        assert_eq!(
            grant_command("dev", "kb", &[Scope::Read], &["/a b".into(), "/c".into()]),
            "cauldron mcp grant kb --env dev --scopes read --project '/a b' --project /c"
        );
    }

    #[test]
    fn a_cauldron_made_again_under_a_granted_name_is_not_opened_by_the_old_grant() {
        let root = scratch("identity");
        let e = env_at(&root);
        let make = |created: &str| {
            let dir = e.data_root.join("cauldrons").join("kb");
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("soup")).unwrap();
            std::fs::write(dir.join("cauldron.toml"), format!("name = \"kb\"\ncreated_at = \"{created}\"\nenabled = true\n")).unwrap();
            std::fs::write(dir.join("soup").join(spine::spine_db::SPINE_FILE), b"").unwrap();
        };
        make("2026-01-01T00:00:00Z");
        let (gr, _) = grant(&e, "kb", &Scope::ALL, &[], "tty").unwrap();
        assert_eq!(gr.cauldron_created_at.as_deref(), Some("2026-01-01T00:00:00Z"));
        assert!(Grants::load(&e).unwrap().authorize(&e, "kb", Scope::Write).is_ok());
        // Removed and made again under the same name: the old grant does not open it.
        make("2026-02-02T00:00:00Z");
        let err = Grants::load(&e).unwrap().authorize(&e, "kb", Scope::Read).unwrap_err();
        assert_eq!(err.code, Code::CauldronGone, "{err}");
        assert!(err.message.contains("cauldron mcp grant kb --env dev") && err.message.contains("made since"), "{err}");
        // Granting again binds the grant to the new one.
        grant(&e, "kb", &Scope::ALL, &[], "tty").unwrap();
        assert!(Grants::load(&e).unwrap().authorize(&e, "kb", Scope::Read).is_ok());
        // A store written before grants carried the field still loads; its
        // grant is refused (never bound on use), naming the grant to make.
        let old = r#"{"version":1,"grants":[{"cauldron":"kb","scopes":["read"],"projects":["/p"],"granted_at":"2026-01-01T00:00:00Z","by":"tty"}]}"#;
        std::fs::write(store_path(&e), old).unwrap();
        let store = Grants::load(&e).unwrap();
        assert_eq!(store.get("kb").unwrap().cauldron_created_at, None);
        // It is named before any call is made, with the same command.
        let w = name_only(&e, &store);
        assert_eq!(w.len(), 1, "{w:?}");
        assert_eq!((w[0]["cauldron"].as_str(), w[0]["regrant"].as_str()), (Some("kb"), Some("cauldron mcp grant kb --env dev --scopes read --project /p")));
        assert!(w[0]["note"].as_str().unwrap().contains("only by name") && w[0]["note"].as_str().unwrap().ends_with("--project /p"), "{w:?}");
        let err = store.authorize(&e, "kb", Scope::Read).unwrap_err();
        assert_eq!(err.code, Code::NotGranted, "{err}");
        assert!(err.message.contains("only by name") && err.message.contains("cauldron mcp grant kb --env dev --scopes read --project /p"), "{err}");
        assert!(!store.get("kb").unwrap().is_for(&e.find_cauldron("kb").unwrap().unwrap()));
        assert_eq!(Grants::load(&e).unwrap().get("kb").unwrap().cauldron_created_at, None, "nothing was bound");
        // Granting again binds it, and it is no longer named.
        grant(&e, "kb", &[Scope::Read], &[], "tty").unwrap();
        assert!(Grants::load(&e).unwrap().authorize(&e, "kb", Scope::Read).is_ok());
        assert!(name_only(&e, &Grants::load(&e).unwrap()).is_empty());
        // A cauldron with no identity of its own: a name-only grant is honoured,
        // and not named.
        let dir = e.data_root.join("cauldrons").join("kb");
        std::fs::write(dir.join("cauldron.toml"), "name = \"kb\"\nenabled = true\n").unwrap();
        std::fs::write(store_path(&e), old).unwrap();
        assert!(Grants::load(&e).unwrap().authorize(&e, "kb", Scope::Read).is_ok());
        assert!(name_only(&e, &Grants::load(&e).unwrap()).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}
