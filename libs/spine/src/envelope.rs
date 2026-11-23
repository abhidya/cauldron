//! The statement envelope — what a caller hands the door (each field with
//! its why; the envelope format version; the provenance).
//!
//! # What is implemented here
//!
//! - [`Envelope::hash_identity`], exactly as this module's frozen doc
//!   specifies it (SHA-256 over length-prefixed elements; `mode` and
//!   `format_version` deliberately outside the identity).
//! - Envelope VALIDATION, in three named gates the door composes:
//!   [`Envelope::check_format_version`] (refuses naming BOTH versions),
//!   [`Envelope::check_provenance`] (the harness must have supplied it —
//!   nothing self-reports inside the statement), and
//!   [`Envelope::validate`], which runs both, parses the text, applies the
//!   staged-coverage table and proves the bindings cover every `$param`.

use crate::error::{Result, SpineError};
use crate::witness::Witness;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// THE envelope format version this crate speaks. A mismatching envelope refuses with
/// [`crate::witness::Witness::VersionMismatch`] naming both versions.
/// Bump policy (a default): it moves when the envelope
/// grammar changes shape, bumped by whoever changes the grammar; an old
/// batch is never best-effort parsed.
pub const ENVELOPE_FORMAT_VERSION: u32 = 1;

/// Named parameters for one statement: `$name` in the text resolves here.
/// A `BTreeMap` DELIBERATELY — sorted keys are what make the canonical JSON
/// of the hash identity canonical.
pub type Bindings = BTreeMap<String, serde_json::Value>;

/// One statement envelope. The statement-shaped door is fixed; the
/// envelope's own four extensions, including the `mode` flag, are not yet
/// settled, so the field set is built-to, never presented as final.
///
/// - text and bindings SEPARATELY: values are never spliced into the text,
///   so the text stays analyzable, reusable, injection-safe by construction.
/// - provenance is HARNESS-SUPPLIED, never self-reported.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    /// The ladle statement text (syntax frozen in [`crate::ladle`]).
    pub statement: String,
    /// The named parameters the text references as `$name`.
    pub bindings: Bindings,
    /// Which envelope format this was written against; mismatch refuses
    /// naming the mismatch.
    pub format_version: u32,
    /// Who asked and through what.
    pub provenance: Provenance,
    /// Commit, or validate-without-landing (one validator, two
    /// callers — the migration's dry run is the SAME statement, mode flipped).
    pub mode: Mode,
    /// Optional caller idempotency nonce, part of the hash identity
    /// so a caller can distinguish two deliberate
    /// identical submissions.
    pub idempotency_nonce: Option<String>,
}

impl Envelope {
    /// The transaction's permanent hash identity: a unique hash ID, by the
    /// fixed algorithm below.
    ///
    /// FROZEN ALGORITHM (every implementation computes exactly this): SHA-256 over the
    /// concatenation, for each element in order — statement text, canonical
    /// JSON of bindings, canonical JSON of initiator, canonical JSON of
    /// instrument, the nonce (or zero bytes when `None`) — of an 8-byte
    /// big-endian length prefix followed by the element's UTF-8 bytes.
    /// Canonical JSON = `serde_json` compact form over the types in this
    /// module (maps are `BTreeMap`s, so keys are sorted). `mode` and
    /// `format_version` are deliberately OUTSIDE the identity: a validate
    /// dry run and its commit are the same ask, and a re-encoded envelope is
    /// not a new transaction. A duplicate hash returns the original outcome
    /// and lands nothing (exactly-once).
    pub fn hash_identity(&self) -> Result<TxHash> {
        let bindings = canonical_json(&self.bindings)?;
        let initiator = canonical_json(&self.provenance.initiator)?;
        let instrument = canonical_json(&self.provenance.instrument)?;
        let nonce = self.idempotency_nonce.as_deref().unwrap_or("");

        let mut h = Sha256::new();
        // The five elements, IN ORDER, each length-prefixed. The prefix is
        // what makes the concatenation injective: without it `("ab","c")` and
        // `("a","bc")` would hash alike and two different asks would dedupe
        // onto one another (exactly-once turns on this).
        for element in [
            self.statement.as_str(),
            bindings.as_str(),
            initiator.as_str(),
            instrument.as_str(),
            nonce,
        ] {
            let bytes = element.as_bytes();
            h.update((bytes.len() as u64).to_be_bytes());
            h.update(bytes);
        }
        let digest = h.finalize();
        let mut out = [0u8; 32];
        out.copy_from_slice(&digest);
        Ok(TxHash(out))
    }

