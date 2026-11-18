//! **The owner's review of a ripple's records**: `cauldron soup approve`,
//! `cauldron ripples approve | reject` and `cauldron ripples review
//! --interactive`, at a terminal, decide records a ripple wrote, named by
//! id, by the dump they cite, or by the run that wrote them, each decision
//! in ONE statement on the cauldron's writer:
//!
//! - approve: `meta.approved: {at, by: "owner"}`; the record reads as
//!   reviewed from then on;
//! - unapprove: `meta.approved: null`; unreviewed again;
//! - reject: `meta.rejected: {at, by: "owner", reason?}`; the record is
//!   kept (its id, its text, its cite to the dump and the decision stay
//!   readable through `get`) and the listing reads leave it out
//!   (`soup_door::reads`);
//! - unreject: `meta.rejected: null`; unreviewed again;
//! - approve and reject at once (one group of the interactive pass): the
//!   records marked approve are approved and those marked reject are
//!   rejected, in the same statement, or none of them is.
//!
//! The two decisions exclude each other: a rejected record is not approved
//! until its rejection is taken back, and an approved one is not rejected
//! until its approval is. Asking again for a state a record is already in
//! changes nothing (it is listed in `skipped`, with why).
//!
//! **Two seam verbs, so that no service misreads a newer command line.**
//!
//! - `DECIDE <key> {cauldron, action, ids | dump | run, reason?,
//!   all_or_nothing?, from?}`, or `{cauldron, action: "approve_and_reject",
//!   approve: [ids], reject: [ids], reason?, all_or_nothing?, from?}`: every
//!   decision this release's command line sends. A key it does not know is
//!   refused, so a field a later command line adds is never silently
//!   ignored. A service older than this verb answers `ERR unknown request
//!   `DECIDE``, writes nothing, and the command line turns that into a
//!   refusal that names the upgrade.
//! - `APPROVE <key> {cauldron, ids, unapprove?}`: the earlier command line's
//!   `cauldron soup approve`, kept so that an older command line still works
//!   against this service. It approves or unapproves by id and nothing else:
//!   a key that body never carried (`action`, `dump`, `run`, `reason`, ...)
//!   is refused, so an approval is never read into a request that meant
//!   something else.
//!
//! **Only the owner's command line can ask.** The key is the approval key
//! this service mints into `approve.secret` at every start
//! ([`crate::secret::APPROVE_FILE`]). The command line reads it only after
//! checking that its input and output are an interactive terminal, as
//! `cauldron mcp grant` does. No MCP tool approves or rejects, so no session
//! can; the `ripples` service holds only the ripples key, which these verbs
//! refuse; and the spine's flag rule refuses `meta.approved` and
//! `meta.rejected` to every ripple anyway. The limit, as for every key here:
//! a process of this user that reads the private file can send the line.
//! Every decision is audited in `calls.jsonl` under the act it is (client
//! `cli`, tool `soup_approve`, `soup_unapprove`, `soup_reject`,
//! `soup_unreject` or `soup_approve_and_reject`, the selector, the counts
//! and the transaction; never text, never the reason).

use std::collections::BTreeMap;

use serde_json::{json, Value};
use shapewire::envelope::Refusal;
use soup_door::reads::DEFAULT_POOL;
use soup_door::review;
use soup_door::Target;
use spine::envelope::{Envelope, Initiator, Instrument, Mode, Provenance, ENVELOPE_FORMAT_VERSION};
use spine::schema_def::PoolName;

use crate::refusal;

fn store(e: impl std::fmt::Display) -> Refusal {
    Refusal::new("store", e.to_string())
}

/// Who a decision is written as.
pub const BY: &str = "owner";

/// The most records one decision writes.
pub const MAX_RECORDS: usize = 2000;

/// The longest reason a rejection keeps.
pub const MAX_REASON: usize = 500;

/// The most ids one request names.
pub const MAX_IDS: usize = 500;

/// The `action` of a decision that approves some records and rejects others.
pub const BOTH: &str = "approve_and_reject";

/// What the owner decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    Approve,
    Unapprove,
    Reject,
    Unreject,
}

