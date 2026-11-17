//! **A cauldron's data is its owner's only.** A cauldron's directory
//! (`data/<env>/cauldrons/<name>/`: its soup, dumps store, face files,
//! events and ripples) holds the user's knowledge graph and whatever was
//! dumped into it, so every directory in it is mode [`DIR_MODE`] (0700) and
//! every file [`FILE_MODE`] (0600), whatever the umask. For the legacy
//! cauldron, whose data is spread over the environment's data root, that is
//! its soup directory, `data/<env>/soup/` (the soup's spine and its dumps
//! store).
//!
//! `cauldron create` makes the directory so: built in a temporary sibling
//! made 0700 (never open to others), tightened, then moved into place. A
//! store made before this release is tightened by the next writer that
//! opens it (`cauldron soup import`/`recover`, and the `mcp` service), with
//! [`tighten_cauldron`]. Symbolic links are never followed or changed.
//! Anything the same user runs (the face, `cauldron list`, the services)
//! reads it exactly as before.

use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::Cauldron;

/// The mode of every directory in a cauldron's data: the owner's only.
pub const DIR_MODE: u32 = 0o700;
/// The mode of every file in a cauldron's data: the owner's only.
pub const FILE_MODE: u32 = 0o600;

/// Make `p` (its parent exists) a directory of mode [`DIR_MODE`], or set an
/// existing one to it. It is made with that mode, so it is never open to
/// others, then set to exactly that mode, whatever the umask. A link or a
/// file in its place is an error.
pub fn make_dir(p: &Path) -> std::io::Result<()> {
    match std::fs::DirBuilder::new().mode(DIR_MODE).create(p) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    let md = std::fs::symlink_metadata(p)?;
    if !md.is_dir() {
        return Err(std::io::Error::other(format!("{} is not a directory", p.display())));
    }
    if md.permissions().mode() & 0o777 != DIR_MODE {
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(DIR_MODE))?;
    }
    Ok(())
}

/// Set `root` and every directory under it to [`DIR_MODE`] and every regular
/// file under it to [`FILE_MODE`], changing only what differs. A directory
/// is set before what is in it is read, so nothing is open to others for
/// longer than it already was. Symbolic links are neither followed nor
/// changed. A path that vanishes while it is walked (a file a running writer
/// removed) is skipped. Absent `root`: nothing to do.
pub fn tighten_tree(root: &Path) -> std::io::Result<()> {
    let md = match std::fs::symlink_metadata(root) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if !md.is_dir() {
        return Err(std::io::Error::other(format!("{} is not a directory", root.display())));
    }
    let mut todo: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(d) = todo.pop() {
        set_mode(&d, DIR_MODE)?;
        let rd = match std::fs::read_dir(&d) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        for entry in rd {
            let entry = entry?;
            let p = entry.path();
            let md = match std::fs::symlink_metadata(&p) {
                Ok(m) => m,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
            };
            if md.is_dir() {
                todo.push(p);
            } else if md.is_file() && md.permissions().mode() & 0o777 != FILE_MODE {
                set_mode(&p, FILE_MODE)?;
            }
        }
    }
    Ok(())
}

fn set_mode(p: &Path, mode: u32) -> std::io::Result<()> {
    match std::fs::symlink_metadata(p) {
        Ok(m) if m.permissions().mode() & 0o777 == mode => Ok(()),
        Ok(_) => match std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            r => r,
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// The directory that holds `c`'s data and is made private: `cauldrons/<name>/`,
/// or for the legacy cauldron its soup directory `data/<env>/soup/`.
pub fn data_dir(c: &Cauldron) -> PathBuf {
    match c.dir() {
        Some(d) => d,
        None => {
            let soup = c.soup();
            soup.parent().map(Path::to_path_buf).unwrap_or(soup)
        }
    }
}

/// **Make `c`'s data private** ([`tighten_tree`] over [`data_dir`]): what a
/// writer does when it opens a cauldron, so a store made before this release
/// (directories 0755, files 0644) is its owner's only from then on.
pub fn tighten_cauldron(c: &Cauldron) -> std::io::Result<()> {
    tighten_tree(&data_dir(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(p: &Path) -> u32 {
        std::fs::symlink_metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn a_tree_is_tightened_and_a_link_is_left_alone() {
        let root = std::env::temp_dir().join(format!("env-private-{}-{}", std::process::id(), line!()));
        let _ = std::fs::remove_dir_all(&root);
        let outside = root.with_extension("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("f"), "x").unwrap();
        std::fs::set_permissions(outside.join("f"), std::fs::Permissions::from_mode(0o644)).unwrap();
        let d = root.join("c");
        std::fs::create_dir_all(d.join("soup/deep")).unwrap();
        for (f, m) in [("c/a", 0o644), ("c/soup/spine.db", 0o664), ("c/soup/deep/x", 0o600)] {
            std::fs::write(root.join(f), "x").unwrap();
            std::fs::set_permissions(root.join(f), std::fs::Permissions::from_mode(m)).unwrap();
        }
        std::fs::set_permissions(d.join("soup"), std::fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink(&outside, d.join("link-dir")).unwrap();
        std::os::unix::fs::symlink(outside.join("f"), d.join("link-file")).unwrap();
        tighten_tree(&d).unwrap();
        for p in ["c", "c/soup", "c/soup/deep"] {
            assert_eq!(mode(&root.join(p)), DIR_MODE, "{p}");
        }
        for p in ["c/a", "c/soup/spine.db", "c/soup/deep/x"] {
            assert_eq!(mode(&root.join(p)), FILE_MODE, "{p}");
        }
        assert_eq!(mode(&outside.join("f")), 0o644, "a link was followed");
        assert_ne!(mode(&outside), DIR_MODE, "a link was followed");
        // Absent: nothing to do. A file in its place: an error.
        assert!(tighten_tree(&root.join("absent")).is_ok());
        assert!(tighten_tree(&root.join("c/a")).is_err());
        // make_dir: made 0700 whatever the umask, and an existing one set so.
        make_dir(&root.join("new")).unwrap();
        assert_eq!(mode(&root.join("new")), DIR_MODE);
        std::fs::set_permissions(root.join("new"), std::fs::Permissions::from_mode(0o755)).unwrap();
        make_dir(&root.join("new")).unwrap();
        assert_eq!(mode(&root.join("new")), DIR_MODE);
        assert!(make_dir(&root.join("c/a")).is_err());
        // What is made private: a cauldron's directory, or the legacy
        // cauldron's soup directory (its spine and its dumps store).
        let at = |kind| Cauldron { name: "dev".into(), env: "dev".into(), kind, data_root: root.join("data/dev") };
        assert_eq!(data_dir(&at(crate::cauldron::CauldronKind::Legacy)), root.join("data/dev/soup"));
        assert_eq!(data_dir(&at(crate::cauldron::CauldronKind::Dir)), root.join("data/dev/cauldrons/dev"));
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }
}
