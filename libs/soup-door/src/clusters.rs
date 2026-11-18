//! **Clusters**: the soup's second pool, `cauldron` (`cauldron@1`), and the
//! claims its clusters make on the regions of the workspace pool `ws`.
//!
//! A cluster is an area of the surface: a name (`handle`) and a boundary
//! description. It CLAIMS a region with a `claims` edge from the cluster to
//! the region (`CL-1@cauldron -[claims]-> R-3@ws`), many-to-many. A claim is
//! a statement about the surface, not a lease on work.
//!
//! # The pool, and a cauldron made before it
//!
//! `cauldron create` (0.1.16 and later) makes both pools. A cauldron made by
//! an earlier release has only `ws`, and gets `cauldron` on its FIRST CLUSTER
//! WRITE (a batch holding a cluster record), by [`ensure_pool`]: one additive
//! act through the writer's own handle, under the writer lock it already
//! holds (`spine::Cauldron::add_pool`: the pool file built whole aside and
//! linked into place in one step). Nothing else in the store is read,
//! migrated or rewritten, and no spine row is written for it, so the act is
//! inside the recovery rules trivially: an add cut short leaves a staging
//! directory no open reads, which the next add removes. A claim never makes
//! the pool: with no pool there is no cluster to claim with.
//!
//! An older release opening a store that has the pool discovers it as a
//! pool and reads it never: its reads and writes name `ws` only.
//!
//! # Reads
//!
//! Every read here opens the pool files through connections that cannot
//! write, and answers nothing (an empty list, a zero) for a store with no
//! `cauldron` pool yet.
//!
//! # A claim is read only once its region is
//!
//! A write holding a region and a cluster's claim of it is one spine row
//! with two parts, one per pool, and the spine lands them one after the
//! other (its commit point is the row, not one SQLite transaction across the
//! two files). The spine lands the far end of a cross-pool edge first, so
//! the `ws` part (the region) is readable before the `cauldron` part (the
//! claim) lands. Every read here also takes its `cauldron` snapshot BEFORE
//! its `ws` snapshot, and leaves out a `claims` edge whose region that `ws`
//! snapshot does not hold ([`Regions`]): a reader never sees a claim of a
//! region it cannot read, whatever stage a write (or a crash, until
//! `cauldron soup recover` or the next open finishes it) is at, and
//! whichever release wrote the store.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use spine::grammar_cauldron::{CLAIMS, CLUSTER};
use spine::pool::{NodeId, Pool, QualifiedNodeId};
use spine::schema_def::PoolName;

use crate::cut::Cut;
use crate::reads::DEFAULT_POOL;
use crate::target::Target;
use crate::{refuse, Code, Error, Result};

/// The soup's cluster pool.
pub const POOL: &str = spine::grammar_cauldron::POOL;
/// The schema the pool speaks.
pub const SCHEMA: &str = "cauldron@1";
/// The one node type the pool holds.
pub const POOL_TYPE: &str = CLUSTER;

fn store(err: impl std::fmt::Display) -> Error {
    Error::new(Code::Store, err.to_string())
}

fn pool_name() -> PoolName {
    PoolName::new(POOL)
}

fn ws() -> PoolName {
    PoolName::new(DEFAULT_POOL)
}

/// Does the cauldron's soup have its cluster pool yet?
pub fn has_pool(c: &env::Cauldron) -> bool {
    Pool::path_in(&c.soup(), &pool_name()).is_file()
}

/// The routes a record write uses: `cluster` records go to the cluster
/// pool; every other type to `ws`.
pub fn routes() -> BTreeMap<String, PoolName> {
    BTreeMap::from([(CLUSTER.to_string(), pool_name())])
}

/// Does this batch hold a cluster record (and so need the cluster pool)?
pub fn wants_pool(records: &[Value]) -> bool {
    records.iter().any(|r| r.get("type").and_then(Value::as_str) == Some(CLUSTER))
}

/// The grammar a record of `node_type` is judged by, whether or not the
/// cauldron has that pool yet: `ws`'s for a workspace type, the cluster
/// pool's for `cluster`.
pub fn grammar_of_type(node_type: &str) -> Option<spine::schema_def::PoolGrammar> {
    if node_type == CLUSTER {
        return Some(spine::grammar_cauldron::cauldron_v1(POOL));
    }
    let g = spine::grammar_v6::workspace_v6(DEFAULT_POOL);
    g.node_types.contains_key(node_type).then_some(g)
}

