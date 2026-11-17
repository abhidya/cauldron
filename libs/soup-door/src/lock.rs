//! **The two proofs every writer of a soup makes before it opens the store**:
//! no soup daemon in the environment holds it, and no other writer is
//! writing it (the soup's writer lock).

use std::fs::File;

use env::seams::{self, Table};
use env::Environment;

use crate::{refuse, Code, Error, Result};

/// The file a writer of a soup locks EXCLUSIVE for as long as it writes:
/// `<soup>/writer.lock`. `import` and `recover` both take it, non-blocking,
/// after the soup-daemon check, and so does the `mcp` service, which holds it
/// for as long as it holds a granted cauldron open; the kernel drops it when
/// the holder exits, however it exits, so a killed writer never leaves it
/// held.
pub const WRITER_LOCK: &str = "writer.lock";

/// The soup's seam name in an environment's seam table.
pub const SOUP_SEAM: &str = "soup";

/// **The soup's writer lock, taken EXCLUSIVE and non-blocking.** Held until
/// the returned file is dropped. Another writer holding it refuses
/// ([`Code::Locked`]). `verb` names the act in the refusal
/// (`cauldron soup <verb>`).
pub fn take_writer_lock(c: &env::Cauldron, verb: &str) -> Result<File> {
    let path = c.soup().join(WRITER_LOCK);
    let fh = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|err| Error::caused(Code::Store, format!("opening {}", path.display()), &err))?;
    match fh.try_lock() {
        Ok(()) => Ok(fh),
        Err(std::fs::TryLockError::WouldBlock) => match held_by_service(c) {
            Some(pid) => refuse!(
                Code::Locked,
                "REFUSED: the `{MCP_SERVICE}` service (pid {pid}) holds cauldron `{}`'s soup: while the cauldron is \
                 granted to the MCP door, that service is its one writer, and it recovers the soup itself when it \
                 opens it. So `cauldron soup {verb}` did not touch it. Nothing was written. See `cauldron mcp status`.",
                c.name
            ),
            None => refuse!(
                Code::Locked,
                "REFUSED: another `cauldron soup import` or `cauldron soup recover` is writing to cauldron `{}`'s soup \
                 right now ({} is held), so `cauldron soup {verb}` did not touch it. Nothing was written. Try again \
                 when it has finished.",
                c.name,
                path.display()
            ),
        },
        Err(std::fs::TryLockError::Error(err)) => Err(Error::caused(Code::Store, format!("locking {}", path.display()), &err)),
    }
}

/// The name of the service that holds a granted cauldron's soup open, and
/// the first word it writes into the writer lock (`mcp <pid>`).
pub const MCP_SERVICE: &str = "mcp";

/// What `<soup>/writer.lock` says about its holder: the `mcp` service writes
/// `mcp <pid>` into it when it takes the lock. Informational only: the lock
/// itself is the truth, and the text stays after the service lets go, so a
/// caller reads this only once taking the lock was refused.
pub fn lock_says(c: &env::Cauldron) -> Option<String> {
    std::fs::read_to_string(c.soup().join(WRITER_LOCK)).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// The pid the writer lock's text names as the `mcp` service, if it names
/// it. Read it only after the lock was refused ([`lock_says`]).
pub fn held_by_service(c: &env::Cauldron) -> Option<u32> {
    let text = lock_says(c)?;
    let (who, pid) = text.split_once(' ')?;
    (who == MCP_SERVICE).then(|| pid.trim().parse().ok()).flatten()
}

/// A running soup daemon in the environment may hold any of its cauldrons,
/// so its live `soup` row refuses a write ([`Code::LiveSoup`]); a seam table
/// that cannot be read refuses too ([`Code::SeamTable`]: no proof). Reads the
/// seam table only.
pub fn refuse_live_soup(e: &Environment) -> Result<()> {
    let table = Table::load(&e.name, &e.data_root, &e.seams()).map_err(|err| {
        Error::new(
            Code::SeamTable,
            format!(
                "REFUSED: cannot read environment `{}`'s seam table, so cannot prove no soup daemon holds the cauldron: {err:#}",
                e.name
            ),
        )
    })?;
    if let Some(s) = table.seams.get(SOUP_SEAM) {
        if !seams::probe(s).process_gone(s) {
            refuse!(
                Code::LiveSoup,
                "REFUSED: environment `{}`'s soup daemon is running (pid {}, {}) and may hold this cauldron's \
                 soup. `cauldron soup import` and `cauldron soup recover` write only to a soup no daemon holds, \
                 because they open the store directly. Nothing was written. While it runs, `import --dry-run` \
                 still judges a batch against a read-only snapshot, and the reads (`soup regions | region | \
                 nodes | search | get`) still work.",
                e.name,
                s.pid,
                s.addr
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cauldron_with_lock(text: Option<&str>) -> (std::path::PathBuf, env::Cauldron) {
        let dir = std::env::temp_dir().join(format!(
            "soup-door-lock-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        let soup = dir.join("cauldrons").join("kb").join("soup");
        std::fs::create_dir_all(&soup).unwrap();
        if let Some(t) = text {
            std::fs::write(soup.join(WRITER_LOCK), t).unwrap();
        }
        let e = env::Environment {
            root: dir.clone(),
            root_source: env::paths::Source::CauldronHome,
            name: "dev".into(),
            name_source: env::paths::Source::CauldronEnv,
            data_root: dir.clone(),
        };
        let c = e.find_cauldron("kb").ok().flatten();
        (dir, c.expect("the cauldron's directory is found"))
    }

    #[test]
    fn only_the_service_s_own_line_names_it_as_the_holder() {
        for (text, want) in [
            (Some("mcp 4242\n"), Some(4242)),
            (Some("mcp 17"), Some(17)),
            (Some("mcp"), None),
            (Some("mcpx 4242"), None),
            (Some("mcp four"), None),
            (Some(""), None),
            (None, None),
        ] {
            let (dir, c) = cauldron_with_lock(text);
            assert_eq!(held_by_service(&c), want, "{text:?}");
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}
