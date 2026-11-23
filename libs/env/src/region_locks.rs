//! **Region locks**: the rows a writer holds on the regions of a cauldron's
//! soup while it prepares a write it will make later (a ripple, from the
//! moment it reads the regions until its records land or it gives up).
//!
//! # Where they live
//!
//! One file in the soup, `<soup>/region-locks.json` (mode 0600), a table of
//! rows. Each row is one region of one pool, held `write` (the region the
//! holder will write into: no one else writes into it, or into a region
//! below it) or `link` (an ancestor of such a region: others may still write
//! beside it, but not change its header or re-parent it), with the holder
//! (`{kind: "ripple", run, dump}`), what it is for, since when and until
//! when. The file is read by anyone who may read the soup, and written only
//! by the process holding the soup's writer lock (the `mcp` service's writer
//! for the cauldron, or a `cauldron soup import` in its place), as a whole
//! file replaced atomically.
//!
//! # Rules every reader applies
//!
//! - A row past its `until` is not held, whatever the file says: no lock
//!   outlives its expiry. Renewing moves `until` forward only for a row
//!   still held; an expired row is never revived.
//! - No row is held more than [`MAX_AHEAD_MS`] (900 s, the longest time to
//!   live a writer may ask for) ahead of now. A row whose `until` lies
//!   further out (a hand edit, or a lock taken while the clock was set
//!   ahead and since stepped back) is read as expiring [`MAX_AHEAD_MS`]
//!   after this process first saw it so, and that is logged once
//!   ([`Table::clamp`]); the writer that holds the soup's writer lock then
//!   writes the row back clamped. Writing such a row is refused ([`save`]).
//!   A clock stepped back therefore stretches a lock by at most 900 s; a
//!   clock stepped forward expires locks early, which the head-revision
//!   check at the put catches, and a renewal then finds the row gone
//!   (`renewed: 0`) and never revives it.
//! - Two rows on one region of one pool conflict when either is `write`,
//!   unless both are the same holder's.
//! - A row whose run has landed is not held either (the writer releases it
//!   in the same act that lands the run; a crash between the two leaves a
//!   row the store already knows is done, [`Table::sweep`]).
//!
//! The rows a WRITE names (which regions it writes into, and how) are the
//! spine's (`spine::regions`); this module holds the table and its rules.

use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::Cauldron;

/// The file, in the soup.
pub const FILE: &str = "region-locks.json";
/// The table's format.
pub const FORMAT: u32 = 1;
/// `write`: the region itself.
pub const WRITE: &str = "write";
/// `link`: an ancestor of a region held `write`.
pub const LINK: &str = "link";
/// The furthest ahead of now any row is held: 900 s.
pub const MAX_AHEAD_MS: i64 = 900_000;

/// Where a cauldron's table lives.
pub fn path(c: &Cauldron) -> PathBuf {
    c.soup().join(FILE)
}

/// Who holds a row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Holder {
    /// `ripple`.
    pub kind: String,
    /// The run id.
    pub run: String,
    /// The dump the run is drawing from, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dump: Option<String>,
}

/// One row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Row {
    pub pool: String,
    pub region: String,
    /// [`WRITE`] or [`LINK`].
    pub mode: String,
    pub holder: Holder,
    pub purpose: String,
    /// RFC 3339, UTC.
    pub since: String,
    pub until: String,
    /// The same two instants in milliseconds since the epoch: what the rules
    /// compare.
    pub since_ms: i64,
    pub until_ms: i64,
}

impl Row {
    pub fn live(&self, now_ms: i64) -> bool {
        self.until_ms > now_ms
    }
}

/// The table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Table {
    pub format: u32,
    pub rows: Vec<Row>,
    /// The rows [`Table::clamp`] cut back when this table was read, as
    /// sentences; never written. A writer holding the soup's writer lock
    /// that reads a table with any saves it, so the cut is kept.
    #[serde(skip)]
    pub clamped: Vec<String>,
}

impl Default for Table {
    fn default() -> Table {
        Table { format: FORMAT, rows: vec![], clamped: vec![] }
    }
}

/// When this process first saw each row that ran too far ahead: keyed by
/// the table's path and the row as written.
static FIRST_SEEN: std::sync::Mutex<Option<BTreeMap<String, i64>>> = std::sync::Mutex::new(None);

/// Now, in milliseconds since the epoch.
pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// An instant as RFC 3339 UTC, to the second.
pub fn stamp(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms).map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)).unwrap_or_default()
}