impl Act {
    pub fn parse(s: &str) -> Option<Act> {
        Some(match s {
            "approve" => Act::Approve,
            "unapprove" => Act::Unapprove,
            "reject" => Act::Reject,
            "unreject" => Act::Unreject,
            _ => return None,
        })
    }

    pub fn word(self) -> &'static str {
        match self {
            Act::Approve => "approve",
            Act::Unapprove => "unapprove",
            Act::Reject => "reject",
            Act::Unreject => "unreject",
        }
    }

    /// The answer's key for the records it decided, and its outcome.
    pub fn done(self) -> &'static str {
        match self {
            Act::Approve => "approved",
            Act::Unapprove => "unapproved",
            Act::Reject => "rejected",
            Act::Unreject => "unrejected",
        }
    }

    /// The audit's tool name.
    pub fn tool(self) -> String {
        format!("soup_{}", self.word())
    }

    /// The flag it writes.
    fn flag(self) -> &'static str {
        match self {
            Act::Approve | Act::Unapprove => "approved",
            Act::Reject | Act::Unreject => "rejected",
        }
    }

    /// Why `w` is not decided this way, when it is not.
    fn skip(self, w: &review::Written) -> Option<&'static str> {
        match self {
            Act::Approve if w.rejected => Some("rejected: take the rejection back first (`--unreject`, at a terminal)"),
            Act::Approve if w.approved => Some("already approved"),
            Act::Unapprove if !w.approved => Some("not approved: it already reads as unreviewed"),
            Act::Reject if w.rejected => Some("already rejected"),
            Act::Reject if w.approved => Some("approved: take the approval back first (`--unapprove`, at a terminal)"),
            Act::Unreject if !w.rejected => Some("not rejected"),
            _ => None,
        }
    }
}

/// Which records a decision is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pick {
    Ids(Vec<String>),
    Dump(String),
    Run(String),
}

impl Pick {
    fn name(&self) -> &'static str {
        match self {
            Pick::Ids(_) => "ids",
            Pick::Dump(_) => "dump",
            Pick::Run(_) => "run",
        }
    }
}

/// One decision as asked: one act on one selection, or (from the
/// interactive pass) approve some ids and reject others at once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ask {
    One(Act, Pick),
    Both { approve: Vec<String>, reject: Vec<String> },
}

impl Ask {
    /// The audit's tool name: the act, named as it is.
    pub fn tool(&self) -> String {
        match self {
            Ask::One(act, _) => act.tool(),
            Ask::Both { .. } => format!("soup_{BOTH}"),
        }
    }

    fn word(&self) -> &'static str {
        match self {
            Ask::One(act, _) => act.word(),
            Ask::Both { .. } => BOTH,
        }
    }

    fn selector(&self) -> &'static str {
        match self {
            Ask::One(_, p) => p.name(),
            Ask::Both { .. } => "ids",
        }
    }

    fn rejects(&self) -> bool {
        matches!(self, Ask::One(Act::Reject, _) | Ask::Both { .. })
    }
}

/// The command lines a decision may say it came from (the transaction's
/// `dispatch`); anything else is refused.
pub const FROM: &[&str] = &[
    "cauldron soup approve",
    "cauldron ripples approve",
    "cauldron ripples reject",
    "cauldron ripples review --interactive",
];

/// The records `pick` selects, split into those `act` decides and those it
/// skips (with why, appended to `skipped`).
fn split(t: &Target, act: Act, pick: &Pick, skipped: &mut Vec<Value>) -> Result<(usize, Vec<(String, String)>), Refusal> {
    let (ids, dump, run): (&[String], Option<&str>, Option<&str>) = match pick {
        Pick::Ids(v) => (v, None, None),
        Pick::Dump(d) => (&[], Some(d), None),
        Pick::Run(r) => (&[], None, Some(r)),
    };
    let (found, missing) = review::select(t, ids, dump, run).map_err(refusal)?;
    skipped.extend(missing.iter().map(|(id, why)| json!({"id": id, "why": why})));
    let mut take = Vec::new();
    for w in &found {
        match act.skip(w) {
            Some(why) => skipped.push(json!({"id": w.id, "why": why})),
            None => take.push((w.id.clone(), w.node_type.clone())),
        }
    }
    Ok((found.len(), take))
}

