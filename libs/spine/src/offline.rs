//! **Offline access to a cauldron directory**: what a process that is
//! NOT the soup daemon needs before it touches a cauldron — a connection that
//! cannot write, and the question "is this spine at rest?".
//!
//! Why the question matters. [`crate::Cauldron::open`] is the only open a
//! production consumer has, and it runs the recovery pass and the tranche
//! pass before it returns: they roll forward, abort, sweep orphan locks and
//! reset tranche deadlines. That is right for the daemon that owns the
//! cauldron. A one-shot outside writer (`cauldron soup import`) should open
//! only a spine on which those passes have NOTHING to do, so the one write
//! it makes is its own commit. [`rest_check`] reads the spine file through a
//! read-only connection and names every reason the passes would act; an
//! empty answer means they will find nothing.

use crate::error::{Result, SpineError};
use crate::spine_db::{SPINE_FILE, SPINE_SCHEMA_VERSION};
use rusqlite::{Connection, OpenFlags};
use std::path::Path;

/// A connection to `path` that CANNOT write, and that creates nothing beside
/// the file.
///
/// A plain read-only open of a WAL database still creates `-wal` and `-shm`
/// beside it. So: with no `-wal` present, no writer has the file open and
/// everything is in the main file, which is read `immutable=1` (no side
/// files, no locks); with a `-wal` present, a writer's side files already
/// exist and `mode=ro` reads through them without creating any. (The same
/// rule the CLI's tranche check uses, `cli/src/cauldrons.rs`.)
pub fn read_only_connection(path: &Path) -> Result<Connection> {
    let wal = {
        let mut p = path.as_os_str().to_owned();
        p.push("-wal");
        std::path::PathBuf::from(p)
    };
    let mode = if wal.exists() { "mode=ro" } else { "immutable=1" };
    let text = path
        .display()
        .to_string()
        .replace('%', "%25")
        .replace('?', "%3f")
        .replace('#', "%23");
    Connection::open_with_flags(
        format!("file:{text}?{mode}"),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| SpineError::hard(format!("cannot open `{}` read-only: {e}", path.display())))
}

/// **Every reason the open-time passes would ACT on the cauldron at `dir`**,
/// read through a read-only connection. Empty = at rest: not halted, the
/// spine at this binary's schema version, no un-cleared transaction row, no
/// lock row, no live tranche, no outstanding quiesce, and every pool file
/// carrying the seal column. `Err` when the spine cannot be read at all —
/// which a caller treats as "cannot prove it is at rest".
pub fn rest_check(dir: &Path) -> Result<Vec<String>> {
    let spine = dir.join(SPINE_FILE);
    if !spine.is_file() {
        return Err(SpineError::not_found(format!("`{}` holds no `{SPINE_FILE}`", dir.display())));
    }
    let conn = read_only_connection(&spine)?;
    let mut why = Vec::new();
    let has = |table: &str| -> Result<bool> {
        let n: i64 = conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    };
    let count = |sql: &str| -> Result<i64> { Ok(conn.query_row(sql, [], |r| r.get(0))?) };

    let version: i64 = if conn
        .query_row(
            "SELECT count(*) FROM pragma_table_info('spine_state') WHERE name = 'spine_schema_version'",
            [],
            |r| r.get::<_, i64>(0),
        )?
        > 0
    {
        count("SELECT spine_schema_version FROM spine_state WHERE singleton = 1")?
    } else {
        0
    };
    if version != i64::from(SPINE_SCHEMA_VERSION) {
        why.push(format!(
            "the spine is at schema version {version}, this binary's is {SPINE_SCHEMA_VERSION} (an open would \
             upgrade or refuse it)"
        ));
    }
    let halted: i64 = count("SELECT halted FROM spine_state WHERE singleton = 1")?;
    if halted != 0 {
        why.push("the spine is HALTED".to_string());
    }
    let pending = count("SELECT count(*) FROM transactions WHERE cleared = 0")?;
    if pending > 0 {
        why.push(format!("{pending} transaction row(s) are not cleared (in flight, or stranded for recovery)"));
    }
    if has("locks")? {
        let locks = count("SELECT count(*) FROM locks")?;
        if locks > 0 {
            why.push(format!("{locks} lock row(s) are held"));
        }
    }
    if has("tranches")? {
        let live = count("SELECT count(*) FROM tranches WHERE state IN ('queued','open','staging','landing')")?;
        if live > 0 {
            why.push(format!("{live} tranche(s) are live"));
        }
    }
    if has("quiescers")? {
        let q = count("SELECT count(*) FROM quiescers")?;
        if q > 0 {
            why.push(format!("the spine is quiesced ({q} holder(s))"));
        }
    }
    drop(conn);
    for entry in std::fs::read_dir(dir)
        .map_err(|e| SpineError::hard(format!("cannot list `{}`: {e}", dir.display())))?
    {
        let p = entry.map_err(|e| SpineError::hard(format!("cannot list `{}`: {e}", dir.display())))?.path();
        if p.extension().and_then(|e| e.to_str()) != Some("db") || p.file_name().and_then(|n| n.to_str()) == Some(SPINE_FILE) {
            continue;
        }
        let c = read_only_connection(&p)?;
        let sealed: i64 = c.query_row(
            "SELECT count(*) FROM pragma_table_info('nodes') WHERE name = 'sealed'",
            [],
            |r| r.get(0),
        )?;
        if sealed == 0 {
            why.push(format!("pool file `{}` predates the seal flag (an open would add the column)", p.display()));
        }
    }
    Ok(why)
}

