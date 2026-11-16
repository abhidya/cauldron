//! **`cauldron link`** and the project marker: a project directory names
//! its cauldron, so `cauldron soup …` run anywhere inside the project finds
//! it without `--env` and `--cauldron`.
//!
//! The marker is a small text file called `.cauldron` in the project's root:
//!
//! ```text
//! # cauldron project marker (written by `cauldron link`)
//! env = prod
//! cauldron = kb
//! ```
//!
//! `cauldron link <cauldron> --env <env>` writes it into the current
//! directory (or `--dir`), after checking that the environment and the
//! cauldron exist. `cauldron link --show` prints what the current directory
//! resolves to and where each value came from.
//!
//! How a soup verb finds its target ([`resolve`]):
//!
//! - the environment: `--env` (also a leading `cauldron --env`), else
//!   `$CAULDRON_ENV` / `$ENV`, else the marker;
//! - the cauldron: `--cauldron`, else `$CAULDRON_CAULDRON`, else the marker.
//!
//! Explicit flags and variables always win, and the marker only fills what
//! they leave out. The marker is looked for in the current directory and
//! then in each parent, nearest first, and only a regular FILE counts: the
//! default data root `~/.cauldron` is a directory of the same name, and is
//! never read as a marker. A `.cauldron` that is a symbolic link (to anything
//! but a directory) refuses rather than being followed, and `cauldron link`
//! never writes through one. A marker that cannot be parsed refuses, naming
//! its path; it is never skipped in favour of one further up. When the
//! marker would supply the cauldron but an explicit environment differs from
//! the marker's, the verb refuses rather than look for that cauldron in an
//! environment the marker did not name. Nothing falls back to `prod`: with
//! no flag, no variable and no marker, the verb asks for `--env`.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use env::Environment;
use serde_json::json;

/// The marker's file name.
pub(crate) const MARKER: &str = ".cauldron";

const USAGE: &str = "\
cauldron link — name this project's cauldron, so `cauldron soup` finds it from here

    cauldron link <cauldron> --env <env> [--dir <path>] [--replace]
        Write <path>/.cauldron (default: the current directory) naming the environment
        and the cauldron, after checking both exist. `cauldron soup ...` run in that
        directory or any directory below it then needs neither --env nor --cauldron.
        An existing marker that names something else is refused unless --replace.
    cauldron link --show [--dir <path>] [--env <env>]
        What <path> (default: the current directory) resolves to: the environment and
        the cauldron, where each came from, the marker file if one was used, and
        `writes_refused`: why a soup write would refuse to take its target from that
        marker (tracked by git, or another user's), or null. With --env, the environment
        is that one, and a marker naming another is refused, as a soup verb refuses it.
        Writes nothing.
    cauldron link help

  Explicit --env / --cauldron (and $CAULDRON_ENV, $ENV, $CAULDRON_CAULDRON) always win
  over a marker. The nearest .cauldron FILE in the directory or a parent is used; a
  directory of that name (such as the data root ~/.cauldron) is not a marker. A
  .cauldron that is a symbolic link is never followed: reading refuses, and `link`
  refuses to write through it, even with --replace. `link` writes a new file and
  renames it into place.

  The marker is per-machine: it names an environment and a cauldron on THIS machine,
  so keep it out of version control (add `.cauldron` to .gitignore; `link` prints a
  hint when the directory is in a git work tree and the marker is not ignored). It can
  name only an environment and a cauldron, never a path, a pool or a file. A marker in
  the directory or any parent is obeyed, including one that arrived with a cloned
  repository, and every soup verb's JSON says `resolved_from` when one was used.
  `cauldron soup import` (without --dry-run) and `recover` refuse to take their target
  from a marker that is tracked by git or owned by another user; naming --env and
  --cauldron explicitly writes without reading any marker.
";

/// A parsed project marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Marker {
    pub path: PathBuf,
    pub env: String,
    pub cauldron: String,
}