/// **Decide what `ask` names** (run on the writer thread), in one statement.
/// With `all_or_nothing`, a record that would be skipped refuses the whole
/// decision, so nothing is written unless every record named is decided as
/// asked.
pub fn decide(c: &spine::Cauldron, t: &Target, ask: &Ask, reason: Option<&str>, all_or_nothing: bool, from: Option<&str>) -> Result<Value, Refusal> {
    let by = BY;
    let pool = PoolName::new(DEFAULT_POOL);
    let mut skipped: Vec<Value> = Vec::new();
    // (act, records it decides)
    let mut parts: Vec<(Act, Vec<(String, String)>)> = Vec::new();
    let mut selected = 0;
    match ask {
        Ask::One(act, pick) => {
            let (n, take) = split(t, *act, pick, &mut skipped)?;
            selected += n;
            parts.push((*act, take));
        }
        Ask::Both { approve, reject } => {
            for (act, ids) in [(Act::Approve, approve), (Act::Reject, reject)] {
                let (n, take) = split(t, act, &Pick::Ids(ids.clone()), &mut skipped)?;
                selected += n;
                parts.push((act, take));
            }
        }
    }
    let word = ask.word();
    let mut doc = json!({"env": t.env.name, "cauldron": t.cauldron.name, "pool": pool.as_str(), "action": word,
                         "selected": selected, "skipped": skipped});
    if let Ask::One(_, p) = ask {
        match p {
            Pick::Ids(_) => {}
            Pick::Dump(d) => doc["dump"] = json!(d),
            Pick::Run(r) => doc["run"] = json!(r),
        }
    }
    for (act, _) in &parts {
        doc[act.done()] = json!([]);
    }
    if all_or_nothing && !skipped.is_empty() {
        return Err(Refusal::with_detail(
            "changed",
            format!(
                "{} of the records named are not as they were listed (each with why in `detail.skipped`), and this decision is all \
                 or nothing, so nothing was written. List the queue again and decide what it shows.",
                skipped.len()
            ),
            doc,
        ));
    }
    let total: usize = parts.iter().map(|(_, v)| v.len()).sum();
    if total == 0 {
        doc["outcome"] = json!(format!("nothing-to-{}", if matches!(ask, Ask::Both { .. }) { "decide" } else { word }));
        if let Ask::One(_, p) = ask {
            if selected == 0 && !matches!(p, Pick::Ids(_)) {
                doc["note"] = json!(format!("no record a ripple wrote is in this {}", p.name()));
            }
        }
        return Ok(doc);
    }
    if total > MAX_RECORDS {
        return Err(Refusal::with_detail(
            "too_large",
            format!("{total} records would be decided at once, more than {MAX_RECORDS}; name them by id in parts. Nothing was written."),
            doc,
        ));
    }
    let at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mut lines = Vec::new();
    let mut expects = Vec::new();
    let mut sets = Vec::new();
    let mut params: BTreeMap<String, Value> = BTreeMap::new();
    let mut i = 0;
    for (act, take) in parts.iter().filter(|(_, take)| !take.is_empty()) {
        let value = match act {
            Act::Unapprove | Act::Unreject => Value::Null,
            Act::Approve => json!({"at": at, "by": by}),
            Act::Reject => {
                let mut v = json!({"at": at, "by": by});
                if let Some(r) = reason {
                    v["reason"] = json!(r);
                }
                v
            }
        };
        let v = format!("v_{}", act.word());
        params.insert(v.clone(), value);
        for (id, ty) in take {
            lines.push(format!("MATCH (n{i}: {DEFAULT_POOL}/{ty} {{ id: $id_{i} }})"));
            expects.push(format!("EXPECT COUNT(n{i}) = 1"));
            sets.push(format!("SET n{i}.meta.{} = ${v}", act.flag()));
            params.insert(format!("id_{i}"), json!(id));
            i += 1;
        }
    }
    let dispatch = match from {
        Some(f) => f.to_string(),
        None => match ask {
            Ask::One(Act::Approve | Act::Unapprove, _) => "cauldron ripples approve".into(),
            Ask::One(Act::Reject | Act::Unreject, _) => "cauldron ripples reject".into(),
            Ask::Both { .. } => "cauldron ripples review --interactive".into(),
        },
    };
    let env = Envelope {
        statement: lines.into_iter().chain(expects).chain(sets).collect::<Vec<_>>().join("\n"),
        bindings: params,
        format_version: ENVELOPE_FORMAT_VERSION,
        provenance: Provenance {
            initiator: Initiator::Agent { thread: by.to_string(), dispatch },
            instrument: Instrument { name: "cauldron-mcp".into(), version: env!("CARGO_PKG_VERSION").into() },
        },
        mode: Mode::Commit,
        idempotency_nonce: Some(format!(
            "owner-{word}:{}:{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
        )),
    };
    match c.submit(env) {
        Ok(spine::Outcome::Committed { tx, seq, deduplicated, .. }) => {
            for (act, take) in &parts {
                doc[act.done()] = json!(take.iter().map(|(id, _)| id).collect::<Vec<_>>());
            }
            doc["outcome"] = json!(match ask {
                Ask::One(act, _) => act.done(),
                Ask::Both { .. } => "decided",
            });
            doc["landed"] = json!({"tx": tx.to_hex(), "seq": seq, "deduplicated": deduplicated});
            doc["cursor"] = crate::writer::cursor_after(c, t);
            Ok(doc)
        }
        Ok(spine::Outcome::Validated { .. }) => Err(store(format!("the {word} was validated and not committed"))),
        Err(spine::error::SpineError::Refused { witnesses }) => {
            doc["witnesses"] = json!(witnesses);
            Err(Refusal::with_detail("records_refused", format!("the soup refused the {word}; nothing was written"), doc))
        }
        Err(e) => Err(store(format!("writing the {word}: {e}"))),
    }
}