/// **Give the cauldron its cluster pool if it has none**, through the
/// writer's own `handle` (whose holder holds the soup's writer lock), and
/// make the new file its owner's only. `true` when it added the pool.
pub fn ensure_pool(handle: &mut spine::Cauldron, c: &env::Cauldron) -> Result<bool> {
    let added = handle
        .add_pool(&spine::grammar_cauldron::cauldron_v1(POOL))
        .map_err(|e| store(format!("adding the cluster pool to cauldron `{}`'s soup: {e}", c.name)))?;
    if added {
        env::private::tighten_tree(&c.soup())
            .map_err(|err| Error::caused(Code::Store, format!("making cauldron `{}`'s soup private to its owner", c.name), &err))?;
    }
    Ok(added)
}

fn read_pool(c: &env::Cauldron, pool: &PoolName) -> Result<Pool> {
    Pool::open_read_only(&Pool::path_in(&c.soup(), pool))
        .map_err(|err| store(format!("reading cauldron `{}`'s pool `{pool}`: {err}", c.name)))
}

/// The cluster pool, read-only, or `None` when the soup has none yet.
fn cluster_pool(c: &env::Cauldron) -> Result<Option<Pool>> {
    if !has_pool(c) {
        return Ok(None);
    }
    read_pool(c, &pool_name()).map(Some)
}

fn text_of(n: &spine::pool::NodeRecord, k: &str) -> String {
    n.fields.get(k).and_then(|f| f.value.as_str()).unwrap_or("").to_string()
}

/// **The regions a read may name**: a `ws` snapshot taken AFTER the read's
/// `cauldron` snapshot, asked whether a claim's region is there. A claim
/// whose region it does not hold is left out of every read (module docs).
struct Regions {
    pool: Pool,
}

impl Regions {
    /// Open it: call this only after the `cauldron` snapshot is taken.
    fn after(c: &env::Cauldron) -> Result<Regions> {
        Ok(Regions { pool: read_pool(c, &ws())? })
    }

    /// The regions among `edges`' far ends that `ws` holds, with their
    /// handles.
    fn readable(&self, edges: &[&spine::pool::EdgeRecord]) -> Result<BTreeMap<String, String>> {
        let snap = self.pool.snapshot().map_err(store)?;
        let mut out = BTreeMap::new();
        for e in edges {
            if e.kind != CLAIMS || e.dst.pool != ws() || out.contains_key(&e.dst.id.0) {
                continue;
            }
            if let Some(n) = snap.node(&e.dst.id).map_err(store)? {
                out.insert(e.dst.id.0.clone(), text_of(&n, "handle"));
            }
        }
        Ok(out)
    }
}

/// Is `e` an edge a read may show: not a `claims` edge, or one whose region
/// is in `readable`?
fn shown(e: &spine::pool::EdgeRecord, readable: &BTreeMap<String, String>) -> bool {
    e.kind != CLAIMS || (e.dst.pool == ws() && readable.contains_key(&e.dst.id.0))
}

/// One claim, as a reader sees it: the region id (bare: it is in `ws`) and
/// the note, when there is one (cut by [`crate::notes::shown`]).
fn claim_doc(e: &spine::pool::EdgeRecord, cut: &mut Cut) -> Value {
    let mut v = json!({"region": crate::reads::target_text(&ws(), &e.dst)});
    if let Some(n) = &e.note {
        v["note"] = json!(crate::notes::shown(cut, &e.src.0, &format!("claims/{}/note", e.dst.id.0), n));
    }
    v
}

