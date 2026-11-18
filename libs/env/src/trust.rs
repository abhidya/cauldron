//! **MAY THIS FILE BE EXECUTED?** One answer for every helper a cauldron
//! binary runs by path.
//!
//! A path is only as safe as the directories it passes through: a file you
//! own inside a directory another user can write to can be replaced (or, if
//! it does not exist yet, created) by that user, and then runs as you. So a
//! helper is admitted only when ALL of these hold:
//!
//! - the path is absolute;
//! - it resolves (it exists), and what it resolves to is a regular file;
//! - that file is owned by the user running this process (or by root, when
//!   the caller allows it) and is not writable by group or others;
//! - every directory on the path AS GIVEN and on its CANONICAL form (after
//!   every symlink is resolved) is owned by that user or by root and is not
//!   writable by group or others. A world-writable directory such as
//!   `/private/tmp` fails this even with its sticky bit: anyone may CREATE a
//!   name in it, and a name that does not exist yet is exactly what a
//!   compile-time path left behind.
//!
//! A refusal is a sentence that names the path and the reason, for the
//! caller to show as it is.

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// The effective uid of this process, read once from `/usr/bin/id -u` (this
/// crate forbids `unsafe`, so it does not call `geteuid` itself). `None` when
/// that cannot be read, and every check then refuses.
pub fn current_uid() -> Option<u32> {
    static UID: std::sync::OnceLock<Option<u32>> = std::sync::OnceLock::new();
    *UID.get_or_init(|| {
        let out = std::process::Command::new("/usr/bin/id")
            .arg("-u")
            .env_clear()
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        String::from_utf8(out.stdout).ok()?.trim().parse().ok()
    })
}

/// [`trusted_executable_as`] for the user running this process.
pub fn trusted_executable(path: &Path, allow_root_owned: bool) -> Result<PathBuf, String> {
    let uid = current_uid().ok_or_else(|| "the uid of this process could not be read (`/usr/bin/id -u`)".to_string())?;
    trusted_executable_as(path, uid, allow_root_owned)
}

/// `Ok(<canonical path>)` when `path` may be executed by `uid` (see the
/// module comment); otherwise the reason.
pub fn trusted_executable_as(path: &Path, uid: u32, allow_root_owned: bool) -> Result<PathBuf, String> {
    let shown = path.display();
    if !path.is_absolute() {
        return Err(format!("{shown} is not an absolute path"));
    }
    let canonical = std::fs::canonicalize(path).map_err(|e| format!("{shown} does not resolve ({e})"))?;
    let meta = std::fs::metadata(&canonical).map_err(|e| format!("{} cannot be read ({e})", canonical.display()))?;
    if !meta.file_type().is_file() {
        return Err(format!("{} is not a regular file", canonical.display()));
    }
    let owner_ok = meta.uid() == uid || (allow_root_owned && meta.uid() == 0);
    if !owner_ok {
        let whom = if allow_root_owned { "you or root" } else { "you" };
        return Err(format!("{} is owned by uid {}, not by {whom}", canonical.display(), meta.uid()));
    }
    if meta.mode() & 0o022 != 0 {
        return Err(format!("{} is writable by group or others (mode {:o})", canonical.display(), meta.mode() & 0o7777));
    }
    if meta.mode() & 0o111 == 0 {
        return Err(format!("{} is not executable", canonical.display()));
    }
    for dir in path.ancestors().skip(1).chain(canonical.ancestors().skip(1)) {
        directory_is_safe(dir, uid)?;
    }
    Ok(canonical)
}

