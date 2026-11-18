//! The ripple initiator and the flag rule
//! (`Initiator::Ripple {kind, run_id, step_id, scope}`).
//!
//! - existing initiators store, read and hash EXACTLY as before (the stored
//!   text and the hash identity are pinned to literals computed OUTSIDE this
//!   crate, by the frozen algorithm, in python — never by the code under test);
//! - a ripple row persists and reads back all four fields;
//! - a blank run/step, a scoped type-one and a blank scope entry are `Usage`;
//! - the flag rule: a ripple writing a flag its kind and scope do not grant is
//!   refused `flag_not_writable`, whole, landing nothing, on both callers;
//!   the same write inside scope lands; non-flag writes and non-ripple
//!   initiators are untouched.

use spine::config::SpineConfig;
use spine::envelope::{
    Bindings, Envelope, Initiator, Instrument, Mode, Provenance, RippleKind,
};
use spine::error::SpineError;
use spine::schema_def::{FieldDef, NodeTypeDef, PoolGrammar, PoolKind, PoolName};
use spine::spine_db::SPINE_FILE;
use spine::witness::Witness;
use spine::{Cauldron, Outcome, ReadRequest};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

test_scratch::install!();

const POOL: &str = "board";

fn ty(fields: &[(&str, bool)]) -> NodeTypeDef {
    NodeTypeDef {
        fields: fields
            .iter()
            .map(|(n, req)| {
                (
                    n.to_string(),
                    if *req { FieldDef::required() } else { FieldDef::optional() },
                )
            })
            .collect(),
        ..Default::default()
    }
}

/// A pool carrying the flag fields on the types the table names, plus a
/// `note` type whose `meta` is NOT a flag carrier (type qualification).
fn board() -> PoolGrammar {
    let mut node_types = BTreeMap::new();
    node_types.insert("question".into(), ty(&[("title", true), ("meta", false)]));
    node_types.insert("directive".into(), ty(&[("title", true), ("meta", false)]));
    node_types.insert("region".into(), ty(&[("title", true), ("retired", false)]));
    node_types.insert("note".into(), ty(&[("title", true), ("meta", false)]));
    PoolGrammar {
        pool: PoolName::new(POOL),
        kind: PoolKind::new("board"),
        version: 1,
        node_types,
        edge_kinds: BTreeMap::new(),
    }
}

/// A scratch directory removed on drop (this crate has no `tempfile`).
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn cauldron() -> (Scratch, Cauldron) {
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    let dir = Scratch(test_scratch::tmp_path(format!(
        "spine-r4s16-{}-{}",
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::SeqCst)
    )));
    let _ = std::fs::remove_dir_all(dir.path());
    std::fs::create_dir_all(dir.path()).unwrap();
    let root = dir.path().join("c");
    let c = Cauldron::create(&root, &[board()], SpineConfig::default()).unwrap();
    (dir, c)
}

static NONCE: AtomicU64 = AtomicU64::new(1);

fn ripple(kind: RippleKind, step: &str, scope: &[&str]) -> Initiator {
    Initiator::Ripple {
        kind,
        run_id: format!("run-of-{step}"),
        step_id: step.to_string(),
        scope: scope.iter().map(|s| s.to_string()).collect(),
    }
}

fn mech() -> Initiator {
    Initiator::CoreMechanical {
        operation: "test-setup".into(),
    }
}

fn env(init: Initiator, statement: &str, params: &[(&str, Value)], mode: Mode) -> Envelope {
    let nonce = match &init {
        Initiator::Ripple { step_id, .. } => step_id.clone(),
        _ => format!("n{}", NONCE.fetch_add(1, Ordering::Relaxed)),
    };
    Envelope {
        statement: statement.to_string(),
        bindings: params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(),
        format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
        provenance: Provenance {
            initiator: init,
            instrument: Instrument::door(),
        },
        mode,
        idempotency_nonce: Some(nonce),
    }
}

