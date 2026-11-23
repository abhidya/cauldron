//! **The project marker**: a small text file called `.cauldron` in a
//! project's root that names an environment and a cauldron, so a soup verb
//! run anywhere inside the project finds its target without `--env` and
//! `--cauldron` (the CLI's `cauldron link` writes one).
//!
//! ```text
//! # cauldron project marker (written by `cauldron link`)
//! env = prod
//! cauldron = kb
//! ```
//!
//! How a verb finds its target ([`resolve`]):
//!
//! - the environment: the verb's `--env`, else the process-wide flag and
//!   `$CAULDRON_ENV` / `$ENV`, else the marker;
//! - the cauldron: `--cauldron`, else `$CAULDRON_CAULDRON`, else the marker.
//!
//! Explicit flags and variables always win, and the marker only fills what
//! they leave out. The marker is looked for in the directory and then in
//! each parent, nearest first, and only a regular FILE counts: the default
//! data root `~/.cauldron` is a directory of the same name, and is never read
//! as a marker. A `.cauldron` that is a symbolic link (to anything but a
//! directory) refuses rather than being followed. A marker that cannot be
//! parsed refuses, naming its path; it is never skipped in favour of one
//! further up. When the marker would supply the cauldron but an explicit
//! environment differs from the marker's, the verb refuses rather than look
//! for that cauldron in an environment the marker did not name. Nothing falls
//! back to `prod`.
//!
//! The marker is a convenience of the command line only. The MCP door never
//! reads it: there a cauldron is opened only by a grant ([`crate::grants`]).

use std::path::{Path, PathBuf};

use env::Environment;

use crate::{refuse, Code, Error, Result};

/// The marker's file name.
pub const MARKER: &str = ".cauldron";

/// A parsed project marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Marker {
    pub path: PathBuf,
    pub env: String,
    pub cauldron: String,
}

