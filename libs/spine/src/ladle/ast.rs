//! The ladle AST — the staged clause forms. One struct
//! or enum per clause, shaped so the parser, the coverage check, the claim
//! derivation and the compiler all consume the same tree.
//!
//! The AST deliberately covers the FROZEN GRAMMAR (see [`crate::ladle`]),
//! not just the staged coverage — staging is enforced by the coverage table
//! in [`crate::ladle::compile`], by NAME, so a staged-out form is refused
//! rather than unrepresentable-and-misread.

//! # Derived readers
//!
//! Beside the tree itself, the module carries only DERIVED, pure readers the
//! parser, the coverage table, the claim derivation and the compiler all
//! need — canonical rendering (`Display`), the binding/type index, the
//! referenced-parameter set, and the RETURN column-name rule.

use crate::delta::CountOp;
use crate::schema_def::PoolName;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// One parsed statement: clauses in their fixed order (frozen grammar).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Statement {
    pub matches: Vec<MatchClause>,
    pub wheres: Vec<Condition>,
    pub expects: Vec<Expectation>,
    /// CREATE / SET / REMOVE, in source order (relative order among
    /// mutations is preserved for deterministic compilation).
    pub mutations: Vec<Mutation>,
    pub returns: Option<ReturnClause>,
}

/// A binding name, unique per statement.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Binding(pub String);

/// A literal value or a `$param` reference (values ride the
/// bindings, never the text).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "v", rename_all = "snake_case")]
pub enum ValueExpr {
    Literal(serde_json::Value),
    Param(String),
}

/// MATCH — targets by what they are, not by identifier round-trip; its
/// pattern text is also where the claim footprint derives from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "match", rename_all = "snake_case")]
pub enum MatchClause {
    Node(NodePattern),
    Hop(HopPattern),
}

/// `(binding: pool/type { field: value, ... })` — pool qualification
/// REQUIRED. `id` and `state` are ordinary map entries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodePattern {
    pub binding: Binding,
    pub pool: PoolName,
    pub node_type: String,
    pub fields: Vec<(String, ValueExpr)>,
}

/// One single-hop edge pattern (the staged path length).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HopPattern {
    pub left: PatternEnd,
    pub kind: String,
    pub right: PatternEnd,
    /// Which way the arrow pointed in source: the edge runs from the tail
    /// end to the head end.
    pub arrow: ArrowDir,
}

/// An end of a hop: a fresh head, or a binding already bound.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "end", rename_all = "snake_case")]
pub enum PatternEnd {
    Head(NodePattern),
    Bound(Binding),
}

/// `-[k]->` runs left-to-right; `<-[k]-` right-to-left.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArrowDir {
    LeftToRight,
    RightToLeft,
}

