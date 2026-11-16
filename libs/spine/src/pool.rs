//! Pool file storage — one SQLite file per pool inside the cauldron
//! directory, WAL mode, nothing written beside it
//! (soup idiom, inherited deliberately).
//!
//! Load-bearing storage consequences delivered by this DDL:
//! - **Edges are their own records**: adding an edge does NOT rewrite the
//!   node's body record, or the read/connect carve-out is
//!   physically unimplementable (load-bearing).
//! - **Per-field revisions**: the compare-and-swap bookkeeping EXPECT rides
//!   on.
//! - **Count cells** update by commutative UPDATE inside the landing
//!   transaction, never read-modify-write.
//! - **The schema-version stamp** is a machine-owned column, copied from the
//!   pool's pin at creation, changed only by migration machinery that is
//!   out of scope. It appears in no default render.
//! - **One-door provenance**: every node and edge records the spine-row hash
//!   that created it (the migration ledger's assertion).
//! - **The seal flag** is a machine-owned column, `nodes.sealed`,
//!   0 at creation, set to 1 only by a `Delta::Seal` landing and never
//!   cleared. A sealed node's `body` field refuses every SET
//!   (`schema_def::SEALED_BODY_FIELD`, tag `sealed_body`).

use crate::delta::{CompiledPart, Delta};
use crate::envelope::TxHash;
use crate::error::{Result, SpineError};
use crate::schema_def::{Direction, GrammarFault, PoolGrammar, PoolName, SEALED_BODY_FIELD};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// A node identifier. Minted by the store at compile time (see
/// [`crate::ladle::compile`] for the frozen minting rule); opaque text to
/// callers.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct NodeId(pub String);

impl std::fmt::Display for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A pool-qualified node identifier — how an edge names a far endpoint that
/// may live in another pool (a cross-pool edge) or in the
/// reserved projected namespace (ghost nodes; see
/// [`crate::project`]). The pool is ALWAYS explicit, even when local — an
/// interior choice.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct QualifiedNodeId {
    pub pool: PoolName,
    pub id: NodeId,
}

impl std::fmt::Display for QualifiedNodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.pool, self.id)
    }
}

/// The per-pool DDL, in full, created in one transaction at pool creation —
/// nothing lazy, so a pool that opens is a pool with every table (soup
/// idiom). Timestamps are ISO-8601 UTC TEXT; the timestamp is never an
/// ordering key.
pub const POOL_DDL: &str = r#"
CREATE TABLE pool_meta (
  singleton       INTEGER PRIMARY KEY CHECK (singleton = 1),
  pool            TEXT NOT NULL,
  grammar         TEXT NOT NULL CHECK (json_valid(grammar)),
  grammar_version INTEGER NOT NULL,
  created         TEXT NOT NULL
);

CREATE TABLE nodes (
  id             TEXT PRIMARY KEY,
  node_type      TEXT NOT NULL,
  state          TEXT,
  rev            INTEGER NOT NULL DEFAULT 1,
  schema_version INTEGER NOT NULL,
  created_tx     TEXT NOT NULL,
  updated_tx     TEXT NOT NULL,
  created        TEXT NOT NULL,
  updated        TEXT NOT NULL,
  sealed         INTEGER NOT NULL DEFAULT 0 CHECK (sealed IN (0, 1))
);
CREATE INDEX idx_nodes_type_state ON nodes(node_type, state);

CREATE TABLE node_fields (
  node_id    TEXT NOT NULL,
  field      TEXT NOT NULL,
  value      TEXT NOT NULL CHECK (json_valid(value)),
  rev        INTEGER NOT NULL DEFAULT 1,
  updated_tx TEXT NOT NULL,
  PRIMARY KEY (node_id, field)
) WITHOUT ROWID;

CREATE TABLE edges (
  src        TEXT NOT NULL,
  kind       TEXT NOT NULL,
  dst_pool   TEXT NOT NULL,
  dst        TEXT NOT NULL,
  note       TEXT,
  fields     TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(fields)),
  created_tx TEXT NOT NULL,
  created    TEXT NOT NULL,
  PRIMARY KEY (src, kind, dst_pool, dst)
) WITHOUT ROWID;
CREATE INDEX idx_edges_dst ON edges(dst_pool, dst, kind);

CREATE TABLE count_cells (
  node_id TEXT NOT NULL,
  cell    TEXT NOT NULL,
  value   INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (node_id, cell)
) WITHOUT ROWID;

CREATE TABLE applied_parts (
  tx_hash TEXT NOT NULL,
  part    INTEGER NOT NULL,
  applied TEXT NOT NULL,
  PRIMARY KEY (tx_hash, part)
) WITHOUT ROWID;
"#;

/// A handle on one pool file. The connection is private with no accessor —
/// no transaction handle escapes the crate (shared-contract idiom).
pub struct Pool {
    #[allow(dead_code)] // the handle's one private connection
    conn: Connection,
    name: PoolName,
    grammar: PoolGrammar,
}

/// The pragmas every pool file runs under — WAL,
/// `synchronous = NORMAL`, and a 5-second busy timeout, inherited verbatim
/// from soup's `open_connection` (shared-contract idiom: bundled SQLite, WAL,
/// nothing written beside the database files).
///
/// The busy timeout matters here in its own right: two pool handles on the
/// SAME file are exactly the contention the count-cell commutativity proof
/// needs (N >= 2 or not proven).
fn open_connection(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.busy_timeout(std::time::Duration::from_millis(5000))?;
    Ok(conn)
}

/// A pool file created before the seal flag has no `nodes.sealed` column; every
/// read and landing names it. Added here, on open, as the same additive
/// column the DDL declares (`DEFAULT 0`: nothing in such a file was ever
/// sealed, because nothing could seal it). Idempotent: a file that has the
/// column is not written to.
///
/// THE RACE: two opens can both find the column absent; the
/// loser's `ALTER` fails and the recheck absorbs it. Proven at N = 2 by
/// `tests/seal_race.rs`, which holds both openers in the window through
/// the armed build's `fault::upgrade_window` hook (`path` is only for it).
#[cfg_attr(not(feature = "fault-injection"), allow(unused_variables))]
fn upgrade_sealed_column(conn: &Connection, pool: &str, path: &Path) -> Result<()> {
    let has = |conn: &Connection| -> Result<bool> {
        let n: i64 = conn.query_row(
            "SELECT count(*) FROM pragma_table_info('nodes') WHERE name = 'sealed'",
            [],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    };
    if has(conn)? {
        return Ok(());
    }
    #[cfg(feature = "fault-injection")]
    crate::fault::upgrade_window::reached(path, crate::fault::upgrade_window::Event::BeforeAlter);
    if let Err(e) = conn.execute_batch(
        "ALTER TABLE nodes ADD COLUMN sealed INTEGER NOT NULL DEFAULT 0 CHECK (sealed IN (0, 1))",
    ) {
        // Two opens of one old file can race to add it; the loser finds the
        // column the winner added, which is the state it wanted.
        if has(conn)? {
            #[cfg(feature = "fault-injection")]
            crate::fault::upgrade_window::reached(
                path,
                crate::fault::upgrade_window::Event::AlterLostToARacer,
            );
            return Ok(());
        }
        return Err(SpineError::hard(format!(
            "pool `{pool}` predates the seal flag and the `nodes.sealed` column could not be added: {e}"
        )));
    }
    Ok(())
}

/// One ISO-8601 UTC stamp from SQLite itself, so every stamp in the pool comes
/// from one clock and one formatter (soup idiom). NEVER an ordering key
/// — the sequence number is.
fn now(conn: &Connection) -> Result<String> {
    Ok(conn.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')", [], |r| {
        r.get(0)
    })?)
}

impl Pool {
    /// The pool file's path inside a cauldron directory: `<pool>.db`.
    pub fn path_in(dir: &Path, pool: &PoolName) -> PathBuf {
        dir.join(format!("{pool}.db"))
    }

    /// Create `<grammar.pool>.db` at `path` with the full DDL and the
    /// grammar pinned into `pool_meta` (each pool carries a pinned
    /// grammar). Refuses an existing file — a pool is never
    /// overwritten (soup idiom) — and refuses the reserved name `spine`.
    ///
    /// **`path` is the CAULDRON DIRECTORY**, not the file: the file's name is
    /// derived from the grammar's own pool name, which is what makes
    /// "`<grammar.pool>.db`" true by construction rather than by convention.
    /// [`Pool::open`] takes the FILE, because a file is all it has to go on.
    /// The asymmetry is deliberate; compose with
    /// [`Pool::path_in`]. A `path` that is not an existing directory is a
    /// loud [`SpineError::Usage`] rather than a silently created one.
    ///
    /// VISIBILITY (the one-door law): a pool file dropped into a
    /// LIVE cauldron directory was judged against no cauldron and has no
    /// spine record, and the next `Cauldron::open` discovers it like any
    /// other pool. The set of pools changes through
    /// `Cauldron::create` alone, so this is `pub` only under `test-support`
    /// (the storage suite builds pools by hand) and crate-private otherwise.
    #[cfg(feature = "test-support")]
    pub fn create(path: &Path, grammar: &PoolGrammar) -> Result<Pool> {
        Self::create_interior(path, grammar)
    }
    #[cfg(not(feature = "test-support"))]
    pub(crate) fn create(path: &Path, grammar: &PoolGrammar) -> Result<Pool> {
        Self::create_interior(path, grammar)
    }

