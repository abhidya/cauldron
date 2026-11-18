//! **The door secret**: a fresh bearer secret minted at every boot and
//! written to `<data root>/mcp/door.secret` (mode 0600, in a directory of
//! mode 0700). Every `POST /mcp` must carry it as `Authorization: Bearer
//! <secret>`; anything else is answered 401 before any of the body is read
//! (`crate::http`).
//!
//! It exists because a loopback port is reachable from any local process
//! and, through DNS rebinding, from a web page; only a process that can read
//! this user's files can read the secret. The bridge reads it at each call.
//!
//! The secret is never logged, printed, or written anywhere but that file:
//! [`Secret`]'s `Debug` prints no part of it, and nothing in this crate
//! formats it.

use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// The file's name under the service's directory.
pub const FILE: &str = "door.secret";

/// **The ripples key**: a second secret, minted the same way at every boot
/// into `<dir>/ripples.secret` (0600, in the 0700 directory). Only the
/// `ripples` service reads it: it proves a seam connection is the ripples
/// service's (`crate::ripple`). The bridge never reads it, and no tool of
/// the door accepts it.
pub const RIPPLES_FILE: &str = "ripples.secret";

/// **The approval key**: a third secret, minted the same way at every boot
/// into `<dir>/approve.secret`. Only `cauldron soup approve`, `cauldron
/// ripples approve | reject` and `cauldron ripples review --interactive`
/// read it, each only after it has checked that
/// its input and output are an interactive
/// terminal: it proves a seam connection is the owner's command line
/// (`crate::approve`). The bridge and the `ripples` service never read it,
/// and no tool of the door accepts it.
pub const APPROVE_FILE: &str = "approve.secret";

/// The bearer secret. 64 lowercase hex characters (32 random bytes).
#[derive(Clone)]
pub struct Secret(String);

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(..)")
    }
}

impl Secret {
    /// Does `authorization` (the header's whole value) carry this secret?
    /// Compared in constant time.
    pub fn admits(&self, authorization: Option<&str>) -> bool {
        let Some(given) = authorization.and_then(|a| a.strip_prefix("Bearer ")) else { return false };
        let (a, b) = (given.trim().as_bytes(), self.0.as_bytes());
        if a.len() != b.len() {
            return false;
        }
        a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
    }

    /// Is `given` exactly this secret (no scheme)? Constant time.
    pub fn is(&self, given: &str) -> bool {
        let (a, b) = (given.as_bytes(), self.0.as_bytes());
        a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
    }

    /// The secret itself, for a client that sends it on a seam line. Never
    /// logged.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// The header value a client sends. For tests and for the bridge's own
    /// copy; never logged.
    pub fn bearer(&self) -> String {
        format!("Bearer {}", self.0)
    }
}

/// `<dir>/door.secret`.
pub fn path(dir: &Path) -> PathBuf {
    dir.join(FILE)
}

/// Make `dir` (mode 0700) and mint a new secret into `dir/door.secret`
/// (mode 0600), replacing any earlier one by rename, so a reader sees the
/// old file or the new one and never half of either.
pub fn mint(dir: &Path) -> Result<Secret> {
    mint_named(dir, FILE)
}

/// [`mint`] into `dir/<file>`.
pub fn mint_named(dir: &Path, file: &str) -> Result<Secret> {
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir).with_context(|| format!("creating {}", dir.display()))?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).with_context(|| format!("restricting {}", dir.display()))?;
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut bytes)).context("reading /dev/urandom")?;
    let secret: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let tmp = dir.join(format!(".{file}.{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)
            .with_context(|| format!("creating {}", tmp.display()))?;
        f.write_all(secret.as_bytes()).and_then(|_| f.sync_all()).with_context(|| format!("writing {}", tmp.display()))?;
    }
    std::fs::rename(&tmp, dir.join(file)).with_context(|| format!("installing {}", dir.join(file).display()))?;
    Ok(Secret(secret))
}

/// Read the secret a running service minted (what a client does).
pub fn read(dir: &Path) -> Result<Secret> {
    read_named(dir, FILE)
}

/// [`read`] of `dir/<file>`.
pub fn read_named(dir: &Path, file: &str) -> Result<Secret> {
    let p = dir.join(file);
    let text = std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
    Ok(Secret(text.trim().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_minted_secret_is_private_new_each_time_and_admits_only_itself() {
        let dir = std::env::temp_dir().join(format!("mcp-secret-{}-{}", std::process::id(), chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)));
        let a = mint(&dir).unwrap();
        let meta = std::fs::metadata(path(&dir)).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        let b = mint(&dir).unwrap();
        assert_ne!(a.0, b.0, "a boot mints a new secret");
        assert_eq!(read(&dir).unwrap().0, b.0);
        assert!(b.admits(Some(&b.bearer())));
        assert!(!b.admits(Some(&a.bearer())));
        assert!(!b.admits(None));
        assert!(!b.admits(Some(&b.0)), "the scheme is required");
        assert_eq!(format!("{b:?}"), "Secret(..)");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