/// The name grammar shared by environments and cauldrons (`[a-z0-9_-]{1,32}`,
/// first character `[a-z0-9]`); the reserved names are judged where the name
/// is used, as for `--env` and `--cauldron`.
fn name_ok(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 32
        && n.chars().next().map(|c| c.is_ascii_lowercase() || c.is_ascii_digit()) == Some(true)
        && n.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// Parse a marker's text: `key = value` lines, `#` comments and blank lines;
/// exactly the keys `env` and `cauldron`, each once.
pub(crate) fn parse_marker(path: &Path, text: &str) -> Result<Marker> {
    let bad = |why: String| -> anyhow::Error {
        anyhow::anyhow!(
            "REFUSED: the project marker {} cannot be read: {why}. A marker holds two lines, \
             `env = <name>` and `cauldron = <name>` (`cauldron link` writes one). Fix or remove it, or name \
             --env and --cauldron explicitly.",
            path.display()
        )
    };
    let (mut env, mut cauldron) = (None, None);
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            return Err(bad(format!("line {} is not `key = value`", i + 1)));
        };
        let (k, v) = (k.trim(), v.trim().trim_matches('"'));
        let slot = match k {
            "env" => &mut env,
            "cauldron" => &mut cauldron,
            other => return Err(bad(format!("line {} has an unknown key `{other}`", i + 1))),
        };
        if slot.is_some() {
            return Err(bad(format!("line {} gives `{k}` a second time", i + 1)));
        }
        if !name_ok(v) {
            return Err(bad(format!("line {}: `{v}` is not a usable {k} name", i + 1)));
        }
        *slot = Some(v.to_string());
    }
    match (env, cauldron) {
        (Some(env), Some(cauldron)) => Ok(Marker { path: path.to_path_buf(), env, cauldron }),
        (None, _) => Err(bad("it names no `env`".to_string())),
        (_, None) => Err(bad("it names no `cauldron`".to_string())),
    }
}

/// The nearest marker FILE at `start` or above it. A directory named
/// `.cauldron` (or a symbolic link to one, as the data root may be) is passed
/// over; a marker file that does not parse refuses. A `.cauldron` that is a
/// symbolic link to anything else, dangling or not, or that is not a regular
/// file, refuses too: a marker is only ever read from a regular file, never
/// through a link, so a link cannot make a project borrow a marker from
/// somewhere else.
pub(crate) fn find_marker(start: &Path) -> Result<Option<Marker>> {
    for dir in start.ancestors() {
        let p = dir.join(MARKER);
        let Ok(meta) = std::fs::symlink_metadata(&p) else { continue };
        let kind = meta.file_type();
        if kind.is_dir() || (kind.is_symlink() && p.is_dir()) {
            continue;
        }
        if kind.is_symlink() {
            bail!(
                "REFUSED: {} is a symbolic link, and a project marker is only read from a regular file, so it was \
                 not followed. Replace it with a file (`cauldron link`), or name --env and --cauldron explicitly, \
                 which works without reading any marker.",
                p.display()
            );
        }
        if !kind.is_file() {
            bail!(
                "REFUSED: {} is not a regular file, so it cannot be a project marker and was not read. Remove it, or \
                 name --env and --cauldron explicitly, which works without reading any marker.",
                p.display()
            );
        }
        let text = std::fs::read_to_string(&p).with_context(|| format!("reading the project marker {}", p.display()))?;
        return parse_marker(&p, &text).map(Some);
    }
    Ok(None)
}

/// What a verb resolved its target to, and from where.
#[derive(Debug, Clone)]
pub(crate) struct Resolved {
    pub env: String,
    pub env_from: String,
    pub cauldron: String,
    pub cauldron_from: String,
    /// The marker, when it supplied either value.
    pub marker: Option<PathBuf>,
}