    fn create_interior(path: &Path, grammar: &PoolGrammar) -> Result<Pool> {
        // The grammar is judged BEFORE anything is written: a malformed
        // grammar is never pinned, and the reserved name `spine` refuses here
        // (the projected namespace).
        grammar.check_wellformed()?;
        if !path.is_dir() {
            return Err(SpineError::usage(format!(
                "pool::Pool::create takes the cauldron DIRECTORY (the file name comes from the grammar); `{}` is not a directory",
                path.display()
            )));
        }
        let file = Pool::path_in(path, &grammar.pool);
        if file.exists() {
            return Err(SpineError::usage(format!(
                "pool file `{}` already exists; a pool is never overwritten",
                file.display()
            )));
        }
        let built = (|| -> Result<Connection> {
            let mut conn = open_connection(&file)?;
            let tx = conn.transaction()?;
            tx.execute_batch(POOL_DDL)?;
            let created: String = tx.query_row(
                "SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')",
                [],
                |r| r.get(0),
            )?;
            let json = serde_json::to_string(grammar)
                .map_err(|e| SpineError::hard(format!("grammar will not serialize: {e}")))?;
            tx.execute(
                "INSERT INTO pool_meta (singleton, pool, grammar, grammar_version, created)
                 VALUES (1, ?1, ?2, ?3, ?4)",
                params![grammar.pool.as_str(), json, grammar.version, created],
            )?;
            tx.commit()?;
            Ok(conn)
        })();
        let conn = match built {
            Ok(conn) => conn,
            Err(e) => {
                // Never leave a half-built pool file behind (soup idiom:
                // nothing written beside the database files, and a pool that
                // opens is a pool with every table).
                let _ = std::fs::remove_file(&file);
                let _ = std::fs::remove_file(file.with_extension("db-wal"));
                let _ = std::fs::remove_file(file.with_extension("db-shm"));
                return Err(e);
            }
        };
        Ok(Pool {
            conn,
            name: grammar.pool.clone(),
            grammar: grammar.clone(),
        })
    }

