//! The ladle compiler — staged-coverage check, claim-footprint derivation,
//! and compile-to-deltas.
//!
//! The clause-to-delta mapping is TABLE data, and the commit interior is
//! agnostic to it — a later grammar edit rewrites a
//! table, never the commit machinery.
//!
//! # The three tables
//!
//! Every clause form this build understands is a ROW, and each row is a form
//! NAME plus one constructor function:
//!
//! - [`MUTATION_TABLE`] — CREATE node, CREATE edge, SET, REMOVE edge, each
//!   to its interior deltas.
//! - [`EXPECT_TABLE`] — the three expectation forms, each to its premise
//!   record (and its expected-versus-found refusal).
//! - [`CONDITION_TABLE`] — the WHERE forms that carry a premise record.
//! - [`COVERAGE_TABLE`] — the forms this build stages OUT, each with the
//!   detector that spots it; its names are [`STAGED_OUT_FORMS`].
//!
//! Adding a clause form is a row plus a constructor; NOTHING in
//! [`crate::delta`], [`crate::pool`] or the commit path changes. That is the
//! cheapness posture the design demands, made literal.
//!
//! # This stage is PURE
//!
//! Inputs (statement, parameters, match set, transaction hash, pool
//! grammars) in; deltas, premises, claims, projections and witnesses out.
//! It opens nothing, locks nothing, and lands nothing — `exec`
//! owns every effect.

use crate::delta::{CompiledPart, CompiledPayload, CountOp, Delta, Premise};
use crate::envelope::{Bindings, TxHash};
use crate::error::{Result, SpineError};
use crate::ladle::ast::*;
use crate::ladle::parse::malformed;
use crate::locks::{Claim, ClaimSet, LockMode, Resource};
use crate::pool::{NodeId, NodeRecord, QualifiedNodeId};
use crate::project;
use crate::read::MatchSet;
use crate::schema_def::{Direction, PoolGrammar, PoolName};
use crate::witness::{ExpectationKind, Witness};
use crate::Returned;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

/// The FROZEN form names of the staged-coverage table (every
/// grammatically valid but unimplemented form refuses BY NAME, carried in
/// [`crate::witness::Witness::UnimplementedForm`]). Implementations refuse with
/// exactly these strings; suites assert on them.
///
/// Staged OUT of the first build:
/// - `"match:field-value"` — a MATCH field-map entry on a field other than
///   `id` or `state` (the staged match forms are by identifier and by
///   pool+type+state).
/// - `"match:multi-hop"` — a binding used across more than one hop pattern
///   (paths of length > 1).
/// - `"where:edge-existence"` — the EXISTS/NOT EXISTS condition. The staged
///   list says "the condition clause
///   over bound fields", which this build reads as excluding edge
///   existence; the table entry flips without contract change if that
///   reading is wrong.
/// - `"match:projected-pattern"` — any MATCH into the projected `spine`
///   namespace OTHER than by identifier: a population scan (by type, by
///   state), a field match, or a hop with a projected end. See
///   [`PROJECTED_PATTERN_FORM`].
/// One more [`crate::witness::Witness::UnimplementedForm`] name exists
/// OUTSIDE this list, because it is not detectable from a parsed
/// statement's shape (a table row needs a detector, and a detector that can
/// never fire would be a lie): [`SCALAR_FUNCTION_FORM`] is refused by the
/// PARSER (no AST shape for a function call exists at all). The
/// refuse-by-name contract covers all of them; this list is
/// specifically the coverage TABLE's shape-detected rows.
pub const STAGED_OUT_FORMS: &[&str] = &[
    PROJECTED_PATTERN_FORM,
    FIELD_VALUE_FORM,
    MULTI_HOP_FORM,
    EDGE_EXISTENCE_FORM,
];

/// A MATCH field-map entry on a field other than `id` or `state`.
pub const FIELD_VALUE_FORM: &str = "match:field-value";
/// A path of length > 1 — chained in one clause, or two hop clauses sharing
/// a binding. Named HERE because the parser refuses the chained spelling by
/// this same name (a coverage gap refuses, it never misreads).
pub const MULTI_HOP_FORM: &str = "match:multi-hop";
/// The EXISTS / NOT EXISTS condition.
pub const EDGE_EXISTENCE_FORM: &str = "where:edge-existence";

/// A MATCH into the projected `spine` namespace by anything but identifier.
/// This ONE frozen name retires the two door gates that used to
/// stand here — `match:projected-namespace` (write door) and
/// `read:projected-namespace` (read door) — which refused EVERY touch of the
/// namespace. What now stages IN, at both doors, is MATCH BY IDENTIFIER:
/// `(g: spine/txn { id: $hash })` resolves the ghost through
/// `project::resolve_ghost` (computed on read, never copied — the
/// projection design: resolves
/// transparently on read, refuses on write). Everything else — by type or
/// state (a population scan over a table that is not a pool), by field, or as
/// either end of a hop (a ghost has no outgoing edges, and its incoming slice
/// is the union across every pool of edges aimed at it, which no snapshot
/// holds) — is this form, refused by name.
///
/// DERIVED DECISION: the by-identifier form follows from the
/// projection's own "stable identifier ... resolved at read time" design. To
/// change it, the seam is this row plus the by-identifier arm
/// of `read::match_head`.
///
/// UNDER A TRANCHE the staged-in set is NARROWER: a ghost's
/// `state` and `parts` are computed on every read from the transaction row
/// and the spine's own lifecycle flips move them, which no lock row can hold
/// still — so a tranche, which cannot restart its landing, may not rely on
/// them. [`check_projected_under_tranche`] refuses, under this same frozen
/// name, a `state` entry on a projected head, a `WHERE` or `EXPECT` over a
/// projected binding, and a `RETURN` of a projected binding's field other
/// than `id`. What stays in under a tranche is what cannot move: the head by
/// `id` naming an EXISTING row, as an edge target or returned by `id`. The
/// ordinary door is unchanged.
pub const PROJECTED_PATTERN_FORM: &str = "match:projected-pattern";

/// A scalar function call in a condition or value position. The grammar
/// names "scalar functions" among WHERE's parameters; the staged list
/// ("the condition clause over bound fields") excludes them, so the form
/// refuses BY NAME — it used to fall out of the parser as
/// a bare syntax error, which is a misread of a form the grammar names.
/// Refused by the PARSER, not the coverage
/// table: no AST shape for a call exists, so no detector could fire.
pub const SCALAR_FUNCTION_FORM: &str = "where:scalar-function";

/// One staged-out form: its frozen name and the detector that spots it in a
/// parsed statement. DATA — flipping a form into the build deletes its row.
pub struct CoverageRule {
    pub form: &'static str,
    pub detect: fn(&Statement) -> bool,
}

/// The coverage table. Rows are checked in order, so the
/// refusal a statement gets is deterministic when it breaks two rules. The
/// projected row is FIRST because it is the more specific fact: a field
/// match into `spine` is both `match:field-value` and a pattern into the
/// projection, and the name that tells the caller most is the namespace's.
/// A statement touching no projected head is unaffected by the order.
pub const COVERAGE_TABLE: &[CoverageRule] = &[
    CoverageRule {
        form: PROJECTED_PATTERN_FORM,
        detect: detect_projected_pattern,
    },
    CoverageRule {
        form: FIELD_VALUE_FORM,
        detect: detect_field_value_match,
    },
    CoverageRule {
        form: MULTI_HOP_FORM,
        detect: detect_multi_hop,
    },
    CoverageRule {
        form: EDGE_EXISTENCE_FORM,
        detect: detect_edge_existence,
    },
];