/// What a writer asks to hold, or a write names: `(pool, region, mode)`.
pub type Want = (String, String, String);

fn conflicts_with(row: &Row, pool: &str, region: &str, mode: &str) -> bool {
    row.pool == pool && row.region == region && (row.mode == WRITE || mode == WRITE)
}

impl Table {
    /// **Cut back every row held more than [`MAX_AHEAD_MS`] ahead of
    /// `now_ms`** (module docs): such a row is read as expiring
    /// [`MAX_AHEAD_MS`] after this process first saw it so (`at`, the
    /// table's path, tells tables apart), and each is logged once. Answers
    /// how many were cut, and lists them in [`Table::clamped`].
    pub fn clamp(&mut self, at: &str, now_ms: i64) -> usize {
        let mut seen = FIRST_SEEN.lock().unwrap_or_else(|p| p.into_inner());
        let seen = seen.get_or_insert_with(BTreeMap::new);
        let mut n = 0;
        for r in self.rows.iter_mut().filter(|r| r.until_ms - now_ms > MAX_AHEAD_MS) {
            let key = format!("{at}\u{0}{}\u{0}{}\u{0}{}\u{0}{}\u{0}{}", r.pool, r.region, r.mode, r.holder.run, r.until_ms);
            let fresh = !seen.contains_key(&key);
            let first = *seen.entry(key).or_insert(now_ms);
            let until = (first + MAX_AHEAD_MS).min(r.until_ms);
            let said = format!(
                "the region lock on `{}` (run {}) said it runs until {}, more than {} s ahead of now ({}); it is held only until {}",
                r.region,
                short(&r.holder.run),
                r.until,
                MAX_AHEAD_MS / 1000,
                stamp(now_ms),
                stamp(until)
            );
            if fresh {
                eprintln!("cauldron: {said} (in {at})");
            }
            r.until_ms = until;
            r.until = stamp(until);
            self.clamped.push(said);
            n += 1;
        }
        n
    }

    /// The rows still held at `now_ms`.
    pub fn live(&self, now_ms: i64) -> Vec<&Row> {
        self.rows.iter().filter(|r| r.live(now_ms)).collect()
    }

    /// The rows of other holders than `run` (every holder when `None`) that
    /// conflict with `wanted` at `now_ms`, each once.
    pub fn conflicts(&self, now_ms: i64, run: Option<&str>, wanted: &[Want]) -> Vec<Row> {
        let mut out: Vec<Row> = Vec::new();
        for r in self.live(now_ms) {
            if run == Some(r.holder.run.as_str()) {
                continue;
            }
            if wanted.iter().any(|(p, g, m)| conflicts_with(r, p, g, m)) && !out.contains(r) {
                out.push(r.clone());
            }
        }
        out
    }

    /// **Hold** `write` on each of `regions` of `pool` and `link` on each of
    /// `above` for `holder`, for `ttl_ms` from `now_ms`, replacing every row
    /// it held before. Refused, changing nothing, with the conflicting rows.
    /// Expired rows are dropped first.
    pub fn acquire(&mut self, holder: &Holder, purpose: &str, pool: &str, regions: &[String], above: &[String], now_ms: i64, ttl_ms: i64) -> std::result::Result<(), Vec<Row>> {
        let mut wanted: Vec<Want> = regions.iter().map(|r| (pool.to_string(), r.clone(), WRITE.to_string())).collect();
        wanted.extend(above.iter().filter(|r| !regions.contains(r)).map(|r| (pool.to_string(), r.clone(), LINK.to_string())));
        let refused = self.conflicts(now_ms, Some(&holder.run), &wanted);
        if !refused.is_empty() {
            return Err(refused);
        }
        self.rows.retain(|r| r.live(now_ms) && r.holder.run != holder.run);
        let until = now_ms + ttl_ms.clamp(1, MAX_AHEAD_MS);
        for (p, region, mode) in wanted {
            self.rows.push(Row {
                pool: p,
                region,
                mode,
                holder: holder.clone(),
                purpose: purpose.to_string(),
                since: stamp(now_ms),
                until: stamp(until),
                since_ms: now_ms,
                until_ms: until,
            });
        }
        Ok(())
    }

