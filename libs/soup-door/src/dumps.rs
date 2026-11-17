//! **A cauldron's dumps**: raw text put into the cauldron for later
//! processing, kept beside its soup and never inside it.
//!
//! # Where
//!
//! ```text
//!   <cauldron>/dumps/            (the legacy cauldron: data/<env>/soup/dumps/)
//!       spine.db, dumps.db       a spine of its own, one pool `dumps` on grammar `dump@1`
//!       writer.lock              its writer lock (`mcp <pid>` while the service holds it)
//!       blobs/<sha256>.txt       a text dump's bytes, content-addressed
//!       blobs/<sha256>.jsonl     a session transcript's bytes, verbatim
//! ```
//!
//! The store is the owner's only: `dumps/` and `blobs/` are mode 0700, made
//! so (never open to others first) and set so whatever the umask; every file
//! in them is 0600. Each open for writing sets them so again, which also
//! tightens a store made before this was done.
//!
//! The store is made on the first dump, never before: a cauldron nobody has
//! dumped into has no `dumps/` directory, and every read answers it as empty.
//! The soup is not touched by anything here; an older release, which knows
//! only `soup/`, never looks in `dumps/`.
//!
//! # A dump
//!
//! A `dump` node (`spine::grammar_dump`, a copy of grammar v7's) in state
//! `received`. Each dump is ONE spine transaction: its bytes are first
//! written to the blob store as a temporary file (`blobs/.incoming-*`,
//! locked by its writer while it is written), and then, holding the store
//! ([`Store::commit`]), the caller's last check runs (the service reads the
//! grant again), the file is synced and renamed to the name its SHA-256
//! gives it, and the node is created. The transaction's commit releases the
//! write; nothing else happens inside it. A dump refused or failed at any
//! point leaves nothing: the temporary file is removed, and so is a named
//! blob no dump node refers to. The same bytes dumped twice are two dumps
//! and one blob.
//!
//! A process killed mid-dump can leave a temporary file, or a named blob and
//! no node (its transaction rolled back). Both are swept when the store is
//! next opened for writing, under its writer lock: a temporary file whose
//! writer's lock can be taken (its writer is gone), and a named blob no dump
//! node refers to (every blob is named and its node created under the
//! writer lock, so under that lock an unreferenced blob is an orphan). What
//! was swept is in [`Store::recovered`]'s `swept`.
//!
//! - A TEXT dump (at most [`TEXT_LIMIT`] bytes of UTF-8) keeps its text in
//!   the node's `body`, as v7's `dump` does, and its bytes in the blob store;
//!   `artifacts.text` names the blob.
//! - A SESSION dump keeps a Claude Code session's transcript verbatim in the
//!   blob store (at most [`TRANSCRIPT_LIMIT`] bytes); the node's `body` is a
//!   one-paragraph description and `artifacts.transcript` names the blob.
//!
//! `meta` records where it came from: `source` (`{kind: "text", client,
//! session?, from?}` or `{kind: "session", id, cwd, client}`), the caller's
//! `title`, `cites` and `refs`, `received_at`, and a transcript's `lines`.
//!
//! # Later processing
//!
//! A dump in state `received` is work not yet done, and the dumps spine's
//! committed-commit stream is the queue. Nothing in this release processes a
//! dump: it waits as `received`.

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use spine::envelope::{Bindings, Envelope, Initiator, Instrument, Mode, Provenance, ENVELOPE_FORMAT_VERSION};
use spine::grammar_dump::{self, DUMP, DUMP_PREFIX, POOL, RECEIVED};
use spine::pool::{NodeId, NodeRecord, Pool};
use spine::schema_def::PoolName;

use crate::{refuse, Code, Error, Result};

/// The store's directory name, beside the soup.
pub const DIR: &str = "dumps";
/// The blob store under it.
pub const BLOBS: &str = "blobs";
/// The writer lock of the dumps store (`<dumps>/writer.lock`).
pub const WRITER_LOCK: &str = crate::lock::WRITER_LOCK;

/// The most a text dump may hold: 1 MiB of UTF-8.
pub const TEXT_LIMIT: usize = 1024 * 1024;
/// The most a session transcript may hold: 64 MiB.
pub const TRANSCRIPT_LIMIT: u64 = 64 * 1024 * 1024;
/// The longest `title`, in characters.
pub const TITLE_CAP: usize = 200;
/// The longest `source` note of a text dump, in characters.
pub const SOURCE_CAP: usize = 500;
/// The most `refs` one dump names.
pub const REFS_MAX: usize = 100;
/// How many dumps a list answers when it does not say.
pub const LIST_LIMIT: usize = 50;

/// A text dump's media type.
pub const TEXT_MEDIA: &str = "text/plain; charset=utf-8";
/// A session transcript's media type.
pub const TRANSCRIPT_MEDIA: &str = "application/x-claude-code-transcript+jsonl";

/// What every receipt says about what happens next.
pub const NEXT: &str = "processing of dumps comes in a later release; this dump waits in state `received`";

/// The dumps store of `c`: `<cauldron>/dumps/` (the legacy cauldron's is
/// `data/<env>/soup/dumps/`, beside its soup).
pub fn dir(c: &env::Cauldron) -> PathBuf {
    let soup = c.soup();
    soup.parent().map(|p| p.join(DIR)).unwrap_or_else(|| soup.join(DIR))
}

/// Has anything ever been dumped into `c` (does its store exist)?
pub fn exists(c: &env::Cauldron) -> bool {
    dir(c).join(spine::spine_db::SPINE_FILE).is_file()
}

fn pool() -> PoolName {
    PoolName::new(POOL)
}

fn store_err(c: &env::Cauldron, what: impl std::fmt::Display) -> Error {
    Error::new(Code::Store, format!("cauldron `{}`'s dumps: {what}", c.name))
}

