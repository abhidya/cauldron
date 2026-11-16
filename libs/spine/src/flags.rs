//! **THE FLAG RULE**: a FLAG is a machine-read
//! field on a node, and a ripple may set one only when it is a type-two ripple
//! whose per-write `scope` lists that field.
//!
//! The flag fields are the v6 grammar's (`pinned` is not among them — it is
//! not a v6 field):
//!
//! | flag field | on |
//! |---|---|
//! | `meta.stop` | directive, milestone |
//! | `meta.blocking` | question |
//! | `meta.addressed` | idea |
//! | `retired`, `provisional`, `deferred_by` | region |
//! | `sealed` | every type (only the `SEAL` statement touches it) |
//!
//! WHO IT BINDS — four EXPLICIT arms, one per [`Initiator`] variant:
//!
//! - `Ripple { kind, scope }` — JUDGED: a type-one ripple is refused every
//!   flag; a type-two ripple only the flags its `scope` lists.
//! - `Agent` — ADMITTED through the TRANSITIONAL ARM. This is a tolerated
//!   deviation from the flag rule, not a widening, and it is meant to be REMOVED later: an
//!   `Agent` initiator MAY write the flag fields — `meta.stop`,
//!   `meta.blocking`, `meta.addressed`, region
//!   `retired`/`provisional`/`deferred_by`, and `sealed` by `SEAL` —
//!   through it. Existing agent-side writers and the v5-pool import
//!   both write flags as an `Agent` today.
//! - `CoreMechanical` — ADMITTED: the daemon's own triggers, order two, and
//!   its in-process writes.
//! - `External` — ADMITTED, and not covered by the rule: the flag rule
//!   names the ripple kinds, `CoreMechanical` and `Agent`, and is silent on
//!   `External`. No envelope carrying it reaches this check through either
//!   door (`Envelope::check_provenance` refuses it as `Usage` first), so the
//!   arm is reachable only by a direct caller of this `pub fn`.
//!
//! WHERE IT RUNS: once, over the COMPILED payload, inside the door's compile
//! phase (`exec`, beside the seal and grammar refusals), on both callers —
//! commit and validate — before any row exists. Flag-ness is a static fact of
//! a delta (its path and its node's type, which never changes), so the
//! re-compile under the granted locks, which must equal this payload or the
//! attempt restarts, cannot carry a flag this check did not see.

use crate::delta::{CompiledPayload, Delta};
use crate::envelope::{Initiator, RippleKind};
use crate::error::{Result, SpineError};
use crate::read::MatchSet;
use crate::witness::Witness;
use serde_json::Value;
use std::collections::BTreeMap;

/// One flag field: its dotted path and the node types that carry it (`None` =
/// every type).
pub struct FlagField {
    pub name: &'static str,
    pub path: &'static [&'static str],
    pub on: Option<&'static [&'static str]>,
}

/// The flag fields a SET or CREATE can reach (the table above, less
/// `sealed`, which only a `SEAL` delta touches — see [`SEALED`]).
pub const FLAG_FIELDS: &[FlagField] = &[
    FlagField { name: "meta.stop", path: &["meta", "stop"], on: Some(&["directive", "milestone"]) },
    FlagField { name: "meta.blocking", path: &["meta", "blocking"], on: Some(&["question"]) },
    FlagField { name: "meta.addressed", path: &["meta", "addressed"], on: Some(&["idea"]) },
    FlagField { name: "retired", path: &["retired"], on: Some(&["region"]) },
    FlagField { name: "provisional", path: &["provisional"], on: Some(&["region"]) },
    FlagField { name: "deferred_by", path: &["deferred_by"], on: Some(&["region"]) },
];

/// The seal flag's name in a scope and a witness.
pub const SEALED: &str = "sealed";

fn applies(f: &FlagField, node_type: Option<&str>) -> bool {
    match (f.on, node_type) {
        (None, _) => true,
        // A node whose type the payload cannot name is judged against EVERY
        // flag: an unresolved type never widens what a ripple may write.
        (Some(_), None) => true,
        (Some(types), Some(t)) => types.contains(&t),
    }
}