    /// **Renew** `run`'s rows still held at `now_ms` to `ttl_ms` from now (an
    /// expired row is never revived). How many were renewed.
    pub fn renew(&mut self, run: &str, now_ms: i64, ttl_ms: i64) -> usize {
        let until = now_ms + ttl_ms.clamp(1, MAX_AHEAD_MS);
        let mut n = 0;
        for r in self.rows.iter_mut().filter(|r| r.holder.run == run && r.live(now_ms)) {
            if until > r.until_ms {
                r.until_ms = until;
                r.until = stamp(until);
            }
            n += 1;
        }
        n
    }

    /// **Release** every row of `run`; how many there were (held or not).
    pub fn release(&mut self, run: &str) -> usize {
        let before = self.rows.len();
        self.rows.retain(|r| r.holder.run != run);
        before - self.rows.len()
    }

    /// Drop every row past its expiry and every row of a run `landed` says
    /// has landed; how many were dropped.
    pub fn sweep(&mut self, now_ms: i64, landed: &dyn Fn(&str) -> bool) -> usize {
        let before = self.rows.len();
        let mut done: BTreeMap<String, bool> = BTreeMap::new();
        self.rows.retain(|r| {
            if !r.live(now_ms) {
                return false;
            }
            !*done.entry(r.holder.run.clone()).or_insert_with(|| landed(&r.holder.run))
        });
        before - self.rows.len()
    }
}

/// **Read** a cauldron's table. No file is an empty table; a file that
/// cannot be read or parsed is an error (a reader never guesses it empty).
/// Every row is cut back to at most [`MAX_AHEAD_MS`] ahead of now
/// ([`Table::clamp`]).
pub fn load(c: &Cauldron) -> Result<Table> {
    load_at(&path(c))
}

/// [`load`], by path.
pub fn load_at(p: &Path) -> Result<Table> {
    match std::fs::read_to_string(p) {
        Ok(text) => {
            let mut t: Table = serde_json::from_str(&text).with_context(|| format!("the region-lock table {} is not readable JSON", p.display()))?;
            if t.format != FORMAT {
                anyhow::bail!("the region-lock table {} is format {}, and this build reads format {FORMAT}", p.display(), t.format);
            }
            t.clamp(&p.display().to_string(), now_ms());
            Ok(t)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Table::default()),
        Err(e) => Err(anyhow::Error::new(e).context(format!("reading the region-lock table {}", p.display()))),
    }
}