/// A directory on a trusted path: owned by `uid` or root, and not writable
/// by group or others. Symlinks are followed (the link itself sits in its
/// parent, which is checked as the next ancestor).
fn directory_is_safe(dir: &Path, uid: u32) -> Result<(), String> {
    if dir.as_os_str().is_empty() {
        return Ok(());
    }
    let m = std::fs::metadata(dir).map_err(|e| format!("{} cannot be read ({e})", dir.display()))?;
    if m.uid() != uid && m.uid() != 0 {
        return Err(format!("{} is owned by uid {}, not by you or root, so its owner can replace what is in it", dir.display(), m.uid()));
    }
    if m.mode() & 0o022 != 0 {
        return Err(format!(
            "{} is writable by group or others (mode {:o}), so someone else can create or replace what is in it",
            dir.display(),
            m.mode() & 0o7777
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    struct Scratch(PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn scratch(tag: &str) -> Scratch {
        let p = std::env::temp_dir().join(format!("env-trust-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        Scratch(p.canonicalize().unwrap())
    }
    fn tool(p: &Path, mode: u32) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).unwrap();
    }
    fn me() -> u32 {
        current_uid().expect("id -u")
    }

    #[test]
    fn a_file_you_own_in_directories_only_you_or_root_can_write_is_admitted() {
        let s = scratch("ok");
        let t = s.0.join("bin/helper");
        tool(&t, 0o755);
        assert_eq!(trusted_executable(&t, false), Ok(t.clone()));
    }

    #[test]
    fn a_relative_a_missing_and_a_non_regular_path_are_refused() {
        let s = scratch("shape");
        assert!(trusted_executable(Path::new("./helper"), false).unwrap_err().contains("not an absolute path"));
        assert!(trusted_executable(&s.0.join("nope"), false).unwrap_err().contains("does not resolve"));
        assert!(trusted_executable(&s.0, false).unwrap_err().contains("not a regular file"));
    }

    #[test]
    fn a_writable_file_or_a_writable_directory_anywhere_on_the_path_is_refused() {
        let s = scratch("ww");
        let t = s.0.join("gw/helper");
        tool(&t, 0o775);
        assert!(trusted_executable(&t, false).unwrap_err().contains("writable by group or others"));
        // A file you own inside a world-writable (sticky) directory: refused,
        // because anyone can create a name there.
        let t = s.0.join("open/inner/helper");
        tool(&t, 0o755);
        std::fs::set_permissions(s.0.join("open"), std::fs::Permissions::from_mode(0o1777)).unwrap();
        let e = trusted_executable(&t, false).unwrap_err();
        assert!(e.contains("open") && e.contains("writable by group or others"), "{e}");
        // The same through a symlink whose own directory is safe: the
        // canonical chain is checked too.
        let link = s.0.join("link");
        std::os::unix::fs::symlink(s.0.join("open/inner"), &link).unwrap();
        assert!(trusted_executable(&link.join("helper"), false).unwrap_err().contains("writable by group or others"));
        std::fs::set_permissions(s.0.join("open"), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(trusted_executable(&link.join("helper"), false).is_ok());
    }

    #[test]
    fn a_file_owned_by_someone_else_is_refused_and_root_only_when_allowed() {
        // /bin/sh is root's, in root's directories.
        assert!(trusted_executable(Path::new("/bin/sh"), false).unwrap_err().contains("owned by uid 0"));
        assert!(trusted_executable(Path::new("/bin/sh"), true).is_ok());
        // Another uid owns a file of ours, as far as the check can tell.
        let s = scratch("uid");
        let t = s.0.join("helper");
        tool(&t, 0o755);
        let other = me().wrapping_add(1);
        assert!(trusted_executable_as(&t, other, false).unwrap_err().contains("not by you"));
    }

    /// THE PLANTED CASE: the shape a compile-time build path in the shared
    /// temporary directory has. A helper there that you own, mode 755, in a
    /// directory of yours that nobody else can write, is still refused,
    /// because `/private/tmp` lets anyone create the directory first.
    #[test]
    fn a_helper_planted_under_the_shared_temporary_directory_is_refused() {
        let s = Scratch(PathBuf::from(format!("/private/tmp/env-trust-planted-{}", std::process::id())));
        let _ = std::fs::remove_dir_all(&s.0);
        let t = s.0.join("src/services/threads/scripts/claude-accounts");
        tool(&t, 0o755);
        let e = trusted_executable(&t, false).unwrap_err();
        assert!(e.contains("/private/tmp is writable by group or others"), "{e}");
        // Through the `/tmp` symlink too.
        let via = PathBuf::from("/tmp").join(t.strip_prefix("/private/tmp").unwrap());
        assert!(trusted_executable(&via, false).unwrap_err().contains("writable by group or others"));
    }
}
