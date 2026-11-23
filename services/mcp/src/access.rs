//! **The grants, as of this call.** The owner changes them with `cauldron
//! mcp grant | revoke` at a terminal, which rewrites
//! `<data root>/mcp/grants.json` by rename. Every call stats that file and
//! re-reads it when its inode, size, modification or change time moved, so
//! a revoke takes effect on the very next call. A store that cannot be read
//! refuses every call (`grant_store`): an unreadable store is never read as
//! "everything open", and never as "nothing changed".

use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::Mutex;

use soup_door::grants::{self, Grants, Scope};

/// What identifies one version of the store file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stamp {
    Absent,
    File { dev: u64, ino: u64, size: u64, mtime: (i64, i64), ctime: (i64, i64) },
    Unreadable,
}

fn stamp(path: &std::path::Path) -> Stamp {
    match std::fs::metadata(path) {
        Ok(m) => Stamp::File {
            dev: m.dev(),
            ino: m.ino(),
            size: m.size(),
            mtime: (m.mtime(), m.mtime_nsec()),
            ctime: (m.ctime(), m.ctime_nsec()),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Stamp::Absent,
        Err(_) => Stamp::Unreadable,
    }
}

/// The store of one environment, re-read whenever it changes.
pub struct Access {
    path: PathBuf,
    cache: Mutex<Option<(Stamp, Result<Grants, soup_door::Error>)>>,
}

impl Access {
    pub fn new(e: &env::Environment) -> Access {
        Access { path: grants::store_path(e), cache: Mutex::new(None) }
    }

    /// The grants as they stand now, and whether they changed since the last
    /// look.
    pub fn current(&self) -> (Result<Grants, soup_door::Error>, bool) {
        let now = stamp(&self.path);
        let mut cache = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((seen, g)) = cache.as_ref() {
            if *seen == now && now != Stamp::Unreadable {
                return (g.clone(), false);
            }
        }
        let g = Grants::load_from(&self.path);
        *cache = Some((now, g.clone()));
        (g, true)
    }

    /// [`Access::current`], the grants only.
    pub fn grants(&self) -> Result<Grants, soup_door::Error> {
        self.current().0
    }

    /// Why `cauldron` may not be written to now (`None` when its grant, as
    /// it stands now, allows `write`). The writer thread asks this before
    /// every write, and [`crate::door::Door::refresh`] before keeping a
    /// cauldron open.
    pub fn why_not_writable(&self, cauldron: &env::Cauldron) -> Option<&'static str> {
        self.why_not(cauldron, Scope::Write)
    }

    /// Why `cauldron` may not be dumped into now (`None` when its grant
    /// allows `dump`): asked before every dump lands, and before keeping a
    /// cauldron's dumps store open.
    pub fn why_not_dumpable(&self, cauldron: &env::Cauldron) -> Option<&'static str> {
        self.why_not(cauldron, Scope::Dump)
    }

    fn why_not(&self, cauldron: &env::Cauldron, scope: Scope) -> Option<&'static str> {
        match self.grants() {
            Err(_) => Some("the grant store cannot be read"),
            Ok(g) => match g.get(&cauldron.name) {
                None => Some("its grant was revoked"),
                Some(gr) if !gr.allows(scope) => Some(match scope {
                    Scope::Write => "its grant lost `write`",
                    Scope::Dump => "its grant lost `dump`",
                    Scope::Read => "its grant lost `read`",
                }),
                Some(gr) if !gr.is_for(cauldron) => Some("the cauldron of that name is not the one that was granted"),
                Some(_) => None,
            },
        }
    }
}
