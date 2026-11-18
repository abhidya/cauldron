//! **Regions as a lock hierarchy**: which regions a write writes into, how
//! a region's revision is read, and the guard a writer installs so that a
//! write into a region someone else holds is refused before anything is
//! recorded.
//!
//! # The regions a node is in: `regions*(n)`
//!
//! - a content node: every region it attests (`in_region`) and every
//!   ancestor of those regions (`narrows`, child to parent, a DAG with many
//!   parents allowed and no root);
//! - a content node with no `in_region` edge: the virtual region
//!   [`UNPLACED`];
//! - a region node `R`: `R` and its ancestors;
//! - in a pool whose grammar has no `region` type, every node is in
//!   [`UNPLACED`].
//!
//! The ancestor walk reads the snapshot the write was judged against,
//! never a cached index.
//!
//! # The rows a write names
//!
//! For every delta of a compiled write, [`payload_region_rows`] names the
//! regions it writes into, each `link` (a shared intention: many writers may
//! add members side by side) or `write` (exclusive):
//!
//! | delta | rows |
//! |---|---|
//! | a field set or a seal on content `n` | `link` regions*(n) |
//! | a field set or a seal on region `R` (its header) | `write` on `R`; `link` on every ancestor of `R` |
//! | creating content | `link` regions*(n): the write's own `in_region` targets and their ancestors, or [`UNPLACED`] |
//! | creating a region | `link` on its parents (from the write) and their ancestors; nothing if it has none |
//! | an edge `a -[k]-> b` of an ordinary kind | `link` regions*(a) ∪ regions*(b), both ends, each in its own pool |
//! | `n -[in_region]-> R`, `n -[rejected_by]-> R` | `link` regions*(n) ∪ {R} ∪ ancestors(R) |
//! | `C -[narrows]-> P` | `write` on `C`; `link` on `P`, ancestors(P), and regions*(C) less `C` |
//!
//! An edge removed names what the same edge added names. A node the write
//! itself creates takes its regions from the write; every other node from
//! the snapshot ("its old home"), so moving a node names both homes.
//!
//! Two rows on one region conflict when either is `write`. A holder of a
//! region (`write` on it, `link` on its ancestors) therefore excludes every
//! other write into that region and into any region below it, a header
//! change of any region above it, and a re-parenting that would move it,
//! while writes into sibling regions proceed.
//!
//! # A region's revision
//!
//! [`region_rev`] is a digest of what a reader of the region sees of it:
//! the region node's own revision (its header), its parents, its children,
//! and its direct members with their states and the nodes that supersede
//! them. It moves when a boundary is rewritten, a member is added, moved
//! out, closed or superseded (from any region), or the region is split; it
//! does not move when a member's other fields change.
//! It is derived from the store on every read and is never written, so any
//! writer, of any release, moves it.
//!
//! [`region_head_rev`] is the narrower digest of a region's boundary and
//! identity alone: the region node's own revision and state, its parents,
//! its children and the nodes that supersede it. No member enters it, so a
//! member added, moved, closed or superseded leaves it where it was. It is
//! what a planner that was shown a region's handle and boundary, and never
//! its members, depended on.
//!
//! # The guard
//!
//! A [`RegionGuard`] installed on a handle ([`crate::Cauldron::set_region_guard`])
//! is asked about every COMMIT-mode statement after it compiled and passed
//! the flag rule, and before its row is appended: a refusal there writes
//! nothing at all ([`crate::error::SpineError::RegionLocked`]). A dry run
//! (validate mode) is never asked.

use crate::delta::{CompiledPayload, Delta};
use crate::envelope::Initiator;
use crate::error::{Result, SpineError};
use crate::locks::LockMode;
use crate::pool::{NodeId, Snapshot};
use crate::project;
use crate::schema_def::{PoolGrammar, PoolName};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};

