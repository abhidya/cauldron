//! **`cauldron create | list | rename | remove`** — cauldrons as things you can
//! name, make, rename and put away.
//!
//! A CAULDRON is a directory of the user's data inside an ENVIRONMENT
//! (`data/<env>/cauldrons/<name>/`: `cauldron.toml`, `tui/`, `soup/`, `events/`,
//! `ripples/`). Paths, the name grammar, the listing and the implicit legacy
//! cauldron are all `env::cauldron`'s; this module only WRITES, and only
//! inside `cauldrons/`:
//!
//! - `create` lays the directory in a temp sibling and renames it into place,
//!   so a failure leaves no half-made cauldron.
//! - `rename` / `remove` are refused while a face has the cauldron open (the
//!   face holds `<tui>/open.lock` SHARED; we take it EXCLUSIVE, non-blocking,
//!   and HOLD it across the check and the move — the fd follows the inode), or
//!   while ITS OWN soup holds a live tranche (read off its own `spine.db`,
//!   read-only, never through `Cauldron::open`, whose recovery pass writes).
//! - `remove` is a dry run without `--yes`, and never deletes: it moves the
//!   directory to `cauldrons/.removed/<name>-<UTC stamp>/`.
//! - The implicit LEGACY cauldron is never renamed (`check_renamable`)
//!   and never removed: its data lives directly in the
//!   environment's data root, and both verbs would move or write there.
//!
//! **No soup daemon is told about a cauldron.** `create` lays the directory
//! and tells no running soup to open it; `rename` / `remove` REFUSE while the
//! environment's soup daemon is live, because they have no way to make it let
//! go of the cauldron's soup first.

use std::fs::File;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use env::cauldron::{CauldronMeta, CAULDRONS_DIR, CAULDRON_MANIFEST};
use env::seams::{self, Table};
use env::{Cauldron, Environment};

/// Where `remove` puts a cauldron: `cauldrons/.removed/`.
pub(crate) const REMOVED_DIR: &str = ".removed";

/// The non-terminal tranche states (`libs/spine/src/tranche.rs` `LIVE_STATES`).
const LIVE_TRANCHE_STATES: &[&str] = &["queued", "open", "staging", "landing"];

/// The soup's seam name in an environment's seam table.
const SOUP_SEAM: &str = "soup";

/// `[--env <e>]` plus positionals, from a verb's own arguments. A local `--env`
/// wins over the leading one (`cauldron --env e create …`), as with `env remove`.
struct Args {
    env: Option<String>,
    seed: Option<String>,
    yes: bool,
    json: bool,
    pos: Vec<String>,
}

fn parse(verb: &str, args: &[String], allow: &[&str]) -> Result<Args> {
    let mut a = Args { env: None, seed: None, yes: false, json: false, pos: Vec::new() };
    let mut it = args.iter();
    while let Some(s) = it.next() {
        match s.as_str() {
            "--env" if allow.contains(&"--env") => {
                a.env = Some(it.next().cloned().with_context(|| format!("`cauldron {verb} --env` needs a name"))?)
            }
            s if s.starts_with("--env=") && allow.contains(&"--env") => a.env = Some(s["--env=".len()..].to_string()),
            // The `--seed` hook: checked here, before anything is written.
            f if f == crate::cauldron_seed::SEED_FLAG && allow.contains(&f) => {
                let v = it.next().with_context(|| format!("`cauldron {verb} --seed` needs a value"))?;
                a.seed = Some(crate::cauldron_seed::parse_seed(v)?);
            }
            s if s.starts_with("--seed=") && allow.contains(&"--seed") => {
                a.seed = Some(crate::cauldron_seed::parse_seed(&s["--seed=".len()..])?)
            }
            "--yes" if allow.contains(&"--yes") => a.yes = true,
            "--json" if allow.contains(&"--json") => a.json = true,
            s if s.starts_with('-') => bail!("unknown option `{s}` for `cauldron {verb}`"),
            s => a.pos.push(s.to_string()),
        }
    }
    Ok(a)
}

/// The environment a verb acts on: its local `--env`, else the resolved one.
fn environment(local: Option<&str>) -> Result<Environment> {
    let here = Environment::resolve()?;
    let e = match local {
        Some(n) => here.for_name(n)?,
        None => here,
    };
    Ok(e)
}