/// **The clusters that claim region `region`**: `[{id, handle, note?}]`, by
/// id, from the claims' reverse index. Empty for a soup with no cluster
/// pool. Each handle is cut by `cut`.
pub fn claimants(c: &env::Cauldron, region: &str, cut: &mut Cut) -> Result<Vec<Value>> {
    let Some(p) = cluster_pool(c)? else { return Ok(vec![]) };
    let snap = p.snapshot().map_err(store)?;
    let dst = QualifiedNodeId { pool: ws(), id: NodeId(region.to_string()) };
    let mut out = Vec::new();
    let mut edges = snap.edges_to(&dst, Some(CLAIMS)).map_err(store)?;
    edges.sort_by(|a, b| crate::reads::serial(&a.src.0).cmp(&crate::reads::serial(&b.src.0)));
    for e in edges {
        let handle = snap.node(&e.src).map_err(store)?.map(|n| text_of(&n, "handle")).unwrap_or_default();
        let mut v = json!({"id": e.src.0, "handle": cut.value(&e.src.0, "handle", &json!(handle))});
        if let Some(n) = &e.note {
            v["note"] = json!(crate::notes::shown(cut, &e.src.0, &format!("claims/{region}/note"), n));
        }
        out.push(v);
    }
    Ok(out)
}

/// How many clusters the soup holds (0 with no cluster pool).
pub fn count(c: &env::Cauldron) -> Result<usize> {
    let Some(p) = cluster_pool(c)? else { return Ok(0) };
    let snap = p.snapshot().map_err(store)?;
    let n = snap.nodes_of(CLUSTER, None).map_err(store)?.len();
    Ok(n)
}

/// One cluster as the listing reads carry it.
pub struct Listed {
    pub id: String,
    pub handle: String,
    pub boundary: String,
    /// The regions it claims, by id.
    pub regions: Vec<String>,
    /// The committed-commit counter of the write that made it.
    pub order: Option<i64>,
}

/// Every cluster, newest first (by the spine's commit order, then serial).
pub fn listed(c: &env::Cauldron) -> Result<Vec<Listed>> {
    let Some(p) = cluster_pool(c)? else { return Ok(vec![]) };
    let snap = p.snapshot().map_err(store)?;
    let order = spine::offline::commit_order(&c.soup()).map_err(|err| store(format!("reading cauldron `{}`'s commit order: {err}", c.name)))?;
    let mut read = Vec::new();
    for n in snap.nodes_of(CLUSTER, None).map_err(store)? {
        let claims = snap.edges_from(&n.id, Some(CLAIMS)).map_err(store)?;
        let created = snap.created_tx(&n.id).map_err(store)?;
        read.push((n, claims, created));
    }
    drop(snap);
    let all: Vec<&spine::pool::EdgeRecord> = read.iter().flat_map(|(_, e, _)| e.iter()).collect();
    let readable = Regions::after(c)?.readable(&all)?;
    let mut out = Vec::new();
    for (n, claims, created) in &read {
        let regions = claims.iter().filter(|e| shown(e, &readable)).map(|e| crate::reads::target_text(&ws(), &e.dst)).collect();
        out.push(Listed {
            id: n.id.0.clone(),
            handle: text_of(n, "handle"),
            boundary: text_of(n, "boundary"),
            regions,
            order: created.as_ref().and_then(|h| order.get(&h.to_hex()).copied()),
        });
    }
    out.sort_by(|a, b| {
        let newest = |e: &Listed| e.order.unwrap_or(i64::MAX);
        newest(b).cmp(&newest(a)).then_with(|| crate::reads::serial(&b.id).cmp(&crate::reads::serial(&a.id)))
    });
    Ok(out)
}

/// **`nodes --type cluster`**: `{env, cauldron, pool, total, nodes: [{id,
/// type, handle, regions}]}`, newest first, at most `limit`; `region` keeps
/// the clusters that claim that region. A cluster has no state, so `state`
/// filters out every one.
pub fn nodes(t: &Target, region: Option<&str>, state: Option<&str>, limit: usize, max_chars: Option<usize>) -> Result<Value> {
    let (e, c) = (&t.env, &t.cauldron);
    if let Some(r) = region {
        check_region(c, r)?;
    }
    let all: Vec<Listed> = listed(c)?
        .into_iter()
        .filter(|n| region.is_none_or(|r| n.regions.iter().any(|x| x == r)))
        .filter(|_| state.is_none())
        .collect();
    let total = all.len();
    let more = total.saturating_sub(limit);
    let mut cut = Cut::new(max_chars);
    let list: Vec<Value> = all
        .iter()
        .take(limit)
        .map(|n| json!({"id": n.id, "type": CLUSTER, "handle": cut.value(&n.id, "handle", &json!(n.handle)), "regions": n.regions}))
        .collect();
    let mut doc = json!({"env": e.name, "cauldron": c.name, "pool": POOL, "total": total, "superseded_hidden": 0, "nodes": list});
    if more > 0 {
        doc["more"] = json!(more);
    }
    cut.report(&mut doc);
    Ok(doc)
}