/// Resolve the environment and the cauldron for a verb run in `dir` (the
/// current directory when `None`), from the explicit `flag_env` /
/// `flag_cauldron` (the verb's own flags), the process-wide flags and
/// variables, and then the project marker. The directory is read only when
/// the marker is needed, so a verb given both values works even from a
/// directory that no longer exists. `verb` is only for the messages.
pub(crate) fn resolve(verb: &str, dir: Option<&Path>, flag_env: Option<&str>, flag_cauldron: Option<&str>) -> Result<Resolved> {
    let env_named: Option<(String, String)> = match flag_env {
        Some(n) => Some((n.to_string(), "--env".to_string())),
        // A variable that is set but names no usable environment is an
        // error, never "not named": the marker must not quietly win over it.
        None => match Environment::resolve()? {
            e if e.name_source != env::paths::Source::Default => Some((e.name, e.name_source.as_str().to_string())),
            _ => None,
        },
    };
    let cauldron_named: Option<(String, String)> = match flag_cauldron {
        Some(n) => Some((n.to_string(), "--cauldron".to_string())),
        None => env::cauldron::requested_cauldron()?.map(|(n, s)| (n, s.as_str().to_string())),
    };
    if let (Some((env, env_from)), Some((cauldron, cauldron_from))) = (&env_named, &cauldron_named) {
        return Ok(Resolved {
            env: env.clone(),
            env_from: env_from.clone(),
            cauldron: cauldron.clone(),
            cauldron_from: cauldron_from.clone(),
            marker: None,
        });
    }
    let dir = match dir {
        Some(d) => d.to_path_buf(),
        None => std::env::current_dir().with_context(|| {
            format!("reading the current directory, to look for a project marker ({verb} was not given both --env and --cauldron)")
        })?,
    };
    let marker = find_marker(&dir)?;
    let Some(m) = marker else {
        if env_named.is_none() {
            bail!(
                "{verb} needs --env <name>: it never defaults to prod, and no project marker (`.cauldron`, written \
                 by `cauldron link`) was found in {} or above it",
                dir.display()
            );
        }
        bail!(
            "{verb} needs --cauldron <name>, and no project marker (`.cauldron`, written by `cauldron link`) was \
             found in {} or above it",
            dir.display()
        );
    };
    let from = format!("the project marker {}", m.path.display());
    let (env, env_from) = env_named.clone().unwrap_or((m.env.clone(), from.clone()));
    let (cauldron, cauldron_from) = match cauldron_named {
        Some(c) => c,
        None => {
            if let Some((named, named_from)) = &env_named {
                if named != &m.env {
                    bail!(
                        "REFUSED: {} names cauldron `{}` in environment `{}`, but environment `{named}` was named \
                         ({named_from}). The marker's cauldron is not looked for in another environment: name the \
                         cauldron too (--cauldron), or drop the environment to use the marker's.",
                        from,
                        m.cauldron,
                        m.env
                    );
                }
            }
            (m.cauldron.clone(), from.clone())
        }
    };
    Ok(Resolved { env, env_from, cauldron, cauldron_from, marker: Some(m.path) })
}

/// What git says about the file `name` in `dir`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GitState {
    /// Not inside a git work tree, or git could not be run.
    NoRepo,
    /// Committed (in the index): it travels with the repository.
    Tracked,
    /// Inside a work tree and ignored.
    Ignored,
    /// Inside a work tree, neither tracked nor ignored.
    Untracked,
}

/// Ask git about `dir/name`. git runs with its file-system monitor turned
/// off, so a repository's own configuration cannot make this start a
/// program. Any failure to run git counts as "not in a repository".
pub(crate) fn git_state(dir: &Path, name: &str) -> GitState {
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(["-c", "core.fsmonitor=false", "-C"])
            .arg(dir)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
    };
    match git(&["rev-parse", "--is-inside-work-tree"]) {
        Some(o) if o.status.success() && o.stdout.starts_with(b"true") => {}
        _ => return GitState::NoRepo,
    }
    if git(&["ls-files", "--error-unmatch", "--", name]).is_some_and(|o| o.status.success()) {
        return GitState::Tracked;
    }
    if git(&["check-ignore", "-q", "--", name]).is_some_and(|o| o.status.success()) {
        return GitState::Ignored;
    }
    GitState::Untracked
}