/// Refuse an environment whose data root does not exist: making one is
/// `cauldron env create`'s job, never a side effect of making a cauldron.
fn require_env_exists(e: &Environment) -> Result<()> {
    if !e.data_root.is_dir() {
        bail!(
            "REFUSED: there is no environment `{}` ({} does not exist). Environments are made with \
             `cauldron env create {}`.",
            e.name,
            e.data_root.display(),
            e.name
        );
    }
    Ok(())
}

fn one_name(verb: &str, a: &Args, usage: &str) -> Result<String> {
    match a.pos.as_slice() {
        [n] => Ok(n.clone()),
        [] => bail!("`cauldron {verb}` needs a cauldron name: {usage}"),
        _ => bail!("`cauldron {verb}` takes ONE name (got {}): {usage}", a.pos.len()),
    }
}

// ------------------------------------------------------------------- create

/// **`cauldron create <name> [--env <e>]`**. (`--seed` is parsed so that it
/// is refused by name: this release cannot seed a cauldron.)
pub(crate) fn verb_create(args: &[String]) -> Result<i32> {
    let a = parse("create", args, &["--env", "--seed"])?;
    let name = one_name("create", &a, "cauldron create <name> [--env <env>]")?;
    let e = environment(a.env.as_deref())?;
    require_env_exists(&e)?;
    let made = create_seeded(&e, &name, a.seed)?;
    println!("{}", e.banner());
    println!("cauldron: created cauldron `{name}` in environment `{}` at {}", e.name, made.display());
    Ok(0)
}

/// Make `cauldrons/<name>/` with its manifest and its own soup. Returns the
/// directory. Refuses a bad or reserved name, the legacy cauldron's name, and
/// any name already taken; writes nothing when it refuses.
pub(crate) fn create(e: &Environment, name: &str) -> Result<PathBuf> {
    create_seeded(e, name, None)
}

/// [`create`], with a `seed` (already checked by `parse_seed`) written
/// into `cauldron.toml`. The seed is installed by `cauldron_seed::
/// seed_created` AFTER the directory is in place (its paths are the final
/// ones) and INSIDE the rollback: a seed that fails removes the whole new
/// cauldron, so a failed `create --seed` leaves nothing behind.
pub(crate) fn create_seeded(e: &Environment, name: &str, seed: Option<String>) -> Result<PathBuf> {
    // EVERY create goes through the isolation guard here, at the one place a
    // cauldron is made: `cauldron create` (also guarded at dispatch) AND the
    // create `cauldron tui <unknown>` offers, whose verb (`tui`) is not a
    // control verb.
    admit_create(e)?;
    create_with(e, name, seed, crate::cauldron_seed::seed_created)
}

/// **Under isolation, a cauldron is created only in a scratch root** — the
/// rule `CONTROL_VERBS` applies at dispatch, applied again where the write
/// happens. Not isolated (the user's own shell): admitted.
fn admit_create(e: &Environment) -> Result<()> {
    let Some(why) = env::egress::process_isolation() else { return Ok(()) };
    let homes = env::egress::protected_homes();
    let reals: Vec<Option<PathBuf>> = if homes.is_empty() { vec![None] } else { homes.iter().map(|h| Some(h.join(".cauldron"))).collect() };
    for real in &reals {
        admit_create_with(&e.root, real.as_deref(), &why)?;
    }
    Ok(())
}

/// [`admit_create`] over explicit inputs.
fn admit_create_with(root: &std::path::Path, real: Option<&std::path::Path>, why: &str) -> Result<()> {
    env::egress::admit_daemon_control_with("`cauldron create`", Some(root), real, why).map_err(|e| anyhow::anyhow!(e))
}