/// **The commit order of every landed transaction** of the cauldron at
/// `dir`, read through a read-only connection: transaction hash (hex, as
/// [`crate::pool::Snapshot::created_tx`] gives it) -> the committed-commit
/// counter at its commit. What "newest first" means for an outside reader
/// (`cauldron soup nodes`): the spine's own order, never a timestamp.
/// A transaction that has not landed has no entry.
pub fn commit_order(dir: &Path) -> Result<std::collections::HashMap<String, i64>> {
    let conn = read_only_connection(&dir.join(SPINE_FILE))?;
    let mut stmt = conn.prepare("SELECT hash, committed_at FROM transactions WHERE committed_at IS NOT NULL")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
    let mut out = std::collections::HashMap::new();
    for row in rows {
        let (hash, at) = row?;
        out.insert(hash, at);
    }
    Ok(out)
}

/// **Copy the cauldron at `src` into the empty directory `dst`** without
/// writing to `src`: every `*.db` file (the spine and each pool) is read
/// through [`read_only_connection`] and written out with `VACUUM INTO`,
/// whose only write is the new file under `dst`. Each copy is a consistent
/// snapshot of its own file; the files are taken one after another, so a
/// writer active meanwhile can leave the set mid-commit, which
/// [`rest_check`] on the copy then names. What it is for: a dry run that
/// must not open the live store with [`crate::Cauldron::open`] (whose
/// passes write) runs that open on the copy instead.
pub fn snapshot_copy(src: &Path, dst: &Path) -> Result<()> {
    if !src.join(SPINE_FILE).is_file() {
        return Err(SpineError::not_found(format!("`{}` holds no `{SPINE_FILE}`", src.display())));
    }
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    for entry in std::fs::read_dir(src).map_err(|e| SpineError::hard(format!("cannot list `{}`: {e}", src.display())))? {
        let p = entry.map_err(|e| SpineError::hard(format!("cannot list `{}`: {e}", src.display())))?.path();
        if p.extension().and_then(|e| e.to_str()) == Some("db") {
            files.push(p);
        }
    }
    // The pools first and the spine last: the spine copy can only be AHEAD
    // of a pool copy, so its serial counters never fall behind a node the
    // pool copy holds (a re-mint judged on the copy predicts no id the copy
    // already uses), and a commit caught in flight is an un-cleared row the
    // rest check on the copy names.
    files.sort_by_key(|p| (p.file_name().and_then(|n| n.to_str()) == Some(SPINE_FILE), p.clone()));
    for f in files {
        let out = dst.join(f.file_name().expect("a listed file has a name"));
        if out.exists() {
            return Err(SpineError::usage(format!("`{}` already exists; a snapshot copy goes into an empty directory", out.display())));
        }
        let conn = read_only_connection(&f)?;
        conn.execute("VACUUM INTO ?1", [out.to_str().ok_or_else(|| SpineError::usage(format!("`{}` is not UTF-8", out.display())))?])
            .map_err(|e| SpineError::hard(format!("cannot copy `{}`: {e}", f.display())))?;
    }
    Ok(())
}