/// Approve `ids`, or with `unapprove` take the approval back (run on the
/// writer thread): [`decide`] by id.
pub fn approve(c: &spine::Cauldron, t: &Target, ids: &[String], unapprove: bool) -> Result<Value, Refusal> {
    let act = if unapprove { Act::Unapprove } else { Act::Approve };
    decide(c, t, &Ask::One(act, Pick::Ids(ids.to_vec())), None, false, Some("cauldron soup approve"))
}

/// A rejection's reason, checked: one line (control characters become
/// spaces), at most [`MAX_REASON`] characters, not empty.
pub fn checked_reason(r: &str) -> Result<String, Refusal> {
    let one = soup_door::review::one_line(r);
    if one.is_empty() {
        return Err(Refusal::new("usage", "`reason` is empty. Nothing was written."));
    }
    if one.chars().count() > MAX_REASON {
        return Err(Refusal::new("usage", format!("`reason` is longer than {MAX_REASON} characters. Nothing was written.")));
    }
    Ok(one)
}

/// A list of 1 to [`MAX_IDS`] record ids, or a refusal naming `key`.
fn id_list(v: &Value, key: &str, may_be_empty: bool) -> Result<Vec<String>, Refusal> {
    match v {
        Value::Array(a)
            if (may_be_empty || !a.is_empty()) && a.len() <= MAX_IDS && a.iter().all(|x| x.as_str().is_some_and(|s| !s.trim().is_empty())) =>
        {
            Ok(a.iter().filter_map(Value::as_str).map(str::to_string).collect())
        }
        _ => Err(Refusal::new("usage", format!("`{key}` must be a list of 1 to {MAX_IDS} record ids. Nothing was written."))),
    }
}

/// Refuse any key of `args` not in `known`: a field this service does not
/// read is never silently ignored.
fn only(args: &serde_json::Map<String, Value>, verb: &str, known: &[&str]) -> Result<(), Refusal> {
    if let Some(k) = args.keys().find(|k| !known.contains(&k.as_str())) {
        return Err(Refusal::new(
            "usage",
            format!("{verb} does not take `{k}` (it takes {}). Nothing was written.", known.iter().map(|k| format!("`{k}`")).collect::<Vec<_>>().join(", ")),
        ));
    }
    Ok(())
}

/// What a request asked, read and checked.
struct Asked {
    ask: Ask,
    reason: Option<String>,
    all_or_nothing: bool,
    from: Option<String>,
}