    /// Open an existing pool file, loading its pinned grammar.
    ///
    /// **`path` is the pool FILE** (see [`Pool::create`]'s note). A file that
    /// is not a pool, or a pool whose `pool_meta` singleton is missing or will
    /// not parse, is a HARD failure — never an empty pool (soup idiom: a store
    /// that will not answer is never reported as an empty store).
    pub fn open(path: &Path) -> Result<Pool> {
        if !path.is_file() {
            return Err(SpineError::not_found(format!(
                "no pool file at `{}`",
                path.display()
            )));
        }
        let conn = open_connection(path)?;
        let row: Option<(String, String, i64)> = conn
            .query_row(
                "SELECT pool, grammar, grammar_version FROM pool_meta WHERE singleton = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(|e| {
                SpineError::hard(format!(
                    "`{}` is not a spine pool (no readable pool_meta): {e}",
                    path.display()
                ))
            })?;
        let Some((name, json, version)) = row else {
            return Err(SpineError::hard(format!(
                "`{}` has no pool_meta singleton row; it is a damaged pool, never an empty one",
                path.display()
            )));
        };
        let grammar: PoolGrammar = serde_json::from_str(&json).map_err(|e| {
            SpineError::hard(format!(
                "pool `{name}`'s pinned grammar will not parse: {e}"
            ))
        })?;
        if grammar.pool.as_str() != name || i64::from(grammar.version) != version {
            return Err(SpineError::hard(format!(
                "pool `{name}` disagrees with its own pinned grammar (`{}` v{}); refusing rather than guessing",
                grammar.pool, grammar.version
            )));
        }
        upgrade_sealed_column(&conn, &name, path)?;
        Ok(Pool {
            conn,
            name: PoolName::new(name),
            grammar,
        })
    }

    /// Open an existing pool file for READING ONLY (the offline reads), with
    /// a connection that cannot write: no pragma, no seal-column upgrade, and
    /// the file opened through [`crate::offline::read_only_connection`]. A
    /// pool that predates the seal flag is refused rather than upgraded,
    /// because every read names that column and this open may not add it.
    /// Its [`Pool::snapshot`] reads exactly as an ordinary handle's does; a
    /// write through it fails at SQLite.
    pub fn open_read_only(path: &Path) -> Result<Pool> {
        if !path.is_file() {
            return Err(SpineError::not_found(format!("no pool file at `{}`", path.display())));
        }
        let conn = crate::offline::read_only_connection(path)?;
        let (name, json, version): (String, String, i64) = conn
            .query_row(
                "SELECT pool, grammar, grammar_version FROM pool_meta WHERE singleton = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .map_err(|e| {
                SpineError::hard(format!("`{}` is not a readable spine pool: {e}", path.display()))
            })?;
        let grammar: PoolGrammar = serde_json::from_str(&json)
            .map_err(|e| SpineError::hard(format!("pool `{name}`'s pinned grammar will not parse: {e}")))?;
        if grammar.pool.as_str() != name || i64::from(grammar.version) != version {
            return Err(SpineError::hard(format!(
                "pool `{name}` disagrees with its own pinned grammar (`{}` v{}); refusing rather than guessing",
                grammar.pool, grammar.version
            )));
        }
        let sealed: i64 = conn.query_row(
            "SELECT count(*) FROM pragma_table_info('nodes') WHERE name = 'sealed'",
            [],
            |r| r.get(0),
        )?;
        if sealed == 0 {
            return Err(SpineError::hard(format!(
                "pool `{name}` predates the seal flag; a read-only open cannot add the column (open it \
                 once through its soup)"
            )));
        }
        Ok(Pool { conn, name: PoolName::new(name), grammar })
    }

    pub fn name(&self) -> &PoolName {
        &self.name
    }

    /// The per-statement busy wait, from configuration
    /// (`SpineConfig::busy_timeout`) — overriding the inherited
    /// 5s `open_connection` sets, on every handle the cauldron opens.
    pub(crate) fn set_busy_timeout(&self, timeout: std::time::Duration) -> Result<()> {
        self.conn.busy_timeout(timeout)?;
        Ok(())
    }

    /// The pinned grammar every part touching this pool validates under
    /// (the spine is version-blind; each part validates here).
    pub fn grammar(&self) -> &PoolGrammar {
        &self.grammar
    }

    /// TEST-SUPPORT SIDE DOOR (the one-door law): landing a part without a
    /// spine row is exactly the second write path that is a defect by
    /// construction, so this
    /// is `pub` ONLY under the `test-support` feature — which this crate's
    /// own dev-dependency enables for its test suites, and which no
    /// production consumer gets without asking for it by name. Behavior is
    /// identical in both builds; only reachability changes. The real
    /// contract lives on [`Self::apply_part_interior`]'s docs below.
    #[cfg(feature = "test-support")]
    pub fn apply_part(
        &mut self,
        tx: &TxHash,
        part_ordinal: u32,
        part: &CompiledPart,
    ) -> Result<Landed> {
        self.apply_part_interior(tx, part_ordinal, part, ApplyTarget::Landing)
    }

    /// See [`Self::apply_part_interior`]; crate-interior without
    /// `test-support` (the one-door law).
    #[cfg(not(feature = "test-support"))]
    pub(crate) fn apply_part(
        &mut self,
        tx: &TxHash,
        part_ordinal: u32,
        part: &CompiledPart,
    ) -> Result<Landed> {
        self.apply_part_interior(tx, part_ordinal, part, ApplyTarget::Landing)
    }

    /// Land one compiled part as ONE atomic SQLite transaction on this
    /// pool's file (per-pool landing; cross-file commits do NOT use
    /// ATTACH — the commit point is the spine row flip, never this
    /// landing).
    ///
    /// Inside the one transaction, in order: the idempotence marker row in
    /// `applied_parts` (a part already landed returns
    /// `already_landed: true` and applies nothing — what makes recovery's
    /// roll-forward idempotent), the deltas, the per-field
    /// revision bumps, and the mechanically derived count-cell commutative
    /// increments.
    fn apply_part_interior(
        &mut self,
        tx: &TxHash,
        part_ordinal: u32,
        part: &CompiledPart,
        target: ApplyTarget,
    ) -> Result<Landed> {
        let hex = tx.to_hex();
        let grammar = self.grammar.clone();
        let pin = grammar.version;
        // ONE SQLite transaction for the whole part (per-pool
        // landing). Every early return below drops it un-committed, which is
        // a rollback: a refused part lands NOTHING, including its idempotence
        // marker.
        //
        // BEGIN IMMEDIATE, not DEFERRED, deliberately: a landing is a writer
        // from its first statement, and a deferred transaction that reads
        // first and writes later cannot be made to wait by `busy_timeout` —
        // SQLite returns SQLITE_BUSY immediately on the upgrade rather than
        // blocking. Two concurrent landings on one pool file would then race
        // and one would fail spuriously. Taking the write lock up front makes
        // them queue instead, which is what the N >= 2 count-cell proof needs.
        let sql = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

        // ---- idempotence marker, read FIRST (recovery is idempotent: run
        //      it twice, same result).
        let already: Option<i64> = sql
            .query_row(
                "SELECT 1 FROM applied_parts WHERE tx_hash = ?1 AND part = ?2",
                params![hex, part_ordinal],
                |r| r.get(0),
            )
            .optional()?;
        if already.is_some() {
            sql.commit()?;
            return Ok(Landed {
                tx: *tx,
                already_landed: true,
            });
        }

        // ---- validate under THIS pool's pinned grammar, version-pinned
        //      (the version-blind rule; per-pool validation). The
        //      door validates too; this is the last gate before bytes move,
        //      and it is what makes a poisoned part unable to land even if a
        //      caller reaches the store directly.
        //
        //      The resolver's signature (`schema_def::TypeResolver`) answers
        //      Option, not Result — deliberately, so validation stays pure and
        //      runs identically at compile time and at landing time. That
        //      leaves one honesty hazard: a genuine STORAGE failure here would
        //      look like "no such node" and be reported as the caller's
        //      grammar violation. It is caught in a cell and re-raised as the
        //      hard failure it is, so a physics failure is never dressed up as
        //      a refusal (fail-closed, loudly, by name).
        //
        //      THE OVERLAY TARGET SKIPS THIS GATE. A
        //      tranche's overlay is DERIVED state — rebuilt from the staging
        //      rows, never a pool file — and the part reaching it was judged
        //      moments ago by the same validator, at the door's window
        //      (`FarSide::Door`, against the view). The landing window
        //      cannot judge it: a staged CREATE still carries its
        //      PROVISIONAL id, which `FarSide::Landing` refuses by design
        //      (`provisional_id_at_landing`) because the decided flip has not
        //      numbered it yet — and for a staged child that flip is the
        //      tranche's decision, minutes away. The real landing
        //      runs this gate on the real file with the serial in place.
        let io: std::cell::RefCell<Option<rusqlite::Error>> = std::cell::RefCell::new(None);
        let resolver = |id: &NodeId| -> Option<String> {
            match sql
                .query_row(
                    "SELECT node_type FROM nodes WHERE id = ?1",
                    params![id.0],
                    |r| r.get(0),
                )
                .optional()
            {
                Ok(found) => found,
                Err(e) => {
                    *io.borrow_mut() = Some(e);
                    None
                }
            }
        };
        // `FarSide::Landing`: this file has no window onto another
        // pool's file (no ATTACH), so a cross-pool
        // edge's far half is not re-judged here. The part must instead CARRY
        // the door's record of the far pin it validated under
        // (`CompiledPart::far_versions`); a cross-pool edge without one
        // refuses `far_unvalidated` — a part that never went through the
        // door's cross-pool check cannot land its edge.
        let verdict = match target {
            ApplyTarget::Landing => grammar.validate_part(
                part,
                Some(part_ordinal),
                &resolver,
                &crate::schema_def::FarSide::Landing,
            ),
            ApplyTarget::Overlay => Ok(()),
        };
        if let Some(e) = io.into_inner() {
            return Err(SpineError::hard(format!(
                "pool `{}` could not read a node type while validating part {part_ordinal} of \
                 transaction {hex}: {e}",
                self.name
            )));
        }
        verdict?;

        let stamp = now(&sql)?;
        // Node types learned along the way: created here, or already stored.
        let mut types: BTreeMap<NodeId, String> = BTreeMap::new();
        // The at-most-one slices this part touched, checked after every delta
        // has landed so ordering inside the part (destroy-then-create) works.
        let mut scalar_checks: BTreeSet<(String, ScalarEnd)> = BTreeSet::new();

        for delta in &part.deltas {
            match delta {
                Delta::CreateNode {
                    id,
                    node_type,
                    state,
                    fields,
                } => {
                    sql.execute(
                        "INSERT INTO nodes (id, node_type, state, rev, schema_version,
                                            created_tx, updated_tx, created, updated)
                         VALUES (?1, ?2, ?3, 1, ?4, ?5, ?5, ?6, ?6)",
                        params![id.0, node_type, state, pin, hex, stamp],
                    )?;
                    for (field, value) in fields {
                        sql.execute(
                            "INSERT INTO node_fields (node_id, field, value, rev, updated_tx)
                             VALUES (?1, ?2, ?3, 1, ?4)",
                            params![id.0, field, json_text(value)?, hex],
                        )?;
                    }
                    types.insert(id.clone(), node_type.clone());
                }

                Delta::SetField { node, path, value } => {
                    // The ARITHMETIC is the exported delta-apply's:
                    // `set_target` routes, `state_after_set` /
                    // `field_after_set` compute — the one builder a fold
                    // outside this crate also calls. Only the SQL is here.
                    let Some(target) = set_target(path) else {
                        return Err(SpineError::hard(format!(
                            "pool `{}` part {part_ordinal} of transaction {hex}: a SET on node \
                             `{node}` carries an empty path; nothing compiles one",
                            self.name
                        )));
                    };
                    match target {
                        // `state` lives in its own column (the type+state
                        // index is the staged match form's read path). Its
                        // per-field revision is therefore the WHOLE-NODE
                        // revision; an interior choice.
                        SetTarget::State => {
                            // Unreachable behind the validation above (a
                            // non-string state refuses `unknown_state`), which
                            // is exactly why it is a refusal and not an
                            // `expect`: the one code path that could ever
                            // reach it is a caller who got past the gate, and
                            // that caller deserves a structured witness rather
                            // than a panic (shared-contract idiom: loud failure
                            // with structured payloads, never a process abort).
                            let s = state_after_set(value)
                                .map_err(|f| f.refusal(&self.name, Some(part_ordinal)))?;
                            let n = sql.execute(
                                "UPDATE nodes SET state = ?1, rev = rev + 1, updated_tx = ?2, updated = ?3
                                 WHERE id = ?4",
                                params![s, hex, stamp, node.0],
                            )?;
                            if n != 1 {
                                return Err(GrammarFault::new(
                                    "unknown_node",
                                    format!("pool `{}` holds no node `{node}`", self.name),
                                )
                                .refusal(&self.name, Some(part_ordinal)));
                            }
                        }
                        SetTarget::Field { name: field, sub_path } => {
                            // The seal: defence in depth behind the
                            // door's refusal (`ladle::compile`'s
                            // `check_settable`, under the granted locks), so
                            // a part reaching the store directly cannot edit
                            // a sealed body either. Same tag, same witness.
                            if field == SEALED_BODY_FIELD {
                                let sealed: Option<i64> = sql
                                    .query_row(
                                        "SELECT sealed FROM nodes WHERE id = ?1",
                                        params![node.0],
                                        |r| r.get(0),
                                    )
                                    .optional()?;
                                if sealed == Some(1) {
                                    return Err(sealed_body_fault(&self.name, node)
                                        .refusal(&self.name, Some(part_ordinal)));
                                }
                            }
                            let current: Option<String> = sql
                                .query_row(
                                    "SELECT value FROM node_fields WHERE node_id = ?1 AND field = ?2",
                                    params![node.0, field],
                                    |r| r.get(0),
                                )
                                .optional()?;
                            // A nested SET reads and rewrites ONE field's own
                            // JSON document. That is not the read-modify-write
                            // that is forbidden: the ban is on COUNT
                            // CELLS, whose whole purpose is to commute under
                            // concurrency. A field's value is held still by
                            // its body lock.
                            let current_doc: Option<Value> = match &current {
                                Some(text) => Some(serde_json::from_str(text).map_err(|e| {
                                    SpineError::hard(format!(
                                        "node `{node}` field `{field}` holds unparseable JSON: {e}"
                                    ))
                                })?),
                                None => None,
                            };
                            let next = field_after_set(current_doc.as_ref(), sub_path, value)
                                .map_err(|f| f.refusal(&self.name, Some(part_ordinal)))?;
                            if current.is_some() {
                                sql.execute(
                                    "UPDATE node_fields SET value = ?1, rev = rev + 1, updated_tx = ?2
                                     WHERE node_id = ?3 AND field = ?4",
                                    params![json_text(&next)?, hex, node.0, field],
                                )?;
                            } else {
                                sql.execute(
                                    "INSERT INTO node_fields (node_id, field, value, rev, updated_tx)
                                     VALUES (?1, ?2, ?3, 1, ?4)",
                                    params![node.0, field, json_text(&next)?, hex],
                                )?;
                            }
                            let n = sql.execute(
                                "UPDATE nodes SET rev = rev + 1, updated_tx = ?1, updated = ?2 WHERE id = ?3",
                                params![hex, stamp, node.0],
                            )?;
                            if n != 1 {
                                return Err(GrammarFault::new(
                                    "unknown_node",
                                    format!("pool `{}` holds no node `{node}`", self.name),
                                )
                                .refusal(&self.name, Some(part_ordinal)));
                            }
                        }
                    }
                }

                Delta::AddEdge {
                    src,
                    kind,
                    dst,
                    note,
                    fields,
                } => {
                    // Edges are their OWN records: this INSERT touches no node
                    // body row, which is what makes the
                    // read/connect carve-out physically implementable
                    // (load-bearing).
                    let inserted = sql.execute(
                        "INSERT INTO edges (src, kind, dst_pool, dst, note, fields, created_tx, created)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                         ON CONFLICT (src, kind, dst_pool, dst) DO NOTHING",
                        params![
                            src.0,
                            kind,
                            dst.pool.as_str(),
                            dst.id.0,
                            note,
                            json_text(&Value::Object(fields.clone().into_iter().collect()))?,
                            hex,
                            stamp
                        ],
                    )?;
                    // Edges are a SET: re-adding one already present is the
                    // same set, so it is a no-op — and crucially the count
                    // cells must NOT move for it.
                    //
                    // FIELDS: the identity is still the triple, and
                    // the FIRST edge's fields stand. A re-add with DIFFERENT
                    // fields is refused `edge_fields_differ` by the door at
                    // phase 10b (`exec::check_pins_and_occupancy`), before
                    // the decided flip. It is deliberately NOT re-judged
                    // here: this runs after the flip, where a refusal wedges
                    // the row (the at-most-one lesson). No
                    // door statement reaches this with different fields:
                    // the triple is claimed under an EXCLUSIVE Write lock
                    // (`ladle::compile::claims_for`, `Resource::Edge`), so
                    // two statements on one triple serialize and the second
                    // is judged at phase 10b against the first's edge
                    // (measured: 0/40 with the lock, 2/40 without).
                    // `DO NOTHING` here is the set no-op for an identical
                    // re-add, and the guard for a part landed by hand.
                    if inserted == 1 {
                        self_bump_cells(
                            &sql, &grammar, &mut types, src, kind, dst, &self.name, 1,
                        )?;
                    }
                    note_scalars(&grammar, kind, src, dst, &self.name, &mut scalar_checks);
                }

                Delta::Seal { node } => {
                    // 0 -> 1 bumps the whole-node rev (the record changed);
                    // a node already sealed is left exactly as it is. The
                    // door never emits the second case (a sealed node
                    // compiles to no delta), so it is the hand-built part's
                    // no-op, not a refusal after the decided flip.
                    let n = sql.execute(
                        "UPDATE nodes SET sealed = 1, rev = rev + 1, updated_tx = ?1, updated = ?2
                         WHERE id = ?3 AND sealed = 0",
                        params![hex, stamp, node.0],
                    )?;
                    if n != 1 {
                        let exists: Option<i64> = sql
                            .query_row("SELECT 1 FROM nodes WHERE id = ?1", params![node.0], |r| r.get(0))
                            .optional()?;
                        if exists.is_none() {
                            return Err(GrammarFault::new(
                                "unknown_node",
                                format!("pool `{}` holds no node `{node}`", self.name),
                            )
                            .refusal(&self.name, Some(part_ordinal)));
                        }
                    }
                }

                Delta::RemoveEdge { src, kind, dst } => {
                    // Edges are a SET in BOTH directions: REMOVE
                    // of an edge that is not there is the same set, so it
                    // commits as a no-op exactly as a CREATE of an edge
                    // already present does above — and the count cells do
                    // NOT move for it (`removed == 0`). The row records
                    // what was ASKED, never a claim that
                    // something was removed; a caller who needs strictness
                    // matches the edge and pins `EXPECT COUNT = 1`.
                    let removed = sql.execute(
                        "DELETE FROM edges WHERE src = ?1 AND kind = ?2 AND dst_pool = ?3 AND dst = ?4",
                        params![src.0, kind, dst.pool.as_str(), dst.id.0],
                    )?;
                    if removed == 1 {
                        self_bump_cells(
                            &sql, &grammar, &mut types, src, kind, dst, &self.name, -1,
                        )?;
                    }
                }
            }
        }

        // ---- the at-most-one slices, as OCCUPANCY (a slice constrained to
        //      at most one edge is a scalar). Pure grammar
        //      validation cannot see occupancy — it is a property of the
        //      pool's current edges, not of the part — so the check also
        //      lives here, inside the landing transaction, where a violation
        //      rolls the whole part back.
        //
        //      THIS IS NOW DEFENCE IN DEPTH, NOT THE PRIMARY GATE. The door
        //      evaluates the same rule at
        //      `exec`'s PHASE 10b — under the granted slice WRITE locks and
        //      BEFORE the decided flip — so a statement arriving through the
        //      door has already been judged, and a violation there refuses
        //      whole: no row decided, locks released, nothing landed anywhere
        //      (SETTLED). This check stays because `apply_part` is
        //      the last gate before bytes move and a caller reaching the store
        //      directly (the storage suite, a future tool) is not the door.
        //
        //      For a statement that DID come through the door, reaching this
        //      refusal is an internal invariant violation rather than a
        //      caller's refusal, and `exec` names it as one — see
        //      `exec::REFUSED_AFTER_DECIDED`. The refusal built here is
        //      unchanged and still structured, because a direct caller
        //      deserves the witness.
        for (kind, end) in &scalar_checks {
            let (sql_text, key) = match end {
                ScalarEnd::From(src) => (
                    "SELECT count(*) FROM edges WHERE src = ?1 AND kind = ?2",
                    src.clone(),
                ),
                ScalarEnd::To(pool, dst) => {
                    let _ = pool;
                    (
                        "SELECT count(*) FROM edges WHERE dst_pool = ?3 AND dst = ?1 AND kind = ?2",
                        dst.clone(),
                    )
                }
            };
            let n: i64 = match end {
                ScalarEnd::From(_) => {
                    sql.query_row(sql_text, params![key, kind], |r| r.get(0))?
                }
                ScalarEnd::To(pool, _) => {
                    sql.query_row(sql_text, params![key, kind, pool.as_str()], |r| r.get(0))?
                }
            };
            if n > 1 {
                let which = match end {
                    ScalarEnd::From(s) => format!("leaving `{s}`"),
                    ScalarEnd::To(p, d) => format!("arriving at `{p}/{d}`"),
                };
                return Err(GrammarFault::new(
                    "at_most_one_violated",
                    format!(
                        "edge kind `{kind}` is an at-most-one slice {which}; the part leaves {n}"
                    ),
                )
                .refusal(&self.name, Some(part_ordinal)));
            }
        }

        // ---- the marker, written inside the SAME transaction as the deltas:
        //      landed-and-marked is one atomic fact, so a replay can never see
        //      the deltas without the marker or the marker without the deltas.
        sql.execute(
            "INSERT INTO applied_parts (tx_hash, part, applied) VALUES (?1, ?2, ?3)",
            params![hex, part_ordinal, stamp],
        )?;
        sql.commit()?;
        Ok(Landed {
            tx: *tx,
            already_landed: false,
        })
    }

    /// Open a read snapshot of this pool. A single-pool multi-query read on
    /// one snapshot is consistent by the per-pool atomic landing step and
    /// needs neither locks nor validation.
    ///
    /// Mechanically: a read transaction held open for the snapshot's life. In
    /// WAL a deferred read transaction pins its view at its FIRST read, so one
    /// throwaway read is issued here — without it the "snapshot" would begin
    /// at whatever the caller's first query happened to see. Dropping the
    /// snapshot rolls the read transaction back; no transaction handle escapes
    /// the crate (shared-contract idiom).
    pub fn snapshot(&self) -> Result<Snapshot<'_>> {
        let tx = self.conn.unchecked_transaction()?;
        let _pin: i64 = tx.query_row("SELECT count(*) FROM pool_meta", [], |r| r.get(0))?;
        Ok(Snapshot { tx, overlay: None })
    }

    /// A snapshot of this pool WITH a tranche's overlay layered on top:
    /// the committed snapshot, pinned as
    /// [`Pool::snapshot`] pins it, plus a snapshot of the overlay's own
    /// in-memory database, consulted first by every accessor. The holder's
    /// view; an outside reader never gets one.
    pub(crate) fn snapshot_over<'a>(&'a self, overlay: &'a Overlay) -> Result<Snapshot<'a>> {
        let mut snap = self.snapshot()?;
        snap.overlay = Some(Box::new(overlay.snapshot()?));
        Ok(snap)
    }
}