/// The region node type.
pub const REGION_TYPE: &str = crate::grammar_v6::REGION;
/// Membership: content to region.
pub const IN_REGION: &str = "in_region";
/// Hierarchy: child region to parent region.
pub const NARROWS: &str = "narrows";
/// A regionless node's rejection evidence: content to region.
pub const REJECTED_BY: &str = "rejected_by";
/// Supersession: the newer node to the one it supersedes.
pub const SUPERSEDES: &str = "supersedes";
/// The virtual region of the content nodes no region holds. A minted id
/// never begins with `~`, so it never names a real node.
pub const UNPLACED: &str = "~unplaced";

/// [`UNPLACED`] as a node id.
pub fn unplaced() -> NodeId {
    NodeId(UNPLACED.to_string())
}

/// Does this pool's grammar declare regions at all?
pub fn has_regions(grammar: &PoolGrammar) -> bool {
    grammar.node_types.contains_key(REGION_TYPE)
}

/// Where region facts are read from: one snapshot per pool.
pub trait RegionSource {
    fn snapshot(&self, pool: &PoolName) -> Option<&Snapshot<'_>>;
}

impl RegionSource for crate::read::Snapshots<'_> {
    fn snapshot(&self, pool: &PoolName) -> Option<&Snapshot<'_>> {
        self.by_pool.get(pool)
    }
}

/// No snapshot for any pool: enough for pools whose grammar declares no
/// regions (every write names [`UNPLACED`]); a region pool asked about
/// through it is a loud `Hard`, never a silent empty set.
pub struct NoSnapshots;

impl RegionSource for NoSnapshots {
    fn snapshot(&self, _pool: &PoolName) -> Option<&Snapshot<'_>> {
        None
    }
}

/// One snapshot of one pool, as a [`RegionSource`].
pub struct OneSnapshot<'s, 'a> {
    pub pool: PoolName,
    pub snap: &'s Snapshot<'a>,
}

impl RegionSource for OneSnapshot<'_, '_> {
    fn snapshot(&self, pool: &PoolName) -> Option<&Snapshot<'_>> {
        (pool == &self.pool).then_some(self.snap)
    }
}

/// The parents of region `r`: its `narrows` edges within the pool.
fn snapshot_parents(snap: &Snapshot<'_>, pool: &PoolName, r: &NodeId) -> Result<BTreeSet<NodeId>> {
    Ok(snap.edges_from(r, Some(NARROWS))?.into_iter().filter(|e| e.dst.pool == *pool).map(|e| e.dst.id).collect())
}

/// Every ancestor of `region` over every `narrows` path, not `region`
/// itself. A visited set keeps a damaged pool from looping the walk.
pub fn ancestors_of(snap: &Snapshot<'_>, pool: &PoolName, region: &NodeId) -> Result<BTreeSet<NodeId>> {
    let mut out = BTreeSet::new();
    let mut frontier = vec![region.clone()];
    while let Some(r) = frontier.pop() {
        for p in snapshot_parents(snap, pool, &r)? {
            if p != *region && out.insert(p.clone()) {
                frontier.push(p);
            }
        }
    }
    Ok(out)
}

/// `regions*(n)` over one snapshot (module docs). A node the snapshot does
/// not hold names [`UNPLACED`]: it attests nothing.
pub fn regions_star(snap: &Snapshot<'_>, pool: &PoolName, grammar: &PoolGrammar, node: &NodeId) -> Result<BTreeSet<NodeId>> {
    if !has_regions(grammar) {
        return Ok(BTreeSet::from([unplaced()]));
    }
    let Some(rec) = snap.node(node)? else {
        return Ok(BTreeSet::from([unplaced()]));
    };
    if rec.node_type == REGION_TYPE {
        let mut out = ancestors_of(snap, pool, node)?;
        out.insert(node.clone());
        return Ok(out);
    }
    let direct: BTreeSet<NodeId> = snap.edges_from(node, Some(IN_REGION))?.into_iter().filter(|e| e.dst.pool == *pool).map(|e| e.dst.id).collect();
    if direct.is_empty() {
        return Ok(BTreeSet::from([unplaced()]));
    }
    let mut out = direct.clone();
    for r in &direct {
        out.extend(ancestors_of(snap, pool, r)?);
    }
    Ok(out)
}

