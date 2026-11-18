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
/// beside it, so which open is used depends on whether a writer is there:
///
/// - with a `-wal` present, a writer's side files already exist and
///   `mode=ro` reads through them without creating any, taking part in
///   SQLite's own locking (a checkpoint waits for this reader);
/// - with no `-wal`, everything is in the main file. It is NOT read in place:
///   an `immutable=1` connection takes no lock, so a writer that opens the
///   file meanwhile, commits, and checkpoints its log back into the main
///   file as its last connection closes rewrites pages under a read in
///   flight. That read then sees a mix of old and new pages: SQLite answers
///   `database disk image is malformed`, or, worse, a wrong answer that
///   happens to parse. (Seen by readers hammering a store while a service
///   wrote to it.) So the file is first copied to a private file in a
///   directory of the temporary directory only its owner can enter
///   ([`private_dir`]; a clone, on APFS: no data is copied), and the copy
///   is kept only if no `-wal` appeared and the original's identity (inode,
///   size, modification and change times) did not change across the copy;
///   the copy is opened `immutable=1` and unlinked at once, so the reader
///   holds a snapshot no writer can touch and leaves nothing behind. If the
///   file changed during the copy, it is tried again (a writer is now there,
///   so the next attempt usually reads through its log), a bounded number
///   of times; after that it is read `mode=ro`, which is correct but may
///   leave `-wal` and `-shm` beside the file.
pub fn read_only_connection(path: &Path) -> Result<Connection> {
    let wal = side_file(path, "-wal");
    for attempt in 0..SNAPSHOT_ATTEMPTS {
        if wal.exists() {
            return open_uri(path, "mode=ro");
        }
        if let Some(conn) = settled_copy(path, &wal)? {
            return Ok(conn);
        }
        std::thread::sleep(std::time::Duration::from_millis(1 + attempt as u64));
    }
    open_uri(path, "mode=ro")
}

/// How many times [`read_only_connection`] copies a file that changed during
/// the copy before it reads through SQLite's locking instead.
const SNAPSHOT_ATTEMPTS: usize = 20;

fn side_file(path: &Path, suffix: &str) -> std::path::PathBuf {
    let mut p = path.as_os_str().to_owned();
    p.push(suffix);
    std::path::PathBuf::from(p)
}

/// What changes when a file is written: any write between two readings of
/// it changes its modification and change times.
fn identity(path: &Path) -> Result<(u64, u64, u64, i64, i64, i64, i64)> {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::metadata(path)
        .map_err(|e| SpineError::hard(format!("cannot open `{}` read-only: {e}", path.display())))?;
    Ok((m.dev(), m.ino(), m.size(), m.mtime(), m.mtime_nsec(), m.ctime(), m.ctime_nsec()))
}

/// A private copy of `path` taken while nothing wrote to it, opened
/// `immutable=1` and already unlinked; `None` when a write was seen during
/// the copy, or when no private directory can be had for the copy (then the
/// caller reads through SQLite's locking instead).
fn settled_copy(path: &Path, wal: &Path) -> Result<Option<Connection>> {
    use std::os::unix::fs::MetadataExt;
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let before = identity(path)?;
    let owner = std::fs::metadata(path)
        .map_err(|e| SpineError::hard(format!("cannot open `{}` read-only: {e}", path.display())))?
        .uid();
    let Some(dir) = private_dir(&std::env::temp_dir(), owner) else {
        return Ok(None);
    };
    let copy = dir.join(format!(
        "{}-{}-{}.db",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos())
    ));
    if let Err(e) = std::fs::copy(path, &copy) {
        let _ = std::fs::remove_file(&copy);
        return Err(SpineError::hard(format!("cannot open `{}` read-only (copying it to read: {e})", path.display())));
    }
    let settled = !wal.exists() && identity(path)? == before;
    if !settled {
        let _ = std::fs::remove_file(&copy);
        return Ok(None);
    }
    let conn = open_uri(&copy, "immutable=1");
    let _ = std::fs::remove_file(&copy);
    let conn = conn.map_err(|e| SpineError::hard(format!("cannot open `{}` read-only: {e}", path.display())))?;
    Ok(Some(conn))
}