/// WHERE — boolean premises over bindings, including data the write never
/// touches (the conditional-write case a batch cannot express).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cond", rename_all = "snake_case")]
pub enum Condition {
    Compare {
        left: FieldRef,
        op: CmpOp,
        right: ValueExpr,
    },
    /// Edge existence; the absence test (under NOT) takes the absence watch
    /// — a read on the relevant slice.
    Exists {
        from: Binding,
        kind: String,
        /// `None` = any far endpoint (`()` in the frozen syntax).
        to: Option<Binding>,
        /// The source arrow's direction relative to `from`.
        direction: crate::schema_def::Direction,
    },
    Not(Box<Condition>),
    And(Vec<Condition>),
    Or(Vec<Condition>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

/// `binding.field(.key)*` — path element one is a top-level field; deeper
/// elements address into the metadata object (SET parameters).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldRef {
    pub binding: Binding,
    pub path: Vec<String>,
}

/// EXPECT — compare-and-swap as surface syntax (the count form is our
/// addition).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "expect", rename_all = "snake_case")]
pub enum Expectation {
    /// `EXPECT REV(b) = n` (whole node) / `EXPECT REV(b.f) = n` (per-field).
    Revision {
        binding: Binding,
        field: Option<String>,
        expected: ValueExpr,
    },
    /// `EXPECT b.f = v` — sugar over WHERE, refused with the richer
    /// expected-versus-found witness.
    Value { field: FieldRef, expected: ValueExpr },
    /// `EXPECT COUNT(b) = n` / `EXPECT COUNT(b) <= n` — the bulk guard.
    Count {
        binding: Binding,
        op: CountOp,
        expected: ValueExpr,
    },
}

/// CREATE / SET / REMOVE.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mutation", rename_all = "snake_case")]
pub enum Mutation {
    /// Mint one node; the store mints the identifier and RETURN hands it
    /// back.
    CreateNode(NodePattern),
    /// Mint one edge between matched/created bindings; the triple is the
    /// edge's whole identity.
    CreateEdge {
        from: Binding,
        kind: String,
        to: Binding,
        arrow: ArrowDir,
        note: Option<ValueExpr>,
    },
    /// Field-level change under per-field revision bookkeeping.
    Set { target: FieldRef, value: ValueExpr },
    /// Edges only — nodes are never removed.
    RemoveEdge {
        from: Binding,
        kind: String,
        to: Binding,
        arrow: ArrowDir,
    },
    /// `SEAL (b)` — set the machine-owned `sealed` flag on the ONE node a
    /// MATCH-by-identifier bound to `b`. The only statement form that
    /// touches the flag; nothing clears it. See [`crate::ladle::compile`]'s
    /// `seal:node` row for what it refuses.
    Seal { target: Binding },
    /// `IMPORT (b: pool/type { id: $id, ... })` — land one node under the
    /// SERIAL id its source already holds (carried ids enter ONLY through
    /// this import-only statement form on the one door). Parallel to [`Mutation::CreateNode`] and nothing else:
    /// the head's field map MUST name `id`, the id must carry the type's own
    /// prefix and a serial rest, and only an `Agent` initiator may submit it
    /// (`exec::check_import_initiator`). It compiles to an ordinary
    /// `Delta::CreateNode` — an imported node is a created node whose serial
    /// was known at compile. ADDITIVE serde tag `import_node`: every stored
    /// payload is deltas, never AST, so nothing on disk changes.
    ImportNode(NodePattern),
}

/// RETURN — a projection over bindings, so no create needs a follow-up read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReturnClause {
    pub items: Vec<ReturnItem>,
}

/// One projection item; un-aliased column names are the item's source text
/// (frozen grammar).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReturnItem {
    pub expr: ReturnExpr,
    pub alias: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "ret", rename_all = "snake_case")]
pub enum ReturnExpr {
    /// `b.field(.key)*` — `b.id` returns the identifier.
    Field(FieldRef),
    /// `REV(b)` / `REV(b.f)`.
    Rev {
        binding: Binding,
        field: Option<String>,
    },
}

// ---------------------------------------------------------------------------
// Derived readers. Pure functions over the frozen tree.
// ---------------------------------------------------------------------------

/// The reserved field name for a node's identifier (frozen lexical rule).
pub const FIELD_ID: &str = "id";

/// The machine-owned schema-version stamp's reserved read name (it is
/// invisible in every default render, visible on EXPLICIT ASK — naming it in
/// a RETURN projection or a WHERE condition is that explicit ask). Never
/// declarable as a grammar field (`schema_def::MACHINE_OWNED_FIELDS`), never
/// SETtable, and carrying no per-field revision.
pub const FIELD_SCHEMA_VERSION: &str = "schema_version";
/// The reserved field name for a node's state (frozen lexical rule).
pub const FIELD_STATE: &str = "state";
/// The machine-owned seal flag's read name: readable by field path
/// (`RETURN b.sealed`, `WHERE b.sealed = true`), never declarable
/// (`schema_def::MACHINE_OWNED_FIELDS`), never SETtable, set only by the
/// `SEAL` statement form.
pub const FIELD_SEALED: &str = "sealed";

impl Binding {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Binding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl CmpOp {
    /// The frozen source text of the comparison.
    pub fn as_str(&self) -> &'static str {
        match self {
            CmpOp::Eq => "=",
            CmpOp::Ne => "!=",
            CmpOp::Lt => "<",
            CmpOp::Le => "<=",
            CmpOp::Gt => ">",
            CmpOp::Ge => ">=",
        }
    }
}

impl ArrowDir {
    /// The arrow's source text around a kind, e.g. `-[wrote]->`.
    pub fn render(&self, kind: &str) -> String {
        match self {
            ArrowDir::LeftToRight => format!("-[{kind}]->"),
            ArrowDir::RightToLeft => format!("<-[{kind}]-"),
        }
    }
}