    /// GATE ONE (door phase 3): an
    /// envelope written against another grammar version refuses, naming BOTH
    /// versions — never best-effort parsed, because an old recording read
    /// against a moved grammar is misread SILENTLY.
    pub fn check_format_version(&self) -> Result<()> {
        if self.format_version == ENVELOPE_FORMAT_VERSION {
            return Ok(());
        }
        Err(SpineError::refuse(Witness::VersionMismatch {
            envelope_version: self.format_version,
            supported_version: ENVELOPE_FORMAT_VERSION,
        }))
    }

    /// GATE TWO: provenance is PRESENT.
    /// The door trusts the harness to supply it and never lets the statement
    /// self-report — so the only failure this gate can see is a stamp the
    /// caller left blank, and a blank stamp would land a transaction row
    /// whose whole purpose is to be trusted about the past.
    ///
    /// INTERIOR CHOICE: a blank stamp is
    /// [`SpineError::Usage`], not a refusal witness. It is caller misuse of
    /// the API — the harness, not the statement author — and the witness set
    /// has no shape for it; minting one would be inventing contract
    /// surface a later release inherits.
    pub fn check_provenance(&self) -> Result<()> {
        let p = &self.provenance;
        match &p.initiator {
            Initiator::Agent { thread, dispatch } => {
                if thread.trim().is_empty() || dispatch.trim().is_empty() {
                    return Err(SpineError::usage(
                        "provenance is harness-supplied and must be complete: an `agent` initiator carries both a thread and a dispatch",
                    ));
                }
            }
            Initiator::CoreMechanical { operation } => {
                if operation.trim().is_empty() {
                    return Err(SpineError::usage(
                        "provenance is harness-supplied and must be complete: a `core-mechanical` initiator carries the operation's identity",
                    ));
                }
            }
            // THE RIPPLE INITIATOR (kind, run id, step id AND scope, all four
            // from the runner's per-write assertion). A blank run or step is the same missing
            // stamp a blank thread is; a type-one ripple that carries a scope
            // is a runner defect (type one never sets a flag),
            // and a blank scope entry names no field at all.
            Initiator::Ripple {
                kind,
                run_id,
                step_id,
                scope,
            } => {
                if run_id.trim().is_empty() || step_id.trim().is_empty() {
                    return Err(SpineError::usage(
                        "provenance is harness-supplied and must be complete: a `ripple` initiator carries both a run id and a step id",
                    ));
                }
                if *kind == RippleKind::TypeOne && !scope.is_empty() {
                    return Err(SpineError::usage(
                        "a `type_one` ripple carries no scope: only a type-two ripple is granted flag fields",
                    ));
                }
                // The type-three identity is the tranche's ANCHOR
                // row's provenance only; it writes nothing and is granted
                // nothing, so a scope on it is a runner defect, as on type one.
                if *kind == RippleKind::TypeThree && !scope.is_empty() {
                    return Err(SpineError::usage(
                        "a `type_three` ripple carries no scope: the type-three identity is a tranche's anchor and is granted no flag fields",
                    ));
                }
                if scope.iter().any(|f| f.trim().is_empty()) {
                    return Err(SpineError::usage(
                        "a ripple's scope lists flag fields by name; a blank entry names none",
                    ));
                }
            }
            // `external` is OUT OF SCOPE THIS BUILD: external systems never
            // write directly, and only the enum value exists. The enum value
            // is reserved ROOM; a write actually carrying it in
            // this build is refused loudly — an out-of-scope path that
            // accepted its traffic would be a stub pretending to work.
            Initiator::External => {
                return Err(SpineError::usage(
                    "initiator `external` is out of scope in this build: external systems \
                     never write directly; the enum value exists as reserved \
                     room and the boundary machinery that would stamp it is \
                     not built here",
                ));
            }
        }
        if p.instrument.name.trim().is_empty() || p.instrument.version.trim().is_empty() {
            return Err(SpineError::usage(
                "provenance is harness-supplied and must be complete: the instrument carries a name and a version",
            ));
        }
        Ok(())
    }