/// Which end of an at-most-one edge kind a part touched.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum ScalarEnd {
    From(String),
    To(PoolName, String),
}

fn note_scalars(
    grammar: &PoolGrammar,
    kind: &str,
    src: &NodeId,
    dst: &QualifiedNodeId,
    local: &PoolName,
    out: &mut BTreeSet<(String, ScalarEnd)>,
) {
    if grammar.is_scalar_slice(kind, Direction::Out) {
        out.insert((kind.to_string(), ScalarEnd::From(src.0.clone())));
    }
    // An at-most-one `to` is only enforceable when the slice lives in THIS
    // pool; `check_wellformed` already refuses at-most-one beside any REACH,
    // so a far end — another pool's, or the projected `spine`
    // namespace's — never carries a bound here.
    if grammar.is_scalar_slice(kind, Direction::In) && dst.pool == *local {
        out.insert((
            kind.to_string(),
            ScalarEnd::To(dst.pool.clone(), dst.id.0.clone()),
        ));
    }
}

/// Move every grammar-declared count cell an edge touches, by COMMUTATIVE
/// UPSERT — `value = value + <delta>` computed by SQLite, never read into this
/// process and written back (a plain-write counter would re-serialize every
/// create one level down).
#[allow(clippy::too_many_arguments)]
fn self_bump_cells(
    sql: &rusqlite::Transaction<'_>,
    grammar: &PoolGrammar,
    types: &mut BTreeMap<NodeId, String>,
    src: &NodeId,
    kind: &str,
    dst: &QualifiedNodeId,
    local: &PoolName,
    delta: i64,
) -> Result<()> {
    let src_type = lookup_type(sql, types, src)?;
    if let Some(t) = src_type {
        for cell in grammar.count_cells_for(&t, kind, Direction::Out) {
            bump_cell(sql, &src.0, cell, delta)?;
        }
    }
    // A cell on the far end is only ours to move when the far end is local:
    // one pool's landing transaction never writes another pool's file
    // (no ATTACH, the commit point is the spine row).
    if dst.pool == *local {
        if let Some(t) = lookup_type(sql, types, &dst.id)? {
            for cell in grammar.count_cells_for(&t, kind, Direction::In) {
                bump_cell(sql, &dst.id.0, cell, delta)?;
            }
        }
    }
    Ok(())
}