/// One row a write names: a region of a pool, `link` or `write`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct RegionRow {
    pub pool: PoolName,
    pub region: NodeId,
    pub mode: LockMode,
}

impl RegionRow {
    /// `link` or `write`.
    pub fn mode_word(&self) -> &'static str {
        if self.mode == LockMode::Write {
            "write"
        } else {
            "link"
        }
    }
}

/// What one write creates and which membership edges it adds.
struct PayloadFacts {
    created: BTreeMap<(PoolName, NodeId), String>,
    in_region: BTreeMap<(PoolName, NodeId), BTreeSet<NodeId>>,
    narrows: BTreeMap<(PoolName, NodeId), BTreeSet<NodeId>>,
}

impl PayloadFacts {
    fn of(payload: &CompiledPayload) -> PayloadFacts {
        let mut f = PayloadFacts { created: BTreeMap::new(), in_region: BTreeMap::new(), narrows: BTreeMap::new() };
        for part in &payload.parts {
            for d in &part.deltas {
                match d {
                    Delta::CreateNode { id, node_type, .. } => {
                        f.created.insert((part.pool.clone(), id.clone()), node_type.clone());
                    }
                    Delta::AddEdge { src, kind, dst, .. } if dst.pool == part.pool => {
                        let key = (part.pool.clone(), src.clone());
                        if kind == IN_REGION {
                            f.in_region.entry(key).or_default().insert(dst.id.clone());
                        } else if kind == NARROWS {
                            f.narrows.entry(key).or_default().insert(dst.id.clone());
                        }
                    }
                    _ => {}
                }
            }
        }
        f
    }
}

struct Deriver<'a> {
    facts: PayloadFacts,
    source: &'a dyn RegionSource,
    grammars: &'a dyn Fn(&PoolName) -> Option<PoolGrammar>,
}

impl Deriver<'_> {
    fn grammar(&self, pool: &PoolName) -> Result<PoolGrammar> {
        (self.grammars)(pool).ok_or_else(|| SpineError::hard(format!("region rows: no pinned grammar for pool `{pool}`; refusing rather than guessing its regions")))
    }

    fn snap(&self, pool: &PoolName) -> Result<&Snapshot<'_>> {
        self.source.snapshot(pool).ok_or_else(|| {
            SpineError::hard(format!("region rows: pool `{pool}` declares regions and no snapshot of it was given; a region pool's rows are never derived blind"))
        })
    }

    fn is_region(&self, pool: &PoolName, node: &NodeId) -> Result<bool> {
        if let Some(t) = self.facts.created.get(&(pool.clone(), node.clone())) {
            return Ok(t == REGION_TYPE);
        }
        Ok(self.snap(pool)?.node(node)?.map(|r| r.node_type == REGION_TYPE).unwrap_or(false))
    }

    fn parents(&self, pool: &PoolName, r: &NodeId) -> Result<BTreeSet<NodeId>> {
        let key = (pool.clone(), r.clone());
        if self.facts.created.contains_key(&key) {
            return Ok(self.facts.narrows.get(&key).cloned().unwrap_or_default());
        }
        snapshot_parents(self.snap(pool)?, pool, r)
    }

    fn ancestors(&self, pool: &PoolName, r: &NodeId) -> Result<BTreeSet<NodeId>> {
        let mut out = BTreeSet::new();
        let mut frontier = vec![r.clone()];
        while let Some(x) = frontier.pop() {
            for p in self.parents(pool, &x)? {
                if p != *r && out.insert(p.clone()) {
                    frontier.push(p);
                }
            }
        }
        Ok(out)
    }

    /// `regions*(n)` with the write's own creates folded in.
    fn star(&self, pool: &PoolName, node: &NodeId) -> Result<BTreeSet<NodeId>> {
        if !has_regions(&self.grammar(pool)?) {
            return Ok(BTreeSet::from([unplaced()]));
        }
        let key = (pool.clone(), node.clone());
        if let Some(t) = self.facts.created.get(&key) {
            if t == REGION_TYPE {
                let mut out = self.ancestors(pool, node)?;
                out.insert(node.clone());
                return Ok(out);
            }
            let direct = self.facts.in_region.get(&key).cloned().unwrap_or_default();
            if direct.is_empty() {
                return Ok(BTreeSet::from([unplaced()]));
            }
            let mut out = direct.clone();
            for r in &direct {
                out.extend(self.ancestors(pool, r)?);
            }
            return Ok(out);
        }
        let snap = self.snap(pool)?;
        let Some(rec) = snap.node(node)? else {
            return Ok(BTreeSet::from([unplaced()]));
        };
        if rec.node_type == REGION_TYPE {
            let mut out = self.ancestors(pool, node)?;
            out.insert(node.clone());
            return Ok(out);
        }
        let direct: BTreeSet<NodeId> = snap.edges_from(node, Some(IN_REGION))?.into_iter().filter(|e| e.dst.pool == *pool).map(|e| e.dst.id).collect();
        if direct.is_empty() {
            return Ok(BTreeSet::from([unplaced()]));
        }
        let mut out = direct.clone();
        for r in &direct {
            out.extend(self.ancestors(pool, r)?);
        }
        Ok(out)
    }
}