/// The region `r` of `ws`, checked: it must be a region the soup holds.
fn check_region(c: &env::Cauldron, r: &str) -> Result<()> {
    let p = read_pool(c, &ws())?;
    let snap = p.snapshot().map_err(store)?;
    let found = snap.node(&NodeId(r.to_string())).map_err(store)?;
    match found {
        Some(n) if n.node_type == spine::grammar_v6::REGION => Ok(()),
        Some(n) => refuse!(Code::Usage, "`{r}` is a {}, not a region", n.node_type),
        None => refuse!(Code::NotFound, "there is no region `{r}`; `cauldron soup regions` lists them"),
    }
}

/// **The clusters among `ids`, as records** in the published shape
/// (`schema/soup/cauldron-v1/`): `{id, type, handle, boundary, meta?,
/// edges: [{kind: claims, to: "R-3@ws", note?}], claims: [{region, handle,
/// note?}]}`. `claims` is read from the edges, with each claimed region's
/// handle; it is not a field. Answers the records found and the ids not
/// found, in the order asked.
pub fn get(c: &env::Cauldron, ids: &[String], cut: &mut Cut) -> Result<(Vec<Value>, Vec<String>)> {
    let Some(p) = cluster_pool(c)? else { return Ok((vec![], ids.to_vec())) };
    let snap = p.snapshot().map_err(store)?;
    let mut read = Vec::new();
    let mut missing = Vec::new();
    for id in ids {
        match snap.node(&NodeId(id.clone())).map_err(store)? {
            Some(n) => {
                let edges = snap.edges_from(&n.id, None).map_err(store)?;
                read.push((n, edges));
            }
            None => missing.push(id.clone()),
        }
    }
    drop(snap);
    // The regions, read after the claims (module docs).
    let all: Vec<&spine::pool::EdgeRecord> = read.iter().flat_map(|(_, e)| e.iter()).collect();
    let readable = Regions::after(c)?.readable(&all)?;
    let mut found = Vec::new();
    for (n, out_edges) in read {
        let mut rec = serde_json::Map::new();
        rec.insert("id".into(), json!(n.id.0));
        rec.insert("type".into(), json!(n.node_type));
        for (k, f) in &n.fields {
            rec.insert(k.clone(), cut.value(&n.id.0, k, &f.value));
        }
        let mut edges = Vec::new();
        let mut claims = Vec::new();
        for (j, x) in out_edges.iter().filter(|x| shown(x, &readable)).enumerate() {
            let mut m = serde_json::Map::new();
            m.insert("kind".into(), json!(x.kind));
            m.insert("to".into(), json!(crate::reads::target_text(&pool_name(), &x.dst)));
            if let Some(note) = &x.note {
                m.insert("note".into(), json!(crate::notes::shown(cut, &n.id.0, &format!("edges/{j}/note"), note)));
            }
            edges.push(Value::Object(m));
            if x.kind == CLAIMS {
                let mut cl = claim_doc(x, cut);
                let h = readable.get(&x.dst.id.0);
                cl["handle"] = h.map(|h| cut.value(&x.dst.id.0, "handle", &json!(h))).unwrap_or(Value::Null);
                claims.push(cl);
            }
        }
        if !edges.is_empty() {
            rec.insert("edges".into(), Value::Array(edges));
        }
        rec.insert("claims".into(), Value::Array(claims));
        found.push(Value::Object(rec));
    }
    Ok((found, missing))
}

/// One search hit on a cluster: `(matched terms, score, listing position,
/// cluster)`.
pub type Hit = (usize, usize, usize, Listed);

