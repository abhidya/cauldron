//! **Erasure**: the one write beside the door, for taking a value out of a
//! cauldron's record for good.
//!
//! Every other write goes through [`Cauldron::submit`], and the record of a
//! commit (its row in `spine.db`: statement, bindings, compiled payload,
//! result) is kept forever. That is the right default and the wrong one for
//! a secret someone put in by mistake: after the node that held it has been
//! changed by an ordinary commit, the text is still in the row that created
//! it, in pages SQLite freed but did not overwrite, and in the write-ahead
//! log. Erasure is the way out, and it is deliberately narrow:
//!
//! - [`Cauldron::widen_states`] re-pins a pool's grammar to the next version
//!   when that version only ADDS states to node types (nothing else may
//!   differ), so a node can be moved by an ordinary commit to a state its
//!   original grammar did not have (a dump's `removed`).
//! - [`Cauldron::erase`] rewrites the rows of `spine.db`'s `transactions`
//!   that the caller does not name as kept: whole values equal to one the
//!   caller names are replaced, then every string equal to one of the
//!   caller's needles, or holding one at least [`SUBSTRING_MIN`] bytes long,
//!   has it replaced; it then checks that no such row holds a long needle,
//!   and compacts every database file of the cauldron (WAL
//!   checkpointed and truncated, `VACUUM`, checkpointed again) with
//!   `secure_delete` on and temporary storage in memory, so no freed page and
//!   no log frame keeps the old bytes in the directory.
//!
//! Both consume the handle: the caller holds the cauldron's writer lock (the
//! spine itself has none), and nothing may write while they run. A reader
//! holding the log open makes the compaction busy, which is retried for a
//! few seconds and then reported as an error; running it again finishes it.
//!
//! The hash of a rewritten row is kept: it is the row's identity (other rows
//! and the pools name it), and it is a digest of the whole write, not its
//! text. The rewritten row no longer hashes to it, which nothing re-checks
//! for a cleared row; recovery re-checks only a stranded row, and
//! [`Cauldron::erase`] refuses while any row is landing.

use crate::error::{Result, SpineError};
use crate::pool::Pool;
use crate::schema_def::{PoolGrammar, PoolName};
use crate::spine_db::SPINE_FILE;
use crate::Cauldron;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::Path;

/// What [`Cauldron::erase`] takes out, and from where.
#[derive(Debug, Clone, Default)]
pub struct Erasure {
    /// Transaction hashes whose rows are left as they are (the writes of
    /// nodes that still hold the same values, e.g. another dump of the same
    /// bytes).
    pub keep: BTreeSet<String>,
    /// `(old, new)`: wherever a row's bindings, payload or result holds a
    /// value equal to `old` (at any depth), it becomes `new`.
    pub replace: Vec<(Value, Value)>,
    /// Text no row outside `keep` may hold afterwards. A string value equal
    /// to one becomes [`Erasure::with`]; a needle of at least
    /// [`SUBSTRING_MIN`] bytes is also replaced inside every string that
    /// holds it. A shorter one is never cut out of a longer string, where it
    /// could as well be part of a field name, a type or a state the record
    /// needs. Keys are never rewritten. Empty needles are ignored.
    pub needles: Vec<String>,
    /// What a needle is replaced by.
    pub with: String,
}

/// The shortest needle cut out of a longer string (see [`Erasure::needles`]).
pub const SUBSTRING_MIN: usize = 8;

/// What [`Cauldron::erase`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Erased {
    /// How many transaction rows were rewritten.
    pub rows: u64,
    /// The database files compacted, by name.
    pub files: Vec<String>,
}

impl Cauldron {
    /// **Re-pin `pool`'s grammar to `to`**, which must be the pinned grammar
    /// with its version one higher and states added to node types, and
    /// nothing else changed. Refused while any commit is landing in the
    /// pool (a stranded part would be judged under the wrong pin). A pool
    /// already pinned at `to` is left as it is. Answers a fresh handle.
    pub fn widen_states(self, pool: &PoolName, to: &PoolGrammar) -> Result<Cauldron> {
        let Some(from) = self.grammar(pool).cloned() else {
            return Err(SpineError::not_found(format!("no pool `{pool}` in this cauldron")));
        };
        if &from == to {
            return Ok(self);
        }
        let mut narrowed = to.clone();
        narrowed.version = from.version;
        for (name, t) in narrowed.node_types.iter_mut() {
            if let Some(old) = from.node_types.get(name) {
                if old.states.iter().all(|s| t.states.contains(s)) {
                    t.states = old.states.clone();
                }
            }
        }
        if to.version != from.version + 1 || narrowed != from {
            return Err(SpineError::usage(format!(
                "pool `{pool}` is pinned at v{}; `widen_states` moves a pin only to the next version, and only by adding \
                 states (the grammar offered is v{} and differs otherwise)",
                from.version, to.version
            )));
        }
        to.check_wellformed()?;
        if !self.in_landing(Some(pool))?.is_empty() {
            return Err(SpineError::usage(format!(
                "pool `{pool}` has a commit landing; its pin is not moved until that commit has finished"
            )));
        }
        let json = serde_json::to_string(to).map_err(|e| SpineError::hard(format!("grammar will not serialize: {e}")))?;
        let conn = connection(&Pool::path_in(&self.root, pool))?;
        let n = conn.execute(
            "UPDATE pool_meta SET grammar = ?1, grammar_version = ?2 WHERE singleton = 1 AND grammar_version = ?3",
            params![json, to.version, from.version],
        )?;
        if n != 1 {
            return Err(SpineError::hard(format!(
                "pool `{pool}`'s pin moved while it was being widened; nothing was changed"
            )));
        }
        drop(conn);
        Cauldron::open(&self.root, self.config.clone())
    }