/// Does `value` carry the key path `rest` (each element an object key)?
fn carries(value: &Value, rest: &[&str]) -> bool {
    let mut v = value;
    for k in rest {
        match v.get(*k) {
            Some(next) => v = next,
            None => return false,
        }
    }
    true
}

/// Does a SET at `path` with `value` write flag `f`? Yes when the SET is AT or
/// BELOW the flag's path, and when it replaces an object ABOVE the flag's path
/// at all — a whole-object SET (`SET q.meta = {...}`) replaces whatever the
/// flag held, setting it when the value carries the key and clearing it when
/// it does not, and either is a write of the flag.
fn set_touches(f: &FlagField, path: &[String]) -> bool {
    let n = path.len().min(f.path.len());
    path[..n].iter().map(String::as_str).eq(f.path[..n].iter().copied())
}

/// Does a CREATE's initial `fields` set flag `f`? Only when the value is
/// there: a node minted without the key has never held the flag.
fn create_touches(f: &FlagField, fields: &BTreeMap<String, Value>) -> bool {
    let (head, rest) = f.path.split_first().expect("a flag path is never empty");
    fields.get(*head).is_some_and(|v| carries(v, rest))
}

/// **THE CHECK.** Refuses — whole, every offending delta named — a payload in
/// which a ripple writes a flag field its kind and scope do not grant.
/// Every other initiator is admitted by an explicit arm (see the module doc).
pub fn check_flag_grants(
    initiator: &Initiator,
    payload: &CompiledPayload,
    matched: &MatchSet,
) -> Result<()> {
    let (kind, scope) = match initiator {
        // Type one never; type two in scope.
        Initiator::Ripple { kind, scope, .. } => (kind, scope),
        // THE TRANSITIONAL ARM — a tolerated deviation from the flag rule, not a widening;
        // meant to be removed later.
        Initiator::Agent { .. } => return Ok(()),
        // The daemon's own triggers and in-process writes.
        Initiator::CoreMechanical { .. } => return Ok(()),
        // The flag rule is silent on `External`; today's pass is kept.
        // Unreachable through either door (`Envelope::check_provenance`
        // refuses it as `Usage` before this check) — only a direct caller
        // of this `pub fn` reaches it.
        Initiator::External => return Ok(()),
    };
    let granted = |field: &str| *kind == RippleKind::TypeTwo && scope.iter().any(|s| s == field);
    let mut witnesses = Vec::new();
    for (ordinal, part) in payload.parts.iter().enumerate() {
        let mut types = matched.type_resolver(&part.pool);
        for d in &part.deltas {
            if let Delta::CreateNode { id, node_type, .. } = d {
                types.insert(id.clone(), node_type.clone());
            }
        }
        let mut refuse = |node: &crate::pool::NodeId, field: &str| {
            if !granted(field) {
                witnesses.push(Witness::FlagNotWritable {
                    pool: part.pool.clone(),
                    part: Some(ordinal as u32),
                    node: node.0.clone(),
                    field: field.to_string(),
                    initiator_kind: kind.as_str().to_string(),
                    scope: Some(scope.clone()),
                });
            }
        };
        for d in &part.deltas {
            match d {
                Delta::CreateNode {
                    id,
                    node_type,
                    fields,
                    ..
                } => {
                    for f in FLAG_FIELDS {
                        if applies(f, Some(node_type.as_str())) && create_touches(f, fields) {
                            refuse(id, f.name);
                        }
                    }
                }
                Delta::SetField { node, path, .. } => {
                    let t = types.get(node).map(String::as_str);
                    for f in FLAG_FIELDS {
                        if applies(f, t) && set_touches(f, path) {
                            refuse(node, f.name);
                        }
                    }
                }
                Delta::Seal { node } => refuse(node, SEALED),
                Delta::AddEdge { .. } | Delta::RemoveEdge { .. } => {}
            }
        }
    }
    if witnesses.is_empty() {
        Ok(())
    } else {
        Err(SpineError::Refused { witnesses })
    }
}