/// Is `s` a session id: 1-128 characters of `[A-Za-z0-9._-]`, not starting
/// with `.`? Such an id can never name a path: it holds no separator and is
/// never `.` or `..`.
pub fn session_id_ok(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && !s.starts_with('.')
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Is `s` a dump id (`DM-<n>`)?
pub fn dump_id_ok(s: &str) -> bool {
    s.strip_prefix(DUMP_PREFIX)
        .and_then(|r| r.strip_prefix('-'))
        .is_some_and(|n| !n.is_empty() && n.len() <= 20 && n.bytes().all(|b| b.is_ascii_digit()))
}

fn sha_ok(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

// ------------------------------------------------------------------- lock

/// What `<dumps>/writer.lock` says about its holder.
pub fn lock_says(c: &env::Cauldron) -> Option<String> {
    std::fs::read_to_string(dir(c).join(WRITER_LOCK)).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// The pid the dumps store's lock text names as the `mcp` service, if it
/// does. Read it only once taking the lock was refused.
pub fn held_by_service(c: &env::Cauldron) -> Option<u32> {
    let text = lock_says(c)?;
    let (who, pid) = text.split_once(' ')?;
    (who == crate::lock::MCP_SERVICE).then(|| pid.trim().parse().ok()).flatten()
}

// ------------------------------------------------------------------ modes

/// The mode of the store's directories: the owner's only.
pub const DIR_MODE: u32 = env::private::DIR_MODE;
/// The mode of every file in the store: the owner's only.
pub const FILE_MODE: u32 = env::private::FILE_MODE;

/// Make `p` (whose parent exists) a directory of mode [`DIR_MODE`], or set
/// an existing one to it (`env::private::make_dir`).
fn private_dir(p: &Path) -> std::io::Result<()> {
    env::private::make_dir(p)
}

/// Set every regular file directly in `d` that others could read or write
/// to [`FILE_MODE`] (a store made before its modes were set, or a database
/// file made by SQLite with its own default).
fn tighten_files(d: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let rd = match std::fs::read_dir(d) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for entry in rd.flatten() {
        let Ok(md) = std::fs::symlink_metadata(entry.path()) else { continue };
        if md.is_file() && md.permissions().mode() & 0o777 != FILE_MODE {
            std::fs::set_permissions(entry.path(), std::fs::Permissions::from_mode(FILE_MODE))?;
        }
    }
    Ok(())
}

/// The store's directory and blob store made private (or set private), and
/// every file in them set to [`FILE_MODE`].
fn private_store(c: &env::Cauldron, d: &Path) -> Result<()> {
    for p in [d.to_path_buf(), d.join(BLOBS)] {
        private_dir(&p).map_err(|e| store_err(c, format!("making {} private: {e}", p.display())))?;
    }
    for p in [d.to_path_buf(), d.join(BLOBS)] {
        tighten_files(&p).map_err(|e| store_err(c, format!("making the files in {} private: {e}", p.display())))?;
    }
    Ok(())
}

fn take_lock(c: &env::Cauldron, holder: &str) -> Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    let d = dir(c);
    // The whole cauldron's data is its owner's only (`env::private`); the
    // store is made private before anything is made in it, whatever the umask.
    env::private::tighten_cauldron(c).map_err(|e| store_err(c, format!("making the cauldron's data private to its owner: {e}")))?;
    private_store(c, &d)?;
    let path = d.join(WRITER_LOCK);
    let mut fh = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(FILE_MODE)
        .open(&path)
        .map_err(|e| store_err(c, format!("opening {}: {e}", path.display())))?;
    match fh.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => match held_by_service(c) {
            Some(pid) => refuse!(
                Code::Locked,
                "REFUSED: the `{}` service (pid {pid}) holds cauldron `{}`'s dumps store: while the cauldron is granted \
                 to the MCP door with `dump`, that service is its one writer. Nothing was written. See `cauldron mcp status`.",
                crate::lock::MCP_SERVICE,
                c.name
            ),
            None => refuse!(
                Code::Locked,
                "REFUSED: another writer is writing to cauldron `{}`'s dumps store right now ({} is held). Nothing was \
                 written. Try again when it has finished.",
                c.name,
                path.display()
            ),
        },
        Err(std::fs::TryLockError::Error(e)) => return Err(store_err(c, format!("locking {}: {e}", path.display()))),
    }
    let _ = fh.set_len(0).and_then(|_| writeln!(fh, "{holder}")).and_then(|_| fh.flush());
    Ok(fh)
}

// ------------------------------------------------------------------ store

/// The dumps store of one cauldron, open for writing: its spine and its
/// writer lock, held until this is dropped.
pub struct Store {
    pub cauldron: env::Cauldron,
    dir: PathBuf,
    spine: spine::Cauldron,
    _lock: File,
    /// Whether this open made the store.
    pub created: bool,
    /// What the open recovered: `{interrupted: none|landed|rolled-back|…,
    /// rolled_forward, aborted, swept: {incoming, blobs}}` (transaction
    /// hashes only; `swept` counts what [`Store::open`]'s sweep removed).
    pub recovered: Value,
}

impl Store {
    /// Take the store's writer lock (refused as [`Code::Locked`] when
    /// another writer holds it), writing `holder` into it, and open the
    /// store, making it first when this cauldron has none. The open runs the
    /// spine's recovery: a dump interrupted before its decision is rolled
    /// back, one interrupted after it lands. Then the blob store is swept
    /// of what a killed or refused dump left (module docs).
    pub fn open(c: &env::Cauldron, holder: &str) -> Result<Store> {
        let lock = take_lock(c, holder)?;
        let d = dir(c);
        let created = !d.join(spine::spine_db::SPINE_FILE).is_file();
        let (spine, recovered) = if created {
            let s = spine::Cauldron::create(&d, &[grammar_dump::dumps_v1(POOL)], spine::config::SpineConfig::default())
                .map_err(|e| store_err(c, format!("making the store: {e}")))?;
            (s, json!({"interrupted": "none", "rolled_forward": [], "aborted": []}))
        } else {
            let s = spine::Cauldron::open(&d, spine::config::SpineConfig::default())
                .map_err(|e| store_err(c, format!("opening the store: {e}")))?;
            let rec = s.recovered_at_open();
            let hexes = |v: &[spine::envelope::TxHash]| v.iter().map(|h| h.to_hex()).collect::<Vec<_>>();
            let forward: Vec<String> = hexes(&rec.rolled_forward).into_iter().chain(hexes(&rec.cleared_only)).collect();
            let back: Vec<String> = hexes(&rec.aborted).into_iter().chain(hexes(&rec.aborted_elsewhere)).collect();
            let interrupted = match (forward.is_empty(), back.is_empty()) {
                (true, true) => "none",
                (false, true) => "landed",
                (true, false) => "rolled-back",
                (false, false) => "landed-and-rolled-back",
            };
            let r = json!({"interrupted": interrupted, "rolled_forward": forward, "aborted": back});
            (s, r)
        };
        // The databases SQLite made (with its own default mode) set private.
        private_store(c, &d)?;
        let mut store = Store { cauldron: c.clone(), dir: d, spine, _lock: lock, created, recovered };
        let swept = store.sweep()?;
        store.recovered["swept"] = swept;
        Ok(store)
    }

    /// A writer for one blob of this store.
    pub fn blob(&self, kind: Kind) -> Result<BlobWriter> {
        BlobWriter::new(&self.cauldron, &self.dir.join(BLOBS), kind)
    }

    /// The store's committed-commit counter.
    pub fn counter(&self) -> Result<u64> {
        self.spine.committed_commit_counter().map_err(|e| store_err(&self.cauldron, e))
    }

    /// **Finish and land one dump**, holding this store: `check` first (the
    /// caller's last word, e.g. the grant read again: a refusal there drops
    /// the temporary file), then the blob is synced and named by its hash,
    /// `make` builds the dump from it, and ONE transaction creates its
    /// `DM-<n>` in state `received`. A refusal or an error after the blob
    /// was named removes it unless a dump node refers to it, so a dump that
    /// did not land leaves no byte behind. `err` turns this crate's errors
    /// into the caller's.
    pub fn commit<E>(
        &self,
        w: BlobWriter,
        check: impl FnOnce() -> std::result::Result<(), E>,
        make: impl FnOnce(&Blob) -> std::result::Result<NewDump, E>,
        by: &By,
        err: impl Fn(Error) -> E,
    ) -> std::result::Result<(NewDump, Landed), E> {
        check()?;
        let blob = w.finish().map_err(&err)?;
        let d = match make(&blob) {
            Ok(d) => d,
            Err(e) => {
                self.discard(&blob);
                return Err(e);
            }
        };
        match self.land(&d, by) {
            Ok(l) => Ok((d, l)),
            Err(e) => {
                self.discard(&blob);
                Err(err(e))
            }
        }
    }

    /// The path of a blob of this store.
    pub fn blob_path(&self, b: &Blob) -> PathBuf {
        self.dir.join(&b.file)
    }

    /// Remove `b` when no dump node refers to it (holding the store, so no
    /// other dump is between its blob and its node).
    fn discard(&self, b: &Blob) {
        match referenced(&self.cauldron) {
            Ok(shas) if !shas.contains(&b.sha256) => {
                let _ = std::fs::remove_file(self.blob_path(b));
                if let Ok(d) = File::open(self.dir.join(BLOBS)) {
                    let _ = d.sync_all();
                }
            }
            _ => {}
        }
    }

    /// **Sweep the blob store**, holding the writer lock, after recovery:
    /// a temporary file whose writer's lock can be taken (the writer is
    /// gone), and a named blob no dump node refers to. Answers `{incoming,
    /// blobs}`, the counts removed.
    fn sweep(&self) -> Result<Value> {
        let blobs = self.dir.join(BLOBS);
        let shas = referenced(&self.cauldron)?;
        let (mut incoming, mut orphans) = (0u64, 0u64);
        let rd = match std::fs::read_dir(&blobs) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(json!({"incoming": 0, "blobs": 0})),
            Err(e) => return Err(store_err(&self.cauldron, format!("reading {}: {e}", blobs.display()))),
        };
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let path = entry.path();
            if !entry.file_type().is_ok_and(|t| t.is_file()) {
                continue;
            }
            if name.starts_with(".incoming-") {
                let Ok(f) = File::open(&path) else { continue };
                if f.try_lock().is_ok() && std::fs::remove_file(&path).is_ok() {
                    incoming += 1;
                }
                continue;
            }
            let Some((sha, ext)) = name.split_once('.') else { continue };
            if sha_ok(sha) && (ext == Kind::Text.ext() || ext == Kind::Session.ext()) && !shas.contains(sha) && std::fs::remove_file(&path).is_ok() {
                orphans += 1;
            }
        }
        if incoming + orphans > 0 {
            if let Ok(d) = File::open(&blobs) {
                let _ = d.sync_all();
            }
        }
        Ok(json!({"incoming": incoming, "blobs": orphans}))
    }

    /// **Land one dump**: ONE transaction creating its `DM-<n>` in state
    /// `received`. The blob must already be in the store.
    fn land(&self, d: &NewDump, by: &By) -> Result<Landed> {
        let blob_path = self.dir.join(&d.blob.file);
        if !blob_path.is_file() {
            return Err(store_err(&self.cauldron, format!("its blob {} is not in the store", d.blob.file)));
        }
        let mut b = Bindings::new();
        b.insert("body".into(), json!(d.body()));
        b.insert("env".into(), json!(d.env));
        b.insert("artifacts".into(), d.artifacts());
        b.insert("meta".into(), d.meta());
        let statement = format!(
            "CREATE (d: {POOL}/{DUMP} {{ state: \"{RECEIVED}\", body: $body, env: $env, artifacts: $artifacts, meta: $meta }}) RETURN d.id"
        );
        let env = Envelope {
            statement,
            bindings: b,
            format_version: ENVELOPE_FORMAT_VERSION,
            provenance: Provenance {
                initiator: Initiator::Agent { thread: by.thread.clone(), dispatch: by.dispatch.clone() },
                instrument: Instrument { name: by.instrument.clone(), version: by.version.clone() },
            },
            mode: Mode::Commit,
            // Every dump is its own write: the same bytes sent twice are two
            // dumps, never one deduplicated by the statement's identity.
            // The serial makes it unique in this process whatever the clock.
            idempotency_nonce: Some(format!(
                "dump:{}:{}:{}:{}",
                std::process::id(),
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|x| x.as_nanos()).unwrap_or(0),
                next_serial(),
                d.blob.sha256
            )),
        };
        match self.spine.submit(env) {
            Ok(spine::Outcome::Committed { tx, returned, .. }) => {
                let id = returned
                    .0
                    .first()
                    .and_then(|r| r.get("d.id"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .ok_or_else(|| store_err(&self.cauldron, "the dump landed and its id was not returned"))?;
                Ok(Landed { id, tx: tx.to_hex(), counter: self.counter()? })
            }
            Ok(spine::Outcome::Validated { .. }) => Err(store_err(&self.cauldron, "the dump was validated and not committed")),
            Err(spine::error::SpineError::Refused { witnesses }) => {
                Err(store_err(&self.cauldron, format!("the dumps store refused the dump: {}", json!(witnesses))))
            }
            Err(e) => Err(store_err(&self.cauldron, format!("writing the dump: {e}"))),
        }
    }
}

/// Who writes a dump, as the spine records it.
#[derive(Debug, Clone)]
pub struct By {
    /// The initiator's `thread` (e.g. `mcp:claude-code:<session>`, `cli`).
    pub thread: String,
    /// The initiator's `dispatch` (e.g. `mcp soup_dump`).
    pub dispatch: String,
    /// The instrument's name and version.
    pub instrument: String,
    pub version: String,
}

/// A writer for one blob of `c`'s store (made if absent). Blobs are named
/// by their hash and written to a temporary file first, so writing one needs
/// no lock: two writers of the same bytes end with one blob.
pub fn blob_writer(c: &env::Cauldron, kind: Kind) -> Result<BlobWriter> {
    BlobWriter::new(c, &dir(c).join(BLOBS), kind)
}

/// A number no other call in this process gets: what makes a temporary
/// blob's name, and a dump's write, this process's own whatever the clock.
fn next_serial() -> u64 {
    static SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// What kind of dump.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Text,
    Session,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Text => "text",
            Kind::Session => "session",
        }
    }
    fn ext(self) -> &'static str {
        match self {
            Kind::Text => "txt",
            Kind::Session => "jsonl",
        }
    }
    fn media(self) -> &'static str {
        match self {
            Kind::Text => TEXT_MEDIA,
            Kind::Session => TRANSCRIPT_MEDIA,
        }
    }
    fn artifact(self) -> &'static str {
        match self {
            Kind::Text => "text",
            Kind::Session => "transcript",
        }
    }
    fn limit(self) -> u64 {
        match self {
            Kind::Text => TEXT_LIMIT as u64,
            Kind::Session => TRANSCRIPT_LIMIT,
        }
    }
}

/// A blob in the store.
#[derive(Debug, Clone, PartialEq)]
pub struct Blob {
    pub kind: Kind,
    pub sha256: String,
    pub bytes: u64,
    /// Its path under the store: `blobs/<sha256>.<txt|jsonl>`.
    pub file: String,
    /// Whether the store already held these bytes.
    pub reused: bool,
}

/// **One blob being written**: to a temporary file in the blob store,
/// locked by this writer and hashed as it goes, refused past its kind's
/// limit. [`Store::commit`] syncs it and renames it to the name its hash
/// gives it (or drops it when the store already holds those bytes); dropped
/// unfinished, the temporary file is removed.
pub struct BlobWriter {
    cauldron: String,
    blobs: PathBuf,
    tmp: PathBuf,
    file: Option<File>,
    hash: Sha256,
    bytes: u64,
    kind: Kind,
    done: bool,
}