/// **Search the clusters' handles and boundaries** for `terms` (folded),
/// scored as the workspace search scores a node (a handle hit counts three,
/// a boundary hit one), with `region` keeping the clusters that claim it.
pub fn search(c: &env::Cauldron, terms: &[Vec<char>], phrase: &[char], region: Option<&str>) -> Result<Vec<Hit>> {
    let fold = |s: &str| -> Vec<char> { s.chars().map(|ch| ch.to_lowercase().next().unwrap_or(ch)).collect() };
    let find = |hay: &[char], t: &[char]| !t.is_empty() && t.len() <= hay.len() && (0..=hay.len() - t.len()).any(|i| hay[i..i + t.len()] == *t);
    let mut out = Vec::new();
    for (pos, n) in listed(c)?.into_iter().enumerate() {
        if region.is_some_and(|r| !n.regions.iter().any(|x| x == r)) {
            continue;
        }
        let (h, b) = (fold(&n.handle), fold(&n.boundary));
        let (mut matched, mut score) = (0usize, 0usize);
        for t in terms {
            let (in_h, in_b) = (find(&h, t), find(&b, t));
            if in_h || in_b {
                matched += 1;
            }
            score += 3 * usize::from(in_h) + usize::from(in_b);
        }
        if matched == 0 {
            continue;
        }
        if terms.len() > 1 && (find(&h, phrase) || find(&b, phrase)) {
            score += 2;
        }
        out.push((matched, 10 * matched + score, pos, n));
    }
    Ok(out)
}

// -------------------------------------------------------------------- claim

/// What a claim asks.
#[derive(Debug, Clone)]
pub struct Claim {
    pub cluster: String,
    pub region: String,
    /// Remove the claim instead of adding it.
    pub release: bool,
    /// The claim's note (only when adding).
    pub note: Option<String>,
}

/// Who writes a claim: the `agent` initiator's `thread` and `dispatch`.
#[derive(Debug, Clone)]
pub struct Writer {
    pub thread: String,
    pub dispatch: String,
}