fn lookup_type(
    sql: &rusqlite::Transaction<'_>,
    types: &mut BTreeMap<NodeId, String>,
    id: &NodeId,
) -> Result<Option<String>> {
    if let Some(t) = types.get(id) {
        return Ok(Some(t.clone()));
    }
    let found: Option<String> = sql
        .query_row(
            "SELECT node_type FROM nodes WHERE id = ?1",
            params![id.0],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(t) = &found {
        types.insert(id.clone(), t.clone());
    }
    Ok(found)
}

fn bump_cell(
    sql: &rusqlite::Transaction<'_>,
    node_id: &str,
    cell: &str,
    delta: i64,
) -> Result<()> {
    sql.execute(
        "INSERT INTO count_cells (node_id, cell, value) VALUES (?1, ?2, ?3)
         ON CONFLICT (node_id, cell) DO UPDATE SET value = value + excluded.value",
        params![node_id, cell, delta],
    )?;
    Ok(())
}

fn json_text(v: &Value) -> Result<String> {
    serde_json::to_string(v)
        .map_err(|e| SpineError::hard(format!("field value will not serialize: {e}")))
}

// ---- the exported delta-apply: the SetField arithmetic as pure
//      functions, so a fold outside this crate and the landing are ONE
//      builder. No store, no clock. ----

/// Where a SetField lands — the routing [`Pool::apply_part`]'s SetField arm
/// follows, and the one every fold of a committed SET must follow too.
///
/// # The revision rule both builders follow
///
/// Every SetField bumps the whole-node `rev` by exactly 1, whatever its
/// target. The per-field rev moves only on a [`SetTarget::Field`] target:
/// by 1 on an existing field, and a field that was absent starts at rev 1.
/// A [`SetTarget::State`] target writes the `state` column and has no
/// per-field rev of its own (its revision IS the whole-node one). Edges —
/// AddEdge, RemoveEdge — never bump `rev`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetTarget<'p> {
    /// The path is exactly `["state"]`: the node's `state` column.
    State,
    /// Any other path: field `name`, and the dotted remainder inside its
    /// JSON document (`[]` replaces the whole value).
    Field { name: &'p str, sub_path: &'p [String] },
}

/// Route a SetField path. `["state"]` exactly is [`SetTarget::State`];
/// otherwise `Field { path[0], path[1..] }` — so `["state", "x"]` is a FIELD
/// named `state`, not the state column. `None` on an empty path, which no
/// compiled statement carries (the landing treats it as `Hard`).
pub fn set_target(path: &[String]) -> Option<SetTarget<'_>> {
    match path {
        [] => None,
        [only] if only == "state" => Some(SetTarget::State),
        [name, rest @ ..] => Some(SetTarget::Field {
            name: name.as_str(),
            sub_path: rest,
        }),
    }
}

/// The new `state` a [`SetTarget::State`] SET writes. A non-string refuses
/// `unknown_state` — the landing's own refusal text.
pub fn state_after_set(value: &Value) -> std::result::Result<String, GrammarFault> {
    match value.as_str() {
        Some(s) => Ok(s.to_string()),
        None => Err(GrammarFault::new(
            "unknown_state",
            format!("state must be a string; got `{value}`"),
        )),
    }
}

/// The new value of a field a [`SetTarget::Field`] SET writes: the current
/// value (`None` — the field is absent — starts from `Null`), with `value`
/// set at `sub_path` by the same [`set_at_path`] the door's compile check
/// runs. A dotted path through a scalar or an array refuses `not_an_object`.
pub fn field_after_set(
    current: Option<&Value>,
    sub_path: &[String],
    value: &Value,
) -> std::result::Result<Value, GrammarFault> {
    let mut doc = current.cloned().unwrap_or(Value::Null);
    set_at_path(&mut doc, sub_path, value.clone())?;
    Ok(doc)
}

/// Set `value` at `keys` inside `doc`, minting intermediate objects where
/// the way is ABSENT or `null`. `keys` empty replaces the whole document
/// (the un-dotted SET).
///
/// A dotted key addressing through a value that is NOT an object — a
/// scalar, an array — REFUSES `not_an_object` rather than replacing it
/// (`SET d.title.sub = 1` over `title = "hello"` used to commit and leave
/// `title = {"sub": 1}` — data loss behind a `Committed` reply, the silent
/// misread the grammar forbids). The
/// door refuses the same fact at compile by running THIS function over the
/// value the landing will see (`ladle::compile::check_settable` — the
/// matched record's value with the part's earlier deltas replayed; two
/// judges over two states were shown to disagree), so for a
/// statement through the door this is defence in depth and reaching it
/// after the decided flip is the crate's invariant failure
/// (`exec::REFUSED_AFTER_DECIDED`). `pub(crate)` for exactly that one
/// caller: one validator, two doors.
pub(crate) fn set_at_path(
    doc: &mut Value,
    keys: &[String],
    value: Value,
) -> std::result::Result<(), GrammarFault> {
    match keys.split_first() {
        None => {
            *doc = value;
            Ok(())
        }
        Some((k, rest)) => {
            if doc.is_null() {
                *doc = Value::Object(serde_json::Map::new());
            }
            let Some(obj) = doc.as_object_mut() else {
                return Err(GrammarFault::new(
                    "not_an_object",
                    format!(
                        "the dotted key `{k}` addresses into a value that is not an object \
                         (`{doc}`); a SET never destroys a scalar to reach a sub-key"
                    ),
                ));
            };
            let child = obj.entry(k.clone()).or_insert(Value::Null);
            set_at_path(child, rest, value)
        }
    }
}

/// The `sealed_body` fault — ONE constructor for the door's refusal
/// (`ladle::compile`'s `check_settable`) and the landing's, so the two
/// never word one fact differently. Names the node and the repair.
pub fn sealed_body_fault(pool: &PoolName, node: &NodeId) -> GrammarFault {
    GrammarFault::new(
        "sealed_body",
        format!(
            "node `{pool}/{node}` is sealed: its `{SEALED_BODY_FIELD}` is immutable; correct it by \
             superseding (mint a successor and a `supersedes` edge to `{node}`)"
        ),
    )
}

/// What one landing reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Landed {
    pub tx: TxHash,
    /// True when the idempotence marker showed this part already applied —
    /// nothing was re-applied (recovery idempotence).
    pub already_landed: bool,
}

/// Which target a part is applied to: the pool FILE at a landing,
/// or a tranche's in-memory OVERLAY. The arithmetic is one
/// routine either way — that is the point of running the overlay through
/// `apply_part` at all: field revisions, count cells and the `sealed` mark
/// behave in the view exactly as they will on landing. Only the validator
/// gate differs; see `apply_part_interior`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApplyTarget {
    Landing,
    Overlay,
}

