//! `SpineError` — every way the spine fails, each loudly and each structured.
//!
//! Idiom inherited from `soup` (shared contract): loud failure with structured
//! payloads; a store that will not answer is never reported as an empty store.
//!
//! # The stub shape
//!
//! Every unimplemented body in this crate is a deferred-body stub that fails
//! loudly by name, and the ONE shape used everywhere is:
//!
//! ```text
//! Err(SpineError::NotBuilt("module::function"))
//! ```
//!
//! via the [`not_built!`](crate::not_built) macro. No stub panics, no stub
//! returns a silent `Ok(())`, no stub pretends to work. The name is the
//! module path inside this crate plus the item, e.g. `"ladle::parse::parse"`
//! or `"pool::Pool::apply_part"`. `tests/smoke.rs` asserts this conformance.

use crate::witness::Witness;
use serde_json::{json, Map, Value};

/// Return a deferred-body refusal, loudly, by name.
///
/// The one stub shape (see module docs). Implementing a body DELETES the
/// macro call; real failures never route through it.
#[macro_export]
macro_rules! not_built {
    ($name:literal) => {
        return Err($crate::error::SpineError::NotBuilt($name))
    };
}

/// Every failure the spine can return.
///
/// Refusal (a statement judged and turned away, whole, with witnesses —
/// refusal is whole and structured) is deliberately a
/// DIFFERENT variant from hard failure (physics: full disk, corruption) and
/// from the fail-closed [`SpineError::Halted`] state.
#[derive(Debug)]
pub enum SpineError {
    /// A deferred body, failing loudly by name. The stub shape; never
    /// present once every body is implemented.
    NotBuilt(&'static str),

    /// The statement was refused, WHOLE — nothing landed anywhere. Carries
    /// the structured witnesses every later test suite
    /// asserts on.
    Refused { witnesses: Vec<Witness> },

    /// The spine is in the fail-closed halted state.
    /// Effectively read-only; no new commit begins.
    Halted { reason: String },

    /// A fault-seam variable is set but this build compiled the seam out.
    /// Refusing loudly is what makes "absent or refusing" provable in the production build.
    ///
    /// ONE variant for BOTH forms of the seam (an earlier sibling
    /// `PauseSeamRefused` was folded into it): `variable` names WHICH seam variable was set —
    /// [`crate::fault::FAULT_ENV`] (`SPINE_FAULT`, the kill form) or
    /// [`crate::fault::PAUSE_ENV`] (`SPINE_PAUSE`, the pause form) — as a
    /// structural fact a test asserts on, never a prefix of a string; `value`
    /// is what it was set to. `kind()` is `"fault_seam_refused"` for both.
    FaultSeamRefused { variable: String, value: String },

    /// The match phase exhausted its fuel ceiling.
    /// An ENGINE error, never a silent partial result. The ceiling is
    /// configuration ([`crate::config::SpineConfig::fuel_ceiling`]).
    FuelExhausted { spent: u64, ceiling: u64 },

    /// Physics or corruption: full disk, permissions, a store that will not
    /// parse ("always-writable, honestly bounded"). On the spine
    /// append path this leads to [`SpineError::Halted`].
    Hard { message: String },

    /// The caller misused the API (bad path, malformed config, a reserved
    /// pool name). Fix the call; nothing happened.
    Usage { message: String },

    /// A named thing does not exist. Never conflated with an empty result.
    NotFound { message: String },

    // `RecoveredCommitReplyUnruled` used to live here (an honest
    // refusal: a duplicate of a commit that RECOVERY
    // rolled forward, whose RETURN projection died with its process). It is
    // GONE, not renamed: recovery now records which nodes each
    // binding bound and projects RETURN at its own cleared flip through the
    // same projection the live door runs, so a recovered row's result IS
    // the original outcome and the door answers it like any landed row
    // (`Outcome::Committed { deduplicated: true, .. }`). The question the
    // variant carried dissolved rather than got answered.

    /// The underlying SQLite file is busy past its timeout — a foreign writer
    /// holds it. Distinct from `Hard` so callers wait-and-retry rather than
    /// repair (idiom inherited from soup's exit-code contract).
    Busy { message: String },

    /// A tranche act that cannot run NOW and must be called again (the
    /// `busy` rows that are the tranche's own): `why` is
    /// `queued` | `locked` | `switch` | `landing_in_progress`, and `fields`
    /// carries the row's facts (`position`, `ahead`, `landing_error`, …)
    /// verbatim into the payload beside `why`. Kind `tranche_busy`; the
    /// chassis code is `busy`. NOTE: `locked` and a tranche-named
    /// holder are outside every retry bound.
    TrancheBusy {
        why: String,
        fields: Map<String, Value>,
    },

    /// The key does not admit this act (the `forbidden
    /// {kind: "tranche_key"}` row): the key matches no tranche under `T`,
    /// matches a tranche of another service, or a `begin` under a live `T`
    /// carries another key. `why` is the DISTINGUISHING fact
    /// (`no_tranche` | `other_service` | `live_other_key`) for the spine's
    /// own callers and tests; the door answers all three byte-identically,
    /// so a wrong key learns nothing. Kind `tranche_key`.
    TrancheKey { why: String },

    /// The spine file carries a spine schema version NEWER than this binary
    /// knows: refused at open, by name, because an older
    /// binary's ordinary recovery pass would not know a tranche's anchor row
    /// from any other un-decided row and would abort it. Not the POOL
    /// grammar's `schema_version` (that is `tx_parts.schema_version`); see
    /// [`crate::spine_db::SPINE_SCHEMA_VERSION`].
    SpineSchemaNewer { spine_version: u32, binary_version: u32 },

    /// A path a LATER build lands, refused loudly by name (no stub pretends
    /// to work). `what` names the refused thing and `step`
    /// the build that lands it. Kind
    /// `not_yet`; the chassis code is `unsupported`.
    NotYet { what: String, step: &'static str },
}

impl std::fmt::Display for SpineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for SpineError {}

impl SpineError {
    /// One human-readable line. The MACHINE contract is [`SpineError::payload`];
    /// tests assert on structure, never on this prose.
    pub fn message(&self) -> String {
        match self {
            SpineError::NotBuilt(name) => {
                format!("not built: `{name}` is a deferred-body stub of the skeleton")
            }
            SpineError::Refused { witnesses } => {
                format!(
                    "statement refused whole; nothing landed; {} witness(es)",
                    witnesses.len()
                )
            }
            SpineError::Halted { reason } => {
                format!("the spine is halted (fail-closed, read-only): {reason}")
            }
            SpineError::FaultSeamRefused { variable, value } => format!(
                "{variable}={value} is set but this build has no fault-injection seam; refusing \
                 loudly rather than silently ignoring it"
            ),
            SpineError::FuelExhausted { spent, ceiling } => {
                format!("match fuel exhausted: spent {spent} against ceiling {ceiling}")
            }
            SpineError::Hard { message }
            | SpineError::Usage { message }
            | SpineError::NotFound { message }
            | SpineError::Busy { message } => message.clone(),
            SpineError::TrancheBusy { why, .. } => {
                format!("the tranche cannot take this act now (`{why}`); call again")
            }
            SpineError::TrancheKey { .. } => {
                "the tranche key does not admit this act".to_string()
            }
            SpineError::SpineSchemaNewer {
                spine_version,
                binary_version,
            } => format!(
                "refusing to open: this spine carries spine schema version {spine_version} and this binary knows only {binary_version}; an older binary's recovery would abort a live tranche's anchor row"
            ),
            SpineError::NotYet { what, step } => {
                format!("{what} is not built in this build; it lands with {step}")
            }
        }
    }

    /// The full structured JSON document for a failure. `error` and `kind`
    /// are always present; refusals carry their witnesses under `witnesses`
    /// (structured, machine-readable, asserted on structurally).
    pub fn payload(&self) -> Value {
        let mut m = Map::new();
        m.insert("error".into(), json!(self.message()));
        m.insert("kind".into(), json!(self.kind()));
        match self {
            SpineError::NotBuilt(name) => {
                m.insert("not_built".into(), json!(name));
            }
            SpineError::Refused { witnesses } => {
                m.insert(
                    "witnesses".into(),
                    serde_json::to_value(witnesses).expect("witnesses serialize by construction"),
                );
            }
            SpineError::Halted { reason } => {
                m.insert("reason".into(), json!(reason));
            }
            SpineError::FaultSeamRefused { variable, value } => {
                // Which seam variable, and what it held — the two structural
                // facts (the old per-variant keys `spine_fault` /
                // `spine_pause` folded into these).
                m.insert("variable".into(), json!(variable));
                m.insert("value".into(), json!(value));
            }
            SpineError::FuelExhausted { spent, ceiling } => {
                m.insert("spent".into(), json!(spent));
                m.insert("ceiling".into(), json!(ceiling));
            }
            SpineError::TrancheBusy { why, fields } => {
                m.insert("why".into(), json!(why));
                for (k, v) in fields {
                    m.insert(k.clone(), v.clone());
                }
            }
            SpineError::TrancheKey { why } => {
                m.insert("why".into(), json!(why));
            }
            SpineError::SpineSchemaNewer {
                spine_version,
                binary_version,
            } => {
                m.insert("spine_version".into(), json!(spine_version));
                m.insert("binary_version".into(), json!(binary_version));
            }
            SpineError::NotYet { what, step } => {
                m.insert("what".into(), json!(what));
                m.insert("step".into(), json!(step));
            }
            _ => {}
        }
        Value::Object(m)
    }

    /// A stable machine tag for the variant, for payloads and logs.
    pub fn kind(&self) -> &'static str {
        match self {
            SpineError::NotBuilt(_) => "not_built",
            SpineError::Refused { .. } => "refused",
            SpineError::Halted { .. } => "halted",
            SpineError::FaultSeamRefused { .. } => "fault_seam_refused",
            SpineError::FuelExhausted { .. } => "fuel_exhausted",
            SpineError::Hard { .. } => "hard",
            SpineError::Usage { .. } => "usage",
            SpineError::NotFound { .. } => "not_found",
            SpineError::Busy { .. } => "busy",
            SpineError::TrancheBusy { .. } => "tranche_busy",
            SpineError::TrancheKey { .. } => "tranche_key",
            SpineError::SpineSchemaNewer { .. } => "spine_schema_newer",
            SpineError::NotYet { .. } => "not_yet",
        }
    }

    /// One refusal from one witness — the common constructor.
    pub fn refuse(witness: Witness) -> SpineError {
        SpineError::Refused {
            witnesses: vec![witness],
        }
    }

    pub fn hard(message: impl Into<String>) -> SpineError {
        SpineError::Hard {
            message: message.into(),
        }
    }

    pub fn usage(message: impl Into<String>) -> SpineError {
        SpineError::Usage {
            message: message.into(),
        }
    }

    pub fn not_found(message: impl Into<String>) -> SpineError {
        SpineError::NotFound {
            message: message.into(),
        }
    }
}

pub type Result<T> = std::result::Result<T, SpineError>;

/// A rusqlite failure becomes a spine failure. BUSY/LOCKED surviving the
/// busy timeout is [`SpineError::Busy`] (wait and retry — lock WAITING is
/// bounded polling, but the files themselves can
/// still contend); everything else is hard — a store that will not answer is
/// never an empty store (soup idiom, inherited deliberately).
impl From<rusqlite::Error> for SpineError {
    fn from(e: rusqlite::Error) -> Self {
        if let rusqlite::Error::SqliteFailure(f, _) = &e {
            if f.code == rusqlite::ErrorCode::DatabaseBusy
                || f.code == rusqlite::ErrorCode::DatabaseLocked
            {
                return SpineError::Busy {
                    message: format!("database busy past the timeout: {e}"),
                };
            }
        }
        SpineError::Hard {
            message: format!("storage failure: {e}"),
        }
    }
}