/// The directory the private copies go in: `spine-read-<uid>` in the
/// temporary directory, made with mode 0700 and used only while it is a
/// real directory (not a symlink) owned by the store's owner that nobody
/// else can enter. One someone else made first is never used.
fn private_dir(base: &Path, owner: u32) -> Option<std::path::PathBuf> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    let dir = base.join(format!("spine-read-{owner}"));
    match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return None,
    }
    let m = std::fs::symlink_metadata(&dir).ok()?;
    (m.file_type().is_dir() && m.uid() == owner && m.mode() & 0o077 == 0).then_some(dir)
}

fn open_uri(path: &Path, mode: &str) -> Result<Connection> {
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

#[cfg(test)]
mod tests {
    use rusqlite::Connection;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    /// Readers hammering a WAL file while a writer lands write after write,
    /// each on a connection of its own that it closes (so its log is
    /// checkpointed into the main file and removed, as the spine's are).
    /// Every write moves value between rows and keeps the total, so a read
    /// that mixed pages of two states would see another total (reading the
    /// main file in place, this saw both wrong totals and `database disk
    /// image is malformed`). Zero refusals and every total right.
    #[test]
    fn reads_while_writes_land_are_never_refused_or_torn() {
        let d = std::env::temp_dir().join(format!(
            "spine-offline-hammer-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("pool.db");
        const ROWS: i64 = 3000;
        {
            let c = Connection::open(&p).unwrap();
            c.pragma_update(None, "journal_mode", "WAL").unwrap();
            c.execute_batch("CREATE TABLE t(id INTEGER PRIMARY KEY, v INTEGER, pad BLOB); CREATE INDEX tv ON t(v, pad);").unwrap();
            let tx = c.unchecked_transaction().unwrap();
            for i in 0..ROWS {
                tx.execute("INSERT INTO t VALUES (?1, 100, randomblob(150))", [i]).unwrap();
            }
            tx.commit().unwrap();
        }
        assert!(!super::side_file(&p, "-wal").exists(), "the writer's close removed its log");
        let stop = Arc::new(AtomicBool::new(false));
        let writer = {
            let (p, stop) = (p.clone(), stop.clone());
            std::thread::spawn(move || {
                let mut n = 0u64;
                while !stop.load(Ordering::SeqCst) {
                    let c = Connection::open(&p).unwrap();
                    c.busy_timeout(std::time::Duration::from_secs(10)).unwrap();
                    let a = (n * 7919) as i64 % ROWS;
                    c.execute_batch(&format!(
                        "BEGIN IMMEDIATE; UPDATE t SET v = v - 5, pad = randomblob(150) WHERE id % 11 = {}; \
                         UPDATE t SET v = v + 5, pad = randomblob(150) WHERE id IN (SELECT id + 1 FROM t WHERE id % 11 = {} AND id + 1 < {ROWS}); \
                         UPDATE t SET v = v + 5 * (SELECT count(*) FROM t WHERE id % 11 = {} AND id + 1 >= {ROWS}) WHERE id = {a}; COMMIT;",
                        n % 11, n % 11, n % 11
                    ))
                    .unwrap();
                    drop(c);
                    n += 1;
                }
                n
            })
        };
        let t0 = std::time::Instant::now();
        let readers: Vec<_> = (0..4)
            .map(|_| {
                let p = p.clone();
                std::thread::spawn(move || {
                    let (mut reads, mut failures) = (0u64, Vec::new());
                    while t0.elapsed() < std::time::Duration::from_secs(6) {
                        let got = (|| -> Result<i64, String> {
                            let c = super::read_only_connection(&p).map_err(|e| e.to_string())?;
                            let mut st = c.prepare("SELECT v, length(pad) FROM t INDEXED BY tv").map_err(|e| e.to_string())?;
                            let mut rows = st.query([]).map_err(|e| e.to_string())?;
                            let (mut total, mut k) = (0i64, 0u64);
                            while let Some(r) = rows.next().map_err(|e| e.to_string())? {
                                total += r.get::<_, i64>(0).map_err(|e| e.to_string())?;
                                k += 1;
                                if k % 25 == 0 {
                                    std::thread::sleep(std::time::Duration::from_micros(100));
                                }
                            }
                            Ok(total)
                        })();
                        reads += 1;
                        match got {
                            Ok(total) if total == ROWS * 100 => {}
                            Ok(total) => failures.push(format!("torn read: total {total}")),
                            Err(e) => failures.push(e),
                        }
                    }
                    (reads, failures)
                })
            })
            .collect();
        let mut all = Vec::new();
        let mut reads = 0;
        for r in readers {
            let (n, f) = r.join().unwrap();
            reads += n;
            all.extend(f);
        }
        stop.store(true, Ordering::SeqCst);
        let writes = writer.join().unwrap();
        let _ = std::fs::remove_dir_all(&d);
        eprintln!("{reads} reads during {writes} writes");
        assert!(writes > 50 && reads > 50, "{reads} reads, {writes} writes");
        assert!(all.is_empty(), "{} of {reads} reads failed: {:?}", all.len(), &all[..all.len().min(5)]);
    }

    /// No side file and no copy is left behind by a read of a file at rest.
    #[test]
    fn a_read_of_a_file_at_rest_leaves_nothing() {
        let d = std::env::temp_dir().join(format!("spine-offline-rest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("pool.db");
        {
            let c = Connection::open(&p).unwrap();
            c.pragma_update(None, "journal_mode", "WAL").unwrap();
            c.execute_batch("CREATE TABLE t(x); INSERT INTO t VALUES (1);").unwrap();
        }
        let list = || {
            let mut v: Vec<String> = std::fs::read_dir(&d).unwrap().flatten().map(|e| e.file_name().into_string().unwrap()).collect();
            v.sort();
            v
        };
        let before = (list(), super::identity(&p).unwrap());
        use std::os::unix::fs::MetadataExt;
        let owner = std::fs::metadata(&p).unwrap().uid();
        let mine = || {
            super::private_dir(&std::env::temp_dir(), owner).map_or(0, |dir| {
                std::fs::read_dir(dir).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with(&format!("{}-", std::process::id()))).count()
            })
        };
        let tmp_before = mine();
        let c = super::read_only_connection(&p).unwrap();
        assert_eq!(c.query_row("SELECT x FROM t", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
        assert!(c.execute("INSERT INTO t VALUES (2)", []).is_err(), "the connection cannot write");
        drop(c);
        assert_eq!((list(), super::identity(&p).unwrap()), before, "the read wrote beside or to the file");
        let tmp_after = mine();
        assert!(tmp_after <= tmp_before, "a private copy was left behind");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The directory for the private copies is used only when it is a real
    /// directory of the store's owner that nobody else can enter: one that is
    /// a symlink, or open to others, is refused (and the read goes through
    /// SQLite's locking instead).
    #[test]
    fn a_private_directory_someone_could_reach_is_not_used() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let base = std::env::temp_dir().join(format!("spine-offline-dir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let me = std::fs::metadata(&base).unwrap().uid();
        let made = super::private_dir(&base, me).expect("made fresh");
        assert_eq!(std::fs::metadata(&made).unwrap().mode() & 0o777, 0o700);
        assert_eq!(super::private_dir(&base, me), Some(made.clone()), "reused");
        std::fs::set_permissions(&made, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert_eq!(super::private_dir(&base, me), None, "open to others");
        std::fs::remove_dir(&made).unwrap();
        let elsewhere = base.join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        std::fs::set_permissions(&elsewhere, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &made).unwrap();
        assert_eq!(super::private_dir(&base, me), None, "a symlink");
        assert_eq!(super::private_dir(&base, me + 1), None, "another owner's name");
        let _ = std::fs::remove_dir_all(&base);
    }
}