/// **A write may not be aimed by a marker this user did not make here.**
/// Called before a soup verb WRITES to a target that the marker at
/// `marker` supplied (the environment, the cauldron, or both). Refused when
/// the marker is tracked by git (it came with, or will travel with, the
/// repository, so it was written on another machine or by someone else) or
/// is owned by a different user than the store being written (`store`, the
/// environment's data root). Naming --env and --cauldron explicitly writes
/// without reading the marker at all. Reads are never refused by this.
pub(crate) fn refuse_untrusted_marker_for_write(verb: &str, r: &Resolved, store: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let Some(marker) = &r.marker else { return Ok(()) };
    let target = format!("cauldron `{}` in environment `{}`", r.cauldron, r.env);
    let explicit = format!("--env {} --cauldron {}", r.env, r.cauldron);
    let owner = std::fs::symlink_metadata(marker).with_context(|| format!("reading {}", marker.display()))?.uid();
    let store_owner = std::fs::metadata(store).with_context(|| format!("reading {}", store.display()))?.uid();
    if owner != store_owner {
        bail!(
            "REFUSED: {verb} would write to {target}, named by the project marker {}, which belongs to another \
             user (uid {owner}, while the store belongs to uid {store_owner}). A marker only aims a write when it \
             is your own. Nothing was written. To write there, name it explicitly: {explicit}.",
            marker.display()
        );
    }
    let dir = marker.parent().unwrap_or(Path::new("/"));
    if git_state(dir, MARKER) == GitState::Tracked {
        bail!(
            "REFUSED: {verb} would write to {target}, named by the project marker {}, which is tracked by git, so \
             it came with the repository rather than from `cauldron link` on this machine. A marker names \
             cauldrons on one machine and belongs in .gitignore. Nothing was written. To write there, name it \
             explicitly: {explicit}; or untrack the marker (`git rm --cached .cauldron`), add it to .gitignore, \
             and link the project yourself.",
            marker.display()
        );
    }
    Ok(())
}

/// The environment by name, which must exist.
fn existing_env(name: &str) -> Result<Environment> {
    let e = Environment::resolve()?.for_name(name)?;
    if !e.data_root.is_dir() {
        bail!("REFUSED: there is no environment `{}` ({} does not exist)", e.name, e.data_root.display());
    }
    Ok(e)
}