/// One consistent read view of one pool. Every method reads the same
/// snapshot. The validated cross-pool read composes these —
/// see [`crate::read`].
///
/// # The view
///
/// A snapshot opened through [`Pool::snapshot_over`] carries a tranche's
/// OVERLAY — the staged children's deltas, in an in-memory database with
/// this pool's own tables — and every one of the five accessors then
/// consults the overlay FIRST and the committed snapshot SECOND, uniting the
/// two for scans: the nodes of a type are the committed nodes not in the
/// overlay plus the overlay's; the edges at a node are the committed edges
/// minus the overlay's tombstones plus the overlay's. A node the overlay
/// holds (created, or copied in on first touch) SHADOWS the committed one
/// whole. A count cell is `committed now + (overlay − the committed value
/// at copy)`, so a foreign bump on an untouched slice still shows. A
/// snapshot opened through [`Pool::snapshot`] has no overlay and reads the
/// committed pool alone — the outside reader's view.
pub struct Snapshot<'a> {
    /// A read transaction held open for the snapshot's life; dropping it
    /// rolls back. Private with no accessor — no transaction handle escapes
    /// the crate (shared-contract idiom).
    tx: rusqlite::Transaction<'a>,
    /// The tranche's staged deltas, as a snapshot over the overlay's own
    /// in-memory database (itself overlay-less). `None` = committed state.
    overlay: Option<Box<Snapshot<'a>>>,
}