impl fmt::Display for FieldRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.binding)?;
        for p in &self.path {
            write!(f, ".{p}")?;
        }
        Ok(())
    }
}

impl fmt::Display for ValueExpr {
    /// Canonical source text of a value: JSON compact form for literals,
    /// `$name` for parameters.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ValueExpr::Literal(v) => f.write_str(&v.to_string()),
            ValueExpr::Param(p) => write!(f, "${p}"),
        }
    }
}

impl fmt::Display for Condition {
    /// The condition's canonical source text — what
    /// [`crate::witness::Witness::FailedCondition`] carries as `condition`
    /// (the failing condition and the binding it failed on).
    /// Whitespace is normalized; every other lexeme is the author's.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Condition::Compare { left, op, right } => {
                write!(f, "{left} {} {right}", op.as_str())
            }
            Condition::Exists {
                from,
                kind,
                to,
                direction,
            } => {
                let arrow = match direction {
                    crate::schema_def::Direction::Out => ArrowDir::LeftToRight,
                    crate::schema_def::Direction::In => ArrowDir::RightToLeft,
                };
                let far = match to {
                    Some(b) => format!("({b})"),
                    None => "()".to_string(),
                };
                write!(f, "EXISTS ({from}) {} {far}", arrow.render(kind))
            }
            Condition::Not(inner) => write!(f, "NOT ({inner})"),
            Condition::And(parts) => write_joined(f, parts, "AND"),
            Condition::Or(parts) => write_joined(f, parts, "OR"),
        }
    }
}

fn write_joined(f: &mut fmt::Formatter<'_>, parts: &[Condition], sep: &str) -> fmt::Result {
    f.write_str("(")?;
    for (i, p) in parts.iter().enumerate() {
        if i > 0 {
            write!(f, " {sep} ")?;
        }
        write!(f, "{p}")?;
    }
    f.write_str(")")
}

impl fmt::Display for ReturnExpr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReturnExpr::Field(r) => write!(f, "{r}"),
            ReturnExpr::Rev { binding, field } => match field {
                Some(fd) => write!(f, "REV({binding}.{fd})"),
                None => write!(f, "REV({binding})"),
            },
        }
    }
}

impl ReturnItem {
    /// The projection's column name: the alias when given, otherwise the
    /// item's source text (frozen grammar: `"new.id"`, `"REV(old)"`).
    ///
    /// INTERIOR CHOICE: the un-aliased name
    /// is the item's CANONICAL rendering, so interior whitespace an author
    /// wrote inside `REV( old )` is normalized to `REV(old)`. The AST carries
    /// no source spans; storing them would have added a field to a frozen
    /// struct for a whitespace nicety.
    pub fn column_name(&self) -> String {
        match &self.alias {
            Some(a) => a.clone(),
            None => self.expr.to_string(),
        }
    }

    /// The binding this item projects from.
    pub fn binding(&self) -> &Binding {
        match &self.expr {
            ReturnExpr::Field(r) => &r.binding,
            ReturnExpr::Rev { binding, .. } => binding,
        }
    }
}

impl NodePattern {
    /// The value bound to a field-map entry, if the pattern names it.
    pub fn field(&self, name: &str) -> Option<&ValueExpr> {
        self.fields.iter().find(|(f, _)| f == name).map(|(_, v)| v)
    }
}

impl MatchClause {
    /// Every fresh pattern head this clause introduces, in source order.
    pub fn heads(&self) -> Vec<&NodePattern> {
        match self {
            MatchClause::Node(h) => vec![h],
            MatchClause::Hop(h) => [&h.left, &h.right]
                .into_iter()
                .filter_map(|e| match e {
                    PatternEnd::Head(p) => Some(p),
                    PatternEnd::Bound(_) => None,
                })
                .collect(),
        }
    }

    /// Every binding this clause mentions (fresh heads and bound ends).
    pub fn bindings(&self) -> Vec<&Binding> {
        match self {
            MatchClause::Node(h) => vec![&h.binding],
            MatchClause::Hop(h) => [&h.left, &h.right]
                .into_iter()
                .map(|e| match e {
                    PatternEnd::Head(p) => &p.binding,
                    PatternEnd::Bound(b) => b,
                })
                .collect(),
        }
    }
}