/// `cauldron link …`.
pub(crate) fn verb_link(args: &[String]) -> Result<i32> {
    let (mut env_flag, mut dir, mut show, mut replace, mut pos) = (None, None, false, false, Vec::new());
    let mut it = args.iter();
    while let Some(s) = it.next() {
        let (flag, inline) = match s.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (s.as_str(), None),
        };
        let mut value = |f: &str| -> Result<String> {
            match &inline {
                Some(v) => Ok(v.clone()),
                None => it.next().cloned().with_context(|| format!("`cauldron link {f}` needs a value")),
            }
        };
        match flag {
            "help" | "-h" | "--help" if pos.is_empty() => {
                print!("{USAGE}");
                return Ok(0);
            }
            "--env" => env_flag = Some(value("--env")?),
            "--dir" => dir = Some(PathBuf::from(value("--dir")?)),
            "--show" => show = true,
            "--replace" => replace = true,
            f if f.starts_with('-') => bail!("unknown option `{s}` for `cauldron link` (`cauldron link help`)"),
            _ => pos.push(s.clone()),
        }
    }
    let dir = match dir {
        Some(d) => d,
        None => std::env::current_dir().context("reading the current directory")?,
    };
    if !dir.is_dir() {
        bail!("REFUSED: {} is not a directory", dir.display());
    }

    if show {
        if !pos.is_empty() || replace {
            bail!("`cauldron link --show` takes only --dir (and --env); `cauldron link help`");
        }
        let r = resolve("`cauldron link --show`", Some(&dir), env_flag.as_deref(), None)?;
        let e = existing_env(&r.env)?;
        let exists = e.find_cauldron(&r.cauldron)?.is_some();
        // Whether `cauldron soup import` would take its target from this
        // marker, or refuse it (tracked by git, or another user's).
        let writes_refused = refuse_untrusted_marker_for_write("a soup write", &r, &e.data_root).err().map(|err| err.to_string());
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "dir": dir.display().to_string(),
                "env": r.env,
                "env_from": r.env_from,
                "cauldron": r.cauldron,
                "cauldron_from": r.cauldron_from,
                "marker": r.marker.map(|p| p.display().to_string()),
                "cauldron_exists": exists,
                "writes_refused": writes_refused,
            }))?
        );
        return Ok(if exists { 0 } else { 1 });
    }

    let [cauldron] = pos.as_slice() else {
        eprint!("{USAGE}");
        return Ok(2);
    };
    let named = match env_flag {
        Some(n) => Some(n),
        None => Some(Environment::resolve()?).filter(|e| e.name_source != env::paths::Source::Default).map(|e| e.name),
    };
    let Some(env_name) = named else {
        bail!("`cauldron link` needs --env <name>: a marker names its environment, and it is never assumed to be prod");
    };
    if !name_ok(cauldron) {
        bail!("REFUSED: `{cauldron}` is not a usable cauldron name ([a-z0-9_-], 1-32 characters, starting with [a-z0-9])");
    }
    let e = existing_env(&env_name)?;
    if e.find_cauldron(cauldron)?.is_none() {
        bail!(
            "REFUSED: there is no cauldron `{cauldron}` in environment `{}`. `cauldron list --env {}` shows its cauldrons; \
             `cauldron create {cauldron} --env {}` makes one. Nothing was written.",
            e.name,
            e.name,
            e.name
        );
    }
    let path = dir.join(MARKER);
    // Judged on the entry itself, never on what it points at: a link is
    // refused whatever it points to (or if it points nowhere), with or without
    // --replace, so `link` can never create or overwrite a file outside `dir`.
    let existing = match std::fs::symlink_metadata(&path) {
        Ok(m) => Some(m.file_type()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
    };
    if existing.is_some_and(|k| k.is_symlink()) {
        bail!(
            "REFUSED: {} is a symbolic link, and `cauldron link` never writes through one (not even with \
             --replace): it could create or overwrite a file outside {}. Nothing was written. If the link is \
             yours, remove it and run `cauldron link` again.",
            path.display(),
            dir.display()
        );
    }
    if existing.is_some_and(|k| k.is_dir()) {
        bail!(
            "REFUSED: {} exists and is not a file (in a home directory it is the default data root), so it cannot \
             hold a project marker. Link a project directory instead. Nothing was written.",
            path.display()
        );
    }
    if existing.is_some_and(|k| !k.is_file()) {
        bail!("REFUSED: {} exists and is not a regular file, so it was left alone. Nothing was written.", path.display());
    }
    let want = Marker { path: path.clone(), env: e.name.clone(), cauldron: cauldron.clone() };
    if existing.is_some() {
        let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        match parse_marker(&path, &text) {
            Ok(have) if have == want => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "outcome": "unchanged", "marker": path.display().to_string(), "env": want.env, "cauldron": want.cauldron,
                    }))?
                );
                gitignore_hint(&dir, &path);
                return Ok(0);
            }
            Ok(have) if !replace => bail!(
                "REFUSED: {} already names cauldron `{}` in environment `{}`. Pass --replace to point this project \
                 at `{}` in `{}` instead. Nothing was written.",
                path.display(),
                have.cauldron,
                have.env,
                want.cauldron,
                want.env
            ),
            Err(err) if !replace => return Err(err.context("pass --replace to overwrite it")),
            _ => {}
        }
    }
    let body = format!(
        "# cauldron project marker (written by `cauldron link`). `cauldron soup ...` run in this\n\
         # directory or below uses this cauldron unless --env / --cauldron say otherwise.\n\
         env = {}\ncauldron = {}\n",
        want.env, want.cauldron
    );
    write_replacing(&dir, &path, &body)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "outcome": "linked", "marker": path.display().to_string(), "env": want.env, "cauldron": want.cauldron,
        }))?
    );
    gitignore_hint(&dir, &path);
    Ok(0)
}

/// One line on stderr when the marker sits in a git work tree and git does
/// not ignore it: the marker is per-machine and should stay out of the
/// repository. `.gitignore` itself is never edited.
fn gitignore_hint(dir: &Path, path: &Path) {
    match git_state(dir, MARKER) {
        GitState::Untracked => eprintln!(
            "cauldron link: hint: {} is inside a git work tree and not ignored; it names cauldrons on this machine \
             only, so add `.cauldron` to .gitignore.",
            path.display()
        ),
        GitState::Tracked => eprintln!(
            "cauldron link: hint: {} is tracked by git; it names cauldrons on this machine only, and soup writes \
             refuse to take their target from a tracked marker. Untrack it (`git rm --cached .cauldron`) and add \
             `.cauldron` to .gitignore.",
            path.display()
        ),
        GitState::NoRepo | GitState::Ignored => {}
    }
}