impl BlobWriter {
    fn new(c: &env::Cauldron, blobs: &Path, kind: Kind) -> Result<BlobWriter> {
        for p in blobs.parent().into_iter().chain([blobs]) {
            private_dir(p).map_err(|e| store_err(c, format!("making {} private: {e}", p.display())))?;
        }
        // The name is this process's and this writer's own: a serial from a
        // process-wide counter, not the clock alone. Writers are made OUTSIDE
        // the store's serialization (the bytes arrive before the store is
        // held), and macOS's clock reads in microseconds, so two concurrent
        // dumps in one process could mint the same name and the second was
        // refused `File exists` (0.1.7).
        let tmp = blobs.join(format!(
            ".incoming-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|x| x.as_nanos()).unwrap_or(0),
            next_serial()
        ));
        use std::os::unix::fs::OpenOptionsExt;
        let file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(FILE_MODE)
            .open(&tmp)
            .map_err(|e| store_err(c, format!("creating {}: {e}", tmp.display())))?;
        // Locked while it is written: a sweep takes the lock to know its
        // writer is gone, and never removes a file still being written.
        if let Err(e) = file.try_lock() {
            let _ = std::fs::remove_file(&tmp);
            return Err(store_err(c, format!("locking {}: {e}", tmp.display())));
        }
        Ok(BlobWriter { cauldron: c.name.clone(), blobs: blobs.to_path_buf(), tmp, file: Some(file), hash: Sha256::new(), bytes: 0, kind, done: false })
    }

    /// Bytes written so far.
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Sync, name by hash, and sync the directory. Called only by
    /// [`Store::commit`], holding the store.
    fn finish(mut self) -> Result<Blob> {
        let err = |what: String| Error::new(Code::Store, format!("cauldron `{}`'s dumps: {what}", self.cauldron));
        let f = self.file.take().expect("finished once");
        f.sync_all().map_err(|e| err(format!("syncing {}: {e}", self.tmp.display())))?;
        // Renamed while still open and locked; let go after.
        let _keep = f;
        let sha = format!("{:x}", self.hash.clone().finalize());
        let name = format!("{sha}.{}", self.kind.ext());
        let path = self.blobs.join(&name);
        let reused = match std::fs::metadata(&path) {
            Ok(m) if m.is_file() && m.len() == self.bytes => true,
            _ => {
                std::fs::rename(&self.tmp, &path).map_err(|e| err(format!("naming {}: {e}", path.display())))?;
                false
            }
        };
        if reused {
            let _ = std::fs::remove_file(&self.tmp);
        }
        if let Ok(d) = File::open(&self.blobs) {
            let _ = d.sync_all();
        }
        self.done = true;
        Ok(Blob { kind: self.kind, sha256: sha, bytes: self.bytes, file: format!("{BLOBS}/{name}"), reused })
    }
}

impl Write for BlobWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.bytes + buf.len() as u64 > self.kind.limit() {
            return Err(std::io::Error::new(std::io::ErrorKind::FileTooLarge, format!("over the {} byte limit", self.kind.limit())));
        }
        let f = self.file.as_mut().ok_or_else(|| std::io::Error::other("the blob is finished"))?;
        f.write_all(buf)?;
        self.hash.update(buf);
        self.bytes += buf.len() as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.as_mut().map_or(Ok(()), |f| f.flush())
    }
}

impl Drop for BlobWriter {
    fn drop(&mut self) {
        if !self.done {
            drop(self.file.take());
            let _ = std::fs::remove_file(&self.tmp);
        }
    }
}

/// What a dump holds besides its blob.
#[derive(Debug, Clone)]
pub enum Content {
    /// A text dump: the text itself (it is also the blob's bytes).
    Text { text: String, from: Option<String> },
    /// A session transcript: what the store read off it.
    Session { session_id: String, cwd: Option<String>, lines: u64 },
}

/// A dump about to land.
#[derive(Debug, Clone)]
pub struct NewDump {
    pub env: String,
    pub content: Content,
    pub blob: Blob,
    pub title: Option<String>,
    pub cite: Option<Value>,
    pub refs: Vec<String>,
    /// The client that sent it (`claude-code`, `cli`, …).
    pub client: String,
    /// The session that sent it, when known.
    pub session: Option<String>,
}

impl NewDump {
    pub fn kind(&self) -> Kind {
        self.blob.kind
    }

    fn body(&self) -> String {
        match &self.content {
            Content::Text { text, .. } => text.clone(),
            Content::Session { session_id, cwd, lines } => format!(
                "The transcript of Claude Code session {session_id}, stored verbatim: {lines} lines, {} bytes{}. Its bytes are \
                 the blob {} (sha256 {}).",
                self.blob.bytes,
                cwd.as_ref().map(|c| format!(", recorded in {c}")).unwrap_or_default(),
                self.blob.file,
                self.blob.sha256
            ),
        }
    }

    fn artifacts(&self) -> Value {
        let b = &self.blob;
        let mut m = Map::new();
        m.insert(
            self.kind().artifact().into(),
            json!({"sha256": b.sha256, "bytes": b.bytes, "media": self.kind().media(), "file": b.file}),
        );
        Value::Object(m)
    }

    fn meta(&self) -> Value {
        let mut m = Map::new();
        let source = match &self.content {
            Content::Text { from, .. } => {
                let mut s = json!({"kind": "text", "client": self.client});
                if let Some(x) = &self.session {
                    s["session"] = json!(x);
                }
                if let Some(f) = from {
                    s["from"] = json!(f);
                }
                s
            }
            Content::Session { session_id, cwd, lines } => {
                m.insert("lines".into(), json!(lines));
                json!({"kind": "session", "id": session_id, "cwd": cwd, "client": self.client})
            }
        };
        m.insert("source".into(), source);
        if let Some(t) = &self.title {
            m.insert("title".into(), json!(t));
        }
        if let Some(c) = &self.cite {
            m.insert("cites".into(), json!([c]));
        }
        if !self.refs.is_empty() {
            m.insert("refs".into(), json!(self.refs));
        }
        m.insert("received_at".into(), json!(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)));
        Value::Object(m)
    }
}

/// A dump that landed.
#[derive(Debug, Clone)]
pub struct Landed {
    pub id: String,
    pub tx: String,
    /// The dumps store's counter after it.
    pub counter: u64,
}

/// The receipt: ids and sizes, never the text.
pub fn receipt(cauldron: &str, env: &str, d: &NewDump, l: &Landed) -> Value {
    let mut r = json!({
        "env": env,
        "cauldron": cauldron,
        "dump_id": l.id,
        "state": RECEIVED,
        "kind": d.kind().as_str(),
        "bytes": d.blob.bytes,
        "sha256": d.blob.sha256,
        "blob_reused": d.blob.reused,
        "tx": l.tx,
        "next": NEXT,
    });
    if let Some(t) = &d.title {
        r["title"] = json!(t);
    }
    if let Content::Session { session_id, cwd, lines } = &d.content {
        r["session_id"] = json!(session_id);
        r["lines"] = json!(lines);
        r["cwd"] = json!(cwd);
    }
    r
}

/// Check a text dump's text: non-empty, at most [`TEXT_LIMIT`] bytes
/// ([`Code::TooLarge`] otherwise).
pub fn check_text(text: &str) -> Result<()> {
    if text.trim().is_empty() {
        refuse!(Code::Usage, "REFUSED: a dump needs some text; this one is empty. Nothing was written.");
    }
    if text.len() > TEXT_LIMIT {
        refuse!(
            Code::TooLarge,
            "REFUSED: this text is {} bytes, over the {TEXT_LIMIT} bytes (1 MiB) one dump holds. Nothing was written. \
             Split it into several dumps.",
            text.len()
        );
    }
    Ok(())
}

/// Check the optional fields a caller gives: `title` and `from` lengths,
/// `refs` count and shape, `cite`'s shape.
pub fn check_extras(title: Option<&str>, from: Option<&str>, cite: Option<&Value>, refs: &[String]) -> Result<()> {
    if let Some(t) = title {
        if t.trim().is_empty() || t.chars().count() > TITLE_CAP {
            refuse!(Code::Usage, "REFUSED: a dump's title is 1 to {TITLE_CAP} characters. Nothing was written.");
        }
    }
    if let Some(s) = from {
        if s.trim().is_empty() || s.chars().count() > SOURCE_CAP {
            refuse!(Code::Usage, "REFUSED: a dump's source is 1 to {SOURCE_CAP} characters. Nothing was written.");
        }
    }
    if let Some(c) = cite {
        if !c.get("kind").and_then(Value::as_str).is_some_and(|k| !k.is_empty()) {
            refuse!(Code::Usage, "REFUSED: `cite` must be an object with a `kind` (meta.cites form). Nothing was written.");
        }
    }
    if refs.len() > REFS_MAX || refs.iter().any(|r| r.trim().is_empty() || r.len() > 64) {
        refuse!(Code::Usage, "REFUSED: `refs` is at most {REFS_MAX} node ids. Nothing was written.");
    }
    Ok(())
}

/// What a transcript says about itself: its line count, and the first
/// `cwd` a line records (read from at most the first 20,000 lines).
pub fn transcript_facts(path: &Path) -> std::io::Result<(u64, Option<String>)> {
    let mut r = BufReader::new(File::open(path)?);
    let mut lines = 0u64;
    let mut cwd = None;
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let n = r.read_until(b'\n', &mut buf)?;
        if n == 0 {
            break;
        }
        lines += 1;
        if cwd.is_none() && lines <= 20_000 && buf.windows(5).any(|w| w == b"\"cwd\"") {
            if let Ok(v) = serde_json::from_slice::<Value>(&buf) {
                cwd = v.get("cwd").and_then(Value::as_str).filter(|c| !c.is_empty()).map(str::to_string);
            }
        }
    }
    Ok((lines, cwd))
}

// ------------------------------------------------------------------ reads

/// The dumps store's committed-commit counter, read through a connection
/// that cannot write; 0 when the cauldron has no store.
pub fn counter(c: &env::Cauldron) -> Result<u64> {
    if !exists(c) {
        return Ok(0);
    }
    let conn = spine::offline::read_only_connection(&dir(c).join(spine::spine_db::SPINE_FILE)).map_err(|e| store_err(c, e))?;
    conn.query_row("SELECT committed_commits FROM counters WHERE singleton = 1", [], |r| r.get::<_, i64>(0))
        .map(|n| n.max(0) as u64)
        .map_err(|e| store_err(c, e))
}

fn read_pool(c: &env::Cauldron) -> Result<Option<Pool>> {
    if !exists(c) {
        return Ok(None);
    }
    Pool::open_read_only(&Pool::path_in(&dir(c), &pool())).map(Some).map_err(|e| store_err(c, e))
}

fn all(c: &env::Cauldron) -> Result<Vec<NodeRecord>> {
    let Some(p) = read_pool(c)? else { return Ok(vec![]) };
    let snap = p.snapshot().map_err(|e| store_err(c, e))?;
    let mut v = snap.nodes_of(DUMP, None).map_err(|e| store_err(c, e))?;
    v.sort_by_key(|n| std::cmp::Reverse(serial(&n.id.0)));
    Ok(v)
}

/// The hashes of every blob a dump node refers to.
fn referenced(c: &env::Cauldron) -> Result<std::collections::HashSet<String>> {
    Ok(all(c)?
        .iter()
        .filter_map(|n| artifact(n).1.get("sha256").and_then(Value::as_str).map(str::to_string))
        .collect())
}

fn serial(id: &str) -> u64 {
    id.rsplit('-').next().and_then(|n| n.parse().ok()).unwrap_or(0)
}

fn field<'a>(n: &'a NodeRecord, k: &str) -> Option<&'a Value> {
    n.fields.get(k).map(|f| &f.value)
}