/// **The rows of one compiled write** (the table in the module docs),
/// each handed to `add(pool, region, mode)`. A far end in the projected
/// `spine` namespace names nothing (it has no pool).
pub fn payload_region_rows(
    payload: &CompiledPayload,
    grammars: &dyn Fn(&PoolName) -> Option<PoolGrammar>,
    source: &dyn RegionSource,
    add: &mut dyn FnMut(&PoolName, NodeId, LockMode),
) -> Result<()> {
    let d = Deriver { facts: PayloadFacts::of(payload), source, grammars };
    for part in &payload.parts {
        let pool = &part.pool;
        if project::is_projected(pool) {
            continue;
        }
        let regional = has_regions(&d.grammar(pool)?);
        for delta in &part.deltas {
            if !regional {
                add(pool, unplaced(), LockMode::Link);
                if let Delta::AddEdge { dst, .. } | Delta::RemoveEdge { dst, .. } = delta {
                    if dst.pool != *pool && !project::is_projected(&dst.pool) {
                        let far = d.star(&dst.pool, &dst.id)?;
                        link_all(add, &dst.pool, far);
                    }
                }
                continue;
            }
            match delta {
                Delta::CreateNode { id, node_type, .. } => {
                    let mut set = d.star(pool, id)?;
                    if node_type == REGION_TYPE {
                        // A new region is nobody else's yet: its parents
                        // and theirs.
                        set.remove(id);
                    }
                    link_all(add, pool, set);
                }
                Delta::SetField { node, .. } | Delta::Seal { node } => {
                    if d.is_region(pool, node)? {
                        add(pool, node.clone(), LockMode::Write);
                        let mut set = d.star(pool, node)?;
                        set.remove(node);
                        link_all(add, pool, set);
                    } else {
                        link_all(add, pool, d.star(pool, node)?);
                    }
                }
                Delta::AddEdge { src, kind, dst, .. } | Delta::RemoveEdge { src, kind, dst } => {
                    let local = dst.pool == *pool;
                    if local && (kind == IN_REGION || kind == REJECTED_BY) {
                        let mut set = d.star(pool, src)?;
                        set.insert(dst.id.clone());
                        set.extend(d.ancestors(pool, &dst.id)?);
                        link_all(add, pool, set);
                    } else if local && kind == NARROWS {
                        add(pool, src.clone(), LockMode::Write);
                        let mut set = d.star(pool, src)?;
                        set.remove(src);
                        set.insert(dst.id.clone());
                        set.extend(d.ancestors(pool, &dst.id)?);
                        link_all(add, pool, set);
                    } else {
                        link_all(add, pool, d.star(pool, src)?);
                        if !project::is_projected(&dst.pool) {
                            let far = d.star(&dst.pool, &dst.id)?;
                            link_all(add, &dst.pool, far);
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

fn link_all(add: &mut dyn FnMut(&PoolName, NodeId, LockMode), pool: &PoolName, set: BTreeSet<NodeId>) {
    for r in set {
        add(pool, r, LockMode::Link);
    }
}

/// [`payload_region_rows`] collected: one row per (pool, region), `write`
/// where any delta named it `write`, sorted.
pub fn rows_of(payload: &CompiledPayload, grammars: &dyn Fn(&PoolName) -> Option<PoolGrammar>, source: &dyn RegionSource) -> Result<Vec<RegionRow>> {
    let mut m: BTreeMap<(PoolName, NodeId), LockMode> = BTreeMap::new();
    payload_region_rows(payload, grammars, source, &mut |pool, region, mode| {
        let e = m.entry((pool.clone(), region)).or_insert(mode);
        if mode == LockMode::Write {
            *e = LockMode::Write;
        }
    })?;
    Ok(m.into_iter().map(|((pool, region), mode)| RegionRow { pool, region, mode }).collect())
}

/// **The revision of region `region`** (module docs): a digest, as 16 hex
/// digits, of its header revision, its parents, its children and its direct
/// members with their states and the nodes that supersede them. `None` when the snapshot holds no such
/// region. [`UNPLACED`] is answered over the content nodes that attest no
/// region.
pub fn region_rev(snap: &Snapshot<'_>, pool: &PoolName, grammar: &PoolGrammar, region: &NodeId) -> Result<Option<String>> {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    let mut put = |s: &str| {
        h.update(s.as_bytes());
        h.update([0u8]);
    };
    if region.0 == UNPLACED {
        put("unplaced");
        let mut members: Vec<(String, String)> = Vec::new();
        for t in grammar.node_types.keys().filter(|t| t.as_str() != REGION_TYPE) {
            for n in snap.nodes_of(t, None)? {
                if !has_regions(grammar) || snap.edges_from(&n.id, Some(IN_REGION))?.is_empty() {
                    members.push((n.id.0.clone(), n.state.clone().unwrap_or_default()));
                }
            }
        }
        members.sort();
        for (id, state) in members {
            put(&id);
            put(&state);
        }
    } else {
        let Some(rec) = snap.node(region)? else { return Ok(None) };
        if rec.node_type != REGION_TYPE {
            return Ok(None);
        }
        put("region");
        put(&region.0);
        put(&rec.rev.to_string());
        put(rec.state.as_deref().unwrap_or(""));
        let parents = snapshot_parents(snap, pool, region)?;
        put("parents");
        for p in &parents {
            put(&p.0);
        }
        let q = crate::pool::QualifiedNodeId { pool: pool.clone(), id: region.clone() };
        let mut children: Vec<String> = snap.edges_to(&q, Some(NARROWS))?.into_iter().map(|e| e.src.0).collect();
        children.sort();
        put("children");
        for c in &children {
            put(c);
        }
        let mut members: Vec<(String, String, Vec<String>)> = Vec::new();
        for e in snap.edges_to(&q, Some(IN_REGION))? {
            let state = snap.node(&e.src)?.and_then(|n| n.state).unwrap_or_default();
            let m = crate::pool::QualifiedNodeId { pool: pool.clone(), id: e.src.clone() };
            let mut by: Vec<String> = snap.edges_to(&m, Some(SUPERSEDES))?.into_iter().map(|x| x.src.0).collect();
            by.sort();
            members.push((e.src.0, state, by));
        }
        members.sort();
        put("members");
        for (id, state, by) in members {
            put(&id);
            put(&state);
            for b in by {
                put(&b);
            }
            put("");
        }
    }
    let digest = h.finalize();
    Ok(Some(digest.iter().take(8).map(|b| format!("{b:02x}")).collect()))
}

/// **The head revision of region `region`** (module docs): a digest, as 16
/// hex digits, of the region node's revision and state, its parents, its
/// children and the nodes that supersede it; no member enters it. `None`
/// when the snapshot holds no such region (and for [`UNPLACED`], which has
/// no boundary).
pub fn region_head_rev(snap: &Snapshot<'_>, pool: &PoolName, region: &NodeId) -> Result<Option<String>> {
    use sha2::{Digest, Sha256};
    if region.0 == UNPLACED {
        return Ok(None);
    }
    let Some(rec) = snap.node(region)? else { return Ok(None) };
    if rec.node_type != REGION_TYPE {
        return Ok(None);
    }
    let mut h = Sha256::new();
    let mut put = |s: &str| {
        h.update(s.as_bytes());
        h.update([0u8]);
    };
    put("region-head");
    put(&region.0);
    put(&rec.rev.to_string());
    put(rec.state.as_deref().unwrap_or(""));
    put("parents");
    for p in &snapshot_parents(snap, pool, region)? {
        put(&p.0);
    }
    let q = crate::pool::QualifiedNodeId { pool: pool.clone(), id: region.clone() };
    let mut children: Vec<String> = snap.edges_to(&q, Some(NARROWS))?.into_iter().map(|e| e.src.0).collect();
    children.sort();
    put("children");
    for c in &children {
        put(c);
    }
    let mut by: Vec<String> = snap.edges_to(&q, Some(SUPERSEDES))?.into_iter().map(|e| e.src.0).collect();
    by.sort();
    put("superseded_by");
    for b in &by {
        put(b);
    }
    let digest = h.finalize();
    Ok(Some(digest.iter().take(8).map(|b| format!("{b:02x}")).collect()))
}

/// **What a writer installs to hold writes to the regions others hold.**
/// Asked once per commit-mode statement with the rows it names; `Err`
/// refuses the whole statement before its row is appended, with the facts
/// of the refusal (who holds what, since when, until when).
pub trait RegionGuard: Send + Sync {
    fn check(&self, initiator: &Initiator, rows: &[RegionRow]) -> std::result::Result<(), (String, Map<String, Value>)>;
}

/// Ask `guard` about `payload` (the door calls this after the flag rule,
/// for a commit only).
pub(crate) fn check_guard(cauldron: &crate::Cauldron, guard: &dyn RegionGuard, initiator: &Initiator, payload: &CompiledPayload) -> Result<()> {
    let mut names: Vec<PoolName> = Vec::new();
    for part in &payload.parts {
        if !project::is_projected(&part.pool) && !names.contains(&part.pool) {
            names.push(part.pool.clone());
        }
        for d in &part.deltas {
            if let Delta::AddEdge { dst, .. } | Delta::RemoveEdge { dst, .. } = d {
                if !project::is_projected(&dst.pool) && !names.contains(&dst.pool) {
                    names.push(dst.pool.clone());
                }
            }
        }
    }
    if names.is_empty() {
        return Ok(());
    }
    let open = crate::read::open_pools(cauldron, &names)?;
    let snaps = crate::read::open_snapshots(&open)?;
    let lookup = |p: &PoolName| cauldron.grammar(p).cloned();
    let rows = rows_of(payload, &lookup, &snaps)?;
    drop(snaps);
    if rows.is_empty() {
        return Ok(());
    }
    guard.check(initiator, &rows).map_err(|(message, fields)| SpineError::RegionLocked { message, fields })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_row_says_its_mode() {
        let r = RegionRow { pool: PoolName::new("ws"), region: NodeId("R-1".into()), mode: LockMode::Write };
        assert_eq!(r.mode_word(), "write");
        let r = RegionRow { mode: LockMode::Link, ..r };
        assert_eq!(r.mode_word(), "link");
    }
}
