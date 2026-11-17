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
//! The marker itself (parsing, finding, resolving, and the rule that a
//! write is never aimed by a marker this user did not make here) lives in
//! `soup_door::marker`; this module is the `cauldron link` verb.
//!
//! How a soup verb finds its target (`soup_door::marker::resolve`):
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

use soup_door::marker::{
    git_state, name_ok, parse_marker, refuse_untrusted_marker_for_write, resolve, GitState, Marker, MARKER,
};

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
            Err(err) if !replace => return Err(anyhow::Error::new(err).context("pass --replace to overwrite it")),
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