/// The blob a dump names: its kind, `{sha256, bytes, media, file}`.
fn artifact(n: &NodeRecord) -> (Kind, Value) {
    let a = field(n, "artifacts").cloned().unwrap_or(Value::Null);
    if let Some(t) = a.get("transcript") {
        return (Kind::Session, t.clone());
    }
    (Kind::Text, a.get("text").cloned().unwrap_or(Value::Null))
}

/// One dump's summary: never its text.
fn summary_of(n: &NodeRecord) -> Value {
    let (kind, a) = artifact(n);
    let meta = field(n, "meta").cloned().unwrap_or(Value::Null);
    let mut s = json!({
        "id": n.id.0,
        "state": n.state,
        "kind": kind.as_str(),
        "bytes": a["bytes"],
        "sha256": a["sha256"],
        "received_at": meta["received_at"],
    });
    for k in ["title", "lines", "refs", "cites"] {
        if let Some(v) = meta.get(k) {
            s[k] = v.clone();
        }
    }
    if let Some(src) = meta.get("source") {
        s["source"] = src.clone();
    }
    s
}

/// `{store, total, by_state}` for `soup_status`.
pub fn counts(c: &env::Cauldron) -> Result<Value> {
    let v = all(c)?;
    let mut by = Map::new();
    for n in &v {
        let s = n.state.clone().unwrap_or_default();
        let k = by.entry(s).or_insert(json!(0));
        *k = json!(k.as_u64().unwrap_or(0) + 1);
    }
    Ok(json!({"store": if exists(c) { "present" } else { "absent" }, "total": v.len(), "by_state": by}))
}

/// What a dump list is filtered by.
#[derive(Debug, Clone, Default)]
pub struct ListOpts {
    pub state: Option<String>,
    pub kind: Option<String>,
    pub limit: Option<usize>,
}

/// The dumps, newest first, as summaries (never their text): `{env,
/// cauldron, total, dumps, more?}`.
pub fn list(c: &env::Cauldron, o: &ListOpts) -> Result<Value> {
    if let Some(k) = &o.kind {
        if k != "text" && k != "session" {
            refuse!(Code::Usage, "REFUSED: a dump's kind is `text` or `session`, not `{k}`");
        }
    }
    if let Some(s) = &o.state {
        if !grammar_dump::DUMP_STATES.contains(&s.as_str()) {
            refuse!(Code::Usage, "REFUSED: a dump's state is one of {}, not `{s}`", grammar_dump::DUMP_STATES.join(", "));
        }
    }
    let v: Vec<Value> = all(c)?
        .iter()
        .filter(|n| o.state.as_deref().is_none_or(|s| n.state.as_deref() == Some(s)))
        .map(summary_of)
        .filter(|s| o.kind.as_deref().is_none_or(|k| s["kind"] == k))
        .collect();
    let total = v.len();
    let limit = o.limit.unwrap_or(LIST_LIMIT);
    let more = total.saturating_sub(limit);
    let shown: Vec<Value> = v.into_iter().take(limit).collect();
    let mut doc = json!({"env": c.env, "cauldron": c.name, "total": total, "dumps": shown});
    if more > 0 {
        doc["more"] = json!(more);
    }
    Ok(doc)
}

fn text_window(bytes: &[u8]) -> (String, usize) {
    // The longest valid prefix; a lone bad byte is shown as U+FFFD and
    // counted as one byte, so reading on never stalls.
    match std::str::from_utf8(bytes) {
        Ok(s) => (s.to_string(), bytes.len()),
        Err(e) if e.valid_up_to() > 0 => (std::str::from_utf8(&bytes[..e.valid_up_to()]).unwrap_or("").to_string(), e.valid_up_to()),
        Err(e) if e.error_len().is_some() => ("\u{FFFD}".to_string(), 1),
        Err(_) => (String::new(), 0),
    }
}

/// **One dump, its content from byte `offset`**, the whole answer's compact
/// JSON at most `max_chars` characters (see `crate::budget`): `{env,
/// cauldron, dump: <summary>, offset, text, text_bytes, total_bytes, more,
/// next_offset?}`. `text` is the content from `offset`, cut at a character
/// boundary; read on with `offset: next_offset`. A text dump's content is
/// its text; a session dump's is its transcript, verbatim.
pub fn get(c: &env::Cauldron, id: &str, offset: u64, max_chars: usize) -> Result<Value> {
    let n = find(c, id)?;
    let (kind, a) = artifact(&n);
    let content: Vec<u8> = match kind {
        Kind::Text => field(&n, "body").and_then(Value::as_str).unwrap_or("").as_bytes().to_vec(),
        Kind::Session => {
            let sha = a["sha256"].as_str().unwrap_or("");
            if !sha_ok(sha) {
                return Err(store_err(c, format!("dump `{id}` names no usable blob")));
            }
            // The path is built from the node's own hash, never from a caller.
            let path = dir(c).join(BLOBS).join(format!("{sha}.{}", kind.ext()));
            let mut f = File::open(&path).map_err(|e| store_err(c, format!("reading the transcript of `{id}`: {e}")))?;
            let total = f.metadata().map(|m| m.len()).unwrap_or(0);
            let start = offset.min(total);
            f.seek(SeekFrom::Start(start)).map_err(|e| store_err(c, e))?;
            let want = (max_chars as u64 * 4 + 4).min(total - start);
            let mut buf = vec![0u8; want as usize];
            f.read_exact(&mut buf).map_err(|e| store_err(c, e))?;
            return window(c, &n, start, total, &buf, max_chars);
        }
    };
    let total = content.len() as u64;
    let start = offset.min(total) as usize;
    let end = (start + max_chars * 4 + 4).min(content.len());
    window(c, &n, start as u64, total, &content[start..end], max_chars)
}

fn window(c: &env::Cauldron, n: &NodeRecord, mut start: u64, total: u64, bytes: &[u8], max_chars: usize) -> Result<Value> {
    // An offset inside a character moves on to the next one.
    let skip = bytes.iter().take(3).take_while(|b| (**b & 0xC0) == 0x80).count();
    start += skip as u64;
    let (text, valid) = text_window(&bytes[skip..]);
    let build = |chars: &str, used: usize| -> Value {
        let end = start + used as u64;
        let mut d = json!({
            "env": c.env, "cauldron": c.name, "dump": summary_of(n),
            "offset": start, "text": chars, "text_bytes": used, "total_bytes": total, "more": end < total,
        });
        if end < total {
            d["next_offset"] = json!(end);
        }
        d
    };
    // The most characters that fit, found by halving.
    let idx: Vec<usize> = text.char_indices().map(|(i, _)| i).chain(std::iter::once(text.len())).collect();
    let fits = |k: usize| crate::budget::size(&build(&text[..idx[k]], if idx[k] == text.len() { valid } else { idx[k] })) <= max_chars;
    let (mut lo, mut hi) = (0usize, idx.len() - 1);
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if fits(mid) {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    // A page that cannot carry its fixed part, or not one character of what
    // is left, is refused: never a page over `max_chars`, never one that
    // does not move on.
    if !fits(lo) || (lo == 0 && !text.is_empty()) {
        let id = n.id.to_string();
        refuse!(
            Code::Usage,
            "REFUSED: max_chars {max_chars} leaves no room for any of `{id}`'s content after the page's fixed part \
             ({} characters); ask for more",
            crate::budget::size(&build("", 0))
        );
    }
    let used = if idx[lo] == text.len() { valid } else { idx[lo] };
    Ok(build(&text[..idx[lo]], used))
}

/// The dump `id` of `c`: `usage` when `id` is not a dump id, `not_found`
/// when the cauldron holds no such dump.
fn find(c: &env::Cauldron, id: &str) -> Result<NodeRecord> {
    if !dump_id_ok(id) {
        refuse!(Code::Usage, "REFUSED: `{id}` is not a dump id (DM-<n>)");
    }
    let Some(p) = read_pool(c)? else {
        refuse!(Code::NotFound, "REFUSED: cauldron `{}` holds no dump `{id}` (nothing has been dumped into it)", c.name);
    };
    let snap = p.snapshot().map_err(|e| store_err(c, e))?;
    let Some(n) = snap.node(&NodeId(id.to_string())).map_err(|e| store_err(c, e))? else {
        refuse!(Code::NotFound, "REFUSED: cauldron `{}` holds no dump `{id}`", c.name);
    };
    Ok(n)
}

// ------------------------------------------------------------------ the text view

/// What the text view of a transcript leaves out, counted. The text view
/// keeps only what the person typed and what the assistant wrote as text;
/// everything else is left out and counted here, by kind:
///
/// - `tool_calls`, `tool_results`, `thinking`: content blocks of those kinds;
/// - `shell`: a `!` command the person ran in the session and its output
///   (user text holding `<bash-input>`, `<bash-stdout>`, `<bash-stderr>`, …);
/// - `local_command`: a local command's output (`<local-command-stdout>`,
///   `<local-command-stderr>`, `<local-command-caveat>`, …);
/// - `commands`: a slash command's wrapper (`<command-name>`,
///   `<command-message>`, `<command-args>`, …);
/// - `system_reminders`: each `<system-reminder>` span in user text (the
///   rest of that text is kept);
/// - `generated`: other user content Claude Code writes itself: a
///   compaction summary (`isCompactSummary`), a line shown only in the
///   transcript, or a text that is wholly one `<tag-name>…</tag-name>`
///   wrapper;
/// - `meta`: lines Claude Code marks `isMeta`;
/// - `attachments`: attachment lines;
/// - `other_blocks`: other content blocks (images, documents);
/// - `other_lines`: lines that are not a user or assistant message
///   (summaries, snapshots, system lines);
/// - `unreadable_lines`: lines that are not JSON.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Elided {
    pub tool_calls: u64,
    pub tool_results: u64,
    pub thinking: u64,
    pub shell: u64,
    pub local_command: u64,
    pub commands: u64,
    pub system_reminders: u64,
    pub generated: u64,
    pub meta: u64,
    pub attachments: u64,
    pub other_blocks: u64,
    pub other_lines: u64,
    pub unreadable_lines: u64,
}

impl Elided {
    fn add(&mut self, o: &Elided) {
        self.tool_calls += o.tool_calls;
        self.tool_results += o.tool_results;
        self.thinking += o.thinking;
        self.shell += o.shell;
        self.local_command += o.local_command;
        self.commands += o.commands;
        self.system_reminders += o.system_reminders;
        self.generated += o.generated;
        self.meta += o.meta;
        self.attachments += o.attachments;
        self.other_blocks += o.other_blocks;
        self.other_lines += o.other_lines;
        self.unreadable_lines += o.unreadable_lines;
    }

    /// As the text view answers it.
    pub fn value(&self) -> Value {
        json!({
            "tool_calls": self.tool_calls, "tool_results": self.tool_results, "thinking": self.thinking,
            "shell": self.shell, "local_command": self.local_command, "commands": self.commands,
            "system_reminders": self.system_reminders, "generated": self.generated, "meta": self.meta,
            "attachments": self.attachments, "other_blocks": self.other_blocks, "other_lines": self.other_lines,
            "unreadable_lines": self.unreadable_lines,
        })
    }
}