/// Read a `DECIDE` body.
fn read_decide(args: &serde_json::Map<String, Value>) -> Result<Asked, Refusal> {
    let action = args.get("action").and_then(Value::as_str).unwrap_or("");
    let ask = if action == BOTH {
        only(args, "DECIDE", &["cauldron", "action", "approve", "reject", "reason", "all_or_nothing", "from"])?;
        let approve = id_list(args.get("approve").unwrap_or(&json!([])), "approve", true)?;
        let reject = id_list(args.get("reject").unwrap_or(&json!([])), "reject", true)?;
        if approve.is_empty() || reject.is_empty() {
            return Err(Refusal::new("usage", format!("`{BOTH}` names records in both `approve` and `reject`. Nothing was written.")));
        }
        if approve.len() + reject.len() > MAX_IDS {
            return Err(Refusal::new("usage", format!("one decision names at most {MAX_IDS} ids. Nothing was written.")));
        }
        if let Some(id) = approve.iter().find(|id| reject.contains(id)) {
            return Err(Refusal::new("usage", format!("`{id}` is named to be approved and rejected. Nothing was written.")));
        }
        Ask::Both { approve, reject }
    } else {
        let Some(act) = Act::parse(action) else {
            return Err(Refusal::new("usage", format!("`action` is approve, unapprove, reject, unreject or {BOTH}. Nothing was written.")));
        };
        only(args, "DECIDE", &["cauldron", "action", "ids", "dump", "run", "reason", "all_or_nothing", "from"])?;
        let given = ["ids", "dump", "run"].iter().filter(|k| args.get(**k).is_some_and(|v| !v.is_null())).count();
        if given != 1 {
            return Err(Refusal::new("usage", "name the records one way: `ids`, `dump` or `run`. Nothing was written."));
        }
        let pick = if let Some(v) = args.get("ids").filter(|v| !v.is_null()) {
            Pick::Ids(id_list(v, "ids", false)?)
        } else if let Some(d) = args.get("dump").filter(|v| !v.is_null()) {
            Pick::Dump(soup_door::review::checked_dump(d.as_str().unwrap_or("")).map_err(refusal)?)
        } else {
            let r = args.get("run").and_then(Value::as_str).unwrap_or("");
            Pick::Run(soup_door::review::checked_run(r).map_err(refusal)?)
        };
        Ask::One(act, pick)
    };
    let reason = match args.get("reason").filter(|v| !v.is_null()) {
        None => None,
        Some(_) if !ask.rejects() => return Err(Refusal::new("usage", "`reason` goes with a rejection only. Nothing was written.")),
        Some(Value::String(r)) => Some(checked_reason(r)?),
        Some(_) => return Err(Refusal::new("usage", "`reason` is a string. Nothing was written.")),
    };
    let all_or_nothing = match args.get("all_or_nothing") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err(Refusal::new("usage", "`all_or_nothing` is true or false. Nothing was written.")),
    };
    let from = match args.get("from").filter(|v| !v.is_null()) {
        None => None,
        Some(Value::String(f)) if FROM.contains(&f.as_str()) => Some(f.clone()),
        Some(_) => return Err(Refusal::new("usage", format!("`from` is one of {}. Nothing was written.", FROM.join(", ")))),
    };
    Ok(Asked { ask, reason, all_or_nothing, from })
}

/// Read an `APPROVE` body: the earlier command line's, `{cauldron, ids,
/// unapprove?}`, and nothing else.
fn read_approve(args: &serde_json::Map<String, Value>) -> Result<Asked, Refusal> {
    if let Some(k) = args.keys().find(|k| !["cauldron", "ids", "unapprove"].contains(&k.as_str())) {
        return Err(Refusal::new(
            "usage",
            format!(
                "APPROVE takes only `cauldron`, `ids` and `unapprove` (the body of an earlier `cauldron soup approve`), not `{k}`: \
                 every other decision goes on DECIDE. Nothing was written."
            ),
        ));
    }
    let unapprove = match args.get("unapprove") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err(Refusal::new("usage", "`unapprove` is true or false. Nothing was written.")),
    };
    let ids = id_list(args.get("ids").unwrap_or(&Value::Null), "ids", false)?;
    let act = if unapprove { Act::Unapprove } else { Act::Approve };
    Ok(Asked { ask: Ask::One(act, Pick::Ids(ids)), reason: None, all_or_nothing: false, from: Some("cauldron soup approve".into()) })
}

