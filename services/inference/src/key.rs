//! **The door key**: a secret minted at every start into
//! `<data root>/inference/door.secret` (mode 0600, in a 0700 directory),
//! as the mcp door's secret is. Every verb but `PING` answers only a line
//! that carries it, so only a process that can read this user's private
//! files (the ripples service, `cauldron inference`) can load a model or
//! spend this machine's memory and time on a completion.

use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

use anyhow::{Context, Result};

/// The file's name under the service's directory.
pub const FILE: &str = "door.secret";

/// 32 random bytes as hex.
pub fn random_hex() -> String {
    let mut bytes = [0u8; 32];
    if std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut bytes)).is_err() {
        // No randomness is no key: a key nobody can guess is the point.
        panic!("cannot read /dev/urandom");
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

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

/// Make `dir` (0700) and mint a new key into `dir/door.secret` (0600) by
/// rename.
pub fn mint(dir: &Path) -> Result<Key> {
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir).with_context(|| format!("creating {}", dir.display()))?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    let key = random_hex();
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