/// [`create_seeded`]'s body, with the seed step passed in (so its failure can
/// be forced in a test). Writes nothing when it refuses; on any failure after
/// it started writing it removes what it made — the new cauldron, and the
/// `cauldrons/` directory too when this call created it.
fn create_with<T>(
    e: &Environment,
    name: &str,
    seed: Option<String>,
    seed_step: impl FnOnce(&Cauldron) -> Result<T>,
) -> Result<PathBuf> {
    refuse_env_name(e, name)?;
    let c = e.cauldron_named(name)?;
    let Some(dir) = c.dir() else {
        bail!(
            "REFUSED: `{name}` is already a cauldron of environment `{}`: its implicit legacy \
             cauldron, named after the environment.",
            e.name
        );
    };
    if e.find_cauldron(name)?.is_some() || std::fs::symlink_metadata(&dir).is_ok() {
        bail!("REFUSED: cauldron `{name}` already exists in environment `{}` ({})", e.name, dir.display());
    }
    let parent = e.cauldrons_dir();
    let made_parent = !parent.exists();
    std::fs::create_dir_all(&parent).with_context(|| format!("creating {}", parent.display()))?;
    // Undo what this call made, on any failure from here on.
    let undo = |made: Option<&PathBuf>| {
        if let Some(d) = made {
            let _ = std::fs::remove_dir_all(d);
        }
        if made_parent {
            // Only while empty: never anything another create put there.
            let _ = std::fs::remove_dir(&parent);
        }
    };
    let tmp = parent.join(format!(".creating-{name}-{}", std::process::id()));
    if tmp.exists() {
        std::fs::remove_dir_all(&tmp).with_context(|| format!("clearing a stale {}", tmp.display()))?;
    }
    let built = (|| -> Result<()> {
        std::fs::create_dir(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
        for sub in ["tui", "events", "ripples"] {
            std::fs::create_dir(tmp.join(sub)).with_context(|| format!("creating {sub}/"))?;
        }
        let meta = CauldronMeta {
            name: Some(name.to_string()),
            created_at: Some(chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()),
            seed: seed.clone(),
            ..CauldronMeta::default()
        };
        std::fs::write(tmp.join(CAULDRON_MANIFEST), meta.to_toml()?).context("writing cauldron.toml")?;
        // Its OWN soup: a fresh spine `Cauldron`, one `ws` pool on grammar v6,
        // exactly what the supervisor makes for the legacy soup.
        crate::engine::ensure_soup_cauldron(&tmp.join("soup"))?;
        Ok(())
    })();
    if let Err(err) = built {
        undo(Some(&tmp));
        return Err(err);
    }
    // No-clobber move into place: a racer that made `<name>` first wins.
    if std::fs::symlink_metadata(&dir).is_ok() {
        undo(Some(&tmp));
        bail!("REFUSED: cauldron `{name}` appeared while it was being created; it was left as it is");
    }
    if let Err(err) = std::fs::rename(&tmp, &dir) {
        undo(Some(&tmp));
        return Err(err).with_context(|| format!("moving the new cauldron into {}", dir.display()));
    }
    // Only this call made `dir`, and nothing else names it yet: removing it
    // on a seed failure undoes exactly this create.
    if let Err(err) = seed_step(&c) {
        undo(Some(&dir));
        return Err(err).context("the seed failed, so the new cauldron was removed; nothing was created");
    }
    Ok(dir)
}

/// **A cauldron is never named after its environment**: that name IS the
/// env's legacy cauldron. Today `engine.rs
/// before_launch` makes the legacy `soup/cauldron/` for any env whose soup
/// launches, so a `cauldrons/<env>/` made in a fresh env would end up beside
/// legacy data, and `Environment::cauldrons()` then refuses the whole env.
/// Refused whether or not the legacy data exists yet.
fn refuse_env_name(e: &Environment, name: &str) -> Result<()> {
    if name == e.name {
        bail!(
            "REFUSED: `{name}` is environment `{env}`'s own name, and that name is the env's legacy \
             cauldron; choose another. Nothing was written.",
            env = e.name
        );
    }
    Ok(())
}

// --------------------------------------------------------------------- list

/// **`cauldron list [--env <e>] [--json]`** — writes nothing.
pub(crate) fn verb_list(args: &[String]) -> Result<i32> {
    let a = parse("list", args, &["--env", "--json"])?;
    if !a.pos.is_empty() {
        bail!("`cauldron list` takes no names (did you mean `--env {}`?)", a.pos[0]);
    }
    let e = environment(a.env.as_deref())?;
    require_env_exists(&e)?;
    let all = e.cauldrons()?;
    if a.json {
        let rows: Vec<serde_json::Value> = all
            .iter()
            .map(|c| {
                let meta = c.meta().ok();
                serde_json::json!({
                    "name": c.name,
                    "env": c.env,
                    "kind": c.kind,
                    "enabled": meta.as_ref().map(|m| m.enabled).unwrap_or(true),
                    "seed": meta.as_ref().and_then(|m| m.seed.clone()),
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(0);
    }
    println!("{}", e.banner());
    if all.is_empty() {
        println!("cauldron: environment `{}` has no cauldrons. `cauldron create <name>` makes one.", e.name);
        return Ok(0);
    }
    for c in &all {
        let meta = c.meta();
        let mut notes = Vec::new();
        match &meta {
            Ok(m) if !m.enabled => notes.push("disabled".to_string()),
            Ok(_) => {}
            Err(err) => notes.push(format!("unreadable cauldron.toml: {err:#}")),
        }
        if let Ok(Some(s)) = meta.as_ref().map(|m| m.seed.clone()) {
            notes.push(format!("seed {s}"));
        }
        if notes.is_empty() {
            println!("  {}", c.display_name());
        } else {
            println!("  {}  ({})", c.display_name(), notes.join(", "));
        }
    }
    Ok(0)
}

// ------------------------------------------------------- the shared refusals

/// **The open lock, taken EXCLUSIVE and non-blocking.** Held by the returned
/// file until it is dropped; a face holding it shared makes this refuse.
/// Created if absent (the taker creates it) — only ever
/// inside a `Dir` cauldron's `tui/`.
pub(crate) fn take_open_lock(c: &Cauldron) -> Result<File> {
    let path = c.open_lock();
    if let Some(p) = path.parent() {
        if !p.is_dir() {
            std::fs::create_dir_all(p).with_context(|| format!("creating {}", p.display()))?;
        }
    }
    let fh = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    match fh.try_lock() {
        Ok(()) => Ok(fh),
        Err(std::fs::TryLockError::WouldBlock) => bail!(
            "REFUSED: cauldron `{}` is open in a face ({} is held). Close it there first.",
            c.name,
            path.display()
        ),
        Err(std::fs::TryLockError::Error(err)) => Err(err).with_context(|| format!("locking {}", path.display())),
    }
}

/// **The live tranches of THIS cauldron's own soup**, read off its own
/// `spine.db`, read-only. No `spine.db` is no tranche. Never opens the soup
/// through `spine::Cauldron::open` (its recovery pass writes).
pub(crate) fn live_tranches(c: &Cauldron) -> Result<Vec<(String, String)>> {
    let db = c.soup().join(spine::spine_db::SPINE_FILE);
    if !db.is_file() {
        return Ok(Vec::new());
    }
    // ZERO WRITES. A plain read-only open of a WAL database still CREATES
    // `-wal` and `-shm` beside it (proved by the tests' before-and-after tree comparison). So:
    // no `-wal` beside it means no writer has it open and everything is in the
    // main file, which is read `immutable` (no side files, no locks); a `-wal`
    // present means a writer's side files already exist, and `mode=ro` reads
    // through them without creating any.
    let wal = c.soup().join(format!("{}-wal", spine::spine_db::SPINE_FILE));
    let mode = if wal.exists() { "mode=ro" } else { "immutable=1" };
    let path = db.display().to_string().replace('%', "%25").replace('?', "%3f").replace('#', "%23");
    let conn = rusqlite::Connection::open_with_flags(
        format!("file:{path}?{mode}"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
            | rusqlite::OpenFlags::SQLITE_OPEN_URI
            | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| {
        format!("REFUSED: cannot open {} read-only, so cannot verify that no tranche is held", db.display())
    })?;
    let has_table: bool = conn
        .query_row("SELECT count(*) FROM sqlite_master WHERE type='table' AND name='tranches'", [], |r| {
            r.get::<_, i64>(0)
        })
        .map(|n| n > 0)
        .with_context(|| format!("REFUSED: cannot read {}, so cannot verify that no tranche is held", db.display()))?;
    if !has_table {
        return Ok(Vec::new());
    }
    let q = format!(
        "SELECT id, state FROM tranches WHERE state IN ({}) ORDER BY seq",
        LIVE_TRANCHE_STATES.iter().map(|s| format!("'{s}'")).collect::<Vec<_>>().join(",")
    );
    let mut stmt = conn.prepare(&q)?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

fn refuse_held_tranche(c: &Cauldron) -> Result<()> {
    let live = live_tranches(c)?;
    if !live.is_empty() {
        let list: Vec<String> = live.iter().map(|(id, st)| format!("{id} ({st})")).collect();
        bail!(
            "REFUSED: cauldron `{}`'s soup holds {} live tranche(s): {}. Wait for them to land or \
             roll back.",
            c.name,
            live.len(),
            list.join(", ")
        );
    }
    Ok(())
}

/// While the environment's soup daemon is live, a cauldron cannot be moved:
/// the daemon may hold its soup open and nothing can make it let go. Reads the seam table only.
fn refuse_live_soup(e: &Environment, verb: &str) -> Result<()> {
    let table = Table::load(&e.name, &e.data_root, &e.seams())?;
    if let Some(s) = table.seams.get(SOUP_SEAM) {
        if !seams::probe(s).process_gone(s) {
            bail!(
                "REFUSED: environment `{}`'s soup is running (pid {}, {}), and `cauldron {verb}` \
                 cannot make it let go of a cauldron's soup. Stop it first: `cauldron --env {} stop soup`.",
                e.name,
                s.pid,
                s.addr,
                e.name
            );
        }
    }
    Ok(())
}

/// The existing `Dir` cauldron `name`, or a refusal naming what it is instead.
fn existing(e: &Environment, name: &str, verb: &str) -> Result<Cauldron> {
    match e.find_cauldron(name)? {
        Some(c) => Ok(c),
        None => bail!(
            "REFUSED: there is no cauldron `{name}` in environment `{}` to {verb}. `cauldron list \
             --env {}` shows its cauldrons.",
            e.name,
            e.name
        ),
    }
}

// ------------------------------------------------------------------- rename

/// **`cauldron rename <old> <new> [--env <e>]`**.
pub(crate) fn verb_rename(args: &[String]) -> Result<i32> {
    let a = parse("rename", args, &["--env"])?;
    let [old, new] = a.pos.as_slice() else {
        bail!("`cauldron rename <old> <new> [--env <env>]` takes two names (got {})", a.pos.len());
    };
    let e = environment(a.env.as_deref())?;
    require_env_exists(&e)?;
    let c = existing(&e, old, "rename")?;
    c.check_renamable()?;
    if old == new {
        bail!("REFUSED: `{old}` is already called `{new}`");
    }
    refuse_env_name(&e, new)?;
    let target = e.cauldron_named(new)?;
    let Some(to) = target.dir() else {
        bail!("REFUSED: `{new}` is environment `{}`'s legacy cauldron's name", e.name);
    };
    if e.find_cauldron(new)?.is_some() || std::fs::symlink_metadata(&to).is_ok() {
        bail!("REFUSED: cauldron `{new}` already exists in environment `{}`", e.name);
    }
    let from = c.dir().expect("check_renamable admits only Dir cauldrons");
    // Order: the lock first (held to the end), then the tranche, then the soup.
    let _lock = take_open_lock(&c)?;
    refuse_held_tranche(&c)?;
    refuse_live_soup(&e, "rename")?;
    // The manifest's name first: a crash between the two leaves a directory
    // whose manifest names where it was going, never a moved dir with a lie.
    let mut meta = c.meta()?;
    meta.name = Some(new.clone());
    let manifest = from.join(CAULDRON_MANIFEST);
    let tmp = from.join(format!(".{CAULDRON_MANIFEST}.{}", std::process::id()));
    std::fs::write(&tmp, meta.to_toml()?).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, &manifest).with_context(|| format!("replacing {}", manifest.display()))?;
    std::fs::rename(&from, &to).with_context(|| format!("renaming {} to {}", from.display(), to.display()))?;
    println!("{}", e.banner());
    println!("cauldron: renamed cauldron `{old}` to `{new}` in environment `{}`", e.name);
    Ok(0)
}

// ------------------------------------------------------------------- remove

/// **`cauldron remove <name> [--env <e>] [--yes]`** — never deletes.
pub(crate) fn verb_remove(args: &[String]) -> Result<i32> {
    let a = parse("remove", args, &["--env", "--yes"])?;
    let name = one_name("remove", &a, "cauldron remove <name> [--env <env>] [--yes]")?;
    let e = environment(a.env.as_deref())?;
    require_env_exists(&e)?;
    let c = existing(&e, &name, "remove")?;
    // The legacy cauldron's data IS the environment's data root; moving it
    // would move the user's live legacy paths.
    if c.is_legacy() {
        bail!(
            "REFUSED: `{name}` is the implicit legacy cauldron of environment `{}`: its data lives \
             directly in the environment's data root, and `cauldron remove` never moves that. \
             Nothing was changed.",
            e.name
        );
    }
    let from = c.dir().expect("not legacy");
    let to = removed_path(&e, &name);
    let _lock = take_open_lock(&c)?;
    refuse_held_tranche(&c)?;
    refuse_live_soup(&e, "remove")?;
    println!("{}", e.banner());
    if !a.yes {
        println!("cauldron: DRY RUN — would remove cauldron `{name}` from environment `{}`:", e.name);
        println!("  moved          {}", from.display());
        println!("  to             {} (nothing is deleted)", to.display());
        println!("  untouched      every other cauldron, and the environment itself");
        println!("cauldron: re-run with `--yes` to do it.");
        return Ok(2);
    }
    let parent = to.parent().expect("under .removed");
    std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    if std::fs::symlink_metadata(&to).is_ok() {
        bail!("REFUSED: {} already exists; try again in a second", to.display());
    }
    std::fs::rename(&from, &to).with_context(|| format!("moving {} to {}", from.display(), to.display()))?;
    println!("cauldron: removed cauldron `{name}`: moved to {}", to.display());
    Ok(0)
}

/// `cauldrons/.removed/<name>-<UTC yyyymmddThhmmssZ>`.
pub(crate) fn removed_path(e: &Environment, name: &str) -> PathBuf {
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    e.data_root.join(CAULDRONS_DIR).join(REMOVED_DIR).join(format!("{name}-{stamp}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;

    fn scratch(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "ce3u-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        let p = p.canonicalize().unwrap();
        if let Some(h) = std::env::var_os("HOME") {
            assert!(!p.starts_with(Path::new(&h).join(".cauldron")), "scratch under ~/.cauldron");
        }
        p
    }

    fn env_at(root: &Path, name: &str) -> Environment {
        Environment {
            root: root.to_path_buf(),
            root_source: env::paths::Source::CauldronHome,
            name: name.into(),
            name_source: env::paths::Source::Default,
            data_root: root.join("data").join(name),
        }
    }

    fn tree(dir: &Path) -> BTreeMap<String, (bool, u64, i64)> {
        fn walk(base: &Path, d: &Path, out: &mut BTreeMap<String, (bool, u64, i64)>) {
            let Ok(rd) = std::fs::read_dir(d) else { return };
            for e in rd.flatten() {
                let p = e.path();
                let m = std::fs::symlink_metadata(&p).unwrap();
                out.insert(p.strip_prefix(base).unwrap().display().to_string(), (m.is_dir(), if m.is_dir() { 0 } else { m.len() }, m.mtime() * 1_000_000_000 + m.mtime_nsec()));
                if m.is_dir() {
                    walk(base, &p, out);
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(dir, dir, &mut out);
        out
    }

    /// **A failing seed undoes the WHOLE create** — the new
    /// cauldron AND the `cauldrons/` directory this create made — so the data
    /// root is exactly as it was. Opposed: when `cauldrons/` already held a
    /// cauldron, it is left (and so is that cauldron).
    #[test]
    fn a_failed_seed_removes_the_cauldron_and_the_cauldrons_dir_it_made() {
        let root = scratch("u1");
        let e = env_at(&root, "dev");
        std::fs::create_dir_all(&e.data_root).unwrap();
        // The data root's own mtime moves when `cauldrons/` is made and
        // removed in it; every entry and every byte must be as before.
        let entries = |root: &Path| {
            let mut t = tree(root);
            if let Some(v) = t.get_mut("data/dev") {
                v.2 = 0;
            }
            t
        };
        let before = entries(&root);
        let err = create_with(&e, "demo", None, |_| -> Result<()> { bail!("forced seed failure") }).unwrap_err();
        assert!(format!("{err:#}").contains("forced seed failure"), "{err:#}");
        assert!(!e.cauldrons_dir().exists(), "an empty cauldrons/ was left behind");
        assert_eq!(entries(&root), before, "a failed create left something behind");
        // OPPOSED: an existing cauldrons/ (with a sibling) is never removed.
        create_with(&e, "keep", None, |_| -> Result<()> { Ok(()) }).unwrap();
        let err = create_with(&e, "demo", None, |_| -> Result<()> { bail!("forced seed failure") });
        assert!(err.is_err());
        assert!(e.cauldrons_dir().join("keep").join("cauldron.toml").is_file(), "the sibling was touched");
        assert!(!e.cauldrons_dir().join("demo").exists(), "the failed cauldron was left");
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// **The create guard itself** — every create, whatever verb reached it,
    /// is refused on the user's real root (or inside it) under
    /// isolation and admitted on a scratch one.
    #[test]
    fn a_create_under_isolation_is_admitted_only_on_a_scratch_root() {
        let real = Path::new("/Users/someone/.cauldron");
        assert!(admit_create_with(real, Some(real), "a test").is_err(), "admitted on the real root");
        assert!(admit_create_with(&real.join("x"), Some(real), "a test").is_err(), "admitted inside it");
        assert!(admit_create_with(real, None, "a test").is_err(), "admitted with the home unknown");
        assert!(admit_create_with(Path::new("/tmp/scratch/.cauldron"), Some(real), "a test").is_ok(), "refused on scratch");
    }
}