/// **Write** a cauldron's table whole (a temporary file, fsynced, renamed
/// over it, the directory fsynced), mode 0600. Only the holder of the soup's
/// writer lock calls this. An empty table leaves an empty file, never none,
/// once one was written.
pub fn save(c: &Cauldron, t: &Table) -> Result<()> {
    let p = path(c);
    let now = now_ms();
    if let Some(r) = t.rows.iter().find(|r| r.until_ms - now > MAX_AHEAD_MS) {
        anyhow::bail!(
            "refused to write the region-lock table {}: the row on `{}` (run {}) runs until {}, more than {} s ahead of now ({}); nothing was written",
            p.display(),
            r.region,
            short(&r.holder.run),
            r.until,
            MAX_AHEAD_MS / 1000,
            stamp(now)
        );
    }
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let tmp = p.with_file_name(format!(".{FILE}.{}.{}.tmp", std::process::id(), SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
    let text = serde_json::to_string_pretty(t)?;
    let written = (|| -> std::io::Result<()> {
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(crate::private::FILE_MODE).open(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(anyhow::Error::new(e).context(format!("writing {}", tmp.display())));
    }
    if let Err(e) = std::fs::rename(&tmp, &p) {
        let _ = std::fs::remove_file(&tmp);
        return Err(anyhow::Error::new(e).context(format!("renaming {} over {}", tmp.display(), p.display())));
    }
    if let Some(dir) = p.parent() {
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
    }
    Ok(())
}

/// Seconds from `now_ms` until the last of `rows` expires, at least 1.
pub fn retry_after_secs(rows: &[Row], now_ms: i64) -> i64 {
    let until = rows.iter().map(|r| r.until_ms).max().unwrap_or(now_ms);
    ((until - now_ms).max(0) + 999) / 1000
}

/// **The held rows, by holder**, as status shows them: `[{holder, purpose,
/// since, until, regions: [held write], above: [held link]}]`.
pub fn view(t: &Table, now_ms: i64) -> Value {
    let mut by: BTreeMap<String, (Row, Vec<String>, Vec<String>)> = BTreeMap::new();
    for r in t.live(now_ms) {
        let e = by.entry(r.holder.run.clone()).or_insert_with(|| (r.clone(), vec![], vec![]));
        if r.mode == WRITE {
            e.1.push(r.region.clone());
        } else {
            e.2.push(r.region.clone());
        }
        if r.until_ms > e.0.until_ms {
            e.0.until_ms = r.until_ms;
            e.0.until = r.until.clone();
        }
    }
    Value::Array(
        by.into_values()
            .map(|(r, w, l)| {
                json!({
                    "holder": r.holder, "purpose": r.purpose, "since": r.since, "until": r.until,
                    "expires_in_secs": ((r.until_ms - now_ms).max(0) + 999) / 1000,
                    "regions": w, "above": l,
                })
            })
            .collect(),
    )
}

/// The held rows of a cauldron, as [`view`] shows them; a table that cannot
/// be read is `{"error": ...}` rather than nothing.
pub fn view_of(c: &Cauldron) -> Value {
    match load(c) {
        Ok(t) => view(&t, now_ms()),
        Err(e) => json!({"error": format!("{e:#}")}),
    }
}

/// **Every cauldron of an environment that holds a region now** (of
/// `only` when given), as status shows them: `[{cauldron, locks: [...]}]`
/// ([`view`]); a cauldron whose table cannot be read is listed with its
/// error.
pub fn env_view(e: &crate::Environment, only: Option<&str>) -> Value {
    let now = now_ms();
    let mut out = Vec::new();
    for c in e.cauldrons().unwrap_or_default() {
        if only.is_some_and(|o| o != c.name) {
            continue;
        }
        match load(&c) {
            Ok(t) => {
                let v = view(&t, now);
                if v.as_array().is_some_and(|a| !a.is_empty()) {
                    out.push(json!({"cauldron": c.name, "locks": v}));
                }
            }
            Err(err) => out.push(json!({"cauldron": c.name, "error": format!("{err:#}")})),
        }
    }
    Value::Array(out)
}

/// The write-held row of `region` in `pool` at `now_ms`, if any: what a
/// regions listing marks.
pub fn holder_of<'a>(t: &'a Table, now_ms: i64, pool: &str, region: &str) -> Option<&'a Row> {
    t.live(now_ms).into_iter().find(|r| r.pool == pool && r.region == region && r.mode == WRITE)
}

/// **The sentence a refused writer reads**: which region, who holds it,
/// since when, until when, and what to do.
pub fn refusal_text(rows: &[Row], now_ms: i64) -> String {
    let mut parts = Vec::new();
    for r in rows {
        let who = match (&r.holder.kind[..], &r.holder.dump) {
            ("ripple", Some(d)) => format!("a ripple (run {}, drawing records from dump {d})", short(&r.holder.run)),
            (k, _) => format!("a {k} (run {})", short(&r.holder.run)),
        };
        let how = if r.mode == WRITE { "is locked" } else { "has a region below it locked" };
        parts.push(format!("region `{}` {how} by {who} for {}, since {} until {}", r.region, r.purpose, r.since, r.until));
    }
    format!(
        "{}. Nothing was written. Try again in about {} s (the lock is let go as soon as that write lands or gives up, and at {} at the latest), or, if the records belong in another region, write them there.",
        parts.join("; "),
        retry_after_secs(rows, now_ms),
        rows.iter().max_by_key(|r| r.until_ms).map(|r| r.until.clone()).unwrap_or_default()
    )
}

fn short(run: &str) -> &str {
    &run[..run.len().min(12)]
}

/// The facts of a refusal, for a caller that reads JSON:
/// `{locks: [...rows], retry_after_secs}`.
pub fn refusal_detail(rows: &[Row], now_ms: i64) -> Value {
    json!({"locks": rows, "retry_after_secs": retry_after_secs(rows, now_ms)})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(run: &str) -> Holder {
        Holder { kind: "ripple".into(), run: run.into(), dump: Some("DM-1".into()) }
    }

    fn v(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn write_excludes_all_and_link_shares_link() {
        let mut t = Table::default();
        t.acquire(&h("a"), "p", "ws", &v(&["R-2"]), &v(&["R-1"]), 1_000, 10_000).unwrap();
        // Another holder: write on R-2 refused; write on R-1 (a link held) refused;
        // link on R-1 shared; write on R-3 fine.
        assert_eq!(t.conflicts(2_000, Some("b"), &[("ws".into(), "R-2".into(), LINK.into())]).len(), 1);
        assert_eq!(t.conflicts(2_000, Some("b"), &[("ws".into(), "R-1".into(), WRITE.into())]).len(), 1);
        assert!(t.conflicts(2_000, Some("b"), &[("ws".into(), "R-1".into(), LINK.into())]).is_empty());
        assert!(t.conflicts(2_000, Some("b"), &[("ws".into(), "R-3".into(), WRITE.into())]).is_empty());
        // Another pool's R-2 is another region.
        assert!(t.conflicts(2_000, Some("b"), &[("other".into(), "R-2".into(), WRITE.into())]).is_empty());
        // The holder itself is never in conflict with its own rows.
        assert!(t.conflicts(2_000, Some("a"), &[("ws".into(), "R-2".into(), WRITE.into())]).is_empty());
        // Every writer that is no holder (None) is.
        assert_eq!(t.conflicts(2_000, None, &[("ws".into(), "R-2".into(), LINK.into())]).len(), 1);
        // A second holder is refused and nothing changes.
        let before = t.clone();
        assert!(t.acquire(&h("b"), "p", "ws", &v(&["R-2"]), &[], 2_000, 10_000).is_err());
        assert_eq!(t, before);
        // Siblings: b holds R-3 under the same parent R-1 (link shared).
        t.acquire(&h("b"), "p", "ws", &v(&["R-3"]), &v(&["R-1"]), 2_000, 10_000).unwrap();
        assert_eq!(t.live(2_000).len(), 4);
    }

    #[test]
    fn no_lock_outlives_its_expiry_and_renewal_never_revives() {
        let mut t = Table::default();
        t.acquire(&h("a"), "p", "ws", &v(&["R-2"]), &[], 1_000, 1_000).unwrap();
        assert_eq!(t.live(1_999).len(), 1);
        assert!(t.live(2_000).is_empty(), "held until 2000 exclusive");
        assert!(t.conflicts(2_000, None, &[("ws".into(), "R-2".into(), WRITE.into())]).is_empty());
        assert_eq!(t.renew("a", 2_500, 10_000), 0, "an expired row is never revived");
        assert!(t.live(3_000).is_empty());
        // A live one renews forward, never back.
        t.acquire(&h("a"), "p", "ws", &v(&["R-2"]), &[], 3_000, 1_000).unwrap();
        assert_eq!(t.renew("a", 3_500, 1_000), 1);
        assert_eq!(t.rows.iter().find(|r| r.holder.run == "a").unwrap().until_ms, 4_500);
        assert_eq!(t.renew("a", 3_600, 10), 1);
        assert_eq!(t.rows.iter().find(|r| r.holder.run == "a").unwrap().until_ms, 4_500, "never moved back");
        // Re-acquiring replaces the run's rows.
        t.acquire(&h("a"), "p", "ws", &v(&["R-5"]), &[], 3_700, 1_000).unwrap();
        assert_eq!(t.rows.len(), 1);
        assert_eq!(t.rows[0].region, "R-5");
    }

    #[test]
    fn sweep_drops_the_expired_and_the_landed_and_release_drops_one_run() {
        let mut t = Table::default();
        t.acquire(&h("a"), "p", "ws", &v(&["R-1"]), &[], 0, 100).unwrap();
        t.acquire(&h("b"), "p", "ws", &v(&["R-2"]), &[], 0, 10_000).unwrap();
        t.acquire(&h("c"), "p", "ws", &v(&["R-3"]), &[], 0, 10_000).unwrap();
        assert_eq!(t.sweep(200, &|run| run == "b"), 2);
        assert_eq!(t.rows.len(), 1);
        assert_eq!(t.rows[0].holder.run, "c");
        assert_eq!(t.release("c"), 1);
        assert!(t.rows.is_empty());
    }

    #[test]
    fn the_refusal_names_the_region_the_holder_since_and_until() {
        let mut t = Table::default();
        t.acquire(&h("runabcdefghijklmnop"), "writing the records it drew from a dump", "ws", &v(&["R-2"]), &[], 0, 90_000).unwrap();
        let rows = t.conflicts(30_000, None, &[("ws".into(), "R-2".into(), LINK.into())]);
        let text = refusal_text(&rows, 30_000);
        assert!(text.contains("region `R-2` is locked by a ripple (run runabcdefghi, drawing records from dump DM-1)"), "{text}");
        assert!(text.contains("since 1970-01-01T00:00:00Z until 1970-01-01T00:01:30Z"), "{text}");
        assert!(text.contains("Try again in about 60 s"), "{text}");
        assert_eq!(refusal_detail(&rows, 30_000)["retry_after_secs"], json!(60));
    }

    fn far(run: &str, region: &str, since_ms: i64, until_ms: i64) -> Row {
        Row { pool: "ws".into(), region: region.into(), mode: WRITE.into(), holder: h(run), purpose: "p".into(), since: stamp(since_ms), until: stamp(until_ms), since_ms, until_ms }
    }

    /// A far-future `until` holds at most 900 s from when it was first
    /// seen, however often it is read; it is not moved on by later reads.
    #[test]
    fn a_far_future_row_is_held_at_most_the_maximum_from_when_first_seen() {
        let year_2058 = 2_791_000_000_000i64;
        let now = 1_000_000_000_000i64;
        let mut t = Table { rows: vec![far("hand-edited", "R-2", now, year_2058)], ..Table::default() };
        assert_eq!(t.clamp("first-seen-test", now), 1);
        assert_eq!(t.rows[0].until_ms, now + MAX_AHEAD_MS);
        assert_eq!(t.clamped.len(), 1);
        assert!(t.clamped[0].contains("more than 900 s ahead"), "{:?}", t.clamped);
        assert_eq!(retry_after_secs(&t.rows, now), 900);
        // Read again later, from the file as written: still from first seen.
        let mut again = Table { rows: vec![far("hand-edited", "R-2", now, year_2058)], ..Table::default() };
        again.clamp("first-seen-test", now + 600_000);
        assert_eq!(again.rows[0].until_ms, now + MAX_AHEAD_MS, "not moved on by a later read");
        assert!(again.live(now + MAX_AHEAD_MS).is_empty(), "expired at the maximum");
        // A row within the maximum is untouched.
        let mut ok = Table { rows: vec![far("fine", "R-1", now, now + 120_000)], ..Table::default() };
        assert_eq!(ok.clamp("first-seen-test", now), 0);
        assert_eq!(ok.rows[0].until_ms, now + 120_000);
    }

    /// The clock stepped back a day: a row written under the later clock
    /// is held at most 900 s more, not a day. Stepped forward: the row has
    /// expired, and renewing never revives it.
    #[test]
    fn clock_steps_back_stretch_a_lock_by_at_most_the_maximum_and_forward_expire_it() {
        let then = 1_000_000_000_000i64;
        let day = 86_400_000i64;
        let mut t = Table::default();
        t.acquire(&h("a"), "p", "ws", &v(&["R-1"]), &[], then, 120_000).unwrap();
        let back = then - day;
        assert_eq!(t.clamp("clock-step-test", back), 1);
        assert_eq!(t.rows[0].until_ms, back + MAX_AHEAD_MS);
        assert!(t.live(back + MAX_AHEAD_MS).is_empty());
        // Forward a day from when it was taken: expired, never revived.
        let mut t = Table::default();
        t.acquire(&h("b"), "p", "ws", &v(&["R-1"]), &[], then, 120_000).unwrap();
        let fwd = then + day;
        assert_eq!(t.clamp("clock-step-test", fwd), 0);
        assert!(t.live(fwd).is_empty());
        assert_eq!(t.renew("b", fwd, 120_000), 0);
        // No writer may ask for more than the maximum.
        let mut t = Table::default();
        t.acquire(&h("c"), "p", "ws", &v(&["R-1"]), &[], then, 10 * day).unwrap();
        assert_eq!(t.rows[0].until_ms, then + MAX_AHEAD_MS);
        assert_eq!(t.renew("c", then + 1, 10 * day), 1);
        assert_eq!(t.rows[0].until_ms, then + 1 + MAX_AHEAD_MS);
    }

    #[test]
    fn a_table_round_trips_and_an_unreadable_one_is_an_error() {
        let dir = std::env::temp_dir().join(format!("env-region-locks-{}-{}", std::process::id(), now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(FILE);
        assert_eq!(load_at(&p).unwrap(), Table::default());
        std::fs::write(&p, "{not json").unwrap();
        assert!(load_at(&p).is_err());
        std::fs::write(&p, r#"{"format": 9, "rows": []}"#).unwrap();
        assert!(load_at(&p).is_err());
        // Read from the file, a far-future row is cut back on reading.
        let now = now_ms();
        let t = Table { rows: vec![far("from-file", "R-3", now, now + 50 * 86_400_000)], ..Table::default() };
        std::fs::write(&p, serde_json::to_string(&t).unwrap()).unwrap();
        let read = load_at(&p).unwrap();
        assert!(read.rows[0].until_ms - now_ms() <= MAX_AHEAD_MS, "{read:?}");
        assert_eq!(read.clamped.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