/// The name grammar shared by environments and cauldrons (`[a-z0-9_-]{1,32}`,
/// first character `[a-z0-9]`); the reserved names are judged where the name
/// is used, as for `--env` and `--cauldron`.
pub fn name_ok(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 32
        && n.chars().next().map(|c| c.is_ascii_lowercase() || c.is_ascii_digit()) == Some(true)
        && n.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// Parse a marker's text: `key = value` lines, `#` comments and blank lines;
/// exactly the keys `env` and `cauldron`, each once.
pub fn parse_marker(path: &Path, text: &str) -> Result<Marker> {
    let bad = |why: String| -> Error {
        Error::new(
            Code::Marker,
            format!(
                "REFUSED: the project marker {} cannot be read: {why}. A marker holds two lines, \
                 `env = <name>` and `cauldron = <name>` (`cauldron link` writes one). Fix or remove it, or name \
                 --env and --cauldron explicitly.",
                path.display()
            ),
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
pub fn find_marker(start: &Path) -> Result<Option<Marker>> {
    for dir in start.ancestors() {
        let p = dir.join(MARKER);
        let Ok(meta) = std::fs::symlink_metadata(&p) else { continue };
        let kind = meta.file_type();
        if kind.is_dir() || (kind.is_symlink() && p.is_dir()) {
            continue;
        }
        if kind.is_symlink() {
            refuse!(
                Code::Marker,
                "REFUSED: {} is a symbolic link, and a project marker is only read from a regular file, so it was \
                 not followed. Replace it with a file (`cauldron link`), or name --env and --cauldron explicitly, \
                 which works without reading any marker.",
                p.display()
            );
        }
        if !kind.is_file() {
            refuse!(
                Code::Marker,
                "REFUSED: {} is not a regular file, so it cannot be a project marker and was not read. Remove it, or \
                 name --env and --cauldron explicitly, which works without reading any marker.",
                p.display()
            );
        }
        let text = std::fs::read_to_string(&p)
            .map_err(|err| Error::caused(Code::Marker, format!("reading the project marker {}", p.display()), &err))?;
        return parse_marker(&p, &text).map(Some);
    }
    Ok(None)
}

/// What a verb resolved its target to, and from where.
#[derive(Debug, Clone)]
pub struct Resolved {
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
pub fn resolve(verb: &str, dir: Option<&Path>, flag_env: Option<&str>, flag_cauldron: Option<&str>) -> Result<Resolved> {
    let env_named: Option<(String, String)> = match flag_env {
        Some(n) => Some((n.to_string(), "--env".to_string())),
        // A variable that is set but names no usable environment is an
        // error, never "not named": the marker must not quietly win over it.
        None => match Environment::resolve().map_err(|e| Error::from_anyhow(Code::NoEnvironment, e))? {
            e if e.name_source != env::paths::Source::Default => Some((e.name, e.name_source.as_str().to_string())),
            _ => None,
        },
    };
    let cauldron_named: Option<(String, String)> = match flag_cauldron {
        Some(n) => Some((n.to_string(), "--cauldron".to_string())),
        None => env::cauldron::requested_cauldron()
            .map_err(|e| Error::from_anyhow(Code::Usage, e))?
            .map(|(n, s)| (n, s.as_str().to_string())),
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
        None => std::env::current_dir().map_err(|err| {
            Error::caused(
                Code::Marker,
                format!("reading the current directory, to look for a project marker ({verb} was not given both --env and --cauldron)"),
                &err,
            )
        })?,
    };
    let marker = find_marker(&dir)?;
    let Some(m) = marker else {
        if env_named.is_none() {
            refuse!(
                Code::NoEnvironment,
                "{verb} needs --env <name>: it never defaults to prod, and no project marker (`.cauldron`, written \
                 by `cauldron link`) was found in {} or above it",
                dir.display()
            );
        }
        refuse!(
            Code::NoCauldron,
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
                    refuse!(
                        Code::Marker,
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
pub enum GitState {
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
pub fn git_state(dir: &Path, name: &str) -> GitState {
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
/// Called before a soup verb WRITES to a target that the marker supplied
/// (the environment, the cauldron, or both). Refused when the marker is
/// tracked by git (it came with, or will travel with, the repository, so it
/// was written on another machine or by someone else) or is owned by a
/// different user than the store being written (`store`, the environment's
/// data root). Naming --env and --cauldron explicitly writes without reading
/// the marker at all. Reads are never refused by this.
pub fn refuse_untrusted_marker_for_write(verb: &str, r: &Resolved, store: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let Some(marker) = &r.marker else { return Ok(()) };
    let target = format!("cauldron `{}` in environment `{}`", r.cauldron, r.env);
    let explicit = format!("--env {} --cauldron {}", r.env, r.cauldron);
    let owner = std::fs::symlink_metadata(marker)
        .map_err(|err| Error::caused(Code::Marker, format!("reading {}", marker.display()), &err))?
        .uid();
    let store_owner = std::fs::metadata(store)
        .map_err(|err| Error::caused(Code::Store, format!("reading {}", store.display()), &err))?
        .uid();
    if owner != store_owner {
        refuse!(
            Code::Marker,
            "REFUSED: {verb} would write to {target}, named by the project marker {}, which belongs to another \
             user (uid {owner}, while the store belongs to uid {store_owner}). A marker only aims a write when it \
             is your own. Nothing was written. To write there, name it explicitly: {explicit}.",
            marker.display()
        );
    }
    let dir = marker.parent().unwrap_or(Path::new("/"));
    if git_state(dir, MARKER) == GitState::Tracked {
        refuse!(
            Code::Marker,
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
            let err = parse_marker(p, text).unwrap_err();
            assert_eq!(err.code, Code::Marker);
            let err = err.to_string();
            assert!(err.contains(why) && err.contains("/p/.cauldron"), "{text:?}: {err}");
        }
    }

    #[test]
    fn the_nearest_marker_file_wins_and_a_directory_of_that_name_is_passed_over() {
        let base = std::env::temp_dir().join(format!("soup-door-marker-{}", std::process::id()));
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
        let base = std::env::temp_dir().join(format!("soup-door-marker-sym-{}", std::process::id()));
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