/// The projected-pattern detector: a head into `spine` without an `id`
/// entry, or a hop any of whose ends (fresh head or earlier-bound binding)
/// lives in `spine`.
pub fn detect_projected_pattern(stmt: &Statement) -> bool {
    let types = stmt.binding_types();
    for m in &stmt.matches {
        match m {
            MatchClause::Node(h) => {
                if project::is_projected(&h.pool) && h.field(FIELD_ID).is_none() {
                    return true;
                }
            }
            MatchClause::Hop(_) => {
                for b in m.bindings() {
                    if types.get(&b.0).is_some_and(|(p, _)| project::is_projected(p)) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

fn detect_field_value_match(stmt: &Statement) -> bool {
    stmt.matches
        .iter()
        .flat_map(|m| m.heads())
        .any(|h| h.fields.iter().any(|(f, _)| f != FIELD_ID && f != FIELD_STATE))
}

fn detect_multi_hop(stmt: &Statement) -> bool {
    let mut hops_per_binding: BTreeMap<String, BTreeSet<usize>> = BTreeMap::new();
    for (i, m) in stmt.matches.iter().enumerate() {
        if let MatchClause::Hop(_) = m {
            for b in m.bindings() {
                hops_per_binding.entry(b.0.clone()).or_default().insert(i);
            }
        }
    }
    hops_per_binding.values().any(|hops| hops.len() > 1)
}

fn detect_edge_existence(stmt: &Statement) -> bool {
    fn walk(c: &Condition) -> bool {
        match c {
            Condition::Exists { .. } => true,
            Condition::Compare { .. } => false,
            Condition::Not(i) => walk(i),
            Condition::And(ps) | Condition::Or(ps) => ps.iter().any(walk),
        }
    }
    stmt.wheres.iter().any(walk)
}

/// Refuse, by name, every clause form of `stmt` the first build stages out
/// (the staged coverage cut). Runs after parse, before matching.
pub fn check_coverage(stmt: &Statement) -> Result<()> {
    for rule in COVERAGE_TABLE {
        if (rule.detect)(stmt) {
            return Err(SpineError::refuse(Witness::UnimplementedForm {
                form: rule.form.to_string(),
            }));
        }
    }
    Ok(())
}

/// THE TRANCHE-ONLY PROJECTED CHECK (the projected namespace
/// under a tranche). Runs after
/// [`check_coverage`], for a child (`exec::stage`) and for a read under key
/// (`read::read_under`) — and from nowhere on the ordinary door, whose
/// staged-in projected forms stay as they are.
///
/// Refuses, as the bare witness `unimplemented_form { form:
/// "match:projected-pattern" }` (the variant has one field and carries no
/// prose), every form through which a tranche would rely on a ghost's
/// moving fields:
/// - a `state` entry on a projected head (`{ id: $h, state: "pending" }`);
/// - a `WHERE` over a projected binding — any comparison whose field
///   reference names it, any edge-existence premise with it at either end;
/// - an `EXPECT` over a projected binding (`REV`, value, or count);
/// - a `RETURN` of a projected binding's field other than `id`, or of its
///   `REV`.
///
/// Why: a ghost's `state` (`pending` → `decided` → `cleared`) and `parts`
/// are computed on every read from the transaction row, and the spine's own
/// flips move them; no lock row holds a spine row still, so a landing's
/// re-validation could refuse a tranche that did nothing wrong. The
/// `RETURN` refusal is the "a read under key answered it" half,
/// over-restricted the same way. The absent-row half of the same closure —
/// a projected head must name a row that EXISTS — is the ghost guard run
/// over every projected head (`exec::guard_projected_heads`), not this
/// function: it needs the spine, and this check is shape-only.
pub fn check_projected_under_tranche(stmt: &Statement) -> Result<()> {
    let types = stmt.binding_types();
    let projected = |b: &Binding| -> bool {
        types
            .get(&b.0)
            .is_some_and(|(pool, _)| project::is_projected(pool))
    };
    let refuse = || -> Result<()> { Err(unimplemented_form(PROJECTED_PATTERN_FORM)) };

    for head in stmt.matches.iter().flat_map(|m| m.heads()) {
        if project::is_projected(&head.pool) && head.field(FIELD_STATE).is_some() {
            return refuse();
        }
    }
    fn names_projected(c: &Condition, projected: &dyn Fn(&Binding) -> bool) -> bool {
        match c {
            Condition::Compare { left, .. } => projected(&left.binding),
            Condition::Exists { from, to, .. } => {
                projected(from) || to.as_ref().is_some_and(|b| projected(b))
            }
            Condition::Not(inner) => names_projected(inner, projected),
            Condition::And(ps) | Condition::Or(ps) => {
                ps.iter().any(|p| names_projected(p, projected))
            }
        }
    }
    if stmt.wheres.iter().any(|c| names_projected(c, &projected)) {
        return refuse();
    }
    for e in &stmt.expects {
        let b = match e {
            Expectation::Revision { binding, .. } | Expectation::Count { binding, .. } => binding,
            Expectation::Value { field, .. } => &field.binding,
        };
        if projected(b) {
            return refuse();
        }
    }
    if let Some(ret) = &stmt.returns {
        for item in &ret.items {
            if !projected(item.binding()) {
                continue;
            }
            let by_id_only = matches!(
                &item.expr,
                ReturnExpr::Field(FieldRef { path, .. }) if path.len() == 1 && path[0] == FIELD_ID
            );
            if !by_id_only {
                return refuse();
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The claim footprint — statically from the text, then concretely from the
// match (the second, load-bearing job of MATCH).
// ---------------------------------------------------------------------------

/// What a statement's TEXT alone says it can touch (the pattern's
/// footprint — which pools, types, edge kinds and fields it can possibly
/// touch — is readable from its text). Derived BEFORE matching; the door uses
/// `pools` to know which snapshots to open.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Footprint {
    pub pools: BTreeSet<PoolName>,
    /// `(pool, node type)` pairs the statement names.
    pub types: BTreeSet<(PoolName, String)>,
    /// `(pool of the edge's source end, edge kind)`.
    pub edge_kinds: BTreeSet<(PoolName, String)>,
    /// `(pool, node type, dotted field path)` — every field the statement
    /// reads or writes.
    pub fields: BTreeSet<(PoolName, String, String)>,
}

/// The footprint's name for the WHOLE-NODE revision — `REV(b)` with no field
/// (an earlier draft recorded it as the `id` field, which
/// named a field the clause never reads). `rev` is one of the machine-owned
/// names in [`crate::schema_def::MACHINE_OWNED_FIELDS`], so the footprint
/// stays honest about touching bookkeeping rather than a declared field.
pub const FIELD_REV: &str = "rev";

/// Derive the static footprint from the statement's own text (pre-match).
pub fn footprint(stmt: &Statement) -> Footprint {
    let types = stmt.binding_types();
    let mut fp = Footprint::default();
    for (pool, ty) in types.values() {
        fp.pools.insert(pool.clone());
        fp.types.insert((pool.clone(), ty.clone()));
    }
    let note_field = |fp: &mut Footprint, binding: &Binding, path: &str| {
        if let Some((pool, ty)) = types.get(&binding.0) {
            fp.fields
                .insert((pool.clone(), ty.clone(), path.to_string()));
        }
    };
    for m in &stmt.matches {
        for h in m.heads() {
            for (f, _) in &h.fields {
                note_field(&mut fp, &h.binding, f);
            }
        }
        if let MatchClause::Hop(hop) = m {
            let src = match hop.arrow {
                ArrowDir::LeftToRight => &hop.left,
                ArrowDir::RightToLeft => &hop.right,
            };
            let src_binding = match src {
                PatternEnd::Head(h) => &h.binding,
                PatternEnd::Bound(b) => b,
            };
            if let Some((pool, _)) = types.get(&src_binding.0) {
                fp.edge_kinds.insert((pool.clone(), hop.kind.clone()));
            }
        }
    }
    for c in &stmt.wheres {
        collect_condition_fields(c, &types, &mut fp);
    }
    for e in &stmt.expects {
        match e {
            Expectation::Revision { binding, field, .. } => {
                note_field(&mut fp, binding, field.as_deref().unwrap_or(FIELD_REV));
            }
            Expectation::Value { field, .. } => {
                note_field(&mut fp, &field.binding, &field.path.join("."));
            }
            Expectation::Count { .. } => {}
        }
    }
    for m in &stmt.mutations {
        match m {
            Mutation::CreateNode(h) | Mutation::ImportNode(h) => {
                for (f, _) in &h.fields {
                    note_field(&mut fp, &h.binding, f);
                }
            }
            Mutation::CreateEdge { from, kind, to, arrow, .. }
            | Mutation::RemoveEdge { from, kind, to, arrow } => {
                let src = match arrow {
                    ArrowDir::LeftToRight => from,
                    ArrowDir::RightToLeft => to,
                };
                if let Some((pool, _)) = types.get(&src.0) {
                    fp.edge_kinds.insert((pool.clone(), kind.clone()));
                }
            }
            Mutation::Set { target, .. } => {
                note_field(&mut fp, &target.binding, &target.path.join("."));
            }
            Mutation::Seal { target } => {
                note_field(&mut fp, target, FIELD_SEALED);
            }
        }
    }
    if let Some(r) = &stmt.returns {
        for item in &r.items {
            match &item.expr {
                ReturnExpr::Field(fr) => note_field(&mut fp, &fr.binding, &fr.path.join(".")),
                ReturnExpr::Rev { binding, field } => {
                    note_field(&mut fp, binding, field.as_deref().unwrap_or(FIELD_REV))
                }
            }
        }
    }
    fp
}

fn collect_condition_fields(
    c: &Condition,
    types: &BTreeMap<String, (PoolName, String)>,
    fp: &mut Footprint,
) {
    match c {
        Condition::Compare { left, .. } => {
            if let Some((pool, ty)) = types.get(&left.binding.0) {
                fp.fields
                    .insert((pool.clone(), ty.clone(), left.path.join(".")));
            }
        }
        Condition::Exists { from, kind, .. } => {
            if let Some((pool, _)) = types.get(&from.0) {
                fp.edge_kinds.insert((pool.clone(), kind.clone()));
            }
        }
        Condition::Not(i) => collect_condition_fields(i, types, fp),
        Condition::And(ps) | Condition::Or(ps) => {
            for p in ps {
                collect_condition_fields(p, types, fp)
            }
        }
    }
}

/// Derive the transaction's COMPLETE claim set from the statement's own
/// text — patterns are pool-qualified, so the footprint (pools, types, edge
/// kinds, fields it can possibly touch) is readable statically (MATCH's
/// second, load-bearing job). Claims are complete AT OPEN; growth
/// restarts.
///
/// The claim table for the common acts: read on a
/// read node's body; connecting an edge takes write on the NEW edge, link on
/// both endpoint slices, read on both endpoint bodies — never a write lock on
/// either endpoint; a create takes write on the fresh node plus link on the
/// pool's type slice; an at-most-one slice is a SCALAR — link forbidden,
/// membership changes are slice WRITES.
///
/// # What this function can and cannot know
///
/// Text alone names POPULATIONS, never identities: a body, an edge and an
/// edge slice are all keyed by node identifier, and no identifier exists
/// until the match has run. So this call returns the type-slice-grain claims
/// the text can prove — the population reads a scanning match performs, the
/// link claims its creates take, the population reads its count expectations
/// hold still — and [`claims_for`] returns the concrete, complete set the
/// door actually acquires, from the same statement plus its match. Both use
/// the same table above; neither invents a claim the other lacks a reason
/// for.
pub fn claim_footprint(
    stmt: &Statement,
    grammars: &dyn Fn(&crate::schema_def::PoolName) -> Option<crate::schema_def::PoolGrammar>,
) -> Result<ClaimSet> {
    let types = stmt.binding_types();
    // Refuses an unknown pool or an undeclared node type by name, before any
    // claim is spoken.
    resolve_grammars(&types, grammars)?;
    let mut b = ClaimBuilder::default();

    for m in &stmt.matches {
        for h in m.heads() {
            // A head that names `id` addresses ONE node and never scans the
            // population; a head without it reads the type slice.
            if h.field(FIELD_ID).is_none() {
                b.add(
                    &h.pool,
                    Resource::TypeSlice {
                        node_type: h.node_type.clone(),
                    },
                    LockMode::Read,
                );
            }
        }
    }
    for h in stmt.created_bindings().into_iter().chain(stmt.imported_bindings()) {
        // A create takes link on the pool's type slice — shared with every
        // other create, so a minting pool never serializes
        // against itself. An IMPORT head lands a node exactly as a
        // create does, so it takes the same link.
        b.add(
            &h.pool,
            Resource::TypeSlice {
                node_type: h.node_type.clone(),
            },
            LockMode::Link,
        );
    }
    for e in &stmt.expects {
        if let Expectation::Count { binding, .. } = e {
            if let Some((pool, ty)) = types.get(&binding.0) {
                b.add(
                    pool,
                    Resource::TypeSlice {
                        node_type: ty.clone(),
                    },
                    LockMode::Read,
                );
            }
        }
    }
    Ok(b.finish())
}

/// The CONCRETE, complete claim set — the claim table applied to what the
/// match actually bound and what the compilation actually emits. This is the
/// set the door acquires in spine-sequence order.
///
/// The door needs both this and the static half — see [`claim_footprint`]'s
/// note.
pub fn claims_for(
    stmt: &Statement,
    matched: &MatchSet,
    payload: &CompiledPayload,
    grammars: &dyn Fn(&PoolName) -> Option<PoolGrammar>,
) -> Result<ClaimSet> {
    let types = stmt.binding_types();
    let known = resolve_grammars(&types, grammars)?;
    let mut b = ClaimBuilder::default();

    // Everything the match bound is read: read on its body —
    // except a GHOST. A by-identifier match into `spine` takes NO claim:
    // nothing can ever write the projection (the pen refuses every
    // write to it), and a spine row's lifecycle flips are the
    // spine's own mechanics, not a lockable pool resource. A read lock on a
    // body no pool holds would be a lock row that protects nothing.
    for nodes in matched.bindings.values() {
        for n in nodes {
            if project::is_projected(&n.pool) {
                continue;
            }
            b.add(
                &n.pool,
                Resource::Body {
                    node: n.node.id.clone(),
                },
                LockMode::Read,
            );
        }
    }
    // A count expectation needs the population held still.
    for e in &stmt.expects {
        if let Expectation::Count { binding, .. } = e {
            if let Some((pool, ty)) = types.get(&binding.0) {
                b.add(
                    pool,
                    Resource::TypeSlice {
                        node_type: ty.clone(),
                    },
                    LockMode::Read,
                );
            }
        }
    }
    for (pi, part) in payload.parts.iter().enumerate() {
        for (di, delta) in part.deltas.iter().enumerate() {
            match delta {
                Delta::CreateNode {
                    id,
                    node_type,
                    state,
                    ..
                } => {
                    b.add(
                        &part.pool,
                        Resource::Body { node: id.clone() },
                        LockMode::Write,
                    );
                    b.add(
                        &part.pool,
                        Resource::TypeSlice {
                            node_type: node_type.clone(),
                        },
                        LockMode::Link,
                    );
                    // THE STATE SLICE the created node ENTERS: `link` on `sslice:<type>:<s>` for a
                    // CREATE or IMPORT with `state: Some(s)`. With `None`
                    // the node enters no slice and takes no slice link —
                    // whether the type has a vocabulary (it lands `NULL`,
                    // in no scanned set) or not (no state slice can exist
                    // for it). Redundant under today's table — every
                    // filtered scan also holds `tslice:<type> read` and
                    // every create takes `tslice:<type> link` — and kept as
                    // a deliberate over-restriction, against a future
                    // loosening of the type-slice row.
                    if let Some(s) = state {
                        b.add(
                            &part.pool,
                            Resource::StateSlice {
                                node_type: node_type.clone(),
                                state: s.clone(),
                            },
                            LockMode::Link,
                        );
                    }
                }
                // A SET of `state` — the path `pool::set_target` routes to
                // its `State` arm, the one that rewrites the `nodes.state`
                // column — moves the node ACROSS state slices: `link` on the slice
                // of the state it ENTERS and on the slice of the state it
                // LEAVES, beside the body write every SET takes. The type
                // and the leaving state are read off the matched record of
                // that node (a SET addresses a bound binding), or off an
                // earlier `CreateNode` of the same node in this payload; a
                // found record whose `state` is `None` leaves no slice, so
                // such a SET takes the entering link only. A node found in
                // NEITHER place — a hand-built part through a direct caller
                // — takes the fallback: `link` on every state slice of
                // every type in that pool that declares a vocabulary,
                // over-restricted and harmless (the door never reaches it:
                // `exec::contested_claims` derives from the pre-grant match
                // set). A hand-built dotted path under `state` lands as an
                // ordinary field row named `state` and crosses no slice.
                // `link`, not `write`: two foreign state changes of
                // different nodes commute and share the slice, and `link`
                // conflicts with the `read` a filtered scan holds, which is
                // all the tranche needs.
                Delta::SetField { node, path, value } if path.len() == 1 && path[0] == FIELD_STATE => {
                    b.add(
                        &part.pool,
                        Resource::Body { node: node.clone() },
                        LockMode::Write,
                    );
                    let found: Option<(String, Option<String>)> = matched
                        .bindings
                        .values()
                        .flatten()
                        .find(|n| n.pool == part.pool && n.node.id == *node)
                        .map(|n| (n.node.node_type.clone(), n.node.state.clone()))
                        .or_else(|| {
                            // An EARLIER `CreateNode` of the same node in
                            // this payload, in the same pool: the deltas
                            // before (part `pi`, delta `di`) in payload order.
                            for (qi, q) in payload.parts.iter().enumerate() {
                                if q.pool != part.pool {
                                    continue;
                                }
                                for (ei, d) in q.deltas.iter().enumerate() {
                                    if (qi, ei) >= (pi, di) {
                                        return None;
                                    }
                                    if let Delta::CreateNode {
                                        id,
                                        node_type,
                                        state,
                                        ..
                                    } = d
                                    {
                                        if id == node {
                                            return Some((node_type.clone(), state.clone()));
                                        }
                                    }
                                }
                            }
                            None
                        });
                    match found {
                        Some((node_type, leaving)) => {
                            if let Some(entering) = value.as_str() {
                                b.add(
                                    &part.pool,
                                    Resource::StateSlice {
                                        node_type: node_type.clone(),
                                        state: entering.to_string(),
                                    },
                                    LockMode::Link,
                                );
                            }
                            if let Some(leaving) = leaving {
                                b.add(
                                    &part.pool,
                                    Resource::StateSlice {
                                        node_type,
                                        state: leaving,
                                    },
                                    LockMode::Link,
                                );
                            }
                        }
                        None => {
                            let grammar = known
                                .get(&part.pool)
                                .cloned()
                                .or_else(|| grammars(&part.pool));
                            if let Some(g) = grammar {
                                for (ty, def) in &g.node_types {
                                    for st in &def.states {
                                        b.add(
                                            &part.pool,
                                            Resource::StateSlice {
                                                node_type: ty.clone(),
                                                state: st.clone(),
                                            },
                                            LockMode::Link,
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
                // A seal writes the node's record (the flag, the rev), so it
                // takes the body WRITE lock a SET takes: a SEAL and a body
                // SET on one node serialize, and the second is re-judged
                // under its grant against the first's landed record.
                Delta::SetField { node, .. } | Delta::Seal { node } => {
                    b.add(
                        &part.pool,
                        Resource::Body { node: node.clone() },
                        LockMode::Write,
                    );
                }
                Delta::AddEdge {
                    src, kind, dst, ..
                }
                | Delta::RemoveEdge { src, kind, dst } => {
                    // Through `Cauldron::submit` this arm cannot fire: the
                    // payload is `compile`'s own, and `compile` refuses an
                    // undeclared kind first (the CREATE/REMOVE-edge site,
                    // same tag). It is NOT dead: `claims_for` is public and a
                    // direct caller may hand it a payload it did not compile
                    // — `tests/grammar_v6_corrections.rs`
                    // `claims_for_refuses_an_undeclared_edge_kind_in_a_direct_payload`
                    // reaches it and was forced red against it.
                    let def = known
                        .get(&part.pool)
                        .and_then(|g| g.edge_kinds.get(kind))
                        .ok_or_else(|| {
                            grammar_fault(
                                "unknown_edge_kind",
                                &part.pool,
                                format!("edge kind `{kind}` is not declared by this pool's grammar"),
                            )
                        })?;
                    b.add(
                        &part.pool,
                        Resource::Edge {
                            src: src.clone(),
                            kind: kind.clone(),
                            dst: dst.clone(),
                        },
                        LockMode::Write,
                    );
                    // An at-most-one slice is a SCALAR: link is forbidden on
                    // it and every membership change is a slice WRITE. The
                    // rule lives in ONE place — `locks::membership_mode` —
                    // and this call site consumes it rather than re-deriving
                    // it (prose copies of the rule would drift).
                    b.add(
                        &part.pool,
                        Resource::EdgeSlice {
                            node: src.clone(),
                            kind: kind.clone(),
                            direction: Direction::Out,
                        },
                        crate::locks::membership_mode(def.at_most_one_per_from),
                    );
                    // Read on the source body — never a write lock on an
                    // endpoint (a deliberate carve-out).
                    b.add(
                        &part.pool,
                        Resource::Body { node: src.clone() },
                        LockMode::Read,
                    );
                    // The FAR end's in-slice and body — unless the far end is
                    // a GHOST, which takes no claim at all: a ghost
                    // has no body any pool holds, its incoming slice is the
                    // union across every pool of edges aimed at it (no pool
                    // stores that slice, and `check_wellformed` refuses
                    // at-most-one on any reach, so nothing ever needs it
                    // held still), and nothing can ever write the projection.
                    if !project::is_projected(&dst.pool) {
                        b.add(
                            &dst.pool,
                            Resource::EdgeSlice {
                                node: dst.id.clone(),
                                kind: kind.clone(),
                                direction: Direction::In,
                            },
                            // Same one-home rule as the out-slice above.
                            crate::locks::membership_mode(def.at_most_one_per_to),
                        );
                        b.add(
                            &dst.pool,
                            Resource::Body {
                                node: dst.id.clone(),
                            },
                            LockMode::Read,
                        );
                    }
                }
            }
        }
    }
    Ok(b.finish())
}

/// THE TRANCHE'S READ-SIDE ADDITIONS (rows 2, 3 and 5 of the tranche claim
/// table's six read rows): what a child's or a
/// read-under-key's claim set needs BEYOND [`claim_footprint`] (row 1 and
/// row 6 — the type slices a scanning head reads and a count expectation
/// holds still) and the bound bodies (`read::body_claims` / [`claims_for`]'s
/// read on everything the match bound — row 4):
///
/// - **a filtered scan — a `MATCH` head by type narrowed by `state`, naming
///   no `id`** (row 2) → `read` on the state slice `sslice:<type>:<state>`, the
///   population `Pool::nodes_of(type, Some(state))` read. The matcher binds
///   only what survives the hop, so a candidate it read and did not bind
///   is held by nothing else; a foreign `SET` moving an unbound node of the
///   type INTO the scanned state (or a bound one out of it) takes `link` on
///   that slice ([`claims_for`]) and conflicts. The state is read off the
///   head's text with the parameters resolved — so the claim is in the
///   FIRST claim set, before any grant — and a non-text value, which the
///   matcher refuses by name, names no slice here. A head with `id` AND
///   `state` takes no state slice: its one body is held by the row below.
/// - **a `MATCH` head by `id`, whether or not it bound** (row 3) → `read`
///   on `body:<id>`. [`claims_for`] reads only what bound; a head that bound
///   NOTHING could gain its node between staging and landing, and the
///   landing's re-match would then bind one and refuse the tranche.
/// - **every slice a hop traversed** (row 5) → `read` on
///   `eslice:<tail>:<kind>:out` for each tail-end candidate and
///   `eslice:<head>:<kind>:in` for each head-end candidate, taken from the
///   matcher's own trace ([`crate::read::HopTrace`]) BEFORE the hop narrowed
///   either end — a candidate the hop dropped could gain a connecting edge.
///
/// All three exist because an ordinary escalated read may restart and a
/// tranche's landing may not. A head into the projected `spine`
/// namespace takes no claim, as everywhere else (and under a tranche a
/// filtered scan of it is refused by [`check_projected_under_tranche`]
/// before any claim is spoken). `read` on a slice conflicts with the
/// `link`/`write` a membership change takes, which is what holds the
/// traversal, the population and the scanned state still.
pub fn tranche_read_claims(
    stmt: &Statement,
    params: &Bindings,
    hops: &[crate::read::HopTrace],
) -> Result<ClaimSet> {
    let mut b = ClaimBuilder::default();
    for m in &stmt.matches {
        for h in m.heads() {
            if project::is_projected(&h.pool) {
                continue;
            }
            let Some(expr) = h.field(FIELD_ID) else {
                // No `id`: a scan. Narrowed by `state`, it is a FILTERED
                // SCAN and holds the state slice it read (row 2).
                if let Some(state) = h.field(FIELD_STATE) {
                    let value = resolve_value(state, params)?;
                    if let Some(s) = value.as_str() {
                        b.add(
                            &h.pool,
                            Resource::StateSlice {
                                node_type: h.node_type.clone(),
                                state: s.to_string(),
                            },
                            LockMode::Read,
                        );
                    }
                }
                continue;
            };
            let value = resolve_value(expr, params)?;
            // A non-text `id` is refused by the matcher by name; here it
            // simply names no body to hold.
            if let Some(id) = value.as_str() {
                b.add(
                    &h.pool,
                    Resource::Body {
                        node: NodeId(id.to_string()),
                    },
                    LockMode::Read,
                );
            }
        }
    }
    for hop in hops {
        for (pool, node) in &hop.src {
            if project::is_projected(pool) {
                continue;
            }
            b.add(
                pool,
                Resource::EdgeSlice {
                    node: node.clone(),
                    kind: hop.kind.clone(),
                    direction: Direction::Out,
                },
                LockMode::Read,
            );
        }
        for (pool, node) in &hop.dst {
            if project::is_projected(pool) {
                continue;
            }
            b.add(
                pool,
                Resource::EdgeSlice {
                    node: node.clone(),
                    kind: hop.kind.clone(),
                    direction: Direction::In,
                },
                LockMode::Read,
            );
        }
    }
    Ok(b.finish())
}

/// Accumulates claims, collapsing two modes on ONE resource to the mode that
/// satisfies both. Interior choice (recorded): `read`+`link` on the same
/// resource escalates to `write` — the two conflict by the lock table's own
/// compatibility rule, and one transaction holding both must hold the
/// stronger, never the weaker.
#[derive(Default)]
struct ClaimBuilder {
    modes: BTreeMap<(PoolName, Resource), LockMode>,
}

impl ClaimBuilder {
    fn add(&mut self, pool: &PoolName, resource: Resource, mode: LockMode) {
        let key = (pool.clone(), resource);
        let merged = match self.modes.get(&key) {
            None => mode,
            Some(existing) => combine_modes(*existing, mode),
        };
        self.modes.insert(key, merged);
    }

    fn finish(self) -> ClaimSet {
        let mut set = ClaimSet::new();
        for ((pool, resource), mode) in self.modes {
            set.insert(Claim {
                pool,
                resource,
                mode,
            });
        }
        set
    }
}

fn combine_modes(a: LockMode, b: LockMode) -> LockMode {
    use LockMode::*;
    if a == b {
        return a;
    }
    match (a, b) {
        (Write, _) | (_, Write) => Write,
        (Read, Link) | (Link, Read) => Write,
        (UpgradeIntent, Link) | (Link, UpgradeIntent) => Write,
        (UpgradeIntent, Read) | (Read, UpgradeIntent) => UpgradeIntent,
        _ => Write,
    }
}

fn resolve_grammars(
    types: &BTreeMap<String, (PoolName, String)>,
    grammars: &dyn Fn(&PoolName) -> Option<PoolGrammar>,
) -> Result<BTreeMap<PoolName, PoolGrammar>> {
    let mut out: BTreeMap<PoolName, PoolGrammar> = BTreeMap::new();
    for (pool, node_type) in types.values() {
        if !out.contains_key(pool) {
            // The projected namespace's grammar is CODE, not a pin in a file
            // — so a head into `spine` resolves against
            // it exactly as a head into a real pool resolves against that
            // pool's pin: `spine/txn` is a declared type, `spine/anything`
            // else is `unknown_node_type` in pool `spine`.
            //
            // A pool the cauldron does not hold refuses with the FROZEN tag
            // `foreign_pool_unknown` (ONE fact,
            // ONE tag): "this cauldron does not hold that pool" is the same
            // fact whether the door finds it here, at head resolution, or the
            // validator finds it for a direct caller's cross-pool edge — so it
            // is the same tag, never a second one. Stamped with the pool the
            // statement NAMED: at head resolution there is no source part and
            // no grammar to refuse under, and the named pool is the one the
            // caller has to go and fix. (It used to be untagged prose, which
            // `grammar_fault_tag` answered `None` for.)
            let g = if project::is_projected(pool) {
                project::projected_grammar()
            } else {
                (grammars)(pool).ok_or_else(|| {
                    crate::schema_def::GrammarFault::new(
                        "foreign_pool_unknown",
                        format!("no pool named `{pool}` in this cauldron"),
                    )
                    .refusal(pool, None)
                })?
            };
            out.insert(pool.clone(), g);
        }
        let g = &out[pool];
        if !g.node_types.contains_key(node_type) {
            return Err(grammar_fault(
                "unknown_node_type",
                pool,
                format!("node type `{node_type}` is not declared by this pool's grammar"),
            ));
        }
    }
    Ok(out)
}

/// An UNTAGGED compile-stage refusal. Kept for exactly one fact: "no pinned
/// grammar for this pool" (`compile`'s part loop, `Emit::grammar`), which is
/// unreachable through the door — `resolve_grammars` refuses an unheld pool
/// first, tagged `foreign_pool_unknown` — and so names an internal
/// inconsistency rather than a grammar fault the caller can repair. Every
/// refusal a caller CAN reach goes through [`grammar_fault`].
fn grammar_violation(pool: &PoolName, message: impl Into<String>) -> SpineError {
    SpineError::refuse(Witness::GrammarViolation {
        pool: pool.clone(),
        part: None,
        message: message.into(),
    })
}

/// A compile-stage grammar refusal carrying its FROZEN tag: the same
/// `schema_def::GrammarFault` -> `Witness::GrammarViolation` construction the
/// one validator uses, so `grammar_fault_tag` reads the tag back off a door
/// refusal exactly as off a validator one, and the two never disagree about
/// which tag one fact carries. `tag` must be in `GRAMMAR_FAULTS` (debug-
/// asserted by `GrammarFault::new`).
fn grammar_fault(tag: &'static str, pool: &PoolName, detail: impl Into<String>) -> SpineError {
    crate::schema_def::GrammarFault::new(tag, detail).refusal(pool, None)
}

// ---------------------------------------------------------------------------
// Compilation
// ---------------------------------------------------------------------------

/// What compilation needs beyond the statement itself: the per-pool pinned
/// grammars every part validates under, and the match
/// phase's fuel tank.
///
/// SIGNATURE CHANGE: a bare
/// `compile(stmt, params, matched, hash)` could not be honest —
/// [`CompiledPart::schema_version`] is the pool's PINNED version and the
/// function's own contract promises `GrammarViolation` refusals, and neither
/// is reachable without the grammars. The grammars and the fuel travel
/// together in this one added parameter.
pub struct CompileCtx<'a> {
    pub grammars: &'a dyn Fn(&PoolName) -> Option<PoolGrammar>,
    /// Every pool of one KIND, with its pinned grammar — what a reach by
    /// kind is judged against (`FarSide::Door::declared_by_kind`).
    /// `Cauldron::kind_lookup` is the door's; a suite
    /// compiling by hand derives it from the grammars it holds.
    pub pools_of_kind: &'a dyn Fn(&crate::schema_def::PoolKind) -> Vec<PoolGrammar>,
    pub fuel: &'a mut Fuel,
}

/// Compile the matched statement into the four interior deltas plus premise
/// records — the spine row's durable payload. Parts are grouped per touched pool and ordered by ascending
/// pool name (frozen — see [`crate::delta::CompiledPayload::parts`]).
///
/// # The frozen node-identifier minting rule
///
/// CREATE mints identifiers HERE, deterministically, so roll-forward replay
/// of the payload re-creates identical rows:
/// `id = "n" + first 16 hex chars of SHA-256(hash.0 || u32_be(k))` where
/// `hash` is the transaction's hash identity and `k` is the create's
/// zero-based ordinal among the statement's CREATE-node mutations.
///
/// EXCEPT for a node type whose grammar sets `id_prefix`: its id
/// is the SERIAL `<prefix>-<n>`, numbered by the spine at the decided flip,
/// and what this function mints for it is the PROVISIONAL
/// `<prefix>-~<same 16 hex>` ([`provisional_serial_id`]) the flip rewrites
/// — still a deterministic function of `(hash, k)`, so the re-validation
/// compare and a restart re-mint the same text, and roll-forward replays
/// the ROW's payload, which carries the serial from the flip on.
///
/// Refusals: `MissingParameter` for an unsupplied or unused
/// `$param`; `GrammarViolation` (pool-named) when a delta violates the
/// touched pool's own pinned grammar; `FailedExpectation` when an EXPECT
/// does not hold against what matched; `ProjectedNamespaceWrite` from
/// [`crate::project::guard_write_target`], which this function calls on
/// every CREATE target, every SET target and every edge SOURCE — the pen's
/// one net, before any row exists. Fuel is debited per row expanded and per
/// clause evaluated; exhaustion is [`SpineError::FuelExhausted`], an ENGINE
/// error, never a partial payload.
///
/// The EMPTY MATCH lands nothing and succeeds: every
/// mutation over a binding that bound zero rows expands to zero deltas, and
/// only a count expectation turns emptiness into a refusal.
pub fn compile(
    stmt: &Statement,
    params: &Bindings,
    matched: &MatchSet,
    hash: &TxHash,
    ctx: &mut CompileCtx<'_>,
) -> Result<CompiledPayload> {
    check_params(stmt, params)?;
    let types = stmt.binding_types();
    let grammars = resolve_grammars(&types, ctx.grammars)?;

    // ---- mint identifiers, in CREATE order (the frozen rule) ----
    let mut minted: BTreeMap<String, QualifiedNodeId> = BTreeMap::new();
    for (k, head) in stmt.created_bindings().into_iter().enumerate() {
        // THE PEN'S NET, site 1 of 3: every CREATE target (the
        // three write-target sites of this file all go through ONE guard,
        // `project::guard_write_target`, with ONE witness constructor). A
        // created head has no identifier yet, so the target text is the
        // head itself, `CREATE (<binding>)` — the SAME text `exec`'s earlier
        // phase-5 duplicate writes, so the two nets refuse one fact with one
        // witness and the duplicate is a duplicate, not a second shape.
        project::guard_write_target(&QualifiedNodeId {
            pool: head.pool.clone(),
            id: NodeId(format!("CREATE ({})", head.binding.0)),
        })?;
        let ordinal = u32::try_from(k).map_err(|_| {
            SpineError::usage("a statement cannot carry more than u32::MAX creates")
        })?;
        // A type whose grammar sets `id_prefix` takes a SERIAL id, minted by
        // the spine at the decided flip; here it takes the
        // provisional the flip rewrites (`delta::PROVISIONAL_MARK`).
        let prefix = grammars
            .get(&head.pool)
            .and_then(|g| g.node_types.get(&head.node_type))
            .and_then(|t| t.id_prefix.as_deref());
        let id = match prefix {
            Some(p) => provisional_serial_id(p, hash, ordinal),
            None => mint_node_id(hash, ordinal),
        };
        minted.insert(
            head.binding.0.clone(),
            QualifiedNodeId {
                pool: head.pool.clone(),
                id,
            },
        );
    }

    // ---- carried identifiers, in IMPORT order ----
    // AFTER the CREATE mint loop, so every CREATE's ordinal `k` is exactly
    // what it was before the form existed.
    for head in stmt.imported_bindings() {
        // THE PEN'S NET, site 4 of 4: every IMPORT target. The SAME
        // one guard as the three sites below and the CREATE site above; the
        // target text is the head, `IMPORT (<binding>)`, the same text
        // `exec`'s phase-5 duplicate writes.
        project::guard_write_target(&QualifiedNodeId {
            pool: head.pool.clone(),
            id: NodeId(format!("IMPORT ({})", head.binding.0)),
        })?;
        let id = carried_id(head, params, &grammars)?;
        let q = QualifiedNodeId {
            pool: head.pool.clone(),
            id,
        };
        // Two IMPORT heads naming one id in one statement: the validator's
        // `duplicate_node_id` judges an id against the KNOWN set (the match
        // set, the pool), never against the part's own other creates, so the
        // statement-level duplicate is refused here, by the same tag.
        if minted.values().any(|m| *m == q) {
            return Err(grammar_fault(
                "duplicate_node_id",
                &head.pool,
                format!(
                    "node `{}` is IMPORTed twice in one statement in pool `{}`",
                    q.id, head.pool
                ),
            ));
        }
        minted.insert(head.binding.0.clone(), q);
    }

    let mut emit = Emit {
        params,
        matched,
        types: &types,
        grammars: &grammars,
        minted: &minted,
        by_pool: BTreeMap::new(),
    };

    // ---- premises: expectations, then conditions ----
    for e in stmt.expects.iter() {
        let form = expectation_form(e);
        let rule = EXPECT_TABLE
            .iter()
            .find(|r| r.form == form)
            .ok_or_else(|| unimplemented_form(form))?;
        ctx.fuel.spend(FUEL_PER_EVAL)?;
        (rule.emit)(&mut emit, e, ctx.fuel)?;
    }
    for c in stmt.wheres.iter() {
        // The premise is CHECKED before it is recorded. The rule: "A
        // premise that fails at commit time refuses the statement with the
        // failing condition as witness" — so WHERE never filters rows, it
        // refuses. Recording a premise without checking it would write a
        // durable claim that was never true.
        check_condition(&emit, c, ctx.fuel)?;
        emit_condition(&mut emit, c, ctx.fuel)?;
    }

    // ---- SEAL targets: by identifier, matched, PRESENT ----
    // Judged for EVERY seal clause before any delta is emitted, so a SEAL
    // naming one absent node refuses the WHOLE statement and seals nothing
    // — never the present ones with the absent one skipped.
    check_seal_targets(stmt, params, matched, &minted)?;

    // ---- deltas, through the clause-form table ----
    for m in stmt.mutations.iter() {
        let form = mutation_form(m);
        let rule = MUTATION_TABLE
            .iter()
            .find(|r| r.form == form)
            .ok_or_else(|| unimplemented_form(form))?;
        ctx.fuel.spend(FUEL_PER_EVAL)?;
        (rule.emit)(&mut emit, m, ctx.fuel)?;
    }

    // ---- the FAR side of every cross-pool edge ----
    // Two lookups the one validator consults at the door, closed over what
    // this compile already holds: a far pool's pinned grammar (the resolved
    // set first, the cauldron's lookup behind it; `spine` answers through
    // its code-defined grammar inside the validator), and a far node's
    // ACTUAL type as the far pool resolves it — a node this statement
    // matched there, or one it mints there.
    let lookup = ctx.grammars;
    let far_grammar = |p: &PoolName| -> Option<PoolGrammar> {
        grammars.get(p).cloned().or_else(|| (lookup)(p))
    };
    let far_type = |q: &QualifiedNodeId| -> Option<String> {
        for (binding, mq) in &minted {
            if *mq == *q {
                return types.get(binding).map(|(_, t)| t.clone());
            }
        }
        matched
            .bindings
            .values()
            .flatten()
            .find(|n| n.pool == q.pool && n.node.id == q.id)
            .map(|n| n.node.node_type.clone())
    };
    let kinds = ctx.pools_of_kind;
    let declared_by_kind = |k: &crate::schema_def::PoolKind, t: &str| -> bool {
        (kinds)(k).iter().any(|g| g.node_types.contains_key(t))
    };
    let far = crate::schema_def::FarSide::Door {
        grammar: &far_grammar,
        node_type: &far_type,
        declared_by_kind: &declared_by_kind,
    };

    // ---- parts, ascending by pool name (frozen landing order) ----
    let mut parts = Vec::new();
    for (ordinal, (pool, accum)) in emit.by_pool.into_iter().enumerate() {
        let grammar = grammars
            .get(&pool)
            .ok_or_else(|| grammar_violation(&pool, "no pinned grammar for this pool"))?;
        // The far pin every cross-pool edge in this part validates against,
        // RECORDED on the part before validation so the row carries it
        // forever and the landing can ask for it ("check the schema version
        // on it", made durable). A far pool the cauldron does not
        // hold records nothing here and refuses inside the validator by name.
        let mut far_versions: BTreeMap<PoolName, u32> = BTreeMap::new();
        for d in &accum.deltas {
            let dst = match d {
                Delta::AddEdge { dst, .. } | Delta::RemoveEdge { dst, .. } => dst,
                _ => continue,
            };
            if dst.pool == pool || far_versions.contains_key(&dst.pool) {
                continue;
            }
            let far_pin = if project::is_projected(&dst.pool) {
                Some(project::projected_grammar().version)
            } else {
                far_grammar(&dst.pool).map(|g| g.version)
            };
            if let Some(v) = far_pin {
                far_versions.insert(dst.pool.clone(), v);
            }
        }
        let part = CompiledPart {
            pool: pool.clone(),
            schema_version: grammar.version,
            deltas: accum.deltas,
            premises: accum.premises,
            far_versions,
        };
        // ONE VALIDATOR, TWO CALLERS (the validate-mode shape, applied to the
        // grammar check): `PoolGrammar::validate_part` runs HERE against the
        // match set and again inside `Pool::apply_part` against the pool's
        // own `nodes` table, and it is the same function both times. The
        // weaker second implementation this file used to carry was removed
        // as drift-prone (no machine-owned-field check, no
        // count-cell check, no duplicate-id check, no endpoint-type
        // resolution, and untagged witness messages). The
        // far-endpoint check `edge_deltas` used to carry for the same
        // reason was removed too: the cross-pool half now lives in the validator alone.
        let types = matched.type_resolver(&pool);
        let resolver = |id: &NodeId| -> Option<String> { types.get(id).cloned() };
        let ordinal = u32::try_from(ordinal)
            .map_err(|_| SpineError::usage("a statement cannot touch more than u32::MAX pools"))?;
        grammar.validate_part(&part, Some(ordinal), &resolver, &far)?;
        parts.push(part);
    }
    Ok(CompiledPayload { parts, minted })
}

/// The frozen node-identifier minting rule (roll-forward's determinism
/// requirement): `"n"` plus the first 16 hex
/// characters of `SHA-256(tx hash || u32 big-endian create ordinal)`.
pub fn mint_node_id(hash: &TxHash, ordinal: u32) -> NodeId {
    let mut h = Sha256::new();
    h.update(hash.0);
    h.update(ordinal.to_be_bytes());
    let digest = h.finalize();
    let mut s = String::with_capacity(17);
    s.push('n');
    for b in &digest[..8] {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    NodeId(s)
}

/// The PROVISIONAL identifier of a node whose type sets `id_prefix`:
/// `<prefix>-~` plus the hex of [`mint_node_id`]'s frozen rule, so two
/// attempts at one identity re-mint the same text. The spine's decided flip
/// rewrites it to the serial `<prefix>-<n>`; it never lands in a pool (see
/// [`crate::delta::PROVISIONAL_MARK`]).
pub fn provisional_serial_id(prefix: &str, hash: &TxHash, ordinal: u32) -> NodeId {
    let hashed = mint_node_id(hash, ordinal);
    let hex = hashed.0.strip_prefix('n').unwrap_or(&hashed.0);
    NodeId(format!("{prefix}-{}{hex}", crate::delta::PROVISIONAL_MARK))
}

/// Every `$param` the text references must be supplied, and every supplied
/// binding must be referenced — both directions refuse (the text
/// and its values travel separately, so neither may be silently ignored).
pub fn check_params(stmt: &Statement, params: &Bindings) -> Result<()> {
    let referenced = stmt.referenced_params();
    for name in &referenced {
        if !params.contains_key(name) {
            return Err(SpineError::refuse(Witness::MissingParameter {
                name: name.clone(),
            }));
        }
    }
    for name in params.keys() {
        if !referenced.contains(name) {
            // Its OWN name: the repair is "drop the binding", the opposite
            // of the arm above's "supply it".
            return Err(SpineError::refuse(Witness::UnusedParameter {
                name: name.clone(),
            }));
        }
    }
    Ok(())
}

/// A MATCH head's `state` entry must be a state the head's type can HOLD
/// (the match-by-pool-plus-type-plus-state form; closed
/// vocabularies). A state outside a closed vocabulary, or a state on a type
/// that carries none, is not a query that happens to match nothing — it is a
/// query the grammar can prove can never match, and the same fact refuses
/// BY NAME on the write side (`unknown_state`, `state_not_declared`). Judged
/// here by the same names, before the match runs, so the closed vocabulary
/// is closed for readers exactly as it is for writers. An undeclared TYPE is already refused by name
/// at head resolution; this is the state half of the same fact.
///
/// A non-text state value is left to the matcher, which refuses it as
/// malformed. Both doors call this (write: `exec` phase 5; read:
/// `read::read_request`).
pub fn check_match_states(
    stmt: &Statement,
    params: &Bindings,
    grammars: &dyn Fn(&PoolName) -> Option<PoolGrammar>,
) -> Result<()> {
    let types = stmt.binding_types();
    let known = resolve_grammars(&types, grammars)?;
    for head in stmt.matches.iter().flat_map(|m| m.heads()) {
        let Some(v) = head.field(FIELD_STATE) else {
            continue;
        };
        let value = resolve_value(v, params)?;
        let Some(s) = value.as_str() else {
            continue;
        };
        let Some(ty) = known
            .get(&head.pool)
            .and_then(|g| g.node_types.get(&head.node_type))
        else {
            continue; // `resolve_grammars` refused it by name already
        };
        if ty.states.is_empty() {
            return Err(crate::schema_def::GrammarFault::new(
                "state_not_declared",
                format!(
                    "MATCH addresses `state` on `{}`, which carries no state vocabulary",
                    head.node_type
                ),
            )
            .refusal(&head.pool, None));
        }
        if !ty.states.iter().any(|x| x == s) {
            return Err(crate::schema_def::GrammarFault::new(
                "unknown_state",
                format!(
                    "MATCH names state `{s}`, which is outside `{}`'s closed state vocabulary ({})",
                    head.node_type,
                    ty.states.join(", ")
                ),
            )
            .refusal(&head.pool, None));
        }
    }
    Ok(())
}

/// The write door speaks the WRITE statement. A statement
/// with no mutation clause and no premise — no CREATE, SET or REMOVE, no
/// EXPECT, no WHERE — is the bulk-read form (the same grammar
/// with no mutation clauses, executed as a READ TRANSACTION), and the
/// write door refuses it BY NAME rather than running it: the match would
/// bind fine and the door would then do nothing, append a decided-and-cleared
/// row with zero parts that the committed-commit counter never counted, and
/// answer `Committed` for a commit that committed nothing. That is the
/// code-only no-op principle: code that is invoked and does not
/// do its work while reporting success is an error. The principle applies
/// to code only; application-level "no change" stays a success (an idempotent
/// upsert; a pass over data that legitimately has nothing to act on), which
/// is why an empty match is NOT refused. A PREMISE-ONLY statement (EXPECT
/// or WHERE, no mutation)
/// is NOT refused here: it is appended and decided as a row
/// with premise-only parts, and whether the nothing-done rule
/// reaches it is an open question.
pub fn check_write_form(stmt: &Statement) -> Result<()> {
    if stmt.mutations.is_empty() && stmt.expects.is_empty() && stmt.wheres.is_empty() {
        return Err(malformed(
            "the statement has no mutation clause and no premise: it is the bulk-read form, \
             which the write door does not speak — submit it to `Cauldron::read`; \
             the write door lands nothing for it and will not answer Committed for a commit \
             that committed nothing",
            NO_OFFSET,
        ));
    }
    // AN IMPORT EDITS NOTHING IT DID NOT IMPORT. The soup tags a commit
    // carrying ANY
    // `IMPORT` clause `import` WHOLE and fires no trigger on it,
    // so a `SET`, `SEAL` or `REMOVE` on a matched, pre-existing row riding
    // in one would escape every trigger. Refused whole, before any pool is
    // opened, on both modes. `CREATE` stays admitted beside `IMPORT`: the v5
    // regeneration (`migrate::run`) lands a pool as ONE statement whose
    // store-minted viewer roles and edges are `CREATE` clauses, and those
    // are the snapshot's own.
    if stmt.mutations.iter().any(|m| matches!(m, Mutation::ImportNode(_))) {
        if let Some(edit) = stmt
            .mutations
            .iter()
            .find(|m| matches!(m, Mutation::Set { .. } | Mutation::Seal { .. } | Mutation::RemoveEdge { .. }))
        {
            return Err(SpineError::refuse(Witness::ImportMixedWithEdit {
                form: mutation_form(edit).to_string(),
            }));
        }
    }
    Ok(())
}

fn unimplemented_form(form: &str) -> SpineError {
    SpineError::refuse(Witness::UnimplementedForm {
        form: form.to_string(),
    })
}

// ---- the tables ----

/// One mutation form: its frozen name and its delta constructor.
pub struct MutationRule {
    pub form: &'static str,
    emit: fn(&mut Emit<'_>, &Mutation, &mut Fuel) -> Result<()>,
}

/// The clause-form -> delta constructor table (the compiler's
/// table is grammar DATA; the commit interior is agnostic to it).
pub const MUTATION_TABLE: &[MutationRule] = &[
    MutationRule {
        form: "create:node",
        emit: emit_create_node,
    },
    MutationRule {
        form: "create:edge",
        emit: emit_create_edge,
    },
    MutationRule {
        form: "set:field",
        emit: emit_set_field,
    },
    MutationRule {
        form: "remove:edge",
        emit: emit_remove_edge,
    },
    MutationRule {
        form: SEAL_FORM,
        emit: emit_seal,
    },
    MutationRule {
        form: IMPORT_FORM,
        emit: emit_import_node,
    },
];

/// The `IMPORT (b: p/t { id: $id, ... })` form's frozen name.
pub const IMPORT_FORM: &str = "import:node";

/// The `SEAL (b)` form's frozen name.
pub const SEAL_FORM: &str = "seal:node";

/// The frozen form name of one mutation clause.
pub fn mutation_form(m: &Mutation) -> &'static str {
    match m {
        Mutation::CreateNode(_) => "create:node",
        Mutation::CreateEdge { .. } => "create:edge",
        Mutation::Set { .. } => "set:field",
        Mutation::RemoveEdge { .. } => "remove:edge",
        Mutation::Seal { .. } => SEAL_FORM,
        Mutation::ImportNode(_) => IMPORT_FORM,
    }
}

/// One expectation form: its frozen name and its premise constructor.
pub struct ExpectRule {
    pub form: &'static str,
    emit: fn(&mut Emit<'_>, &Expectation, &mut Fuel) -> Result<()>,
}

/// The expectation-form -> premise table (the EXPECT parameters).
pub const EXPECT_TABLE: &[ExpectRule] = &[
    ExpectRule {
        form: "expect:revision",
        emit: emit_expect_revision,
    },
    ExpectRule {
        form: "expect:value",
        emit: emit_expect_value,
    },
    ExpectRule {
        form: "expect:count",
        emit: emit_expect_count,
    },
];

/// The frozen form name of one expectation clause.
pub fn expectation_form(e: &Expectation) -> &'static str {
    match e {
        Expectation::Revision { .. } => "expect:revision",
        Expectation::Value { .. } => "expect:value",
        Expectation::Count { .. } => "expect:count",
    }
}

/// One WHERE form that carries a premise record.
pub struct ConditionRule {
    pub form: &'static str,
    emit: fn(&mut Emit<'_>, &Condition, &mut Fuel) -> Result<()>,
}

/// The condition-form -> premise table.
///
/// Only the EQUALITY comparison has a row, and that is a REPORTED GAP, not a
/// coverage cut: [`crate::delta::Premise`] (frozen) has no
/// variant carrying a comparison OPERATOR, so `x.f > 5` cannot be recorded
/// as a premise at all. Such a condition is still ENFORCED — as a premise,
/// universally over every row the binding bound, by [`check_condition`]
/// before anything is recorded (so a bulk SET with one violating row refuses
/// WHOLE, never narrows), and again by the re-match under locks on restart —
/// but it leaves no durable premise record. (This doc used to say the
/// matcher "filters on it"; the matcher never filters on WHERE — see
/// `read::run_match`.) `where:edge-existence` has no row
/// because it is staged OUT and refuses by name before compilation.
pub const CONDITION_TABLE: &[ConditionRule] = &[ConditionRule {
    form: "where:compare-equal",
    emit: emit_where_equal,
}];

// ---- fuel accounting ----

/// One fuel unit per row a clause expands over.
pub const FUEL_PER_ROW: u64 = 1;
/// One fuel unit per clause evaluated.
pub const FUEL_PER_EVAL: u64 = 1;
/// One fuel unit per interior delta emitted.
pub const FUEL_PER_DELTA: u64 = 1;

// ---- emission ----

#[derive(Default)]
struct PartAccum {
    deltas: Vec<Delta>,
    premises: Vec<Premise>,
}

struct Emit<'a> {
    params: &'a Bindings,
    matched: &'a MatchSet,
    types: &'a BTreeMap<String, (PoolName, String)>,
    grammars: &'a BTreeMap<PoolName, PoolGrammar>,
    minted: &'a BTreeMap<String, QualifiedNodeId>,
    by_pool: BTreeMap<PoolName, PartAccum>,
}

/// One end a clause resolves to: a node that matched, or the node this
/// statement mints.
#[derive(Debug, Clone)]
struct Endpoint {
    pool: PoolName,
    id: NodeId,
    node_type: String,
    /// `None` for a minted node — it has no record yet.
    record: Option<NodeRecord>,
}

impl Emit<'_> {
    fn part(&mut self, pool: &PoolName) -> &mut PartAccum {
        self.by_pool.entry(pool.clone()).or_default()
    }

    fn push_delta(&mut self, pool: &PoolName, d: Delta, fuel: &mut Fuel) -> Result<()> {
        fuel.spend(FUEL_PER_DELTA)?;
        self.part(pool).deltas.push(d);
        Ok(())
    }

    fn push_premise(&mut self, pool: &PoolName, p: Premise) {
        if project::is_projected(pool) {
            // A premise over a GHOST is CHECKED (the evaluators above run
            // over the projected record like any other) but records no
            // durable premise: there is no `spine` part to carry it, because
            // a part is a pool landing and nothing ever lands in the
            // projection. Re-validation under locks re-resolves the ghost
            // and re-checks (the same "enforced, not recorded"
            // status the CONDITION_TABLE docs already give non-equality
            // comparisons).
            return;
        }
        self.part(pool).premises.push(p);
    }

    fn grammar(&self, pool: &PoolName) -> Result<&PoolGrammar> {
        self.grammars
            .get(pool)
            .ok_or_else(|| grammar_violation(pool, "no pinned grammar for this pool"))
    }

    /// Every node a binding denotes: its matched set, or the one node it
    /// mints. Fuel is debited per row.
    fn endpoints(&self, binding: &Binding, fuel: &mut Fuel) -> Result<Vec<Endpoint>> {
        let (_pool, node_type) = self.types.get(&binding.0).ok_or_else(|| {
            malformed(
                format!("`{binding}` is not bound by any pattern head"),
                NO_OFFSET,
            )
        })?;
        if let Some(q) = self.minted.get(&binding.0) {
            fuel.spend(FUEL_PER_ROW)?;
            return Ok(vec![Endpoint {
                pool: q.pool.clone(),
                id: q.id.clone(),
                node_type: node_type.clone(),
                record: None,
            }]);
        }
        let mut out = Vec::new();
        if let Some(nodes) = self.matched.bindings.get(&binding.0) {
            for n in nodes {
                fuel.spend(FUEL_PER_ROW)?;
                out.push(Endpoint {
                    pool: n.pool.clone(),
                    id: n.node.id.clone(),
                    node_type: n.node.node_type.clone(),
                    record: Some(n.node.clone()),
                });
            }
        }
        Ok(out)
    }

    fn resolve(&self, v: &ValueExpr) -> Result<Value> {
        resolve_value(v, self.params)
    }
}

/// The offset a post-parse refusal carries: the statement PARSED, so no byte
/// position is the culprit (a convention).
const NO_OFFSET: usize = 0;

fn resolve_value(v: &ValueExpr, params: &Bindings) -> Result<Value> {
    match v {
        ValueExpr::Literal(x) => Ok(x.clone()),
        ValueExpr::Param(p) => params
            .get(p)
            .cloned()
            .ok_or_else(|| SpineError::refuse(Witness::MissingParameter { name: p.clone() })),
    }
}

fn as_i64(v: &Value, what: &str) -> Result<i64> {
    v.as_i64().ok_or_else(|| {
        malformed(
            format!("{what} takes an integer; `{v}` is not one"),
            NO_OFFSET,
        )
    })
}

/// Which end of a source-text edge is the source (a normalization:
/// the AST holds left/right plus the direction).
fn src_dst<'a>(from: &'a Binding, to: &'a Binding, arrow: &ArrowDir) -> (&'a Binding, &'a Binding) {
    match arrow {
        ArrowDir::LeftToRight => (from, to),
        ArrowDir::RightToLeft => (to, from),
    }
}

// ---- mutation constructors (the table's rows) ----

fn emit_create_node(e: &mut Emit<'_>, m: &Mutation, fuel: &mut Fuel) -> Result<()> {
    let Mutation::CreateNode(head) = m else {
        unreachable!("table row matches the form")
    };
    let q = e
        .minted
        .get(&head.binding.0)
        .cloned()
        .ok_or_else(|| malformed("a CREATE head must have minted an identifier", NO_OFFSET))?;
    fuel.spend(FUEL_PER_ROW)?;

    let mut state: Option<String> = None;
    let mut fields: BTreeMap<String, Value> = BTreeMap::new();
    for (name, v) in &head.fields {
        let value = e.resolve(v)?;
        match name.as_str() {
            FIELD_ID => {
                // A caller-supplied id on a type whose grammar sets
                // `id_prefix`: one that does not carry the prefix is
                // refused by the id-prefix rule — the one validator, called here on
                // the id alone, so the witness is `id_prefix_mismatch` naming
                // the prefix the type mints. One that does carry it is still
                // the spine's to mint and refuses below like every other.
                caller_id_against_prefix(e, head, &value)?;
                return Err(grammar_fault(
                    "machine_owned_field",
                    &head.pool,
                    "`id` is store-minted: a CREATE never names it (the store mints the identifier and RETURN hands it back)",
                ))
            }
            FIELD_STATE => {
                let s = value.as_str().ok_or_else(|| {
                    grammar_fault("unknown_state", &head.pool, "`state` takes a string from the type's vocabulary")
                })?;
                state = Some(s.to_string());
            }
            _ => {
                fields.insert(name.clone(), value);
            }
        }
    }
    e.push_delta(
        &q.pool,
        Delta::CreateNode {
            id: q.id.clone(),
            node_type: head.node_type.clone(),
            state,
            fields,
        },
        fuel,
    )
}

/// `IMPORT (b: p/t { id: $id, ... })`: exactly [`emit_create_node`],
/// except that the `id` entry is CONSUMED — the carried id is already in
/// `minted`, judged by [`carried_id`] — rather than refused. It pushes an
/// ordinary `Delta::CreateNode`: the delta vocabulary does not grow.
fn emit_import_node(e: &mut Emit<'_>, m: &Mutation, fuel: &mut Fuel) -> Result<()> {
    let Mutation::ImportNode(head) = m else {
        unreachable!("table row matches the form")
    };
    let q = e
        .minted
        .get(&head.binding.0)
        .cloned()
        .ok_or_else(|| malformed("an IMPORT head must have carried an identifier", NO_OFFSET))?;
    fuel.spend(FUEL_PER_ROW)?;

    let mut state: Option<String> = None;
    let mut fields: BTreeMap<String, Value> = BTreeMap::new();
    for (name, v) in &head.fields {
        let value = e.resolve(v)?;
        match name.as_str() {
            // Consumed: judged by `carried_id` in the IMPORT loop.
            FIELD_ID => {}
            FIELD_STATE => {
                let s = value.as_str().ok_or_else(|| {
                    grammar_fault("unknown_state", &head.pool, "`state` takes a string from the type's vocabulary")
                })?;
                state = Some(s.to_string());
            }
            _ => {
                fields.insert(name.clone(), value);
            }
        }
    }
    e.push_delta(
        &q.pool,
        Delta::CreateNode {
            id: q.id.clone(),
            node_type: head.node_type.clone(),
            state,
            fields,
        },
        fuel,
    )
}

/// The carried id of one IMPORT head, judged in this fixed order:
/// (1) the type's grammar sets `id_prefix`, else `machine_owned_field`;
/// (2) the value is a string carrying that prefix, else `id_prefix_mismatch`
/// (the same probe a CREATE's caller id goes through); (3) the rest after
/// `<prefix>-` is a serial `[1-9][0-9]*` that parses as `u64` and
/// round-trips through [`crate::delta::serial_node_id`], else
/// `carried_id_not_serial` — the provisional `-~` form among them, so a
/// carried id can never be mistaken for a mint the flip must number.
fn carried_id(
    head: &NodePattern,
    params: &Bindings,
    grammars: &BTreeMap<PoolName, PoolGrammar>,
) -> Result<NodeId> {
    let grammar = grammars
        .get(&head.pool)
        .ok_or_else(|| grammar_violation(&head.pool, "no pinned grammar for this pool"))?;
    let expr = head
        .field(FIELD_ID)
        .ok_or_else(|| malformed("IMPORT carries a serial id the source already holds: name `id`; to mint one, write CREATE", NO_OFFSET))?;
    let value = resolve_value(expr, params)?;
    let Some(prefix) = grammar
        .node_types
        .get(&head.node_type)
        .and_then(|t| t.id_prefix.as_deref())
    else {
        return Err(grammar_fault(
            "machine_owned_field",
            &head.pool,
            format!(
                "IMPORT carries a serial id; `{}` mints no serial, so `id` is store-minted",
                head.node_type
            ),
        ));
    };
    id_prefix_probe(grammar, head, &value)?;
    let Some(text) = value.as_str() else {
        return Err(grammar_fault(
            "id_prefix_mismatch",
            &head.pool,
            format!(
                "node type `{}` mints ids `{prefix}-<n>`; `{value}` is not a string carrying that prefix",
                head.node_type
            ),
        ));
    };
    // The prefix the carried id actually carries: the type's own, or its
    // LEGACY import prefix (a region's `SUB-<n>`),
    // which the probe above admitted. The serial rule below is the same for
    // both, so `SUB-0`, `SUB-007` and `SUB-~ab` refuse `carried_id_not_serial`
    // exactly as their `R-` forms do.
    let prefix = match crate::grammar_v6::legacy_import_prefix(grammar.kind.as_str(), &head.node_type) {
        Some(legacy) if text.strip_prefix(legacy).is_some_and(|r| r.starts_with('-')) => legacy,
        _ => prefix,
    };
    let serial = text
        .strip_prefix(prefix)
        .and_then(|r| r.strip_prefix('-'))
        .filter(|r| {
            r.bytes().next().is_some_and(|b| (b'1'..=b'9').contains(&b))
                && r.bytes().all(|b| b.is_ascii_digit())
        })
        .and_then(|r| r.parse::<u64>().ok())
        .filter(|n| crate::delta::serial_node_id(prefix, *n).0 == text);
    if serial.is_none() {
        return Err(grammar_fault(
            "carried_id_not_serial",
            &head.pool,
            format!(
                "IMPORT carries `{text}`, which is not a serial `{prefix}-<n>` (n a decimal 1 or above, no padding, no provisional `~`)"
            ),
        ));
    }
    Ok(NodeId(text.to_string()))
}

/// A caller-supplied `id` on a CREATE of a PREFIXED type, judged by the
/// id-prefix rule and nothing else: a one-delta probe part carrying
/// that id goes through [`PoolGrammar::validate_part`], and an
/// `id_prefix_mismatch` it answers is the refusal. Anything else it answers
/// (accepted, or a fault about the probe's absent fields) means the prefix
/// was carried, and the caller's `emit_create_node` refuses the id as
/// store-minted. A type with no prefix, or a non-text id, judges nothing
/// here.
fn caller_id_against_prefix(e: &Emit<'_>, head: &NodePattern, value: &Value) -> Result<()> {
    let grammar = e.grammar(&head.pool)?;
    id_prefix_probe(grammar, head, value)?;
    // A CREATE's id is a MINT: the legacy import
    // prefix the part-level rule admits for a carried id is NOT a prefix a
    // CREATE may name — judged by the SAME rule function under
    // `IdPrefixRule::MintOnly`, so `CREATE (… region {id: "SUB-3"})` answers
    // exactly the wrong-prefix tag and witness it answered before.
    if let Some(id) = value.as_str() {
        if let Some(f) = grammar.id_prefix_fault(
            &head.node_type,
            &NodeId(id.to_string()),
            crate::schema_def::IdPrefixRule::MintOnly,
        ) {
            return Err(f.refusal(&grammar.pool, None));
        }
    }
    Ok(())
}

/// The body of [`caller_id_against_prefix`], over a grammar rather than an
/// `Emit`, so the IMPORT loop (which runs before the `Emit` exists) judges a
/// carried id by the SAME probe through the SAME validator.
fn id_prefix_probe(grammar: &PoolGrammar, head: &NodePattern, value: &Value) -> Result<()> {
    let prefixed = grammar
        .node_types
        .get(&head.node_type)
        .is_some_and(|t| t.id_prefix.is_some());
    let Some(id) = value.as_str() else {
        return Ok(());
    };
    if !prefixed {
        return Ok(());
    }
    let probe = CompiledPart {
        pool: head.pool.clone(),
        schema_version: grammar.version,
        deltas: vec![Delta::CreateNode {
            id: NodeId(id.to_string()),
            node_type: head.node_type.clone(),
            state: None,
            fields: BTreeMap::new(),
        }],
        premises: vec![],
        far_versions: BTreeMap::new(),
    };
    let nobody = |_: &NodeId| -> Option<String> { None };
    match grammar.validate_part(&probe, None, &nobody, &crate::schema_def::FarSide::Landing) {
        Err(err @ SpineError::Refused { .. }) if refused_with_tag(&err, "id_prefix_mismatch") => {
            Err(err)
        }
        _ => Ok(()),
    }
}

/// Does this refusal carry a grammar fault with exactly this tag?
fn refused_with_tag(err: &SpineError, tag: &str) -> bool {
    match err {
        SpineError::Refused { witnesses } => witnesses
            .iter()
            .any(|w| crate::schema_def::grammar_fault_tag(w) == Some(tag)),
        _ => false,
    }
}

fn emit_set_field(e: &mut Emit<'_>, m: &Mutation, fuel: &mut Fuel) -> Result<()> {
    let Mutation::Set { target, value } = m else {
        unreachable!("table row matches the form")
    };
    let value = e.resolve(value)?;
    for ep in e.endpoints(&target.binding, fuel)? {
        // THE PEN'S NET, site 2 of 3: every SET target. A
        // MATCH-by-identifier ghost is BINDABLE, so `SET g.state = ...` on a
        // matched ghost reaches here — and this is the ONLY by-name net for
        // it: without it the projected grammar's `txn` states accept the
        // value, the row is appended, and the door fails HARD at phase 10b
        // (no pinned grammar for pool `spine`). Refusing here is before the
        // row append (phase 8), so nothing is written.
        project::guard_write_target(&QualifiedNodeId {
            pool: ep.pool.clone(),
            id: ep.id.clone(),
        })?;
        check_settable(e, &ep, &target.path, &value)?;
        e.push_delta(
            &ep.pool.clone(),
            Delta::SetField {
                node: ep.id.clone(),
                path: target.path.clone(),
                value: value.clone(),
            },
            fuel,
        )?;
    }
    Ok(())
}

/// A SET must address a field the touched pool's grammar declares — and the
/// reserved names are governed: `id` is machine-owned, `state` is closed to
/// the type's vocabulary (and the machine-owned stamp).
fn check_settable(e: &Emit<'_>, ep: &Endpoint, path: &[String], value: &Value) -> Result<()> {
    let grammar = e.grammar(&ep.pool)?;
    let ty = grammar.node_types.get(&ep.node_type).ok_or_else(|| {
        grammar_fault(
            "unknown_node_type",
            &ep.pool,
            format!("node type `{}` is not declared by this pool's grammar", ep.node_type),
        )
    })?;
    // THE SEAL: a SET routed — by the landing's own exported router,
    // `pool::set_target` — into the body of a SEALED node refuses here,
    // before any row exists. Sealed means the matched record's flag (judged
    // again under the granted locks, where the record is current), or a
    // SEAL earlier in THIS statement's part on this node, because the
    // landing applies a part's deltas in order and would see it sealed.
    if let Some(crate::pool::SetTarget::Field { name, .. }) = crate::pool::set_target(path) {
        if name == crate::schema_def::SEALED_BODY_FIELD && sealed_at_this_point(e, ep) {
            return Err(crate::pool::sealed_body_fault(&ep.pool, &ep.id).refusal(&ep.pool, None));
        }
    }
    let head = path.first().map(String::as_str).unwrap_or_default();
    match head {
        FIELD_ID => Err(grammar_fault(
            "machine_owned_field",
            &ep.pool,
            "`id` is store-minted and machine-owned: it is never SET",
        )),
        // Every other machine-owned name — `sealed` among them (no
        // write statement sets or clears the flag; `SEAL` is the only form
        // that touches it).
        h if crate::schema_def::MACHINE_OWNED_FIELDS.contains(&h) => Err(grammar_fault(
            "machine_owned_field",
            &ep.pool,
            format!("`{h}` is machine-owned: it is never SET"),
        )),
        FIELD_STATE => {
            if path.len() > 1 {
                return Err(grammar_fault(
                    "undeclared_field",
                    &ep.pool,
                    "`state` is a scalar: it takes no dotted key",
                ));
            }
            if ty.states.is_empty() {
                return Err(grammar_fault(
                    "state_not_declared",
                    &ep.pool,
                    format!("node type `{}` carries no state vocabulary", ep.node_type),
                ));
            }
            let s = value.as_str().ok_or_else(|| {
                grammar_fault("unknown_state", &ep.pool, "`state` takes a string from the type's vocabulary")
            })?;
            if !ty.states.iter().any(|v| v == s) {
                return Err(grammar_fault(
                    "unknown_state",
                    &ep.pool,
                    format!(
                        "`{s}` is outside the closed state vocabulary of `{}` ({})",
                        ep.node_type,
                        ty.states.join(", ")
                    ),
                ));
            }
            Ok(())
        }
        other => {
            // INTERIOR CHOICE: the grammar says "dotted keys address
            // into the metadata object", and defines "the metadata object"
            // NOWHERE — no node shape in the grammar or the fixtures
            // carries one. This build reads a dotted SET path as addressing
            // into a DECLARED field's JSON value (head = the field, deeper
            // elements = keys within it), which is the only reading the
            // fixtures can express; if a first-class metadata object is ever
            // defined, its name becomes a declared-field head and this code
            // does not move. The grammar is silent on this case.
            if !ty.fields.contains_key(other) {
                return Err(grammar_fault(
                    "undeclared_field",
                    &ep.pool,
                    format!(
                        "field `{other}` is not declared on node type `{}` by this pool's grammar",
                        ep.node_type
                    ),
                ));
            }
            // A DOTTED path addresses THROUGH the field's value, and every
            // value a non-final key passes through must be an object (or
            // absent / `null`, which the landing mints as one). A scalar or
            // an array in the way would have to be DESTROYED to satisfy the
            // SET — data loss behind a `Committed` reply, the silent misread
            // the coverage rule forbids. Judged HERE, before any row
            // exists, and judged AGAINST THE STATE THE LANDING WILL SEE: the
            // landing applies a part's deltas in order, so the value this
            // SET addresses through is the matched record's value (or
            // nothing, for a node this statement mints) with every earlier
            // delta of this part on this node's field replayed — the CREATE
            // that minted it, the SETs before it. Judging the pre-statement
            // record instead (an earlier cut) made two judges over two
            // states: `SET d.t.a = 1 SET d.t.a.b = 2` passed here and refused
            // at the landing AFTER the decided flip (the invariant wedge),
            // while `SET d.t = null SET d.t.lang = "en"` refused here and
            // lands there. The replay runs the landing's
            // own `set_at_path`, so the two doors are one function over one
            // state, and the landing's refusal stays unreachable for a
            // statement that came through this door.
            let mut doc: Value = ep
                .record
                .as_ref()
                .and_then(|r| r.fields.get(other).map(|fv| fv.value.clone()))
                .unwrap_or(Value::Null);
            if let Some(part) = e.by_pool.get(&ep.pool) {
                for d in &part.deltas {
                    match d {
                        Delta::CreateNode { id, fields, .. } if *id == ep.id => {
                            doc = fields.get(other).cloned().unwrap_or(Value::Null);
                        }
                        Delta::SetField {
                            node,
                            path: earlier,
                            value: v,
                        } if *node == ep.id && earlier.first().map(String::as_str) == Some(other) => {
                            // Each earlier SET was judged at its own turn
                            // against the same replayed prefix, so this
                            // cannot refuse; the `?` keeps it honest if it
                            // ever does.
                            crate::pool::set_at_path(&mut doc, &earlier[1..], v.clone())
                                .map_err(|f| f.refusal(&ep.pool, None))?;
                        }
                        _ => {}
                    }
                }
            }
            crate::pool::set_at_path(&mut doc, &path[1..], value.clone()).map_err(|f| {
                crate::schema_def::GrammarFault::new(
                    f.tag,
                    format!("SET {}.{}: {}", ep.node_type, path.join("."), f.detail),
                )
                .refusal(&ep.pool, None)
            })?;
            Ok(())
        }
    }
}

/// Is this endpoint sealed at the point the landing will reach the clause
/// being compiled? Its matched record's flag, or an earlier `Seal` delta of
/// this part on the same node.
fn sealed_at_this_point(e: &Emit<'_>, ep: &Endpoint) -> bool {
    if ep.record.as_ref().is_some_and(|r| r.sealed) {
        return true;
    }
    e.by_pool.get(&ep.pool).is_some_and(|part| {
        part.deltas
            .iter()
            .any(|d| matches!(d, Delta::Seal { node } if *node == ep.id))
    })
}

/// The static and presence rules of every `SEAL (b)` in the statement
/// judged before any delta exists:
///
/// - `b` must be bound by a MATCH head naming `id` — a seal names nodes, it
///   never sweeps a population — and never by a CREATE (a node minted in
///   this statement is not sealed by it): malformed otherwise.
/// - the head must have bound a node: a SEAL of an ABSENT node refuses
///   `unknown_node`, naming the pool, type and identifier it was given.
fn check_seal_targets(
    stmt: &Statement,
    params: &Bindings,
    matched: &MatchSet,
    minted: &BTreeMap<String, QualifiedNodeId>,
) -> Result<()> {
    for m in &stmt.mutations {
        let Mutation::Seal { target } = m else {
            continue;
        };
        if minted.contains_key(&target.0) {
            return Err(malformed(
                format!(
                    "SEAL ({target}): `{target}` is minted by this statement; SEAL takes a node \
                     MATCHed by identifier"
                ),
                NO_OFFSET,
            ));
        }
        let head = stmt
            .matches
            .iter()
            .flat_map(|c| c.heads())
            .find(|h| h.binding == *target)
            .ok_or_else(|| {
                malformed(format!("SEAL ({target}): `{target}` is not bound by a MATCH head"), NO_OFFSET)
            })?;
        let Some(id) = head.field(FIELD_ID) else {
            return Err(malformed(
                format!(
                    "SEAL ({target}): SEAL names nodes by identifier — write `MATCH ({target}: {}/{} {{ id: $id }})`",
                    head.pool, head.node_type
                ),
                NO_OFFSET,
            ));
        };
        let bound = matched.bindings.get(&target.0).map(Vec::len).unwrap_or(0);
        if bound == 0 {
            let id = resolve_value(id, params)?;
            let id = id.as_str().map(str::to_string).unwrap_or_else(|| id.to_string());
            return Err(grammar_fault(
                "unknown_node",
                &head.pool,
                format!(
                    "SEAL ({target}) names `{}/{id}`, but pool `{}` holds no `{}` node `{id}`; \
                     nothing is sealed",
                    head.pool, head.pool, head.node_type
                ),
            ));
        }
    }
    Ok(())
}

/// `SEAL (b)` -> one `Delta::Seal` per node `b` bound. A node that is
/// already sealed — in its matched record, or by an earlier SEAL of this
/// statement — compiles to NO delta: sealing is idempotent, a re-seal lands
/// nothing for that node and does not bump its revision.
fn emit_seal(e: &mut Emit<'_>, m: &Mutation, fuel: &mut Fuel) -> Result<()> {
    let Mutation::Seal { target } = m else {
        unreachable!("table row matches the form")
    };
    for ep in e.endpoints(target, fuel)? {
        // THE PEN'S NET on a seal target: a matched GHOST is bindable, and
        // nothing ever writes the projection.
        project::guard_write_target(&QualifiedNodeId {
            pool: ep.pool.clone(),
            id: ep.id.clone(),
        })?;
        if sealed_at_this_point(e, &ep) {
            continue;
        }
        e.push_delta(&ep.pool.clone(), Delta::Seal { node: ep.id.clone() }, fuel)?;
    }
    Ok(())
}

fn emit_create_edge(e: &mut Emit<'_>, m: &Mutation, fuel: &mut Fuel) -> Result<()> {
    let Mutation::CreateEdge {
        from,
        kind,
        to,
        arrow,
        note,
    } = m
    else {
        unreachable!("table row matches the form")
    };
    // `NOTE` carries either the free-text note (a string) or
    // the edge's DECLARED FIELDS (an object, `NOTE $f` with `$f` bound to
    // `{"admits": "..."}`): values ride the bindings, never the text,
    // and an object is the only shape a field map has. A dedicated
    // clause keyword would be a parser change; this is the carrier the
    // frozen grammar already has. The grammar judges the fields
    // (`PoolGrammar::check_edge_fields`); the compiler only routes them.
    let (note, fields) = match note {
        None => (None, BTreeMap::new()),
        Some(v) => {
            let value = e.resolve(v)?;
            match value {
                Value::String(s) => (Some(s), BTreeMap::new()),
                Value::Null => (None, BTreeMap::new()),
                Value::Object(m) => (None, m.into_iter().collect()),
                other => {
                    return Err(malformed(
                        format!(
                            "an edge NOTE takes a string (the note) or an object (the edge's fields); `{other}` is neither"
                        ),
                        NO_OFFSET,
                    ))
                }
            }
        }
    };
    edge_deltas(e, from, kind, to, arrow, fuel, |src, kind, dst| Delta::AddEdge {
        src,
        kind,
        dst,
        note: note.clone(),
        fields: fields.clone(),
    })
}

fn emit_remove_edge(e: &mut Emit<'_>, m: &Mutation, fuel: &mut Fuel) -> Result<()> {
    let Mutation::RemoveEdge {
        from,
        kind,
        to,
        arrow,
    } = m
    else {
        unreachable!("table row matches the form")
    };
    edge_deltas(e, from, kind, to, arrow, fuel, |src, kind, dst| {
        Delta::RemoveEdge { src, kind, dst }
    })
}

/// The shared body of the two edge forms: the CARTESIAN product of the two
/// ends (a bound set on either side dissolves or builds a cluster in one
/// line — REMOVE), each pair validated against the source pool's
/// declaration of the kind.
fn edge_deltas(
    e: &mut Emit<'_>,
    from: &Binding,
    kind: &str,
    to: &Binding,
    arrow: &ArrowDir,
    fuel: &mut Fuel,
    make: impl Fn(NodeId, String, QualifiedNodeId) -> Delta,
) -> Result<()> {
    let (src_b, dst_b) = src_dst(from, to, arrow);
    let srcs = e.endpoints(src_b, fuel)?;
    let dsts = e.endpoints(dst_b, fuel)?;
    for s in &srcs {
        // THE PEN'S NET, site 3 of 3: every edge SOURCE (CREATE and REMOVE
        // alike — the source end owns the edge, and a ghost owns nothing).
        // The DESTINATION is deliberately not guarded here: pointing AT a
        // transaction is the projection's whole point
        // (`project::guard_edge_target`, exec phase 5, and the validator's
        // reach toward kind `spine`).
        project::guard_write_target(&QualifiedNodeId {
            pool: s.pool.clone(),
            id: s.id.clone(),
        })?;
        let def = {
            let grammar = e.grammar(&s.pool)?;
            grammar
                .edge_kinds
                .get(kind)
                .cloned()
                .ok_or_else(|| {
                    grammar_fault(
                        "unknown_edge_kind",
                        &s.pool,
                        format!("edge kind `{kind}` is not declared by this pool's grammar"),
                    )
                })?
        };
        // The source must be one of the types the kind LEAVES — a set
        // (v6's `supersedes` leaves any content type).
        if !def.leaves(&s.node_type) {
            let leaves: Vec<String> = def
                .from
                .iter()
                .filter_map(|t| t.node_type().map(|nt| format!("`{nt}`")))
                .collect();
            return Err(grammar_fault(
                "wrong_endpoint_type",
                &s.pool,
                format!(
                    "edge kind `{kind}` leaves {}, not `{}`",
                    leaves.join(" | "),
                    s.node_type
                ),
            ));
        }
        // The FAR endpoint is NOT judged here: local type, reach
        // by kind, the far pool's own pin and the far node's actual type are
        // all the one validator's (`PoolGrammar::validate_part`, run over
        // every part below), so the cross-pool rule has exactly one home.
        for d in &dsts {
            fuel.spend(FUEL_PER_ROW)?;
            let delta = make(
                s.id.clone(),
                kind.to_string(),
                QualifiedNodeId {
                    pool: d.pool.clone(),
                    id: d.id.clone(),
                },
            );
            e.push_delta(&s.pool.clone(), delta, fuel)?;
        }
    }
    Ok(())
}

// ---- expectation constructors (the table's rows) ----

fn emit_expect_revision(e: &mut Emit<'_>, x: &Expectation, fuel: &mut Fuel) -> Result<()> {
    let Expectation::Revision {
        binding,
        field,
        expected,
    } = x
    else {
        unreachable!("table row matches the form")
    };
    // A per-field revision exists only for DECLARED fields — `state`, `id`
    // and every machine-owned name are columns the store keeps no per-field
    // revision for. Comparing against their absent bookkeeping used to
    // refuse with `found: null`, which read as "the node has no such thing"
    // when the truth was "this name can never have one" — a witness that
    // lies about WHY. Refused as malformed instead, naming the repair.
    if let Some(f) = field {
        if f == FIELD_STATE || f == FIELD_ID || crate::schema_def::MACHINE_OWNED_FIELDS.contains(&f.as_str()) {
            return Err(malformed(
                format!(
                    "EXPECT REV({binding}.{f}): `{f}` carries no per-field revision — the store \
                     keeps per-field revisions for declared fields only; the whole-node revision \
                     (EXPECT REV({binding})) moves on every change including `{f}`"
                ),
                NO_OFFSET,
            ));
        }
    }
    let expected_v = e.resolve(expected)?;
    let want = as_i64(&expected_v, "EXPECT REV")?;
    for ep in e.endpoints(binding, fuel)? {
        fuel.spend(FUEL_PER_EVAL)?;
        let Some(record) = &ep.record else {
            return Err(malformed(
                format!(
                    "EXPECT REV({binding}) names a binding this statement MINTS; an expectation re-establishes a premise about something that already exists"
                ),
                NO_OFFSET,
            ));
        };
        let found = match field {
            None => Some(record.rev),
            Some(f) => record.fields.get(f).map(|fv| fv.rev),
        };
        if found != Some(want) {
            return Err(SpineError::refuse(Witness::FailedExpectation {
                expectation: ExpectationKind::Revision {
                    binding: binding.0.clone(),
                    field: field.clone(),
                },
                expected: Value::from(want),
                found: found.map(Value::from).unwrap_or(Value::Null),
            }));
        }
        e.push_premise(
            &ep.pool.clone(),
            Premise::RevisionIs {
                node: ep.id.clone(),
                field: field.clone(),
                rev: want,
            },
        );
    }
    Ok(())
}

fn emit_expect_value(e: &mut Emit<'_>, x: &Expectation, fuel: &mut Fuel) -> Result<()> {
    let Expectation::Value { field, expected } = x else {
        unreachable!("table row matches the form")
    };
    let want = e.resolve(expected)?;
    for ep in e.endpoints(&field.binding, fuel)? {
        fuel.spend(FUEL_PER_EVAL)?;
        let Some(record) = &ep.record else {
            return Err(malformed(
                format!(
                    "EXPECT {field} names a binding this statement MINTS; an expectation re-establishes a premise about something that already exists"
                ),
                NO_OFFSET,
            ));
        };
        let expectation = ExpectationKind::Value {
            binding: field.binding.0.clone(),
            path: field.path.clone(),
        };
        match read_path(record, &field.path)? {
            // ABSENT is its own witness: a field the node does not carry is
            // not a field that holds `null`, and the two repairs differ.
            None => {
                return Err(SpineError::refuse(Witness::AbsentField {
                    expectation,
                    expected: want,
                }));
            }
            Some(found) if found != want => {
                return Err(SpineError::refuse(Witness::FailedExpectation {
                    expectation,
                    expected: want,
                    found,
                }));
            }
            Some(_) => {}
        }
        e.push_premise(&ep.pool.clone(), value_premise(&ep.id, &field.path, &want));
    }
    Ok(())
}

fn emit_expect_count(e: &mut Emit<'_>, x: &Expectation, _fuel: &mut Fuel) -> Result<()> {
    let Expectation::Count {
        binding,
        op,
        expected,
    } = x
    else {
        unreachable!("table row matches the form")
    };
    let expected_v = e.resolve(expected)?;
    let want = as_i64(&expected_v, "EXPECT COUNT")?;
    let found = if e.minted.contains_key(&binding.0) {
        1
    } else {
        e.matched
            .bindings
            .get(&binding.0)
            .map(|v| v.len() as i64)
            .unwrap_or(0)
    };
    let holds = match op {
        CountOp::Exactly => found == want,
        CountOp::AtMost => found <= want,
    };
    if !holds {
        // The ONLY way an empty match becomes an error: the author pinned emptiness with a count.
        return Err(SpineError::refuse(Witness::FailedExpectation {
            expectation: ExpectationKind::Count {
                binding: binding.0.clone(),
                op: *op,
            },
            expected: Value::from(want),
            found: Value::from(found),
        }));
    }
    let pool = e
        .types
        .get(&binding.0)
        .map(|(p, _)| p.clone())
        .ok_or_else(|| malformed(format!("`{binding}` is not bound"), NO_OFFSET))?;
    e.push_premise(
        &pool,
        Premise::MatchCount {
            binding: binding.0.clone(),
            op: *op,
            expected: want,
            found,
        },
    );
    Ok(())
}

// ---- condition CHECKING (the premise, before it is recorded) ----

/// Refuse the statement if a WHERE premise does not hold over what matched
/// ("A premise that fails at commit time refuses the statement
/// with the failing condition as witness"; the specified shape is
/// [`Witness::FailedCondition`] — the failing condition and the binding it
/// failed on).
///
/// # QUANTIFICATION, decided here
///
/// A binding may have bound MANY rows, and the grammar does not spell out what
/// a condition over a multi-row binding means. This build reads
/// "a premise that FAILS refuses the statement" as UNIVERSAL: the condition
/// must hold for every row the binding bound. WHERE is a premise, not a
/// filter — a filtering reading would silently narrow a bulk SET, which is
/// exactly the surprise the count expectation exists to prevent.
///
/// Consequences that fall straight out and are deliberate:
/// - the EMPTY match satisfies every condition vacuously and lands nothing,
///   which is the empty-match rule holding unchanged;
/// - a condition over a binding this statement MINTS has nothing to
///   re-validate and holds.
fn check_condition(e: &Emit<'_>, c: &Condition, fuel: &mut Fuel) -> Result<()> {
    fuel.spend(FUEL_PER_EVAL)?;
    if eval_condition(e, c, fuel)? {
        return Ok(());
    }
    let (condition, binding) = blame(e, c, fuel)?;
    Err(SpineError::refuse(Witness::FailedCondition {
        condition,
        binding,
    }))
}

fn eval_condition(e: &Emit<'_>, c: &Condition, fuel: &mut Fuel) -> Result<bool> {
    match c {
        Condition::And(parts) => {
            for p in parts {
                if !eval_condition(e, p, fuel)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        Condition::Or(parts) => {
            for p in parts {
                if eval_condition(e, p, fuel)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        Condition::Not(inner) => Ok(!eval_condition(e, inner, fuel)?),
        Condition::Compare { left, op, right } => {
            let want = e.resolve(right)?;
            for ep in e.endpoints(&left.binding, fuel)? {
                fuel.spend(FUEL_PER_EVAL)?;
                let Some(record) = &ep.record else {
                    // A minted node has no prior state to re-establish.
                    continue;
                };
                // A WHERE over an absent field compares as `null` (a condition is a predicate, not an
                // expectation with a found value to report).
                let found = read_path(record, &left.path)?.unwrap_or(Value::Null);
                if !compare_values(&found, *op, &want) {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        // Staged OUT (`where:edge-existence`). Unreachable through the door,
        // which runs `check_coverage` first; a direct caller gets the same
        // refusal BY NAME rather than a quietly-true premise.
        Condition::Exists { .. } => Err(unimplemented_form(EDGE_EXISTENCE_FORM)),
    }
}

/// Which sub-condition to name in the witness. The witness carries the whole
/// FAILING condition's text plus one binding, so the walk descends to the
/// narrowest false part and names its binding.
fn blame(e: &Emit<'_>, c: &Condition, fuel: &mut Fuel) -> Result<(String, String)> {
    match c {
        Condition::And(parts) => {
            for p in parts {
                if !eval_condition(e, p, fuel)? {
                    return blame(e, p, fuel);
                }
            }
            Ok((c.to_string(), first_binding(c)))
        }
        // Every branch of a false OR is false; the first one is named, and
        // the reported condition text is the whole disjunction.
        Condition::Or(parts) => match parts.first() {
            Some(p) => Ok((c.to_string(), first_binding(p))),
            None => Ok((c.to_string(), String::new())),
        },
        _ => Ok((c.to_string(), first_binding(c))),
    }
}

fn first_binding(c: &Condition) -> String {
    match c {
        Condition::Compare { left, .. } => left.binding.0.clone(),
        Condition::Exists { from, .. } => from.0.clone(),
        Condition::Not(inner) => first_binding(inner),
        Condition::And(parts) | Condition::Or(parts) => parts
            .first()
            .map(first_binding)
            .unwrap_or_else(String::new),
    }
}

/// Compare two JSON values under one operator. Equality is STRUCTURAL;
/// ordering is defined for numbers (numerically) and strings
/// (lexicographically) and for nothing else — an ordering comparison across
/// kinds is FALSE, never an invented coercion (interior choice:
/// `FieldDef` carries no value typing, so the compiler has no declared
/// type to coerce toward).
fn compare_values(found: &Value, op: CmpOp, want: &Value) -> bool {
    match op {
        CmpOp::Eq => found == want,
        CmpOp::Ne => found != want,
        _ => {
            let ord = match (found, want) {
                (Value::Number(a), Value::Number(b)) => match (a.as_f64(), b.as_f64()) {
                    (Some(x), Some(y)) => x.partial_cmp(&y),
                    _ => None,
                },
                (Value::String(a), Value::String(b)) => Some(a.cmp(b)),
                _ => None,
            };
            match ord {
                None => false,
                Some(o) => match op {
                    CmpOp::Lt => o.is_lt(),
                    CmpOp::Le => o.is_le(),
                    CmpOp::Gt => o.is_gt(),
                    CmpOp::Ge => o.is_ge(),
                    CmpOp::Eq | CmpOp::Ne => unreachable!("handled above"),
                },
            }
        }
    }
}

// ---- condition constructors ----

fn emit_condition(e: &mut Emit<'_>, c: &Condition, fuel: &mut Fuel) -> Result<()> {
    fuel.spend(FUEL_PER_EVAL)?;
    match c {
        // A conjunction is premise-recordable part by part.
        Condition::And(parts) => {
            for p in parts {
                emit_condition(e, p, fuel)?;
            }
            Ok(())
        }
        Condition::Compare { op: CmpOp::Eq, .. } => {
            let rule = CONDITION_TABLE
                .iter()
                .find(|r| r.form == "where:compare-equal")
                .ok_or_else(|| unimplemented_form("where:compare-equal"))?;
            (rule.emit)(e, c, fuel)
        }
        // Every other shape is enforced by the matcher and by re-match on
        // restart, and records no premise — the frozen `Premise` vocabulary
        // has no variant for it. Reported, not hidden.
        _ => Ok(()),
    }
}

fn emit_where_equal(e: &mut Emit<'_>, c: &Condition, fuel: &mut Fuel) -> Result<()> {
    let Condition::Compare { left, right, .. } = c else {
        unreachable!("table row matches the form")
    };
    let want = e.resolve(right)?;
    for ep in e.endpoints(&left.binding, fuel)? {
        fuel.spend(FUEL_PER_EVAL)?;
        if ep.record.is_none() {
            // A condition over a minted binding has nothing to re-validate.
            continue;
        }
        e.push_premise(&ep.pool.clone(), value_premise(&ep.id, &left.path, &want));
    }
    Ok(())
}

fn value_premise(node: &NodeId, path: &[String], value: &Value) -> Premise {
    if path.len() == 1 && path[0] == FIELD_STATE {
        if let Some(s) = value.as_str() {
            return Premise::StateIs {
                node: node.clone(),
                state: s.to_string(),
            };
        }
    }
    Premise::FieldIs {
        node: node.clone(),
        path: path.to_vec(),
        value: value.clone(),
    }
}

/// Read a dotted path out of a node record: `id`, `state`, `rev` and
/// `schema_version` are reserved names answered from the record's head;
/// any other path element one is a top-level field, deeper elements address
/// into that field's JSON.
///
/// `Ok(None)` is ABSENT — the node carries no such field (or no such key
/// inside it); `Ok(Some(Value::Null))` is a field that EXISTS and holds
/// JSON `null`. The two used to collapse into one `Null`,
/// so a failed expectation's witness lied about what it found; the caller
/// decides what absence means for its clause.
///
/// `Err` is the explicit ask for a MACHINE-OWNED name the record does not
/// carry (`created_tx`, `updated_tx`): refused BY NAME, never a silent
/// `null` that reads as "no such field" — the same rule that applies
/// to `REV(b.state)` and to `schema_version`, and the rule `rev`
/// now follows too (`RETURN d.rev` once answered `null` beside
/// `REV(d) = 1`).
fn read_path(record: &NodeRecord, path: &[String]) -> Result<Option<Value>> {
    let head = match path.first() {
        Some(h) => h.as_str(),
        None => return Ok(None),
    };
    let mut cursor: Value = match head {
        FIELD_ID => Value::String(record.id.0.clone()),
        // A node with no state (a type with no state vocabulary) CARRIES no
        // state: that is the absent case by `AbsentField`'s own definition,
        // not a state that holds `null`. Rendering it as `Some(Null)` made a
        // failed `EXPECT a.state = ...` say "found null" about a value the
        // node does not have. WHERE and RETURN read absent
        // as `null` on their own side, as they do for any absent field.
        FIELD_STATE => match &record.state {
            Some(s) => Value::String(s.clone()),
            None => return Ok(None),
        },
        // The whole-node revision by its field name — the same number
        // `REV(b)` projects; every node has one.
        FIELD_REV => Value::from(record.rev),
        // The machine-owned stamp, on EXPLICIT ASK only (it
        // appears in no default render, and naming it here IS the explicit
        // ask; it can never be a declared field — `MACHINE_OWNED_FIELDS` —
        // so this head can never shadow grammar data). Before this arm
        // existed, an explicit ask fell through to the fields
        // map and answered null — indistinguishable from "no such field",
        // which made the visible-on-ask demand silently false.
        FIELD_SCHEMA_VERSION => Value::from(record.schema_version),
        // The seal flag, by its field name. Machine-owned, so it can
        // never shadow grammar data.
        FIELD_SEALED => Value::Bool(record.sealed),
        other if crate::schema_def::MACHINE_OWNED_FIELDS.contains(&other) => {
            return Err(malformed(
                format!(
                    "`{other}` is machine-owned and is not carried on the node record in this \
                     build; it cannot be read by field path (the record carries `id`, `state`, \
                     `rev`, `schema_version` and `sealed`)"
                ),
                NO_OFFSET,
            ));
        }
        other => match record.fields.get(other) {
            Some(fv) => fv.value.clone(),
            None => return Ok(None),
        },
    };
    for key in &path[1..] {
        match cursor.get(key) {
            Some(v) => cursor = v.clone(),
            None => return Ok(None),
        }
    }
    Ok(Some(cursor))
}

// ---------------------------------------------------------------------------
// Part validation and the RETURN projection
// ---------------------------------------------------------------------------

// GRAMMAR VALIDATION HAS ONE OWNER, and it is not this file.
//
// `ladle::compile::validate_part` used to live here and duplicated
// `PoolGrammar::validate_part` (schema_def.rs) more weakly: no
// machine-owned-field check, no count-cell-spoken-as-field check, no
// duplicate-identifier check, no endpoint-type resolution, and witnesses
// whose messages carried no `GRAMMAR_FAULTS` tag, so a suite asserting on
// tags saw nothing. It was removed as drift-prone. The one validator
// now has exactly two callers, which is the validate-mode shape: `compile`
// (above, against the match set) and `Pool::apply_part` (against the pool's
// stored `nodes` table). Two validators would have drifted precisely where
// the poisoned-grammar variants exist to police.

/// The RETURN projection (no create needs a follow-up
/// read). Pure — so the row-multiplication rule lives in
/// ONE place instead of being re-decided by the door and the bulk-read path.
///
/// ROW MULTIPLICATION, decided here: the rows are the CARTESIAN PRODUCT of the
/// bindings the RETURN items actually name, taken in sorted binding-name
/// order, with a minted binding contributing exactly one row and a matched
/// binding contributing its matched nodes in match order. A RETURN naming
/// only minted bindings is therefore one row; a RETURN naming a binding that
/// matched nothing is zero rows (the empty match returns nothing and
/// succeeds).
pub fn project_returns(
    stmt: &Statement,
    params: &Bindings,
    matched: &MatchSet,
    payload: &CompiledPayload,
    fuel: &mut Fuel,
) -> Result<Returned> {
    let Some(ret) = &stmt.returns else {
        return Ok(Returned::default());
    };
    // RETURN REV(b.f) on a name that carries no per-field revision refuses
    // as malformed rather than projecting a silent null (the
    // same rule, same reason as EXPECT REV's guard in
    // `emit_expect_revision`).
    for item in &ret.items {
        if let ReturnExpr::Rev { field: Some(f), .. } = &item.expr {
            if f == FIELD_STATE
                || f == FIELD_ID
                || crate::schema_def::MACHINE_OWNED_FIELDS.contains(&f.as_str())
            {
                return Err(malformed(
                    format!(
                        "RETURN REV(..{f}): `{f}` carries no per-field revision — project the \
                         whole-node revision (REV(binding)) instead"
                    ),
                    NO_OFFSET,
                ));
            }
        }
    }
    let mut names: BTreeSet<String> = BTreeSet::new();
    for item in &ret.items {
        names.insert(item.binding().0.clone());
    }
    // The created bindings' resolved field maps, for projecting a node that
    // does not exist yet.
    let mut created: BTreeMap<
        String,
        (QualifiedNodeId, Option<String>, BTreeMap<String, Value>, u32),
    > = BTreeMap::new();
    // An IMPORT head projects like a created one: its id is the
    // carried one, already in `minted`; its `id` entry is not a field.
    let heads = stmt
        .created_bindings()
        .into_iter()
        .map(|h| (h, false))
        .chain(stmt.imported_bindings().into_iter().map(|h| (h, true)));
    for (head, imported) in heads {
        let Some(q) = payload.minted.get(&head.binding.0) else {
            continue;
        };
        let mut state = None;
        let mut fields = BTreeMap::new();
        for (f, v) in &head.fields {
            if imported && f == FIELD_ID {
                continue;
            }
            let value = resolve_value(v, params)?;
            if f == FIELD_STATE {
                state = value.as_str().map(str::to_string);
            } else {
                fields.insert(f.clone(), value);
            }
        }
        // The stamp a created node will carry: its pool's pinned version,
        // already recorded on this part of the payload — so an
        // explicit ask projects the same number a matched read would.
        let stamp = payload
            .parts
            .iter()
            .find(|p| p.pool == q.pool)
            .map(|p| p.schema_version)
            .unwrap_or(0);
        created.insert(head.binding.0.clone(), (q.clone(), state, fields, stamp));
    }

    // Each named binding's domain.
    let mut domains: Vec<(String, usize)> = Vec::new();
    for n in &names {
        let size = if created.contains_key(n) {
            1
        } else {
            matched.bindings.get(n).map(|v| v.len()).unwrap_or(0)
        };
        domains.push((n.clone(), size));
    }
    if domains.iter().any(|(_, size)| *size == 0) {
        return Ok(Returned::default());
    }

    let mut rows = Vec::new();
    let mut index = vec![0usize; domains.len()];
    loop {
        fuel.spend(FUEL_PER_ROW)?;
        let mut row: BTreeMap<String, Value> = BTreeMap::new();
        for item in &ret.items {
            let binding = item.binding().0.clone();
            let slot = domains
                .iter()
                .position(|(n, _)| *n == binding)
                .expect("every item's binding has a domain");
            let value = if let Some((q, state, fields, stamp)) = created.get(&binding) {
                project_created(&item.expr, q, state.as_deref(), fields, *stamp)
            } else {
                let node = &matched.bindings[&binding][index[slot]];
                project_matched(&item.expr, &node.node)?
            };
            fuel.spend(FUEL_PER_EVAL)?;
            let column = item.column_name();
            if row.insert(column.clone(), value).is_some() {
                return Err(malformed(
                    format!("RETURN projects the column `{column}` twice; alias one with AS"),
                    NO_OFFSET,
                ));
            }
        }
        rows.push(row);
        // Odometer over the domains, last binding fastest.
        let mut i = domains.len();
        loop {
            if i == 0 {
                return Ok(Returned(rows));
            }
            i -= 1;
            index[i] += 1;
            if index[i] < domains[i].1 {
                break;
            }
            index[i] = 0;
        }
    }
}

/// A matched node's projection. A RETURN of an ABSENT field projects `null`
/// (unchanged: a projection has no witness to carry absence in, and the
/// row shape is the column set); a machine-owned name the record does not
/// carry refuses by name through [`read_path`].
fn project_matched(expr: &ReturnExpr, record: &NodeRecord) -> Result<Value> {
    Ok(match expr {
        ReturnExpr::Field(fr) => read_path(record, &fr.path)?.unwrap_or(Value::Null),
        ReturnExpr::Rev { field, .. } => match field {
            None => Value::from(record.rev),
            Some(f) => record
                .fields
                .get(f)
                .map(|fv| Value::from(fv.rev))
                .unwrap_or(Value::Null),
        },
    })
}

/// A node this statement mints has no record yet; its projection is the
/// minted identifier, the CREATE's own values, and revision 1 (the pool
/// DDL's default for a fresh row).
fn project_created(
    expr: &ReturnExpr,
    q: &QualifiedNodeId,
    state: Option<&str>,
    fields: &BTreeMap<String, Value>,
    stamp: u32,
) -> Value {
    match expr {
        ReturnExpr::Field(fr) => {
            let head = fr.path.first().map(String::as_str).unwrap_or_default();
            let mut cursor = match head {
                FIELD_ID => Value::String(q.id.0.clone()),
                FIELD_STATE => state.map(|s| Value::String(s.to_string())).unwrap_or(Value::Null),
                // A fresh row's revision is 1 (the pool DDL's default) — the
                // same number `REV(b)` projects for it, by its field name.
                FIELD_REV => Value::from(1),
                // Explicit ask: a fresh node's stamp is its
                // pool's pin, recorded on the payload part it lands with.
                FIELD_SCHEMA_VERSION => Value::from(stamp),
                // A fresh node is never sealed (SEAL refuses a minted
                // binding).
                FIELD_SEALED => Value::Bool(false),
                other => fields.get(other).cloned().unwrap_or(Value::Null),
            };
            for key in fr.path.iter().skip(1) {
                cursor = cursor.get(key).cloned().unwrap_or(Value::Null);
            }
            cursor
        }
        ReturnExpr::Rev { .. } => Value::from(1),
    }
}

// ---------------------------------------------------------------------------
// Fuel
// ---------------------------------------------------------------------------

/// The match phase's fuel — the evaluation ceiling. One unit per row
/// visited or predicate evaluated by the
/// matcher (frozen unit). Exhaustion is a
/// loud engine error, never a silent partial result.
///
/// Trivially real (a bounded counter is data, not deferred behavior).
#[derive(Debug, Clone)]
pub struct Fuel {
    remaining: u64,
    ceiling: u64,
}

impl Fuel {
    /// A tank holding `ceiling` units ([`crate::config::SpineConfig::fuel_ceiling`]).
    pub fn new(ceiling: u64) -> Fuel {
        Fuel {
            remaining: ceiling,
            ceiling,
        }
    }

    /// Spend `units`; [`SpineError::FuelExhausted`] the moment the tank
    /// runs dry — loudly, with spent-versus-ceiling, and NEVER a partial
    /// result (exhaustion fails loudly as an engine error).
    ///
    /// `spent` reports what the statement DEMANDED — everything already
    /// burned plus the request that overran — so it is strictly greater than
    /// `ceiling` and reads as the overshoot it is. (An earlier
    /// draft reported `spent: ceiling`, which claimed the tank had
    /// been drained exactly when it had not.)
    pub fn spend(&mut self, units: u64) -> Result<()> {
        if units > self.remaining {
            let spent = self
                .ceiling
                .saturating_sub(self.remaining)
                .saturating_add(units);
            self.remaining = 0;
            return Err(SpineError::FuelExhausted {
                spent,
                ceiling: self.ceiling,
            });
        }
        self.remaining -= units;
        Ok(())
    }

    pub fn remaining(&self) -> u64 {
        self.remaining
    }
}