    /// **Erase** what `e` names from every transaction row outside
    /// `e.keep`, check that no such row still holds a needle, and compact
    /// every database file of the cauldron (module docs). Refused while any
    /// commit is landing.
    pub fn erase(self, e: &Erasure) -> Result<Erased> {
        self.erase_with(e, &|_| {})
    }

    /// [`Cauldron::erase`], calling `at` with the name of each step as it
    /// is passed, so a caller can stop between them (its own test seam):
    /// `history-erased` once the rows are rewritten and committed, then
    /// for each database file `checkpointed:<file>` (before its `VACUUM`),
    /// `vacuumed:<file>` (after it) and `compacted:<file>` (after the last
    /// checkpoint). Every step is safe to stop after: running the erasure
    /// again repeats what is left.
    pub fn erase_with(self, e: &Erasure, at: &dyn Fn(&str)) -> Result<Erased> {
        if !self.in_landing(None)?.is_empty() {
            return Err(SpineError::usage("a commit is landing; nothing is erased until it has finished"));
        }
        let needles: Vec<&str> = e.needles.iter().map(String::as_str).filter(|n| !n.is_empty()).collect();
        let mut rows = 0u64;
        {
            let conn = connection(&self.root.join(SPINE_FILE))?;
            let tx = conn.unchecked_transaction()?;
            let all: Vec<(i64, String, String, String, Option<String>)> = {
                let mut s = tx.prepare("SELECT seq, hash, bindings, payload, result FROM transactions")?;
                let v = s
                    .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
                    .collect::<std::result::Result<_, _>>()?;
                v
            };
            for (seq, hash, bindings, payload, result) in all {
                if e.keep.contains(&hash) {
                    continue;
                }
                let b2 = scrub_text(&bindings, e, &needles)?;
                let p2 = scrub_text(&payload, e, &needles)?;
                let r2 = result.as_deref().map(|r| scrub_text(r, e, &needles)).transpose()?;
                if b2 != bindings || p2 != payload || r2 != result {
                    tx.execute(
                        "UPDATE transactions SET bindings = ?1, payload = ?2, result = ?3 WHERE seq = ?4",
                        params![b2, p2, r2, seq],
                    )?;
                    rows += 1;
                }
            }
            // The check: no row outside `keep` holds a long needle in a column
            // erasure rewrites (as written, or as JSON escapes it).
            let escaped: Vec<String> = needles
                .iter()
                .filter(|n| n.len() >= SUBSTRING_MIN)
                .flat_map(|n| {
                    let j = serde_json::to_string(n).unwrap_or_default();
                    [n.to_string(), j.trim_matches('"').to_string()]
                })
                .collect();
            for n in &escaped {
                let mut s = tx.prepare(
                    "SELECT hash FROM transactions WHERE instr(bindings, ?1) > 0 OR instr(payload, ?1) > 0 \
                     OR instr(coalesce(result, ''), ?1) > 0",
                )?;
                let hits: Vec<String> = s.query_map([n], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?;
                if hits.iter().any(|h| !e.keep.contains(h)) {
                    return Err(SpineError::hard(
                        "a transaction row still holds text being erased after the rewrite; nothing was changed",
                    ));
                }
            }
            tx.commit()?;
        }
        at("history-erased");
        let files = compact(&self.root, at)?;
        Ok(Erased { rows, files })
    }
}

/// One connection for erasure: `secure_delete` (a freed page is zeroed) and
/// temporary storage in memory (a `VACUUM` writes no copy elsewhere).
fn connection(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    conn.busy_timeout(std::time::Duration::from_millis(5000))?;
    conn.pragma_update(None, "secure_delete", "ON")?;
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    Ok(conn)
}

fn scrub_text(text: &str, e: &Erasure, needles: &[&str]) -> Result<String> {
    let mut v: Value = serde_json::from_str(text).map_err(|err| SpineError::hard(format!("a stored row will not parse: {err}")))?;
    scrub(&mut v, e, needles);
    let out = serde_json::to_string(&v).map_err(|err| SpineError::hard(format!("a rewritten row will not serialize: {err}")))?;
    // Unchanged rows keep their stored text byte for byte.
    if serde_json::from_str::<Value>(text).ok().as_ref() == Some(&v) {
        return Ok(text.to_string());
    }
    Ok(out)
}

fn scrub(v: &mut Value, e: &Erasure, needles: &[&str]) {
    if let Some((_, new)) = e.replace.iter().find(|(old, _)| old == v) {
        *v = new.clone();
        return;
    }
    match v {
        Value::String(s) => {
            if needles.contains(&s.as_str()) {
                *s = e.with.clone();
                return;
            }
            for n in needles.iter().filter(|n| n.len() >= SUBSTRING_MIN) {
                if s.contains(n) {
                    *s = s.replace(n, &e.with);
                }
            }
        }
        Value::Array(a) => a.iter_mut().for_each(|x| scrub(x, e, needles)),
        Value::Object(m) => m.values_mut().for_each(|x| scrub(x, e, needles)),
        _ => {}
    }
}

/// Compact every database file in `dir`: checkpoint and truncate the
/// write-ahead log, `VACUUM`, and checkpoint again. A busy checkpoint (a
/// reader holds the log) is retried for a few seconds, then reported.
pub(crate) fn compact(dir: &Path, at: &dyn Fn(&str)) -> Result<Vec<String>> {
    let mut files = vec![];
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| SpineError::hard(format!("reading `{}`: {e}", dir.display())))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("db"))
        .collect();
    entries.sort();
    for path in entries {
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let conn = connection(&path)?;
        checkpoint(&conn, &path)?;
        at(&format!("checkpointed:{name}"));
        conn.execute_batch("VACUUM")?;
        at(&format!("vacuumed:{name}"));
        checkpoint(&conn, &path)?;
        conn.close().map_err(|(_, e)| SpineError::from(e))?;
        at(&format!("compacted:{name}"));
        files.push(name);
    }
    Ok(files)
}