/// One user or assistant text turn of a transcript.
struct Turn {
    role: &'static str,
    text: String,
    timestamp: Option<String>,
    sidechain: bool,
}

/// The `<system-reminder>` spans of `t` taken out (each counted; an
/// unclosed one takes the rest of the text with it).
fn without_reminders(t: &str, el: &mut Elided) -> String {
    const OPEN: &str = "<system-reminder>";
    const CLOSE: &str = "</system-reminder>";
    let mut out = String::new();
    let mut rest = t;
    while let Some(i) = rest.find(OPEN) {
        out.push_str(&rest[..i]);
        el.system_reminders += 1;
        match rest[i..].find(CLOSE) {
            Some(j) => rest = &rest[i + j + CLOSE.len()..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Is `t` wholly one `<tag-name>…</tag-name>` wrapper (a hyphenated tag, as
/// Claude Code's own are)?
fn wholly_wrapped(t: &str) -> bool {
    let t = t.trim();
    let Some(rest) = t.strip_prefix('<') else { return false };
    let Some(end) = rest.find('>') else { return false };
    let tag = &rest[..end];
    let named = !tag.is_empty()
        && tag.contains('-')
        && tag.starts_with(|c: char| c.is_ascii_lowercase())
        && tag.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    named && t.ends_with(&format!("</{tag}>"))
}

/// What the person typed, out of one piece of user text, or nothing when it
/// is something Claude Code recorded on their behalf (counted in `el`).
fn typed(t: &str, el: &mut Elided) -> Option<String> {
    if t.contains("<bash-") {
        el.shell += 1;
        return None;
    }
    if t.contains("<local-command-") {
        el.local_command += 1;
        return None;
    }
    if t.contains("<command-") {
        el.commands += 1;
        return None;
    }
    let before = el.system_reminders;
    let mut kept = without_reminders(t, el);
    if el.system_reminders != before {
        // What was around a reminder, without the blank lines it leaves.
        kept = kept.trim_matches(['\n', '\r']).to_string();
    }
    if wholly_wrapped(&kept) {
        el.generated += 1;
        return None;
    }
    Some(kept)
}

/// What one transcript line is in the text view: a text turn (its text
/// blocks joined by a blank line), or nothing. Whatever it leaves out is
/// counted in `el`.
fn turn_of(line: &[u8], el: &mut Elided) -> Option<Turn> {
    let s = line.trim_ascii();
    if s.is_empty() {
        return None;
    }
    let Ok(v) = serde_json::from_slice::<Value>(s) else {
        el.unreadable_lines += 1;
        return None;
    };
    let role = match v.get("type").and_then(Value::as_str) {
        Some("user") => "user",
        Some("assistant") => "assistant",
        Some("attachment") => {
            el.attachments += 1;
            return None;
        }
        _ => {
            el.other_lines += 1;
            return None;
        }
    };
    let flag = |k: &str| v.get(k) == Some(&Value::Bool(true));
    if flag("isMeta") {
        el.meta += 1;
        return None;
    }
    if role == "user" && (flag("isCompactSummary") || flag("isVisibleInTranscriptOnly")) {
        el.generated += 1;
        return None;
    }
    let mut parts: Vec<String> = vec![];
    let mut keep = |t: &str, el: &mut Elided| {
        if role == "user" {
            if let Some(k) = typed(t, el) {
                parts.push(k);
            }
        } else {
            parts.push(t.to_string());
        }
    };
    match v.get("message").and_then(|m| m.get("content")) {
        Some(Value::String(t)) => keep(t, el),
        Some(Value::Array(blocks)) => {
            for b in blocks {
                let ty = b.get("type").and_then(Value::as_str).unwrap_or("");
                if ty == "text" {
                    if let Some(t) = b.get("text").and_then(Value::as_str) {
                        keep(t, el);
                    }
                } else if ty.ends_with("tool_use") {
                    el.tool_calls += 1;
                } else if ty.ends_with("tool_result") {
                    el.tool_results += 1;
                } else if ty.contains("thinking") {
                    el.thinking += 1;
                } else {
                    el.other_blocks += 1;
                }
            }
        }
        _ => {
            el.other_lines += 1;
            return None;
        }
    }
    let text = parts.iter().map(String::as_str).filter(|t| !t.trim().is_empty()).collect::<Vec<_>>().join("\n\n");
    if text.is_empty() {
        return None;
    }
    Some(Turn {
        role,
        text,
        timestamp: v.get("timestamp").and_then(Value::as_str).map(str::to_string),
        sidechain: v.get("isSidechain") == Some(&Value::Bool(true)),
    })
}

/// **The smallest `max_chars` a page of one dump's content may be asked
/// for** (`soup_dump_get` and `cauldron soup dump-get`, both views). A page
/// carries a fixed part (the cauldron, the dump's summary, the cursor and,
/// in the text view, the counts of what it left out) of up to several
/// hundred characters before any of the content, so the floor every other
/// read has (256) cannot hold one; a smaller `max_chars` is refused as
/// `usage`, never answered with a page over it. A page whose fixed part
/// alone does not fit even so (a very long title) is refused the same way,
/// naming what it needs.
pub const PAGE_MIN_CHARS: usize = 1000;

/// Refuse a `max_chars` under [`PAGE_MIN_CHARS`] (`asked`: what the caller
/// gave; none is the default, which is over it).
pub fn check_page_max(asked: Option<usize>) -> Result<()> {
    match asked {
        Some(n) if n < PAGE_MIN_CHARS => refuse!(
            Code::Usage,
            "REFUSED: max_chars {n} is under {PAGE_MIN_CHARS}, the least a page of a dump's content takes (its fixed part \
             alone is several hundred characters); ask for {PAGE_MIN_CHARS} or more"
        ),
        _ => Ok(()),
    }
}

/// Room kept in a text-view page for what is only known at its end (the
/// next cursor, `more`, and the counts growing).
const TEXT_VIEW_SLACK: usize = 128;

/// **A session transcript's text view**: only its user and assistant text
/// turns, from byte `offset` (where a line starts: 0, or a `next_offset`
/// this view answered), the whole answer's compact JSON at most
/// `max_chars` characters: `{env, cauldron, dump: {id, state, kind, title?,
/// lines?}, view: "text",
/// offset, turn_offset?, turns: [{at, role, text, timestamp?, sidechain?,
/// from_char?, chars?}], elided: {...}, total_bytes, more, next_offset?,
/// next_turn_offset?}`.
///
/// `at` is the byte offset of the turn's line in the transcript, which is
/// stored verbatim and never changes, so a page is the same every time it
/// is read with the same cursor. Tool calls, tool results, thinking and the
/// other lines are left out and counted in `elided` (a line's counts are on
/// the page that shows its turn, or its first piece). A page ends before a
/// turn that does not fit. A turn longer than a whole page is cut: the page
/// carries its first characters with `chars` (the turn's whole length), and
/// `next_turn_offset` says where the next page goes on in it.
pub fn get_text(c: &env::Cauldron, id: &str, offset: u64, turn_offset: u64, max_chars: usize) -> Result<Value> {
    let n = find(c, id)?;
    let (kind, a) = artifact(&n);
    if kind != Kind::Session {
        refuse!(
            Code::Usage,
            "REFUSED: `{id}` is a text dump; the text view is for a session transcript. A text dump's content is its text: read it without `view`"
        );
    }
    let sha = a["sha256"].as_str().unwrap_or("");
    if !sha_ok(sha) {
        return Err(store_err(c, format!("dump `{id}` names no usable blob")));
    }
    let path = dir(c).join(BLOBS).join(format!("{sha}.{}", kind.ext()));
    let mut f = File::open(&path).map_err(|e| store_err(c, format!("reading the transcript of `{id}`: {e}")))?;
    let total = f.metadata().map(|m| m.len()).map_err(|e| store_err(c, e))?;
    if offset > total {
        refuse!(Code::Usage, "REFUSED: offset {offset} is past the end of `{id}`'s transcript ({total} bytes)");
    }
    if offset > 0 && offset < total {
        let mut b = [0u8; 1];
        f.seek(SeekFrom::Start(offset - 1)).map_err(|e| store_err(c, e))?;
        f.read_exact(&mut b).map_err(|e| store_err(c, e))?;
        if b[0] != b'\n' {
            refuse!(
                Code::Usage,
                "REFUSED: offset {offset} is not where a line of `{id}`'s transcript starts: the text view reads from 0 or \
                 from a `next_offset` it answered"
            );
        }
    }
    f.seek(SeekFrom::Start(offset)).map_err(|e| store_err(c, e))?;
    let mut r = BufReader::new(f);
    // A page names its dump briefly; `soup_dumps` has the whole summary.
    let full = summary_of(&n);
    let mut summary = json!({"id": full["id"], "state": full["state"], "kind": full["kind"]});
    for k in ["title", "lines"] {
        if let Some(v) = full.get(k) {
            summary[k] = v.clone();
        }
    }
    let base = |turns: &[Value], el: &Elided| -> Value {
        let mut d = json!({
            "env": c.env, "cauldron": c.name, "dump": summary, "view": "text", "offset": offset,
            "turns": turns, "elided": el.value(), "total_bytes": total, "more": false,
        });
        if turn_offset > 0 {
            d["turn_offset"] = json!(turn_offset);
        }
        d
    };
    let mut turns: Vec<Value> = vec![];
    let mut el = Elided::default();
    let mut size = crate::budget::size(&base(&[], &el)) + TEXT_VIEW_SLACK;
    // The fixed part must fit, with room for the counts to grow: otherwise
    // no page could keep to `max_chars`.
    if size >= max_chars {
        refuse!(
            Code::Usage,
            "REFUSED: max_chars {max_chars} leaves no room for any of `{id}`'s text after the page's fixed part \
             ({size} characters with room for its counts); ask for more"
        );
    }
    let mut pos = offset;
    let mut next: Option<(u64, u64)> = None;
    let mut buf = Vec::new();
    let mut first = true;
    loop {
        buf.clear();
        let got = r.read_until(b'\n', &mut buf).map_err(|e| store_err(c, e))? as u64;
        if got == 0 {
            break;
        }
        let at = pos;
        let mut line_el = Elided::default();
        let t = turn_of(&buf, &mut line_el);
        let skip = if first { turn_offset } else { 0 };
        first = false;
        let Some(t) = t else {
            if skip > 0 {
                refuse!(Code::Usage, "REFUSED: turn_offset {skip} was given, but the line at offset {at} of `{id}` is not a text turn");
            }
            el.add(&line_el);
            pos += got;
            continue;
        };
        let all_chars = t.text.chars().count() as u64;
        if skip >= all_chars && skip > 0 {
            refuse!(Code::Usage, "REFUSED: turn_offset {skip} is past the end of the turn at offset {at} ({all_chars} characters)");
        }
        let rest = match t.text.char_indices().nth(skip as usize) {
            Some((i, _)) => &t.text[i..],
            None => "",
        };
        let mk = |piece: &str, cut: bool| -> Value {
            let mut v = json!({"at": at, "role": t.role, "text": piece});
            if let Some(ts) = &t.timestamp {
                v["timestamp"] = json!(ts);
            }
            if t.sidechain {
                v["sidechain"] = json!(true);
            }
            if skip > 0 {
                v["from_char"] = json!(skip);
            }
            if cut || skip > 0 {
                v["chars"] = json!(all_chars);
            }
            v
        };
        let comma = usize::from(!turns.is_empty());
        let whole = mk(rest, false);
        let add = crate::budget::size(&whole) + comma;
        if size + add <= max_chars {
            size += add;
            turns.push(whole);
            if skip == 0 {
                el.add(&line_el);
            }
            pos += got;
            continue;
        }
        if turns.is_empty() {
            // The turn alone is longer than the page: cut it, the most
            // characters that fit, found by halving.
            let idx: Vec<usize> = rest.char_indices().map(|(i, _)| i).collect();
            let fits = |k: usize| {
                let end = idx.get(k).copied().unwrap_or(rest.len());
                size + crate::budget::size(&mk(&rest[..end], true)) <= max_chars
            };
            let (mut lo, mut hi) = (0usize, idx.len().saturating_sub(1));
            while lo < hi {
                let mid = (lo + hi).div_ceil(2);
                if fits(mid) {
                    lo = mid;
                } else {
                    hi = mid - 1;
                }
            }
            if lo > 0 && fits(lo) {
                let end = idx.get(lo).copied().unwrap_or(rest.len());
                turns.push(mk(&rest[..end], true));
                if skip == 0 {
                    el.add(&line_el);
                }
                next = Some((at, skip + lo as u64));
                break;
            }
            if pos == offset {
                refuse!(
                    Code::Usage,
                    "REFUSED: max_chars {max_chars} leaves no room for any of the turn at offset {at} of `{id}`; ask for more"
                );
            }
        }
        next = Some((at, 0));
        break;
    }
    let mut doc = base(&turns, &el);
    if let Some((o, k)) = next {
        doc["more"] = json!(true);
        doc["next_offset"] = json!(o);
        if k > 0 {
            doc["next_turn_offset"] = json!(k);
        }
    }
    debug_assert!(crate::budget::size(&doc) <= max_chars, "{} > {max_chars}", crate::budget::size(&doc));
    Ok(doc)
}

/// One dump's whole content, verbatim, to read through: a text dump's
/// text, or a session dump's transcript as it was stored.
pub fn open_content(c: &env::Cauldron, id: &str) -> Result<Box<dyn Read>> {
    let n = find(c, id)?;
    let (kind, a) = artifact(&n);
    match kind {
        Kind::Text => Ok(Box::new(std::io::Cursor::new(field(&n, "body").and_then(Value::as_str).unwrap_or("").as_bytes().to_vec()))),
        Kind::Session => {
            let sha = a["sha256"].as_str().unwrap_or("");
            if !sha_ok(sha) {
                return Err(store_err(c, format!("dump `{id}` names no usable blob")));
            }
            let path = dir(c).join(BLOBS).join(format!("{sha}.{}", kind.ext()));
            Ok(Box::new(File::open(&path).map_err(|e| store_err(c, format!("reading the transcript of `{id}`: {e}")))?))
        }
    }
}

/// The dumps that landed after counter `from` up to `head`, oldest first,
/// at most `limit` (+1 to say there were more): `[{seq, at, op, id, kind,
/// bytes, state}]`. `op` is `dump` for a dump received, `update` for a
/// later change to one.
pub fn landed_after(c: &env::Cauldron, from: u64, head: u64, limit: usize) -> Result<Vec<Value>> {
    if !exists(c) || head <= from {
        return Ok(vec![]);
    }
    let d = dir(c);
    let conn = spine::offline::read_only_connection(&d.join(spine::spine_db::SPINE_FILE)).map_err(|e| store_err(c, e))?;
    let mut stmt = conn
        .prepare(
            "SELECT committed_at, hash, ts FROM transactions
             WHERE committed_at IS NOT NULL AND committed_at > ?1 AND committed_at <= ?2
             ORDER BY committed_at LIMIT ?3",
        )
        .map_err(|e| store_err(c, e))?;
    let rows: Vec<(i64, String, String)> = stmt
        .query_map([from as i64, head as i64, limit as i64 + 1], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .map_err(|e| store_err(c, e))?
        .collect::<std::result::Result<_, _>>()
        .map_err(|e| store_err(c, e))?;
    let Some(p) = read_pool(c)? else { return Ok(vec![]) };
    let pool_conn = spine::offline::read_only_connection(&Pool::path_in(&d, &pool())).map_err(|e| store_err(c, e))?;
    let snap = p.snapshot().map_err(|e| store_err(c, e))?;
    let mut out = Vec::new();
    for (seq, hash, at) in rows {
        let ids = |sql: &str| -> Result<Vec<String>> {
            let mut s = pool_conn.prepare(sql).map_err(|e| store_err(c, e))?;
            let v = s
                .query_map([&hash], |r| r.get::<_, String>(0))
                .map_err(|e| store_err(c, e))?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| store_err(c, e))?;
            Ok(v)
        };
        let created = ids("SELECT id FROM nodes WHERE created_tx = ?1 ORDER BY rowid")?;
        let (op, list) = if created.is_empty() {
            ("update", ids("SELECT id FROM nodes WHERE updated_tx = ?1 ORDER BY rowid")?)
        } else {
            ("dump", created)
        };
        for id in list {
            let n = snap.node(&NodeId(id.clone())).map_err(|e| store_err(c, e))?;
            let (kind, a) = n.as_ref().map(artifact).unwrap_or((Kind::Text, Value::Null));
            out.push(json!({"seq": seq, "at": at, "op": op, "id": id, "kind": kind.as_str(), "bytes": a["bytes"],
                            "state": n.and_then(|n| n.state)}));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Tmp(PathBuf);
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn cauldron(tag: &str) -> (Tmp, env::Cauldron) {
        let dir = std::env::temp_dir().join(format!(
            "soup-door-dumps-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        let soup = dir.join("cauldrons").join("kb").join("soup");
        std::fs::create_dir_all(&soup).unwrap();
        let e = env::Environment {
            root: dir.clone(),
            root_source: env::paths::Source::CauldronHome,
            name: "dev".into(),
            name_source: env::paths::Source::CauldronEnv,
            data_root: dir.clone(),
        };
        let c = e.find_cauldron("kb").unwrap().expect("found");
        (Tmp(dir), c)
    }

    fn by() -> By {
        By { thread: "test".into(), dispatch: "test dump".into(), instrument: "soup-door-test".into(), version: "0".into() }
    }

    fn text_dump(s: &Store, text: &str, title: Option<&str>) -> (NewDump, Landed) {
        let mut w = s.blob(Kind::Text).unwrap();
        w.write_all(text.as_bytes()).unwrap();
        s.commit(w, || Ok(()), |blob| Ok(new_text(text, title, blob)), &by(), |e| e).unwrap()
    }

    fn new_text(text: &str, title: Option<&str>, blob: &Blob) -> NewDump {
        NewDump {
            env: "dev".into(),
            content: Content::Text { text: text.into(), from: Some("a note".into()) },
            blob: blob.clone(),
            title: title.map(str::to_string),
            cite: Some(json!({"kind": "url", "url": "https://example.org/x"})),
            refs: vec!["T-1".into()],
            client: "test".into(),
            session: None,
        }
    }

    #[test]
    fn a_dump_lands_received_and_reads_back_bounded() {
        let (_t, c) = cauldron("land");
        assert!(!exists(&c));
        assert_eq!(counter(&c).unwrap(), 0);
        assert_eq!(list(&c, &ListOpts::default()).unwrap()["total"], 0);
        assert_eq!(counts(&c).unwrap()["store"], "absent");
        let s = Store::open(&c, "test 1").unwrap();
        assert!(s.created);
        assert_eq!(lock_says(&c).as_deref(), Some("test 1"));
        // A second writer is refused while the first holds the lock.
        assert_eq!(Store::open(&c, "test 2").err().unwrap().code, Code::Locked);

        let text = "first line\nsecond line, with \"quotes\" and é\n".repeat(200);
        let (d1, l1) = text_dump(&s, &text, Some("Notes"));
        let (d2, l2) = text_dump(&s, &text, None);
        assert_eq!((l1.id.as_str(), l2.id.as_str()), ("DM-1", "DM-2"), "two dumps of the same text are two dumps");
        assert_eq!(d1.blob.sha256, d2.blob.sha256);
        assert!(!d1.blob.reused && d2.blob.reused, "and one blob");
        let blobs: Vec<_> = std::fs::read_dir(dir(&c).join(BLOBS)).unwrap().flatten().collect();
        assert_eq!(blobs.len(), 1, "no temporary file is left");
        assert_eq!(l2.counter, 2);
        let r = receipt("kb", "dev", &d1, &l1);
        assert_eq!((r["state"].clone(), r["kind"].clone(), r["bytes"].clone()), (json!("received"), json!("text"), json!(text.len())));
        assert!(!r.to_string().contains("second line"), "a receipt never holds the text");
        drop(s);

        assert_eq!(counter(&c).unwrap(), 2);
        let l = list(&c, &ListOpts::default()).unwrap();
        assert_eq!(l["dumps"][0]["id"], "DM-2", "newest first");
        assert_eq!(l["dumps"][1]["title"], "Notes");
        assert_eq!(l["dumps"][1]["source"], json!({"kind": "text", "client": "test", "from": "a note"}));
        assert!(!l.to_string().contains("second line"), "a list never holds the text");
        assert_eq!(list(&c, &ListOpts { limit: Some(1), ..ListOpts::default() }).unwrap()["more"], 1);
        assert_eq!(counts(&c).unwrap()["by_state"], json!({"received": 2}));

        // Read back in windows: every window within the bound, together the text.
        let mut got = String::new();
        let mut off = 0u64;
        loop {
            let w = get(&c, "DM-1", off, 700).unwrap();
            assert!(crate::budget::size(&w) <= 700, "{}", crate::budget::size(&w));
            got.push_str(w["text"].as_str().unwrap());
            match w["next_offset"].as_u64() {
                Some(n) => off = n,
                None => break,
            }
        }
        assert_eq!(got, text);
        assert_eq!(get(&c, "DM-9", 0, 1000).err().unwrap().code, Code::NotFound);
        assert_eq!(get(&c, "../x", 0, 1000).err().unwrap().code, Code::Usage);

        let after = landed_after(&c, 1, 2, 10).unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!((after[0]["id"].clone(), after[0]["op"].clone(), after[0]["seq"].clone()), (json!("DM-2"), json!("dump"), json!(2)));

        // The store reopens (recovery runs, finds nothing).
        let s = Store::open(&c, "test 3").unwrap();
        assert!(!s.created);
        assert_eq!(s.recovered["interrupted"], "none");
    }

    fn names(c: &env::Cauldron) -> Vec<String> {
        let mut v: Vec<String> =
            std::fs::read_dir(dir(c).join(BLOBS)).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
        v.sort();
        v
    }

    /// A dump refused by the last check, or failing after its blob was
    /// named, leaves no byte; a blob a landed dump names is kept. The open
    /// sweeps a dead writer's temporary file and a blob no node names, and
    /// keeps a live writer's file.
    #[test]
    fn a_dump_that_does_not_land_leaves_no_byte_and_the_open_sweeps() {
        let (_t, c) = cauldron("sweep");
        let s = Store::open(&c, "t").unwrap();
        // Refused by the check: the temporary file goes.
        let mut w = s.blob(Kind::Text).unwrap();
        w.write_all(b"refused words").unwrap();
        let no = s.commit(w, || Err(Error::new(Code::NotGranted, "revoked")), |b| Ok(new_text("x", None, b)), &by(), |e| e);
        assert_eq!(no.err().unwrap().code, Code::NotGranted);
        assert_eq!(names(&c), Vec::<String>::new());
        // Failing after the blob was named: the blob goes.
        let mut w = s.blob(Kind::Text).unwrap();
        w.write_all(b"failed words").unwrap();
        let no = s.commit(w, || Ok(()), |_| Err(Error::new(Code::Store, "no")), &by(), |e| e);
        assert!(no.is_err());
        assert_eq!(names(&c), Vec::<String>::new());
        // The same bytes as a landed dump: the failed one keeps the blob.
        let (d, _) = text_dump(&s, "kept words", None);
        let mut w = s.blob(Kind::Text).unwrap();
        w.write_all(b"kept words").unwrap();
        assert!(s.commit(w, || Ok(()), |_| Err(Error::new(Code::Store, "no")), &by(), |e| e).is_err());
        assert_eq!(names(&c), [format!("{}.txt", d.blob.sha256)]);
        drop(s);

        // What a killed writer leaves: an unlocked temporary file and a
        // named blob no node names. A live writer's file is kept.
        let blobs = dir(&c).join(BLOBS);
        std::fs::write(blobs.join(".incoming-1-1"), b"partial").unwrap();
        let orphan = format!("{}.jsonl", "0".repeat(64));
        std::fs::write(blobs.join(&orphan), b"orphan").unwrap();
        std::fs::write(blobs.join("unrelated"), b"left alone").unwrap();
        let live = blob_writer(&c, Kind::Session).unwrap();
        let s = Store::open(&c, "t").unwrap();
        assert_eq!(s.recovered["swept"], json!({"incoming": 1, "blobs": 1}), "{}", s.recovered);
        let left = names(&c);
        assert_eq!(left.len(), 3, "{left:?}");
        assert!(left.contains(&format!("{}.txt", d.blob.sha256)) && left.contains(&"unrelated".to_string()));
        assert!(left.iter().any(|n| n.starts_with(".incoming-")), "the live writer's file: {left:?}");
        drop(live);
        assert_eq!(names(&c).len(), 2);
    }

    fn mode(p: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::symlink_metadata(p).unwrap().permissions().mode() & 0o777
    }

    /// Every file in the store and below it, with its mode.
    fn modes(c: &env::Cauldron) -> Vec<(String, u32)> {
        let d = dir(c);
        let mut v = vec![(".".to_string(), mode(&d))];
        for sub in [d.clone(), d.join(BLOBS)] {
            for e in std::fs::read_dir(&sub).unwrap().flatten() {
                v.push((e.path().strip_prefix(&d).unwrap().display().to_string(), mode(&e.path())));
            }
        }
        v.sort();
        v
    }

    /// The store is the owner's only: directories 0700, files 0600; and a
    /// store whose modes were left open is set private at its next open.
    #[test]
    fn the_store_is_private_to_its_owner() {
        use std::os::unix::fs::PermissionsExt;
        let (_t, c) = cauldron("modes");
        let s = Store::open(&c, "t").unwrap();
        text_dump(&s, "private words", None);
        let w = s.blob(Kind::Session).unwrap();
        let all = modes(&c);
        assert!(all.iter().any(|(n, _)| n == "dumps.db") && all.iter().any(|(n, _)| n == "spine.db"), "{all:?}");
        assert!(all.iter().any(|(n, _)| n.starts_with("blobs/.incoming-")), "{all:?}");
        for (name, m) in &all {
            let want = if name == "." || name == "blobs" { DIR_MODE } else { FILE_MODE };
            assert_eq!(*m, want, "{name}: {m:o} ({all:?})");
        }
        drop(w);
        drop(s);
        // Left open (as a store made before modes were set would be).
        for (name, _) in modes(&c) {
            let p = dir(&c).join(&name);
            let m = if name == "." || name == "blobs" { 0o755 } else { 0o644 };
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(m)).unwrap();
        }
        let s = Store::open(&c, "t").unwrap();
        for (name, m) in modes(&c) {
            let want = if name == "." || name == "blobs" { DIR_MODE } else { FILE_MODE };
            assert_eq!(m, want, "{name}: {m:o}");
        }
        drop(s);
    }

    /// A fixture transcript in Claude Code's line shape, and the text turns
    /// and counts its text view must answer.
    fn fixture_transcript() -> (String, Vec<(&'static str, String)>, Elided) {
        let long = "the parser keeps a table of states, ".repeat(90);
        let lines = [
            json!({"type": "summary", "summary": "a summary line", "leafUuid": "leaf-one"}).to_string(),
            json!({"type": "user", "uuid": "u-1", "timestamp": "2026-11-01T10:00:00Z", "cwd": "/work/proj",
                   "message": {"role": "user", "content": "Please look at the parser."}})
            .to_string(),
            json!({"type": "assistant", "uuid": "u-2", "timestamp": "2026-11-01T10:00:05Z",
                   "message": {"role": "assistant", "content": [
                       {"type": "thinking", "thinking": "private reasoning"},
                       {"type": "text", "text": "I will read it."},
                       {"type": "tool_use", "id": "tool-one", "name": "Read", "input": {"file_path": "/work/proj/p.rs"}}]}})
            .to_string(),
            json!({"type": "user", "uuid": "u-3", "message": {"role": "user", "content": [
                       {"type": "tool_result", "tool_use_id": "tool-one", "content": "SECRET FILE CONTENT"}]}})
            .to_string(),
            json!({"type": "assistant", "uuid": "u-4", "message": {"role": "assistant", "content": [
                       {"type": "text", "text": long}]}})
            .to_string(),
            json!({"type": "user", "uuid": "u-5", "isMeta": true, "message": {"role": "user", "content": "Caveat: injected"}})
                .to_string(),
            "this line is not json".to_string(),
            json!({"type": "user", "uuid": "u-6", "message": {"role": "user", "content": [
                       {"type": "text", "text": "And this picture é."},
                       {"type": "image", "source": {"type": "base64", "data": "AAAA"}}]}})
            .to_string(),
            json!({"type": "assistant", "uuid": "u-7", "message": {"role": "assistant", "content": [
                       {"type": "tool_use", "id": "tool-two", "name": "Bash", "input": {"command": "ls"}}]}})
            .to_string(),
            json!({"type": "system", "subtype": "note", "content": "compacted"}).to_string(),
            // What Claude Code records as the user's that the person did not
            // type: each carries a planted secret no page may show.
            json!({"type": "user", "uuid": "u-9", "message": {"role": "user", "content": "<bash-input>cat .env PLANTED_SHELL_IN</bash-input>"}})
                .to_string(),
            json!({"type": "user", "uuid": "u-10", "message": {"role": "user",
                   "content": "<bash-stdout>TOKEN=PLANTED_SHELL_OUT</bash-stdout><bash-stderr>PLANTED_SHELL_ERR</bash-stderr>"}})
            .to_string(),
            json!({"type": "user", "uuid": "u-11", "message": {"role": "user",
                   "content": "<command-name>/deploy</command-name>\n<command-message>deploy</command-message>\n<command-args>PLANTED_COMMAND_ARGS</command-args>"}})
            .to_string(),
            json!({"type": "user", "uuid": "u-12", "message": {"role": "user",
                   "content": "<local-command-stdout>PLANTED_LOCAL_OUT</local-command-stdout>"}})
            .to_string(),
            json!({"type": "user", "uuid": "u-13", "isMeta": true, "message": {"role": "user",
                   "content": "<local-command-caveat>Caveat: PLANTED_CAVEAT</local-command-caveat>"}})
            .to_string(),
            json!({"type": "user", "uuid": "u-14", "message": {"role": "user", "content": [
                       {"type": "text", "text": "Here is my real question."},
                       {"type": "text", "text": "<system-reminder>PLANTED_REMINDER_BLOCK</system-reminder>"}]}})
            .to_string(),
            json!({"type": "user", "uuid": "u-15", "message": {"role": "user",
                   "content": "Typed words.\n\n<system-reminder>PLANTED_REMINDER_INLINE</system-reminder>\n"}})
            .to_string(),
            json!({"type": "attachment", "uuid": "u-16", "attachment": {"type": "file", "content": "PLANTED_ATTACHMENT"}}).to_string(),
            json!({"type": "user", "uuid": "u-17", "isCompactSummary": true, "message": {"role": "user",
                   "content": "This session is being continued: PLANTED_SUMMARY"}})
            .to_string(),
            json!({"type": "user", "uuid": "u-18", "message": {"role": "user",
                   "content": "<task-notification>PLANTED_NOTIFICATION</task-notification>"}})
            .to_string(),
            json!({"type": "user", "uuid": "u-19", "toolUseResult": {"stdout": "PLANTED_TOOL_TOP"}, "message": {"role": "user", "content": [
                       {"type": "tool_result", "tool_use_id": "tool-two", "content": [{"type": "text", "text": "PLANTED_TOOL_ARRAY"}]}]}})
            .to_string(),
            json!({"type": "assistant", "uuid": "u-8", "isSidechain": true, "message": {"role": "assistant", "content": [
                       {"type": "text", "text": "Done."}]}})
            .to_string(),
        ];
        let text = lines.join("\n") + "\n";
        let turns = vec![
            ("user", "Please look at the parser.".to_string()),
            ("assistant", "I will read it.".to_string()),
            ("assistant", long),
            ("user", "And this picture é.".to_string()),
            ("user", "Here is my real question.".to_string()),
            ("user", "Typed words.".to_string()),
            ("assistant", "Done.".to_string()),
        ];
        let el = Elided {
            tool_calls: 2,
            tool_results: 2,
            thinking: 1,
            shell: 2,
            local_command: 1,
            commands: 1,
            system_reminders: 2,
            generated: 2,
            meta: 2,
            attachments: 1,
            other_blocks: 1,
            other_lines: 2,
            unreadable_lines: 1,
        };
        (text, turns, el)
    }

    fn session_dump(s: &Store, transcript: &str) -> Landed {
        let mut w = s.blob(Kind::Session).unwrap();
        w.write_all(transcript.as_bytes()).unwrap();
        let make = |blob: &Blob| {
            Ok(NewDump {
                env: "dev".into(),
                content: Content::Session { session_id: "sess-one".into(), cwd: Some("/work/proj".into()), lines: 11 },
                blob: blob.clone(),
                title: None,
                cite: None,
                refs: vec![],
                client: "test".into(),
                session: None,
            })
        };
        s.commit(w, || Ok(()), make, &by(), |e| e).unwrap().1
    }

    /// Every page of a text view, read through from the start.
    fn text_pages(c: &env::Cauldron, id: &str, max: usize) -> Vec<Value> {
        let (mut off, mut k) = (0u64, 0u64);
        let mut pages = vec![];
        loop {
            let p = get_text(c, id, off, k, max).unwrap();
            assert!(crate::budget::size(&p) <= max, "{} > {max}", crate::budget::size(&p));
            let next = (p["next_offset"].as_u64(), p["next_turn_offset"].as_u64().unwrap_or(0));
            assert_eq!(p["more"], json!(next.0.is_some()));
            pages.push(p);
            match next {
                (Some(o), nk) => {
                    assert!((o, nk) > (off, k), "a page moves the cursor on");
                    (off, k) = (o, nk);
                }
                (None, _) => return pages,
            }
        }
    }

    /// The text view answers only the user and assistant text turns, in
    /// bounded pages that read the same from the same cursor, and counts
    /// what it leaves out; together the pages are the whole text.
    #[test]
    fn the_text_view_pages_a_transcript_s_turns_and_counts_what_it_leaves_out() {
        let (_t, c) = cauldron("textview");
        let s = Store::open(&c, "t").unwrap();
        let (transcript, want, want_el) = fixture_transcript();
        assert_eq!(session_dump(&s, &transcript).id, "DM-1");
        let t = text_dump(&s, "plain text", None).1;
        drop(s);

        for max in [700usize, 2_000, 50_000] {
            let pages = text_pages(&c, "DM-1", max);
            // Pieces of one turn share its `at`; joined, they are its text.
            let mut got: Vec<(u64, String, String)> = vec![];
            let mut el = Elided::default();
            for p in &pages {
                assert_eq!(p["view"], "text");
                assert!(!p.to_string().contains("SECRET FILE CONTENT") && !p.to_string().contains("private reasoning"));
                assert!(!p.to_string().contains("Caveat: injected"));
                assert!(!p.to_string().contains("PLANTED"), "max {max}: a planted secret is shown: {p}");
                assert!(!p.to_string().contains("system-reminder") && !p.to_string().contains("<bash-"), "{p}");
                for (k, f) in [
                    (&mut el.tool_calls, "tool_calls"),
                    (&mut el.tool_results, "tool_results"),
                    (&mut el.thinking, "thinking"),
                    (&mut el.shell, "shell"),
                    (&mut el.local_command, "local_command"),
                    (&mut el.commands, "commands"),
                    (&mut el.system_reminders, "system_reminders"),
                    (&mut el.generated, "generated"),
                    (&mut el.meta, "meta"),
                    (&mut el.attachments, "attachments"),
                    (&mut el.other_blocks, "other_blocks"),
                    (&mut el.other_lines, "other_lines"),
                    (&mut el.unreadable_lines, "unreadable_lines"),
                ] {
                    *k += p["elided"][f].as_u64().unwrap();
                }
                for turn in p["turns"].as_array().unwrap() {
                    let at = turn["at"].as_u64().unwrap();
                    let text = turn["text"].as_str().unwrap();
                    match got.last_mut() {
                        Some(last) if last.0 == at => {
                            assert_eq!(turn["from_char"].as_u64().unwrap() as usize, last.2.chars().count());
                            last.2.push_str(text);
                        }
                        _ => got.push((at, turn["role"].as_str().unwrap().into(), text.into())),
                    }
                    // `at` is where the turn's line starts in the transcript.
                    assert!(at == 0 || transcript.as_bytes()[at as usize - 1] == b'\n');
                }
            }
            let got_turns: Vec<(&str, String)> = got.iter().map(|(_, r, t)| (r.as_str(), t.clone())).collect();
            let want_turns: Vec<(&str, String)> = want.iter().map(|(r, t)| (*r, t.clone())).collect();
            assert_eq!(got_turns, want_turns, "max {max}");
            assert_eq!(el, want_el, "max {max}: every elided item is counted once");
            if max == 50_000 {
                assert_eq!(pages.len(), 1);
                let turns = pages[0]["turns"].as_array().unwrap();
                assert_eq!(turns[0]["timestamp"], "2026-11-01T10:00:00Z");
                assert_eq!(turns[6]["sidechain"], true);
                assert!(turns[2].get("chars").is_none(), "a turn that fits is not cut");
            } else {
                assert!(pages.len() > 2, "max {max}: {} pages", pages.len());
                // Stable: each page read again from its own cursor is the same page.
                for p in &pages {
                    let again = get_text(&c, "DM-1", p["offset"].as_u64().unwrap(), p["turn_offset"].as_u64().unwrap_or(0), max).unwrap();
                    assert_eq!(&again, p);
                }
            }
        }
        // The long turn is cut on a small page, and says its whole length.
        let pages = text_pages(&c, "DM-1", 700);
        let cut = pages.iter().flat_map(|p| p["turns"].as_array().unwrap().clone()).find(|t| t.get("chars").is_some()).unwrap();
        assert_eq!(cut["chars"].as_u64().unwrap() as usize, want[2].1.chars().count());

        // Refusals.
        let usage = |r: Result<Value>| r.err().unwrap().code;
        assert_eq!(usage(get_text(&c, &t.id, 0, 0, 2000)), Code::Usage, "a text dump has no text view");
        assert_eq!(usage(get_text(&c, "DM-1", 3, 0, 2000)), Code::Usage, "an offset inside a line");
        assert_eq!(usage(get_text(&c, "DM-1", transcript.len() as u64 + 1, 0, 2000)), Code::Usage, "past the end");
        assert_eq!(usage(get_text(&c, "DM-1", 0, 5, 2000)), Code::Usage, "turn_offset on a line that is no turn");
        assert_eq!(get_text(&c, "DM-9", 0, 0, 2000).err().unwrap().code, Code::NotFound);
        let end = get_text(&c, "DM-1", transcript.len() as u64, 0, 2000).unwrap();
        assert_eq!((end["turns"].clone(), end["more"].clone()), (json!([]), json!(false)));
        // The raw view is unchanged: the transcript, verbatim, every planted
        // secret still in it (the stored blob holds everything).
        assert_eq!(get(&c, "DM-1", 0, 50_000).unwrap()["text"].as_str().unwrap(), transcript);
        assert_eq!(transcript.matches("PLANTED_").count(), 13);
    }

    #[test]
    fn limits_and_ids() {
        assert_eq!(check_text(&"x".repeat(TEXT_LIMIT)).map(|_| ()), Ok(()));
        assert_eq!(check_text(&"x".repeat(TEXT_LIMIT + 1)).err().unwrap().code, Code::TooLarge);
        assert_eq!(check_text("  \n").err().unwrap().code, Code::Usage);
        for ok in ["sess-one-two", "abc", "a.b_c-d"] {
            assert!(session_id_ok(ok), "{ok}");
        }
        for bad in ["", ".", "..", "../x", "a/b", "a\\b", ".hidden", "a b", &"x".repeat(129)] {
            assert!(!session_id_ok(bad), "{bad}");
        }
        assert!(dump_id_ok("DM-12") && !dump_id_ok("DM-") && !dump_id_ok("T-1") && !dump_id_ok("DM-1/.."));
        let (_t, c) = cauldron("limit");
        let s = Store::open(&c, "t").unwrap();
        let mut w = s.blob(Kind::Text).unwrap();
        assert!(w.write_all(&vec![b'a'; TEXT_LIMIT + 1]).is_err());
        drop(w);
        assert_eq!(std::fs::read_dir(dir(&c).join(BLOBS)).unwrap().count(), 0, "a refused blob leaves nothing");
    }

    /// **Concurrent writers in one process never mint one name.** Writers
    /// are made before the store is held, many at once; before the serial,
    /// their temporary names were the pid and the clock, which macOS reads
    /// in microseconds, and a second writer of the same name was refused
    /// `File exists`.
    #[test]
    fn concurrent_blob_writers_in_one_process_all_get_their_own_file() {
        let (_t, c) = cauldron("concurrent-writers");
        drop(Store::open(&c, "t").unwrap());
        let threads: Vec<_> = (0..16)
            .map(|_| {
                let c = c.clone();
                std::thread::spawn(move || {
                    let mut held = Vec::new();
                    for _ in 0..100 {
                        held.push(blob_writer(&c, Kind::Text).map_err(|e| e.to_string())?);
                    }
                    Ok::<usize, String>(held.len())
                })
            })
            .collect();
        for t in threads {
            assert_eq!(t.join().unwrap(), Ok(100));
        }
        // Each one dropped unfinished: none is left behind.
        let left = std::fs::read_dir(dir(&c).join(BLOBS)).unwrap().count();
        assert_eq!(left, 0);
    }

    /// **No page is ever longer than `max_chars`, at any budget.** A
    /// transcript that opens with lines that are not turns, read at every
    /// budget from 100 to 1,400 characters, in both views: each answer is a
    /// page within its bound, or a `usage` refusal; never a longer page, never
    /// a panic, never a page that does not move on. At the documented floor
    /// every page is answered.
    #[test]
    fn no_page_is_longer_than_max_chars_at_any_budget() {
        let (_t, c) = cauldron("small-pages");
        let s = Store::open(&c, "t").unwrap();
        let mut lines: Vec<String> = (0..6).map(|i| json!({"type": "system", "subtype": "note", "content": format!("note {i}")}).to_string()).collect();
        lines.push(json!({"type": "user", "message": {"role": "user", "content": "A short question."}}).to_string());
        lines.push(json!({"type": "assistant", "message": {"role": "assistant", "content": [{"type": "text", "text": "word ".repeat(400)}]}}).to_string());
        let transcript = lines.join("\n") + "\n";
        assert_eq!(session_dump(&s, &transcript).id, "DM-1");
        let t = text_dump(&s, &"letters ".repeat(300), Some(&"a long title ".repeat(15))).1;
        drop(s);
        for max in (100..=1400).step_by(10) {
            for (what, r) in [
                ("text", get_text(&c, "DM-1", 0, 0, max)),
                ("raw session", get(&c, "DM-1", 0, max)),
                ("raw text", get(&c, &t.id, 0, max)),
            ] {
                match r {
                    Ok(p) => {
                        assert!(crate::budget::size(&p) <= max, "{what} at {max}: {} > {max}", crate::budget::size(&p));
                        if let Some(n) = p["next_offset"].as_u64() {
                            assert!(n > 0 || p["next_turn_offset"].as_u64().is_some_and(|k| k > 0), "{what} at {max}: no progress: {p}");
                        }
                    }
                    Err(e) => {
                        assert_eq!(e.code, Code::Usage, "{what} at {max}: {e}");
                        assert!(max < PAGE_MIN_CHARS, "{what} refused at the floor or over it ({max}): {e}");
                    }
                }
            }
        }
        // Read through at the floor: every page answered and within it.
        let pages = text_pages(&c, "DM-1", PAGE_MIN_CHARS);
        assert!(pages.len() > 1);
        assert_eq!(check_page_max(Some(PAGE_MIN_CHARS - 1)).err().unwrap().code, Code::Usage);
        assert!(check_page_max(Some(PAGE_MIN_CHARS)).is_ok() && check_page_max(None).is_ok());
    }
}