impl Statement {
    /// Every binding the statement introduces, mapped to the pool and node
    /// type its head qualified it with — MATCH heads and CREATE-node heads
    /// alike (pool qualification is required on every head, so
    /// this index is total over the statement's bindings).
    pub fn binding_types(&self) -> BTreeMap<String, (PoolName, String)> {
        let mut out = BTreeMap::new();
        for m in &self.matches {
            for h in m.heads() {
                out.insert(h.binding.0.clone(), (h.pool.clone(), h.node_type.clone()));
            }
        }
        for mu in &self.mutations {
            if let Mutation::CreateNode(h) | Mutation::ImportNode(h) = mu {
                out.insert(h.binding.0.clone(), (h.pool.clone(), h.node_type.clone()));
            }
        }
        out
    }

    /// The bindings CREATE mints, in source order — the ordinal of each is
    /// its position here (the frozen node-id minting rule keys off it).
    pub fn created_bindings(&self) -> Vec<&NodePattern> {
        self.mutations
            .iter()
            .filter_map(|m| match m {
                Mutation::CreateNode(h) => Some(h),
                _ => None,
            })
            .collect()
    }

    /// The bindings IMPORT lands under carried ids, in source order.
    /// Deliberately NOT part of [`Statement::created_bindings`]: that list's
    /// positions are the frozen minting ordinal `k`, and an import mints no
    /// provisional, so every existing statement's ordinals are unchanged.
    pub fn imported_bindings(&self) -> Vec<&NodePattern> {
        self.mutations
            .iter()
            .filter_map(|m| match m {
                Mutation::ImportNode(h) => Some(h),
                _ => None,
            })
            .collect()
    }

    /// The bindings MATCH binds (everything not minted by a CREATE or landed
    /// by an IMPORT — an import head binds the node it lands, never a match
    /// set, so it is excluded here exactly as a CREATE head is).
    pub fn matched_bindings(&self) -> BTreeSet<String> {
        let created: BTreeSet<String> = self
            .created_bindings()
            .iter()
            .chain(self.imported_bindings().iter())
            .map(|h| h.binding.0.clone())
            .collect();
        self.binding_types()
            .into_keys()
            .filter(|b| !created.contains(b))
            .collect()
    }

    /// Every `$param` the text references, anywhere (the text is
    /// analyzable precisely because values never ride inside it).
    pub fn referenced_params(&self) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        let note = |v: &ValueExpr, out: &mut BTreeSet<String>| {
            if let ValueExpr::Param(p) = v {
                out.insert(p.clone());
            }
        };
        for m in &self.matches {
            for h in m.heads() {
                for (_, v) in &h.fields {
                    note(v, &mut out);
                }
            }
        }
        for c in &self.wheres {
            collect_condition_params(c, &mut out);
        }
        for e in &self.expects {
            match e {
                Expectation::Revision { expected, .. }
                | Expectation::Value { expected, .. }
                | Expectation::Count { expected, .. } => note(expected, &mut out),
            }
        }
        for m in &self.mutations {
            match m {
                Mutation::CreateNode(h) | Mutation::ImportNode(h) => {
                    for (_, v) in &h.fields {
                        note(v, &mut out);
                    }
                }
                Mutation::CreateEdge { note: n, .. } => {
                    if let Some(v) = n {
                        note(v, &mut out);
                    }
                }
                Mutation::Set { value, .. } => note(value, &mut out),
                Mutation::RemoveEdge { .. } | Mutation::Seal { .. } => {}
            }
        }
        out
    }

    /// True when the statement mutates nothing — the bulk-read form (the
    /// same grammar with no mutation clauses).
    pub fn is_read_only(&self) -> bool {
        self.mutations.is_empty()
    }
}

fn collect_condition_params(c: &Condition, out: &mut BTreeSet<String>) {
    match c {
        Condition::Compare { right, .. } => {
            if let ValueExpr::Param(p) = right {
                out.insert(p.clone());
            }
        }
        Condition::Exists { .. } => {}
        Condition::Not(inner) => collect_condition_params(inner, out),
        Condition::And(parts) | Condition::Or(parts) => {
            for p in parts {
                collect_condition_params(p, out);
            }
        }
    }
}