fn checkpoint(conn: &Connection, path: &Path) -> Result<()> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let busy: Option<i64> =
            conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| r.get(0)).optional()?;
        if busy.unwrap_or(0) == 0 {
            return Ok(());
        }
        if std::time::Instant::now() > deadline {
            return Err(SpineError::hard(format!(
                "`{}`'s log could not be emptied: a reader holds it open; run this again when nothing is reading",
                path.display()
            )));
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SpineConfig;
    use crate::grammar_dump::{dumps_v1, dumps_v2, POOL};
    use crate::schema_def::{FieldDef, FieldKind};

    fn scratch(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "spine-erase-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ))
    }

    #[test]
    fn a_pin_widens_only_by_added_states_and_only_one_version() {
        let dir = scratch("widen");
        let pool = PoolName::new(POOL);
        let c = Cauldron::create(&dir, &[dumps_v1(POOL)], SpineConfig::default()).unwrap();
        // A changed field is not a widening.
        let mut other = dumps_v2(POOL);
        other.node_types.get_mut("dump").unwrap().fields.insert("x".into(), FieldDef { kind: FieldKind::Text, ..FieldDef::optional() });
        assert!(matches!(c.clone().widen_states(&pool, &other), Err(SpineError::Usage { .. })));
        // Two versions at once is not either.
        let mut skip = dumps_v2(POOL);
        skip.version = 3;
        assert!(matches!(c.clone().widen_states(&pool, &skip), Err(SpineError::Usage { .. })));
        let c = c.widen_states(&pool, &dumps_v2(POOL)).unwrap();
        assert_eq!(c.grammar(&pool).unwrap().version, 2);
        // Re-opened, the pin is the new one; widening to it again is a no-op.
        let c = Cauldron::open(&dir, SpineConfig::default()).unwrap();
        assert_eq!(c.grammar(&pool).unwrap(), &dumps_v2(POOL));
        assert!(c.widen_states(&pool, &dumps_v2(POOL)).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scrubbing_replaces_whole_values_then_needles_in_strings_and_keys() {
        let e = Erasure {
            keep: BTreeSet::new(),
            replace: vec![(serde_json::json!({"t": "s3cret"}), serde_json::json!({}))],
            needles: vec!["s3cret-long".into(), "dump".into()],
            with: "[x]".into(),
        };
        let n = ["s3cret-long", "dump"];
        let mut v = serde_json::json!({"a": {"t": "s3cret"}, "b": ["one s3cret-long two", "dump", "dumps/dump"], "s3cret-long": 1, "n": 2});
        scrub(&mut v, &e, &n);
        // Whole values; a long needle inside strings; a short one only where
        // it is the whole string; keys never.
        assert_eq!(v, serde_json::json!({"a": {}, "b": ["one [x] two", "[x]", "dumps/dump"], "s3cret-long": 1, "n": 2}));
        // Text that holds nothing to erase is kept byte for byte.
        let t = r#"{ "n":  1 }"#;
        assert_eq!(scrub_text(t, &e, &n).unwrap(), t);
    }
}