/// **Add or release one claim** through `handle`, the writer's handle on the
/// target's soup (its region guard installed): judged first, then one
/// statement. Idempotent: a claim already there (or a release of one that is
/// not) writes nothing and answers `outcome: "already-claimed"` (or
/// `"not-claimed"`). Refusals: `not_found` for a cluster or region the soup
/// does not hold, `usage` for an id that is not a region, `region_locked`
/// while a ripple holds the region (as `soup_link`).
pub fn claim(handle: &spine::Cauldron, t: &Target, k: &Claim, w: &Writer) -> Result<Value> {
    let c = &t.cauldron;
    if k.release && k.note.is_some() {
        refuse!(Code::Usage, "a release takes no note: it removes the claim, note and all. Nothing was written");
    }
    if let Some(n) = &k.note {
        crate::notes::check(n, &format!("the claim of `{}` by `{}`", k.region, k.cluster))?;
    }
    let Some(cp) = cluster_pool(c)? else {
        refuse!(
            Code::NotFound,
            "there is no cluster `{}` in cauldron `{}`: it holds no clusters yet (write one first, a record of type `cluster`). \
             Nothing was written",
            k.cluster,
            c.name
        );
    };
    let snap = cp.snapshot().map_err(store)?;
    if snap.node(&NodeId(k.cluster.clone())).map_err(store)?.is_none_or(|n| n.node_type != CLUSTER) {
        refuse!(Code::NotFound, "there is no cluster `{}` in cauldron `{}`. Nothing was written", k.cluster, c.name);
    }
    check_region(c, &k.region).map_err(|e| Error::new(e.code, format!("{}. Nothing was written", e.message)))?;
    let dst = QualifiedNodeId { pool: ws(), id: NodeId(k.region.clone()) };
    let held = snap
        .edges_from(&NodeId(k.cluster.clone()), Some(CLAIMS))
        .map_err(store)?
        .into_iter()
        .find(|e| e.dst == dst);
    let mut doc = json!({
        "env": t.env.name,
        "cauldron": c.name,
        "cluster": k.cluster,
        "region": k.region,
        "release": k.release,
    });
    match (&held, k.release) {
        (Some(e), false) => {
            doc["outcome"] = json!("already-claimed");
            doc["written"] = json!(false);
            if let Some(n) = &e.note {
                // A note stored before the bound is echoed cut to it.
                let mut cut = Cut::new(None);
                doc["note"] = json!(crate::notes::shown(&mut cut, &k.cluster, "note", n));
                cut.report(&mut doc);
            }
            if k.note.is_some() && k.note != e.note {
                doc["note_unchanged"] = json!("the claim was already there; its note is kept as it was (release it and claim again to change it)");
            }
            return Ok(doc);
        }
        (None, true) => {
            doc["outcome"] = json!("not-claimed");
            doc["written"] = json!(false);
            return Ok(doc);
        }
        _ => {}
    }
    drop(snap);
    drop(cp);
    let verb = if k.release { "REMOVE" } else { "CREATE" };
    let mut statement = format!(
        "MATCH (k: {POOL}/{CLUSTER} {{ id: $k }})\nMATCH (r: {DEFAULT_POOL}/region {{ id: $r }})\nEXPECT COUNT(k) = 1\nEXPECT COUNT(r) = 1\n{verb} (k) -[{CLAIMS}]-> (r)"
    );
    let mut bindings = spine::envelope::Bindings::new();
    bindings.insert("k".into(), json!(k.cluster));
    bindings.insert("r".into(), json!(k.region));
    if let Some(n) = &k.note {
        statement.push_str(" NOTE $note");
        bindings.insert("note".into(), json!(n));
    }
    let env = spine::envelope::Envelope {
        statement,
        bindings,
        format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
        provenance: spine::envelope::Provenance {
            initiator: spine::envelope::Initiator::Agent { thread: w.thread.clone(), dispatch: w.dispatch.clone() },
            instrument: spine::envelope::Instrument { name: "cauldron-soup-door".into(), version: env!("CARGO_PKG_VERSION").into() },
        },
        mode: spine::envelope::Mode::Commit,
        idempotency_nonce: Some(format!(
            "soup-claim:{}:{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
        )),
    };
    match handle.submit(env) {
        Ok(spine::Outcome::Committed { tx, seq, deduplicated, .. }) => {
            doc["outcome"] = json!(if k.release { "released" } else { "claimed" });
            doc["written"] = json!(true);
            if let Some(n) = &k.note {
                doc["note"] = json!(n);
            }
            doc["landed"] = json!({"tx": tx.to_hex(), "seq": seq, "deduplicated": deduplicated});
            Ok(doc)
        }
        Ok(other) => Err(store(format!("the claim was not committed: {other:?}"))),
        Err(e @ spine::error::SpineError::RegionLocked { .. }) => Err(crate::region_lock::spine_error(e)),
        Err(spine::error::SpineError::Refused { witnesses }) => Err(Error::new(
            Code::Usage,
            format!("the soup's grammar refused the claim of `{}` by `{}`; nothing was written", k.region, k.cluster),
        )
        .with_detail(json!({"witnesses": witnesses}))),
        Err(e) => Err(store(format!("writing the claim: {e}"))),
    }
}

/// **`cauldron soup claim`, written directly** (no `mcp` service holds the
/// soup): the proofs a real import makes (no live soup daemon, the writer
/// lock, at rest, an open that recovered nothing), the region guard, then
/// [`claim`].
pub fn claim_direct(t: &Target, k: &Claim, w: &Writer) -> Result<Value> {
    let c = &t.cauldron;
    crate::lock::refuse_live_soup(&t.env)?;
    let _lock = crate::lock::take_writer_lock(c, "claim")?;
    let unrest = spine::offline::rest_check(&c.soup())
        .map_err(|err| store(format!("REFUSED: cannot read {}'s spine, so cannot prove it is at rest: {err}", c.name)))?;
    if !unrest.is_empty() {
        refuse!(
            Code::NotAtRest,
            "REFUSED: cauldron `{}`'s soup is not at rest ({}); run `cauldron soup recover --env {} --cauldron {}` first. Nothing was written.",
            c.name,
            unrest.join("; "),
            t.env.name,
            c.name
        );
    }
    let mut handle = spine::Cauldron::open(&c.soup(), spine::config::SpineConfig::default())
        .map_err(|err| store(format!("opening cauldron `{}`'s soup: {err}", c.name)))?;
    handle.set_region_guard(crate::region_lock::Guard::new(c));
    claim(&handle, t, k, w)
}