    /// Every envelope-level gate, in the door's own order (`exec` phases 3
    /// and 5), handing back the parsed statement so the door parses once:
    ///
    /// 1. format version (refuses naming both versions);
    /// 2. provenance present;
    /// 3. parse (frozen syntax, [`crate::ladle`]);
    /// 4. staged coverage — a grammatically valid but unimplemented form
    ///    refuses BY NAME (a dialect misreads, a coverage gap
    ///    refuses);
    /// 5. the bindings cover every `$param` and every supplied binding is
    ///    referenced — an unbound parameter is a refusal (text
    ///    and values travel separately, so neither may be silently ignored).
    ///
    /// PURE: it reads the envelope and nothing else. No pool is opened, no
    /// row is written, nothing is matched.
    pub fn validate(&self) -> Result<crate::ladle::ast::Statement> {
        self.check_format_version()?;
        self.check_provenance()?;
        let stmt = crate::ladle::parse::parse(&self.statement)?;
        crate::ladle::compile::check_coverage(&stmt)?;
        crate::ladle::compile::check_params(&stmt, &self.bindings)?;
        Ok(stmt)
    }
}

/// Canonical JSON = `serde_json`'s compact form over the types in this module
/// (every map is a `BTreeMap`, so keys are sorted). A value that will not
/// serialize is a HARD failure, never a silently-different hash.
fn canonical_json<T: Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value).map_err(|e| {
        SpineError::hard(format!("envelope element does not serialize canonically: {e}"))
    })
}

/// Commit, or validate-without-landing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Commit,
    Validate,
}

/// The provenance stamp: two axes. Stamped on every write;
/// harness-supplied, never self-reported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    pub initiator: Initiator,
    pub instrument: Instrument,
}

/// WHO initiated the write — a closed class, so an
/// audit selects "everything the machinery wrote" in one predicate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "class", rename_all = "snake_case")]
pub enum Initiator {
    /// An agent through the harness, with attribution detail.
    Agent { thread: String, dispatch: String },
    /// The machinery itself, with the operation's identity.
    CoreMechanical { operation: String },
    /// A ripple step's write: minted by the
    /// soup daemon ONLY from the per-write `ripple {kind, run_id, step_id,
    /// scope}` assertion of a verified supervised-service connection, never
    /// from a statement body. `scope` is the list of flag fields the write is
    /// granted (it lives here, in the initiator block, beside
    /// kind, run and step); the flag rule ([`crate::flags`]) reads it.
    /// ADDITIVE: the three variants above serialize exactly as before, so
    /// every stored row and every hash identity minted under them is
    /// unchanged; this one is `{"class":"ripple","kind":…,"run_id":…,
    /// "step_id":…,"scope":[…]}`.
    Ripple {
        kind: RippleKind,
        run_id: String,
        step_id: String,
        scope: Vec<String>,
    },
    /// Out of scope this build; the enum value exists (and one value's
    /// worth of room remains if another writer class is ever added).
    External,
}

/// Which kind of ripple wrote: a type-one ripple never
/// sets a flag field; a type-two ripple may, inside its scope. A type-three
/// ripple is a macro of type-one and type-two steps:
/// it is the provenance of a tranche's anchor row ONLY, carries no scope and
/// writes nothing itself — its children write as themselves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RippleKind {
    TypeOne,
    TypeTwo,
    TypeThree,
}

impl RippleKind {
    /// The wire name (`type_one` | `type_two` | `type_three`), as serde
    /// writes it.
    pub fn as_str(&self) -> &'static str {
        match self {
            RippleKind::TypeOne => "type_one",
            RippleKind::TypeTwo => "type_two",
            RippleKind::TypeThree => "type_three",
        }
    }
}

/// The versioned machinery the write traveled through — once per transaction
/// row, EXACT not lossy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Instrument {
    pub name: String,
    pub version: String,
}

impl Instrument {
    /// This build's statement door at its version.
    pub fn door() -> Instrument {
        Instrument {
            name: "spine-door".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

/// A transaction's SHA-256 hash identity. Stored and
/// displayed as 64 lowercase hex characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TxHash(pub [u8; 32]);

impl TxHash {
    /// The 64-char lowercase hex form — the row's `hash` column, the ghost
    /// node's identifier.
    pub fn to_hex(&self) -> String {
        let mut s = String::with_capacity(64);
        for b in self.0 {
            use std::fmt::Write;
            let _ = write!(s, "{b:02x}");
        }
        s
    }

    /// Parse the 64-char hex form back. `None` on anything else — a hash
    /// that will not parse is never silently truncated.
    pub fn from_hex(s: &str) -> Option<TxHash> {
        if s.len() != 64 {
            return None;
        }
        let mut out = [0u8; 32];
        for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
            let hi = (chunk[0] as char).to_digit(16)?;
            let lo = (chunk[1] as char).to_digit(16)?;
            out[i] = ((hi << 4) | lo) as u8;
        }
        Some(TxHash(out))
    }
}

impl std::fmt::Display for TxHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_hex())
    }
}