/// Write `body` to `path` (a name inside `dir`) by writing a new file in
/// `dir` and renaming it over `path`. The new file is created exclusively, so
/// it is never an existing file or a link; the rename replaces the directory
/// entry `path` itself, never a file a link there might point at.
fn write_replacing(dir: &Path, path: &Path, body: &str) -> Result<()> {
    use std::io::Write as _;
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let tmp = dir.join(format!("{MARKER}.tmp-{}-{nanos}", std::process::id()));
    let written = (|| -> std::io::Result<()> {
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        f.write_all(body.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if let Err(err) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(err).with_context(|| format!("writing {}", path.display()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_marker_parses_its_two_keys_and_refuses_anything_else() {
        let p = Path::new("/p/.cauldron");
        let m = parse_marker(p, "# a comment\n\nenv = dev\ncauldron = \"kb\"\n").unwrap();
        assert_eq!((m.env.as_str(), m.cauldron.as_str()), ("dev", "kb"));
        for (text, why) in [
            ("env = dev\n", "names no `cauldron`"),
            ("cauldron = kb\n", "names no `env`"),
            ("env = dev\ncauldron = kb\nenv = prod\n", "a second time"),
            ("env = dev\ncauldron = kb\npool = ws\n", "unknown key `pool`"),
            ("env dev\ncauldron = kb\n", "not `key = value`"),
            ("env = ../x\ncauldron = kb\n", "not a usable env name"),
            ("env = dev\ncauldron = KB\n", "not a usable cauldron name"),
        ] {
            let err = parse_marker(p, text).unwrap_err().to_string();
            assert!(err.contains(why) && err.contains("/p/.cauldron"), "{text:?}: {err}");
        }
    }

    #[test]
    fn the_nearest_marker_file_wins_and_a_directory_of_that_name_is_passed_over() {
        let base = std::env::temp_dir().join(format!("cauldron-link-unit-{}", std::process::id()));
        let deep = base.join("proj").join("sub").join("deeper");
        std::fs::create_dir_all(&deep).unwrap();
        // A directory named `.cauldron` (as the data root is) in between.
        std::fs::create_dir_all(base.join("proj").join("sub").join(MARKER)).unwrap();
        assert_eq!(find_marker(&deep).unwrap(), None);
        std::fs::write(base.join("proj").join(MARKER), "env = dev\ncauldron = kb\n").unwrap();
        let m = find_marker(&deep).unwrap().unwrap();
        assert_eq!((m.env.as_str(), m.cauldron.as_str()), ("dev", "kb"));
        assert_eq!(m.path, base.join("proj").join(MARKER));
        // A broken marker nearer refuses; it is never skipped.
        std::fs::write(deep.join(MARKER), "nonsense\n").unwrap();
        assert!(find_marker(&deep).is_err());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_marker_is_never_read_through_a_symbolic_link() {
        use std::os::unix::fs::symlink;
        let base = std::env::temp_dir().join(format!("cauldron-link-unit-sym-{}", std::process::id()));
        let proj = base.join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::create_dir_all(base.join("data-root")).unwrap();
        std::fs::write(base.join("valid"), "env = dev\ncauldron = kb\n").unwrap();
        // A link to a directory (a data root kept elsewhere) is passed over.
        symlink(base.join("data-root"), proj.join(MARKER)).unwrap();
        assert_eq!(find_marker(&proj).unwrap(), None);
        std::fs::remove_file(proj.join(MARKER)).unwrap();
        // A link to a valid marker, or a dangling one, refuses, naming itself.
        for target in [base.join("valid"), base.join("missing")] {
            symlink(&target, proj.join(MARKER)).unwrap();
            let err = find_marker(&proj).unwrap_err().to_string();
            assert!(err.contains("symbolic link") && err.contains(proj.join(MARKER).to_str().unwrap()), "{err}");
            assert!(err.contains("--env and --cauldron"), "{err}");
            std::fs::remove_file(proj.join(MARKER)).unwrap();
        }
        let _ = std::fs::remove_dir_all(&base);
    }
}