/// **Answer one `DECIDE` line**: `rest` is `<key> <json>`. Always one JSON
/// line, never an error; every call after the key is audited.
pub fn answer_decide(door: &crate::door::Door, key: &crate::secret::Secret, rest: &str) -> String {
    answer(door, key, "DECIDE", rest, read_decide)
}

/// **Answer one `APPROVE` line** (the earlier command line's body only).
pub fn answer_approve(door: &crate::door::Door, key: &crate::secret::Secret, rest: &str) -> String {
    answer(door, key, "APPROVE", rest, read_approve)
}

fn answer(
    door: &crate::door::Door,
    key: &crate::secret::Secret,
    verb: &str,
    rest: &str,
    read: fn(&serde_json::Map<String, Value>) -> Result<Asked, Refusal>,
) -> String {
    let fail = |r: &Refusal| {
        let mut v = json!({"ok": false, "code": r.code, "message": r.message});
        if let Some(d) = &r.detail {
            v["detail"] = d.clone();
        }
        v.to_string()
    };
    let (given, body) = rest.split_once(' ').unwrap_or((rest, ""));
    if given.is_empty() || !key.is(given) {
        tracing::warn!(verb, "a decision line without the approval key was refused");
        return fail(&Refusal::new(
            "unauthorized",
            "approving and rejecting are the owner's acts: this verb answers only `cauldron soup approve`, `cauldron \
             ripples approve | reject` and `cauldron ripples review --interactive`, run at a terminal, which carry the key \
             this service minted into its directory (approve.secret) at its last start. No MCP session and no ripple can \
             approve or reject. Nothing was written.",
        ));
    }
    let args: serde_json::Map<String, Value> = match serde_json::from_str::<Value>(body.trim()) {
        Ok(Value::Object(m)) => m,
        _ => return fail(&Refusal::new("usage", format!("{verb} takes one JSON object after the key. Nothing was written."))),
    };
    let cauldron = args.get("cauldron").and_then(Value::as_str).unwrap_or("").to_string();
    let asked = read(&args);
    // The audit names the act as it was read; a body whose act could not be
    // read is audited as a decision, never as an approval.
    let tool = asked.as_ref().map_or_else(|_| "soup_decide".to_string(), |a| a.ask.tool());
    let selector = asked.as_ref().map(|a| a.ask.selector()).unwrap_or("");
    let run = match &asked {
        Ok(Asked { ask: Ask::One(_, Pick::Run(r)), .. }) => Some(r.clone()),
        _ => None,
    };
    let out = (|| -> Result<Value, Refusal> {
        let asked = asked?;
        if cauldron.trim().is_empty() {
            return Err(Refusal::new("usage", "`cauldron` must be a non-empty string. Nothing was written."));
        }
        let t = door.owner_target(&cauldron)?;
        let who = crate::writer::Who { client: "cli".into(), thread: BY.into(), session: None };
        door.writers.with(
            &t,
            who,
            Box::new(move |c, t| decide(c, t, &asked.ask, asked.reason.as_deref(), asked.all_or_nothing, asked.from.as_deref())),
        )
    })();
    let mut line = crate::audit::Line {
        client: Some("cli".into()),
        cauldron: (!cauldron.is_empty()).then_some(cauldron),
        tool,
        ..crate::audit::Line::default()
    };
    match &out {
        Ok(v) => {
            line.outcome = v.get("outcome").and_then(Value::as_str).unwrap_or("ok").to_string();
            line.tx = v["landed"].get("tx").and_then(Value::as_str).map(str::to_string);
            line.dump = v.get("dump").and_then(Value::as_str).map(str::to_string);
            let n = |k: &str| v.get(k).and_then(Value::as_array).map_or(0, Vec::len);
            let mut counts = json!({"selector": selector, "selected": v["selected"], "skipped": n("skipped")});
            for k in ["approved", "unapproved", "rejected", "unrejected"] {
                counts[k] = json!(n(k));
            }
            if let Some(r) = &run {
                counts["run"] = json!(r);
            }
            line.counts = counts;
        }
        Err(r) => line.outcome = r.code.to_string(),
    }
    door.audit(line);
    match out {
        Ok(mut v) => {
            v["ok"] = json!(true);
            v.to_string()
        }
        Err(r) => fail(&r),
    }
}