fn submit(c: &Cauldron, init: Initiator, statement: &str, params: &[(&str, Value)]) -> spine::error::Result<Outcome> {
    c.submit(env(init, statement, params, Mode::Commit))
}

fn committed(o: spine::error::Result<Outcome>) -> (spine::spine_db::Seq, BTreeMap<String, String>) {
    match o {
        Ok(Outcome::Committed { seq, deduplicated: false, .. }) => (seq, BTreeMap::new()),
        other => panic!("expected a fresh commit, got {other:?}"),
    }
}

fn witnesses(o: spine::error::Result<Outcome>) -> Vec<Witness> {
    match o {
        Err(SpineError::Refused { witnesses }) => witnesses,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

fn read(c: &Cauldron, statement: &str) -> Vec<BTreeMap<String, Value>> {
    let rows = c
        .read(&ReadRequest {
            statement: statement.to_string(),
            bindings: Bindings::new(),
            format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
        })
        .expect("read")
        .rows
        .0;
    rows.into_iter()
        .map(|r| serde_json::from_value(serde_json::to_value(r).unwrap()).unwrap())
        .collect()
}

fn create_question(c: &Cauldron, init: Initiator, title: &str, meta: Value) -> spine::error::Result<Outcome> {
    submit(
        c,
        init,
        "CREATE (q: board/question { title: $t, meta: $m })",
        &[("t", json!(title)), ("m", meta)],
    )
}

fn question_id(c: &Cauldron, title: &str) -> String {
    let rows = read(c, "MATCH (q: board/question) RETURN q.id, q.title");
    let r = rows
        .iter()
        .find(|r| r["q.title"] == json!(title))
        .unwrap_or_else(|| panic!("no question {title}: {rows:?}"));
    r["q.id"].as_str().unwrap().to_string()
}

fn meta_of(c: &Cauldron, title: &str) -> Value {
    let rows = read(c, "MATCH (q: board/question) RETURN q.title, q.meta");
    rows.iter()
        .find(|r| r["q.title"] == json!(title))
        .map(|r| r["q.meta"].clone())
        .unwrap_or(Value::Null)
}

fn raw_initiator(c_root: &std::path::Path, seq: i64) -> String {
    let db = rusqlite::Connection::open(c_root.join(SPINE_FILE)).unwrap();
    db.query_row(
        "SELECT initiator FROM transactions WHERE seq = ?1",
        [seq],
        |r| r.get::<_, String>(0),
    )
    .unwrap()
}

/// **GUARD 5 — THE EXISTING INITIATORS ARE UNCHANGED.** Their stored text is
/// the literal a build before the ripple initiator wrote, it parses back to the same value, and
/// the hash identity of a fixed envelope under each equals a literal computed
/// by the frozen algorithm outside this crate (python `hashlib`, 8-byte
/// big-endian length prefixes). The Ripple hash is pinned the same way, so
/// the comparison is between DIFFERENT values: a constant serializer or a
/// constant hash fails at least one row.
#[test]
fn existing_initiators_store_read_and_hash_exactly_as_before() {
    let s = r#"CREATE (n: board/note { title: "x" })"#;
    let ins = Instrument {
        name: "pen".into(),
        version: "b1".into(),
    };
    let cases = [
        (
            Initiator::Agent {
                thread: "thread-1".into(),
                dispatch: "boot/1".into(),
            },
            r#"{"class":"agent","thread":"thread-1","dispatch":"boot/1"}"#,
            "nonce-1",
            "531234cb867d1528bdc22aa5d31b28db28974f217a997aaf723dc04c048fb6fd",
        ),
        (
            Initiator::CoreMechanical {
                operation: "checkpoint".into(),
            },
            r#"{"class":"core_mechanical","operation":"checkpoint"}"#,
            "nonce-1",
            "56aa2a02e09fe3658b6f04bc7c17303d5f5afcd71e367b2183e5989589ab3e1d",
        ),
        (
            ripple(RippleKind::TypeTwo, "step-1", &["meta.blocking"]),
            r#"{"class":"ripple","kind":"type_two","run_id":"run-of-step-1","step_id":"step-1","scope":["meta.blocking"]}"#,
            "step-1",
            "0868a67988b1b80118607e48b5939813993f823085a1a83df1d95e389204fe89",
        ),
    ];
    let mut hashes = std::collections::BTreeSet::new();
    for (init, text, nonce, want_hash) in &cases {
        assert_eq!(&serde_json::to_string(init).unwrap(), text, "stored text");
        let back: Initiator = serde_json::from_str(text).unwrap();
        assert_eq!(&back, init, "an old row reads back identically");
        let e = Envelope {
            statement: s.into(),
            bindings: Bindings::new(),
            format_version: 1,
            provenance: Provenance {
                initiator: init.clone(),
                instrument: ins.clone(),
            },
            mode: Mode::Commit,
            idempotency_nonce: Some(nonce.to_string()),
        };
        let h = e.hash_identity().unwrap().to_hex();
        assert_eq!(&h, want_hash, "hash identity of {text}");
        hashes.insert(h);
    }
    // Sentinel: the three pinned identities are three different values.
    assert_eq!(hashes.len(), 3);

    // And through the door: an agent row and a mechanical row land and are
    // stored as exactly that text, and read back as the value.
    let (dir, c) = cauldron();
    for (init, text) in [
        (
            Initiator::Agent {
                thread: "thread-1".into(),
                dispatch: "boot/1".into(),
            },
            r#"{"class":"agent","thread":"thread-1","dispatch":"boot/1"}"#,
        ),
        (
            mech(),
            r#"{"class":"core_mechanical","operation":"test-setup"}"#,
        ),
    ] {
        let (seq, _) = committed(submit(&c, init.clone(), s, &[]));
        assert_eq!(raw_initiator(&dir.path().join("c"), seq.0 as i64), text);
        assert_eq!(c.transaction(seq).unwrap().initiator, init);
    }
}

/// **GUARD 1 (spine half) — A RIPPLE ROW PERSISTS ALL FOUR FIELDS.** Two
/// different ripple writes land and read back each its own kind, run, step
/// and scope, from the row AND from the stored text (opposed: a constant, or
/// a persistence that drops `scope`, fails the pair).
#[test]
fn a_ripple_initiator_persists_and_reads_back_all_four_fields() {
    let (dir, c) = cauldron();
    let a = ripple(RippleKind::TypeTwo, "step-A", &["meta.blocking", "retired"]);
    let b = ripple(RippleKind::TypeOne, "step-B", &[]);
    let (sa, _) = committed(create_question(&c, a.clone(), "qa", json!({"blocking": true})));
    let (sb, _) = committed(submit(&c, b.clone(), r#"CREATE (n: board/note { title: "nb" })"#, &[]));
    assert_eq!(c.transaction(sa).unwrap().initiator, a);
    assert_eq!(c.transaction(sb).unwrap().initiator, b);
    let raw: Value = serde_json::from_str(&raw_initiator(&dir.path().join("c"), sa.0 as i64)).unwrap();
    assert_eq!(
        raw,
        json!({"class":"ripple","kind":"type_two","run_id":"run-of-step-A","step_id":"step-A",
               "scope":["meta.blocking","retired"]})
    );
    assert_eq!(meta_of(&c, "qa"), json!({"blocking": true}));
}

/// A ripple stamp that is incomplete or self-contradictory is the harness's
/// misuse — `Usage`, like a blank thread — and nothing lands. The same write
/// with a complete stamp lands (the positive half).
#[test]
fn a_blank_ripple_stamp_or_a_scoped_type_one_is_usage() {
    let (_dir, c) = cauldron();
    let s = r#"CREATE (n: board/note { title: "u" })"#;
    for (why, init) in [
        ("blank run", Initiator::Ripple { kind: RippleKind::TypeTwo, run_id: " ".into(), step_id: "s1".into(), scope: vec![] }),
        ("blank step", Initiator::Ripple { kind: RippleKind::TypeTwo, run_id: "r".into(), step_id: "".into(), scope: vec![] }),
        ("type one with a scope", Initiator::Ripple { kind: RippleKind::TypeOne, run_id: "r".into(), step_id: "s2".into(), scope: vec!["meta.blocking".into()] }),
        ("blank scope entry", Initiator::Ripple { kind: RippleKind::TypeTwo, run_id: "r".into(), step_id: "s3".into(), scope: vec![" ".into()] }),
    ] {
        match submit(&c, init, s, &[]) {
            Err(SpineError::Usage { .. }) => {}
            other => panic!("{why}: expected Usage, got {other:?}"),
        }
    }
    assert!(read(&c, "MATCH (n: board/note) RETURN n.id").is_empty());
    committed(submit(&c, ripple(RippleKind::TypeTwo, "s4", &["retired"]), s, &[]));
    assert_eq!(read(&c, "MATCH (n: board/note) RETURN n.id").len(), 1);
}

fn only_flag(w: &[Witness]) -> (&str, &str, &str, Option<&Vec<String>>) {
    assert_eq!(w.len(), 1, "one offending delta, one witness: {w:?}");
    match &w[0] {
        Witness::FlagNotWritable { pool, field, initiator_kind, scope, node, .. } => {
            assert_eq!(pool.as_str(), POOL);
            assert!(!node.is_empty());
            (field.as_str(), initiator_kind.as_str(), node.as_str(), scope.as_ref())
        }
        other => panic!("expected flag_not_writable, got {other:?}"),
    }
}

/// **GUARD 2 (spine half) — THE FLAG RULE.** Every way a write reaches a flag
/// — a CREATE carrying it, a dotted SET, a whole-object SET above it, a
/// region flag, a SEAL — is refused `flag_not_writable` for a type-one ripple
/// and for a type-two whose scope does not list it, and NOTHING lands (the
/// question's `meta` is read back unchanged after every refusal). The same
/// writes inside scope land (the positive sentinel, in this test), a ripple's
/// NON-flag write lands, `meta.stop` on a question is not a flag (type
/// qualification), and the mechanical initiator writes flags freely.
#[test]
fn the_flag_rule_refuses_a_ripple_outside_its_scope_and_admits_one_inside() {
    let (_dir, c) = cauldron();
    // Positive sentinel first: in scope, a type-two CREATE carrying the flag lands.
    committed(create_question(&c, ripple(RippleKind::TypeTwo, "in-1", &["meta.blocking"]), "q", json!({"blocking": true})));
    assert_eq!(meta_of(&c, "q"), json!({"blocking": true}));
    let qid = question_id(&c, "q");
    let set = |path: &str| format!("MATCH (q: board/question {{ id: $id }}) SET q.{path} = $v");

    // CREATE carrying the flag, outside scope (type two, another field listed).
    let w = witnesses(create_question(&c, ripple(RippleKind::TypeTwo, "out-1", &["retired"]), "q2", json!({"blocking": true})));
    let (f, k, _, sc) = only_flag(&w);
    assert_eq!((f, k), ("meta.blocking", "type_two"));
    assert_eq!(sc, Some(&vec!["retired".to_string()]));
    // Type one: always refused, even a dotted SET.
    let w = witnesses(submit(&c, ripple(RippleKind::TypeOne, "out-2", &[]), &set("meta.blocking"), &[("id", json!(qid)), ("v", json!(false))]));
    let (f, k, node, _) = only_flag(&w);
    assert_eq!((f, k, node), ("meta.blocking", "type_one", qid.as_str()));
    // A whole-object SET above the flag writes it too.
    // It writes every flag under `meta`: the question's `meta.blocking` and
    // its `meta.approved` and `meta.rejected` (the review flags, on every
    // content type).
    let w = witnesses(submit(&c, ripple(RippleKind::TypeTwo, "out-3", &[]), &set("meta"), &[("id", json!(qid)), ("v", json!({"note": 1}))]));
    let mut fields: Vec<&str> = w
        .iter()
        .map(|x| match x {
            Witness::FlagNotWritable { field, .. } => field.as_str(),
            other => panic!("expected flag_not_writable, got {other:?}"),
        })
        .collect();
    fields.sort();
    assert_eq!(fields, ["meta.approved", "meta.blocking", "meta.rejected"]);
    // SEAL is the `sealed` flag.
    let w = witnesses(submit(&c, ripple(RippleKind::TypeTwo, "out-4", &["meta.blocking"]), "MATCH (q: board/question { id: $id }) SEAL (q)", &[("id", json!(qid))]));
    assert_eq!(only_flag(&w).0, "sealed");
    // Validate mode is the same validator: refused the same way.
    let e = env(ripple(RippleKind::TypeOne, "out-5", &[]), &set("meta.blocking"), &[("id", json!(qid)), ("v", json!(false))], Mode::Validate);
    assert_eq!(only_flag(&witnesses(c.submit(e))).0, "meta.blocking");
    // A region flag.
    let w = witnesses(submit(&c, ripple(RippleKind::TypeTwo, "out-6", &["meta.blocking"]), r#"CREATE (r: board/region { title: "r", retired: true })"#, &[]));
    assert_eq!(only_flag(&w).0, "retired");

    // NOTHING landed: one question, its meta as the sentinel left it, no region.
    assert_eq!(read(&c, "MATCH (q: board/question) RETURN q.id").len(), 1);
    assert_eq!(meta_of(&c, "q"), json!({"blocking": true}));
    assert!(read(&c, "MATCH (r: board/region) RETURN r.id").is_empty());

    // Inside scope, the same dotted SET lands.
    committed(submit(&c, ripple(RippleKind::TypeTwo, "in-2", &["meta.blocking"]), &set("meta.blocking"), &[("id", json!(qid)), ("v", json!(false))]));
    assert_eq!(meta_of(&c, "q"), json!({"blocking": false}));
    // A non-flag key under the same object is no flag: a type one writes it.
    committed(submit(&c, ripple(RippleKind::TypeOne, "in-3", &[]), &set("meta.colour"), &[("id", json!(qid)), ("v", json!("red"))]));
    // `meta.stop` is a flag on a directive, not on a question.
    committed(submit(&c, ripple(RippleKind::TypeOne, "in-4", &[]), &set("meta.stop"), &[("id", json!(qid)), ("v", json!(true))]));
    let w = witnesses(submit(&c, ripple(RippleKind::TypeOne, "out-7", &[]), "CREATE (d: board/directive { title: $t, meta: $m })", &[("t", json!("d")), ("m", json!({"stop": true}))]));
    assert_eq!(only_flag(&w).0, "meta.stop");
    // A type-two region write in scope lands.
    committed(submit(&c, ripple(RippleKind::TypeTwo, "in-5", &["retired"]), r#"CREATE (r: board/region { title: "r", retired: true })"#, &[]));
    // An agent's flag write is unchanged by this step (gen16b.open 4 is
    // undecided; see the module doc of `spine::flags`).
    committed(submit(&c, Initiator::Agent { thread: "t".into(), dispatch: "d".into() }, &set("meta.blocking"), &[("id", json!(qid)), ("v", json!(false))]));
    // The soup's own machinery writes flags freely, SEAL included.
    committed(submit(&c, mech(), &set("meta.blocking"), &[("id", json!(qid)), ("v", json!(true))]));
    committed(submit(&c, mech(), "MATCH (q: board/question { id: $id }) SEAL (q)", &[("id", json!(qid))]));
    assert_eq!(
        meta_of(&c, "q"),
        json!({"blocking": true, "colour": "red", "stop": true})
    );
}

fn tx_rows(c_root: &std::path::Path) -> i64 {
    let db = rusqlite::Connection::open(c_root.join(SPINE_FILE)).unwrap();
    db.query_row("SELECT count(*) FROM transactions", [], |r| r.get(0)).unwrap()
}

/// **ONE CHECK, BOTH PATHS.** The design homes the
/// flag rule in `check_settable` / `Pool::apply_part` (door AND landing); this
/// build has ONE call, in `exec`'s compile phase, inside the restart loop, and
/// before the spine row is appended. This test shows that one call covers the
/// landing too: every landing — the live door's `apply_part` and recovery's
/// roll-forward — lands only a payload a spine ROW records, and a refused
/// ripple write leaves NO row (the `transactions` count is unchanged on BOTH
/// callers, Validate and Commit), so neither landing has anything to land; a
/// `recover()` and a full reopen after the refusals land nothing either. The
/// positive sentinel: the same write inside scope adds exactly one row and
/// lands, so the row count is an instrument that can move.
#[test]
fn a_refused_flag_write_leaves_no_row_for_any_landing_to_land() {
    let (dir, c) = cauldron();
    let root = dir.path().join("c");
    committed(create_question(&c, mech(), "q", json!({"blocking": false})));
    let qid = question_id(&c, "q");
    let set = "MATCH (q: board/question { id: $id }) SET q.meta.blocking = $v";
    let params = [("id", json!(qid)), ("v", json!(true))];
    let before = tx_rows(&root);

    for (step, mode) in [("o-v", Mode::Validate), ("o-c", Mode::Commit)] {
        let e = env(ripple(RippleKind::TypeTwo, step, &["retired"]), set, &params, mode);
        assert_eq!(only_flag(&witnesses(c.submit(e))).0, "meta.blocking", "{mode:?}");
        assert_eq!(tx_rows(&root), before, "{mode:?}: a refused write appends no row");
    }
    c.recover().unwrap();
    assert_eq!(meta_of(&c, "q"), json!({"blocking": false}), "recovery landed nothing");
    drop(c);
    let c = Cauldron::open(&root, SpineConfig::default()).unwrap();
    assert_eq!(tx_rows(&root), before);
    assert_eq!(meta_of(&c, "q"), json!({"blocking": false}), "reopen landed nothing");

    // Sentinel: in scope, the same write appends one row and lands.
    committed(submit(&c, ripple(RippleKind::TypeTwo, "i-c", &["meta.blocking"]), set, &params));
    assert_eq!(tx_rows(&root), before + 1);
    assert_eq!(meta_of(&c, "q"), json!({"blocking": true}));
}

/// **THE REVIEW FLAG.** `meta.approved` is a flag on every content type: a
/// ripple (type one, or type two with the empty scope the ripples service
/// always writes with) is refused it whether it creates a record already
/// approved or sets it on one, dotted or by a whole `meta`; and nothing of
/// those writes lands. An agent (the owner's tools) sets it and it lands.
#[test]
fn a_ripple_never_approves_and_an_agent_does() {
    let (_dir, c) = cauldron();
    // A ripple's own record, unapproved: lands.
    committed(create_question(&c, ripple(RippleKind::TypeTwo, "rv-1", &[]), "r", json!({"cites": []})));
    let qid = question_id(&c, "r");
    let set = |path: &str| format!("MATCH (q: board/question {{ id: $id }}) SET q.{path} = $v");
    // Minted approved: refused.
    let w = witnesses(create_question(&c, ripple(RippleKind::TypeTwo, "rv-2", &[]), "r2", json!({"approved": {"by": "ripple"}})));
    assert_eq!(only_flag(&w).0, "meta.approved");
    // A dotted SET, by type two and by type one: refused.
    for (kind, step) in [(RippleKind::TypeTwo, "rv-3"), (RippleKind::TypeOne, "rv-4")] {
        let w = witnesses(submit(&c, ripple(kind, step, &[]), &set("meta.approved"), &[("id", json!(qid)), ("v", json!(true))]));
        assert_eq!(only_flag(&w).0, "meta.approved");
    }
    // A whole `meta` carrying it: refused.
    let w = witnesses(submit(&c, ripple(RippleKind::TypeTwo, "rv-5", &[]), &set("meta"), &[("id", json!(qid)), ("v", json!({"approved": true}))]));
    assert!(w.iter().any(|x| matches!(x, Witness::FlagNotWritable { field, .. } if field == "meta.approved")), "{w:?}");
    assert_eq!(meta_of(&c, "r"), json!({"cites": []}), "nothing of a refused write landed");
    assert_eq!(read(&c, "MATCH (q: board/question) RETURN q.id").len(), 1);
    // The owner's side (an agent initiator) approves it.
    committed(submit(
        &c,
        Initiator::Agent { thread: "cli".into(), dispatch: "approve".into() },
        &set("meta.approved"),
        &[("id", json!(qid)), ("v", json!({"at": "2026-01-01T00:00:00Z", "by": "cli"}))],
    ));
    assert_eq!(meta_of(&c, "r")["approved"]["by"], json!("cli"));
}

/// **THE REJECTION FLAG.** `meta.rejected` is a flag on every content type,
/// as `meta.approved` is: a ripple is refused it whether it mints a record
/// already rejected, sets it dotted or by a whole `meta`, or clears it; an
/// agent (the owner's tools) sets it and clears it.
#[test]
fn a_ripple_never_rejects_and_an_agent_does() {
    let (_dir, c) = cauldron();
    committed(create_question(&c, ripple(RippleKind::TypeTwo, "rj-1", &[]), "r", json!({"cites": []})));
    let qid = question_id(&c, "r");
    let set = |path: &str| format!("MATCH (q: board/question {{ id: $id }}) SET q.{path} = $v");
    let w = witnesses(create_question(&c, ripple(RippleKind::TypeTwo, "rj-2", &[]), "r2", json!({"rejected": {"by": "ripple"}})));
    assert_eq!(only_flag(&w).0, "meta.rejected");
    for (kind, step) in [(RippleKind::TypeTwo, "rj-3"), (RippleKind::TypeOne, "rj-4")] {
        let w = witnesses(submit(&c, ripple(kind, step, &[]), &set("meta.rejected"), &[("id", json!(qid)), ("v", json!(true))]));
        assert_eq!(only_flag(&w).0, "meta.rejected");
    }
    let w = witnesses(submit(&c, ripple(RippleKind::TypeTwo, "rj-5", &[]), &set("meta"), &[("id", json!(qid)), ("v", json!({"rejected": true}))]));
    assert!(w.iter().any(|x| matches!(x, Witness::FlagNotWritable { field, .. } if field == "meta.rejected")), "{w:?}");
    assert_eq!(meta_of(&c, "r"), json!({"cites": []}), "nothing of a refused write landed");
    let agent = || Initiator::Agent { thread: "owner".into(), dispatch: "reject".into() };
    committed(submit(&c, agent(), &set("meta.rejected"), &[("id", json!(qid)), ("v", json!({"at": "2026-01-01T00:00:00Z", "by": "owner"}))]));
    assert_eq!(meta_of(&c, "r")["rejected"]["by"], json!("owner"));
    // Cleared by a ripple: refused; by the agent: lands.
    let w = witnesses(submit(&c, ripple(RippleKind::TypeTwo, "rj-6", &[]), &set("meta.rejected"), &[("id", json!(qid)), ("v", json!(null))]));
    assert_eq!(only_flag(&w).0, "meta.rejected");
    committed(submit(&c, agent(), &set("meta.rejected"), &[("id", json!(qid)), ("v", json!(null))]));
    assert_eq!(meta_of(&c, "r")["rejected"], json!(null));
}