impl Snapshot<'_> {
    /// One node with all its fields, or `None`. The schema-version stamp
    /// rides the record but appears in no default render (render policy is the reader's, the record is complete; see
    /// [`NodeRecord::render_default`]).
    pub fn node(&self, id: &NodeId) -> Result<Option<NodeRecord>> {
        if let Some(o) = &self.overlay {
            if let Some(n) = o.node_raw(id)? {
                return Ok(Some(n));
            }
        }
        self.node_raw(id)
    }

    fn node_raw(&self, id: &NodeId) -> Result<Option<NodeRecord>> {
        let head: Option<(String, Option<String>, i64, i64, i64)> = self
            .tx
            .query_row(
                "SELECT node_type, state, rev, schema_version, sealed FROM nodes WHERE id = ?1",
                params![id.0],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?;
        let Some((node_type, state, rev, schema_version, sealed)) = head else {
            return Ok(None);
        };
        Ok(Some(NodeRecord {
            id: id.clone(),
            node_type,
            state,
            rev,
            schema_version: schema_version as u32,
            sealed: sealed != 0,
            fields: self.fields_of(&id.0)?,
        }))
    }

    /// The spine row that CREATED a node (`nodes.created_tx`), or `None`
    /// when the node is absent. A stored value that is not a
    /// transaction hash is `Hard`, naming the node — never `None`. Under a
    /// view, a node the overlay created answers the CHILD's hash.
    pub fn created_tx(&self, id: &NodeId) -> Result<Option<TxHash>> {
        if let Some(o) = &self.overlay {
            if let Some(t) = o.created_tx(id)? {
                return Ok(Some(t));
            }
        }
        let text: Option<String> = self
            .tx
            .query_row(
                "SELECT created_tx FROM nodes WHERE id = ?1",
                params![id.0],
                |r| r.get(0),
            )
            .optional()?;
        match text {
            None => Ok(None),
            Some(t) => TxHash::from_hex(&t).map(Some).ok_or_else(|| {
                SpineError::hard(format!(
                    "node `{id}` holds a `created_tx` that is not a transaction hash: `{t}`"
                ))
            }),
        }
    }

    /// All nodes of a type, optionally narrowed by state — the staged
    /// match-by-pool-plus-type-plus-state form's read. Ordered
    /// by identifier, so a matcher's row order is deterministic.
    pub fn nodes_of(&self, node_type: &str, state: Option<&str>) -> Result<Vec<NodeRecord>> {
        let Some(o) = &self.overlay else {
            return self.nodes_of_raw(node_type, state);
        };
        // The overlay SHADOWS whole: a committed node the overlay holds —
        // under any type or state — is answered by the overlay's record or
        // not at all (its state may have moved out of the asked set).
        let shadowed = o.all_ids()?;
        let mut out: Vec<NodeRecord> = self
            .nodes_of_raw(node_type, state)?
            .into_iter()
            .filter(|n| !shadowed.contains(&n.id.0))
            .collect();
        out.extend(o.nodes_of_raw(node_type, state)?);
        out.sort_by(|a, b| a.id.0.cmp(&b.id.0));
        Ok(out)
    }

    fn nodes_of_raw(&self, node_type: &str, state: Option<&str>) -> Result<Vec<NodeRecord>> {
        let mut heads: Vec<(String, String, Option<String>, i64, i64, i64)> = Vec::new();
        {
            let mut stmt = self.tx.prepare(
                "SELECT id, node_type, state, rev, schema_version, sealed FROM nodes
                 WHERE node_type = ?1 AND (?2 IS NULL OR state = ?2)
                 ORDER BY id",
            )?;
            let rows = stmt.query_map(params![node_type, state], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
            })?;
            for row in rows {
                heads.push(row?);
            }
        }
        let mut out = Vec::with_capacity(heads.len());
        for (id, node_type, state, rev, schema_version, sealed) in heads {
            let fields = self.fields_of(&id)?;
            out.push(NodeRecord {
                id: NodeId(id),
                node_type,
                state,
                rev,
                schema_version: schema_version as u32,
                sealed: sealed != 0,
                fields,
            });
        }
        Ok(out)
    }

    /// Every node id this (overlay) snapshot holds.
    fn all_ids(&self) -> Result<BTreeSet<String>> {
        let mut stmt = self.tx.prepare("SELECT id FROM nodes")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.collect::<std::result::Result<_, _>>().map_err(Into::into)
    }

    /// Edges leaving `src`, optionally narrowed by kind.
    pub fn edges_from(&self, src: &NodeId, kind: Option<&str>) -> Result<Vec<EdgeRecord>> {
        let Some(o) = &self.overlay else {
            return self.edges_from_raw(src, kind);
        };
        let mut out = unite_edges(
            self.edges_from_raw(src, kind)?,
            o.edges_from_raw(src, kind)?,
            &o.tombstones()?,
        );
        out.sort_by(|a, b| {
            (a.kind.as_str(), a.dst.pool.as_str(), a.dst.id.0.as_str())
                .cmp(&(b.kind.as_str(), b.dst.pool.as_str(), b.dst.id.0.as_str()))
        });
        Ok(out)
    }

    fn edges_from_raw(&self, src: &NodeId, kind: Option<&str>) -> Result<Vec<EdgeRecord>> {
        let mut stmt = self.tx.prepare(
            "SELECT src, kind, dst_pool, dst, note, fields FROM edges
             WHERE src = ?1 AND (?2 IS NULL OR kind = ?2)
             ORDER BY kind, dst_pool, dst",
        )?;
        let rows = stmt.query_map(params![src.0, kind], edge_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Edges arriving at `dst` (as this pool records them), optionally
    /// narrowed by kind. The destination is pool-qualified because an edge
    /// stored HERE may point into another pool (e.g. `catalogues`) —
    /// `dst_pool` is always explicit, never null-means-local.
    pub fn edges_to(&self, dst: &QualifiedNodeId, kind: Option<&str>) -> Result<Vec<EdgeRecord>> {
        let Some(o) = &self.overlay else {
            return self.edges_to_raw(dst, kind);
        };
        let mut out = unite_edges(
            self.edges_to_raw(dst, kind)?,
            o.edges_to_raw(dst, kind)?,
            &o.tombstones()?,
        );
        out.sort_by(|a, b| (a.kind.as_str(), a.src.0.as_str()).cmp(&(b.kind.as_str(), b.src.0.as_str())));
        Ok(out)
    }

    fn edges_to_raw(&self, dst: &QualifiedNodeId, kind: Option<&str>) -> Result<Vec<EdgeRecord>> {
        let mut stmt = self.tx.prepare(
            "SELECT src, kind, dst_pool, dst, note, fields FROM edges
             WHERE dst_pool = ?1 AND dst = ?2 AND (?3 IS NULL OR kind = ?3)
             ORDER BY kind, src",
        )?;
        let rows = stmt.query_map(params![dst.pool.as_str(), dst.id.0, kind], edge_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// One machine-maintained count cell's value; `None` when never
    /// incremented (readers treat that as zero — the cell is sparse).
    pub fn count_cell(&self, node: &NodeId, cell: &str) -> Result<Option<i64>> {
        let committed = self.count_cell_raw(node, cell)?;
        let Some(o) = &self.overlay else {
            return Ok(committed);
        };
        match o.count_cell_raw(node, cell)? {
            None => Ok(committed),
            Some(staged) => {
                let base = o.cell_base(node, cell)?.unwrap_or(0);
                Ok(Some(committed.unwrap_or(0) + (staged - base)))
            }
        }
    }

    fn count_cell_raw(&self, node: &NodeId, cell: &str) -> Result<Option<i64>> {
        Ok(self
            .tx
            .query_row(
                "SELECT value FROM count_cells WHERE node_id = ?1 AND cell = ?2",
                params![node.0, cell],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Has this (transaction, part) already landed in this pool? The
    /// idempotence marker read on its own — what recovery consults to know a
    /// roll-forward is a no-op (recovery is idempotent). Always
    /// the committed pool's answer: a part applied to an overlay has landed
    /// nowhere.
    pub fn part_applied(&self, tx: &TxHash, part_ordinal: u32) -> Result<bool> {
        let hit: Option<i64> = self
            .tx
            .query_row(
                "SELECT 1 FROM applied_parts WHERE tx_hash = ?1 AND part = ?2",
                params![tx.to_hex(), part_ordinal],
                |r| r.get(0),
            )
            .optional()?;
        Ok(hit.is_some())
    }

    fn fields_of(&self, id: &str) -> Result<BTreeMap<String, FieldValue>> {
        let mut stmt = self
            .tx
            .prepare("SELECT field, value, rev FROM node_fields WHERE node_id = ?1 ORDER BY field")?;
        let rows = stmt.query_map(params![id], |r| {
            let field: String = r.get(0)?;
            let text: String = r.get(1)?;
            let rev: i64 = r.get(2)?;
            Ok((field, text, rev))
        })?;
        let mut out = BTreeMap::new();
        for row in rows {
            let (field, text, rev) = row?;
            let value: Value = serde_json::from_str(&text).map_err(|e| {
                SpineError::hard(format!("node `{id}` field `{field}` will not parse: {e}"))
            })?;
            out.insert(field, FieldValue { value, rev });
        }
        Ok(out)
    }

    // ---- the overlay's own side tables, read off an OVERLAY snapshot ----

    /// Every edge a staged child REMOVED from the committed pool, keyed
    /// `(src, kind, dst_pool, dst)` (removed edges are tombstones).
    fn tombstones(&self) -> Result<BTreeSet<(String, String, String, String)>> {
        let mut stmt = self
            .tx
            .prepare("SELECT src, kind, dst_pool, dst FROM overlay_tombstones")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
        rows.collect::<std::result::Result<_, _>>().map_err(Into::into)
    }

    /// The committed value a count cell held when its node was copied into
    /// the overlay — the subtrahend of the view's cell arithmetic.
    fn cell_base(&self, node: &NodeId, cell: &str) -> Result<Option<i64>> {
        Ok(self
            .tx
            .query_row(
                "SELECT base FROM overlay_cell_base WHERE node_id = ?1 AND cell = ?2",
                params![node.0, cell],
                |r| r.get(0),
            )
            .optional()?)
    }

    // ---- raw rows, for the overlay's copy-on-first-touch ----

    /// The node's whole `nodes` row plus its fields and cells, as stored —
    /// what the overlay copies in before a delta changes an existing node.
    fn node_rows_raw(&self, id: &NodeId) -> Result<Option<RawNode>> {
        let head: Option<(String, Option<String>, i64, i64, String, String, String, String, i64)> =
            self.tx
                .query_row(
                    "SELECT node_type, state, rev, schema_version, created_tx, updated_tx, created,
                            updated, sealed
                     FROM nodes WHERE id = ?1",
                    params![id.0],
                    |r| {
                        Ok((
                            r.get(0)?,
                            r.get(1)?,
                            r.get(2)?,
                            r.get(3)?,
                            r.get(4)?,
                            r.get(5)?,
                            r.get(6)?,
                            r.get(7)?,
                            r.get(8)?,
                        ))
                    },
                )
                .optional()?;
        let Some(head) = head else {
            return Ok(None);
        };
        let fields = {
            let mut stmt = self.tx.prepare(
                "SELECT field, value, rev, updated_tx FROM node_fields WHERE node_id = ?1",
            )?;
            let rows = stmt.query_map(params![id.0], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?;
            rows.collect::<std::result::Result<Vec<(String, String, i64, String)>, _>>()?
        };
        let cells = {
            let mut stmt = self
                .tx
                .prepare("SELECT cell, value FROM count_cells WHERE node_id = ?1")?;
            let rows = stmt.query_map(params![id.0], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect::<std::result::Result<Vec<(String, i64)>, _>>()?
        };
        Ok(Some(RawNode {
            head,
            fields,
            cells,
        }))
    }

    /// One edge's whole row, as stored, or `None`.
    fn edge_row_raw(&self, src: &NodeId, kind: &str, dst: &QualifiedNodeId) -> Result<Option<RawEdge>> {
        Ok(self
            .tx
            .query_row(
                "SELECT note, fields, created_tx, created FROM edges
                 WHERE src = ?1 AND kind = ?2 AND dst_pool = ?3 AND dst = ?4",
                params![src.0, kind, dst.pool.as_str(), dst.id.0],
                |r| {
                    Ok(RawEdge {
                        note: r.get(0)?,
                        fields: r.get(1)?,
                        created_tx: r.get(2)?,
                        created: r.get(3)?,
                    })
                },
            )
            .optional()?)
    }
}

/// A node's rows as stored — the copy-on-first-touch payload.
struct RawNode {
    /// `(node_type, state, rev, schema_version, created_tx, updated_tx,
    /// created, updated, sealed)`.
    head: (String, Option<String>, i64, i64, String, String, String, String, i64),
    /// `(field, value text, rev, updated_tx)`.
    fields: Vec<(String, String, i64, String)>,
    /// `(cell, value)`.
    cells: Vec<(String, i64)>,
}

struct RawEdge {
    note: Option<String>,
    fields: String,
    created_tx: String,
    created: String,
}

/// The view's edge union: the overlay's edges, plus every committed
/// edge the overlay neither holds nor tombstoned. The overlay wins on a
/// shared key, because a copied edge is the committed one and a re-added
/// edge is the staged one.
fn unite_edges(
    committed: Vec<EdgeRecord>,
    overlay: Vec<EdgeRecord>,
    tombstones: &BTreeSet<(String, String, String, String)>,
) -> Vec<EdgeRecord> {
    let key = |e: &EdgeRecord| {
        (
            e.src.0.clone(),
            e.kind.clone(),
            e.dst.pool.as_str().to_string(),
            e.dst.id.0.clone(),
        )
    };
    let held: BTreeSet<_> = overlay.iter().map(key).collect();
    let mut out = overlay;
    for e in committed {
        let k = key(&e);
        if !held.contains(&k) && !tombstones.contains(&k) {
            out.push(e);
        }
    }
    out
}

/// The side tables an overlay database carries beside the pool's own
/// (`POOL_DDL`): the removed-edge TOMBSTONES and each copied count cell's
/// committed BASE value.
const OVERLAY_DDL: &str = r#"
CREATE TABLE overlay_tombstones (
  src      TEXT NOT NULL,
  kind     TEXT NOT NULL,
  dst_pool TEXT NOT NULL,
  dst      TEXT NOT NULL,
  PRIMARY KEY (src, kind, dst_pool, dst)
) WITHOUT ROWID;
CREATE TABLE overlay_cell_base (
  node_id TEXT NOT NULL,
  cell    TEXT NOT NULL,
  base    INTEGER NOT NULL,
  PRIMARY KEY (node_id, cell)
) WITHOUT ROWID;
"#;

/// ONE POOL'S OVERLAY: an in-memory database with the
/// pool's own table definitions, holding only the rows the staged children
/// created or changed. A staged child's part is applied to it through
/// [`Pool::apply_part`]'s own routine (`ApplyTarget::Overlay`), so field
/// revisions, count cells and the `sealed` mark behave as they will on
/// landing. Before a delta changes an existing node, the node's committed
/// record is copied in (copy-on-first-touch); a removed committed edge is
/// copied in, deleted by the apply, and recorded as a tombstone. The grammar
/// has no node deletion, so no node tombstone exists.
///
/// Derived state: nothing of it is on disk. It is rebuilt from the
/// `tranche_children` rows (`crate::read::View::rebuild`) and dropped at
/// the tranche's terminal transition. It never touches a pool file —
/// `tests/tranche/staging.rs` g2 measures the files' bytes and mtimes to
/// say so.
pub struct Overlay {
    pool: Pool,
}

impl Overlay {
    /// A fresh, empty overlay for a pool of this grammar.
    pub(crate) fn new(grammar: &PoolGrammar) -> Result<Overlay> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(POOL_DDL)?;
        conn.execute_batch(OVERLAY_DDL)?;
        let json = serde_json::to_string(grammar)
            .map_err(|e| SpineError::hard(format!("grammar will not serialize: {e}")))?;
        conn.execute(
            "INSERT INTO pool_meta (singleton, pool, grammar, grammar_version, created)
             VALUES (1, ?1, ?2, ?3, strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
            params![grammar.pool.as_str(), json, grammar.version],
        )?;
        Ok(Overlay {
            pool: Pool {
                conn,
                name: grammar.pool.clone(),
                grammar: grammar.clone(),
            },
        })
    }

    /// A snapshot over the overlay alone (overlay-less) — what
    /// [`Pool::snapshot_over`] layers on top of the committed snapshot.
    fn snapshot(&self) -> Result<Snapshot<'_>> {
        self.pool.snapshot()
    }

    /// Apply one staged child's part to this overlay, reading
    /// the committed pool through `committed` for copy-on-first-touch.
    ///
    /// The copy runs BEFORE the apply, for every existing node the part
    /// writes (a SET's or SEAL's node; an edge's local endpoints) and every
    /// committed edge the part removes or re-creates — so the one landing
    /// routine finds the rows it expects, and an identical re-add stays the
    /// set no-op it is at landing (no cell bump). What was copied is also
    /// what the tranche holds a claim on, so the committed record cannot
    /// move after the copy.
    pub(crate) fn apply_child_part(
        &mut self,
        committed: &Snapshot<'_>,
        tx: &TxHash,
        part_ordinal: u32,
        part: &CompiledPart,
    ) -> Result<()> {
        let local = self.pool.name.clone();
        for delta in &part.deltas {
            match delta {
                Delta::CreateNode { .. } => {}
                Delta::SetField { node, .. } | Delta::Seal { node } => {
                    self.copy_node(committed, node)?;
                }
                Delta::AddEdge { src, kind, dst, .. } => {
                    self.copy_node(committed, src)?;
                    if dst.pool == local {
                        self.copy_node(committed, &dst.id)?;
                    }
                    // A re-add of a tombstoned edge is a real add again: the
                    // tombstone lifts and the apply inserts (and bumps), as
                    // the landing's remove-then-add would. A committed edge
                    // never removed is copied so the apply's `DO NOTHING`
                    // fires, exactly as at landing.
                    let lifted = self.pool.conn.execute(
                        "DELETE FROM overlay_tombstones
                         WHERE src = ?1 AND kind = ?2 AND dst_pool = ?3 AND dst = ?4",
                        params![src.0, kind, dst.pool.as_str(), dst.id.0],
                    )?;
                    if lifted == 0 {
                        self.copy_edge(committed, src, kind, dst)?;
                    }
                }
                Delta::RemoveEdge { src, kind, dst } => {
                    self.copy_node(committed, src)?;
                    if dst.pool == local {
                        self.copy_node(committed, &dst.id)?;
                    }
                    // Copied in so the apply's DELETE removes it (and bumps
                    // the cells down), then tombstoned so the committed edge
                    // is hidden from the view.
                    if self.copy_edge(committed, src, kind, dst)? {
                        self.pool.conn.execute(
                            "INSERT OR IGNORE INTO overlay_tombstones (src, kind, dst_pool, dst)
                             VALUES (?1, ?2, ?3, ?4)",
                            params![src.0, kind, dst.pool.as_str(), dst.id.0],
                        )?;
                    }
                }
            }
        }
        self.pool
            .apply_part_interior(tx, part_ordinal, part, ApplyTarget::Overlay)?;
        Ok(())
    }

    /// Copy a committed node's rows into the overlay, once. A node the
    /// overlay already holds (created by an earlier child, or copied) is
    /// left as it is; a node the committed pool does not hold copies nothing
    /// (the apply will refuse it by name, as the landing would).
    fn copy_node(&mut self, committed: &Snapshot<'_>, id: &NodeId) -> Result<()> {
        let held: Option<i64> = self
            .pool
            .conn
            .query_row("SELECT 1 FROM nodes WHERE id = ?1", params![id.0], |r| r.get(0))
            .optional()?;
        if held.is_some() {
            return Ok(());
        }
        let Some(raw) = committed.node_rows_raw(id)? else {
            return Ok(());
        };
        let (node_type, state, rev, schema_version, created_tx, updated_tx, created, updated, sealed) =
            raw.head;
        let sql = self.pool.conn.transaction()?;
        sql.execute(
            "INSERT INTO nodes (id, node_type, state, rev, schema_version, created_tx, updated_tx,
                                created, updated, sealed)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![id.0, node_type, state, rev, schema_version, created_tx, updated_tx, created, updated, sealed],
        )?;
        for (field, value, rev, updated_tx) in raw.fields {
            sql.execute(
                "INSERT INTO node_fields (node_id, field, value, rev, updated_tx) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![id.0, field, value, rev, updated_tx],
            )?;
        }
        for (cell, value) in raw.cells {
            sql.execute(
                "INSERT INTO count_cells (node_id, cell, value) VALUES (?1, ?2, ?3)",
                params![id.0, cell, value],
            )?;
            sql.execute(
                "INSERT INTO overlay_cell_base (node_id, cell, base) VALUES (?1, ?2, ?3)",
                params![id.0, cell, value],
            )?;
        }
        sql.commit()?;
        Ok(())
    }

    /// Copy a committed edge into the overlay, once. `true` when the
    /// committed pool holds the edge (copied now, or already present in the
    /// overlay); `false` when there is no such committed edge.
    fn copy_edge(
        &mut self,
        committed: &Snapshot<'_>,
        src: &NodeId,
        kind: &str,
        dst: &QualifiedNodeId,
    ) -> Result<bool> {
        let Some(raw) = committed.edge_row_raw(src, kind, dst)? else {
            return Ok(false);
        };
        self.pool.conn.execute(
            "INSERT INTO edges (src, kind, dst_pool, dst, note, fields, created_tx, created)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT (src, kind, dst_pool, dst) DO NOTHING",
            params![src.0, kind, dst.pool.as_str(), dst.id.0, raw.note, raw.fields, raw.created_tx, raw.created],
        )?;
        Ok(true)
    }
}

fn edge_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<EdgeRecord> {
    Ok(EdgeRecord {
        src: NodeId(r.get(0)?),
        kind: r.get(1)?,
        dst: QualifiedNodeId {
            pool: PoolName::new(r.get::<_, String>(2)?),
            id: NodeId(r.get(3)?),
        },
        note: r.get(4)?,
        fields: {
            let text: String = r.get(5)?;
            match serde_json::from_str::<Value>(&text) {
                Ok(Value::Object(m)) => m.into_iter().collect(),
                Ok(other) => {
                    return Err(rusqlite::Error::FromSqlConversionFailure(
                        5,
                        rusqlite::types::Type::Text,
                        format!("edge fields hold `{other}`, not an object").into(),
                    ))
                }
                Err(e) => {
                    return Err(rusqlite::Error::FromSqlConversionFailure(
                        5,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    ))
                }
            }
        },
    })
}

/// One node as read: identity, type, state, whole-node revision, the
/// machine-owned schema-version stamp, and every field with
/// its per-field revision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeRecord {
    pub id: NodeId,
    pub node_type: String,
    pub state: Option<String>,
    pub rev: i64,
    /// Machine-owned; no pen, agent or human, ever sets it directly
    /// (SETTLED).
    pub schema_version: u32,
    /// The seal flag: machine-owned, set only by `SEAL`, never
    /// cleared. Serde default so a record serialized before it parses as
    /// unsealed. Like the stamp, it is in no default render; it is read by
    /// field path (`RETURN b.sealed`).
    #[serde(default)]
    pub sealed: bool,
    pub fields: BTreeMap<String, FieldValue>,
}

impl NodeRecord {
    /// The DEFAULT render (SETTLED): the machine-owned
    /// schema-version stamp appears in NO default render — not in ordinary
    /// reads, not in context payloads. Identity, type, state, whole-node
    /// revision and field VALUES; per-field revisions are bookkeeping the
    /// language exposes through `REV(b.f)`, not render material.
    ///
    /// (The `NodeRecord` itself is deliberately complete — the stamp is
    /// frozen onto it and render policy is left to the reader. This is that
    /// policy, given one home in the storage layer so every reader inherits
    /// it rather than each reimplementing invisibility.)
    pub fn render_default(&self) -> Value {
        let mut m = serde_json::Map::new();
        m.insert("id".into(), Value::String(self.id.0.clone()));
        m.insert("type".into(), Value::String(self.node_type.clone()));
        if let Some(s) = &self.state {
            m.insert("state".into(), Value::String(s.clone()));
        }
        m.insert("rev".into(), Value::from(self.rev));
        let fields: serde_json::Map<String, Value> = self
            .fields
            .iter()
            .map(|(k, v)| (k.clone(), v.value.clone()))
            .collect();
        m.insert("fields".into(), Value::Object(fields));
        Value::Object(m)
    }

    /// The explicit-ask render (the stamp becomes visible exactly when it
    /// matters — on explicit ask; when a node's stamp differs from its pool's
    /// pin; and to migration tooling). The ONLY way the stamp reaches
    /// a render.
    pub fn render_with_stamp(&self) -> Value {
        let mut v = self.render_default();
        v.as_object_mut()
            .expect("render_default builds an object")
            .insert("schema_version".into(), Value::from(self.schema_version));
        v
    }

    /// Does this node's stamp differ from its pool's current pin? Skew is
    /// exactly when the stamp carries information.
    pub fn stamp_skewed(&self, grammar: &PoolGrammar) -> bool {
        self.schema_version != grammar.version
    }
}

/// One field's value and its per-field revision (the compare-and-swap
/// bookkeeping).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldValue {
    pub value: serde_json::Value,
    pub rev: i64,
}

/// One edge as read — its own record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EdgeRecord {
    pub src: NodeId,
    pub kind: String,
    pub dst: QualifiedNodeId,
    pub note: Option<String>,
    /// The edge's declared fields as STORED — the first create's,
    /// since identity is the triple. Empty for a kind declaring none.
    pub fields: BTreeMap<String, Value>,
}
