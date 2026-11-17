//! **The run key**: a secret minted at every start into
//! `<data root>/ripples/run.secret` (mode 0600, in a 0700 directory). The
//! seam's `RUN` and `STATUS` answer only a line that carries it, so only a
//! process that can read this user's private files (`cauldron ripples`)
//! starts a run that spends the owner's model budget.

use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

use anyhow::{Context, Result};

/// The file's name under the service's directory.
pub const FILE: &str = "run.secret";

/// The key. Its `Debug` prints none of it.
#[derive(Clone)]
pub struct Key(String);

impl std::fmt::Debug for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Key(..)")
    }
}

impl Key {
    /// Is `given` this key? Constant time.
    pub fn is(&self, given: &str) -> bool {
        let (a, b) = (given.as_bytes(), self.0.as_bytes());
        a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
    }
}

/// Make `dir` (0700) and mint a new key into `dir/run.secret` (0600) by
/// rename.
pub fn mint(dir: &Path) -> Result<Key> {
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir).with_context(|| format!("creating {}", dir.display()))?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut bytes)).context("reading /dev/urandom")?;
    let key: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let tmp = dir.join(format!(".{FILE}.{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    {
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
        f.write_all(key.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, dir.join(FILE))?;
    Ok(Key(key))
}
