//! **`Cauldron::node_view`**: one node hydrated whole (record,
//! out-edges, `created_tx`) with the committed-commit cut it is consistent
//! at — never below a commit whose effects it shows, including while that
//! commit is landing (a hydrate that shows a commit at a cut below its
//! `committed_at` makes the daemon's fold apply it a second time).
//!
//! Also the guard that `read::counter_end` reads `in_landing` BEFORE the
//! counter — which is a source-order scan, honestly so: the race's
//! window is microseconds wide and no storm forces it reliably (the argument
//! is in `counter_end`'s doc).
//!
//! Every guard here was forced RED once; the forcing and what it produced
//! is in a `// RED:` comment above each test (house convention).

use spine::config::SpineConfig;
use spine::delta::{CompiledPart, CompiledPayload, Delta};
use spine::envelope::{Bindings, Envelope, Initiator, Instrument, Mode, Provenance, TxHash};
use spine::error::SpineError;
use spine::pool::{NodeId, Pool, QualifiedNodeId};
use spine::schema_def::{fixtures, PoolName};
use spine::spine_db::{Seq, SpineDb, SPINE_FILE};
use spine::{Cauldron, Outcome, ReadRequest};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

test_scratch::install!();

const ALPHA: &str = "fixture-alpha";
const BETA: &str = "fixture-beta";

// ---------------------------------------------------------------------------
// 1. the whole node, its edges, its creator, and a cut
// ---------------------------------------------------------------------------

// RED: `Snapshot::created_tx` made to read `updated_tx` — after the later
// SET the view's `created_tx` was the SET's hash, not the CREATE's.
#[test]
fn node_view_returns_the_record_out_edges_created_tx_and_a_consistent_cut() {
    let dir = root("whole");
    let c = fresh_cauldron(&dir, SpineConfig::default());
    let (doc, other, author, created) = create_doc_with_two_edges(&c, "whole");
    // A later SET, so `updated_tx` differs from `created_tx`.
    let set_tx = set_title(&c, &doc, "retitled");
    assert_ne!(set_tx, created, "sentinel: the SET and the CREATE are two different rows");

    let view = c
        .node_view(&q(ALPHA, &doc))
        .expect("node_view")
        .expect("the node exists");

    // The record: the pool's own snapshot read, AND the door's bulk read.
    let pool = Pool::open(&Pool::path_in(&dir, &PoolName::new(ALPHA))).expect("alpha");
    let snap = pool.snapshot().expect("snapshot");
    let direct = snap.node(&doc).expect("node").expect("present");
    assert_eq!(view.record, direct, "the record is the pool's node record");
    let read = c
        .read(&ReadRequest {
            statement: r#"MATCH (d: fixture-alpha/doc { id: $id })
                          RETURN d.title AS title, d.state AS state, d.rev AS rev"#
                .into(),
            bindings: bindings(&[("id", serde_json::json!(doc.0))]),
            format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
        })
        .expect("bulk read");
    let row = &read.rows.0[0];
    assert_eq!(row["title"], serde_json::json!("retitled"));
    assert_eq!(view.record.fields["title"].value, row["title"]);
    assert_eq!(serde_json::json!(view.record.state), row["state"]);
    assert_eq!(serde_json::json!(view.record.rev), row["rev"]);

    // The out-edges: exactly the two created, as the pool stores them.
    assert_eq!(view.out, snap.edges_from(&doc, None).expect("edges"));
    let mut got: Vec<(String, String)> = view
        .out
        .iter()
        .map(|e| (e.kind.clone(), e.dst.id.0.clone()))
        .collect();
    got.sort();
    let mut want = vec![
        ("current_of".to_string(), author.0.clone()),
        ("supersedes".to_string(), other.0.clone()),
    ];
    want.sort();
    assert_eq!(got, want, "both out-edges, and only them");

    // The creator: the CREATE's committed tx, not the later SET's.
    assert_eq!(view.created_tx, created, "created_tx names the creating commit");
    // The cut: on a quiet cauldron, the counter itself.
    assert_eq!(view.cut, c.committed_commit_counter().expect("counter"));
    drop(snap);
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// 2. absent, unknown, projected
// ---------------------------------------------------------------------------

// RED: `node_view` mapped `open_pool`'s error to `Ok(None)` — the unknown
// pool and the projected `spine` namespace both came back `Ok(None)`.
#[test]
fn node_view_of_an_absent_node_is_none_and_of_an_unknown_or_projected_pool_is_not_found() {
    let dir = root("absent");
    let c = fresh_cauldron(&dir, SpineConfig::default());
    let doc = create_doc(&c, "present");
    // Counterpart: the present node is Some, so the None below is not a
    // constant answer.
    assert!(c.node_view(&q(ALPHA, &doc)).expect("present").is_some());
    assert!(
        c.node_view(&q(ALPHA, &NodeId("no-such-node".into())))
            .expect("an absent node is an answer, not an error")
            .is_none()
    );
    for pool in ["no-such-pool", "spine"] {
        match c.node_view(&q(pool, &doc)) {
            Err(SpineError::NotFound { .. }) => {}
            other => panic!("pool `{pool}` must be NotFound, got {other:?}"),
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// 3. the MINOR: a commit LANDING while the view is read
// ---------------------------------------------------------------------------

/// Commit X — a SET on `n` — is staged with the test-support primitives:
/// appended, decided, its one part APPLIED to alpha, and NOT marked landed.
/// X's effect is in alpha, X is not counted, X is in landing.
///
/// (a) X held past the bound: `node_view` is `Busy`, never `Ok` with X.
/// (b) X finished by another thread during the retries: `node_view` is `Ok`
///     with X's value and `cut >= committed_at(X)` — so the daemon's fold
///     guard (apply an event iff `committed_at > cut`) SKIPS X, and `rev`
///     moves exactly once.
/// Control: a node in BETA (X touches only alpha) reads `Ok` at
/// `retry_bound` 0 — zero retries — while X stands; alpha does not.
// RED (forcing A): `node_view` read `cut = committed_commit_counter()` and
// then ONE bare snapshot, no window (the `drive` single-pool skip) — arm (a)
// returned Ok with X's value at a cut below committed_at(X), and the fold
// guard applied X a second time: "rev moved twice".
// RED (forcing B, opposed): `window_is_consistent`'s `in_landing` half
// deleted — arm (a) again returned Ok with X's value: "rev moved twice".
#[test]
fn node_view_during_a_landing_never_shows_that_commit_below_its_committed_at() {
    let dir = root("landing");
    let c = fresh_cauldron(&dir, SpineConfig::default());
    let n = create_doc(&c, "before");
    let shelf = create_shelf(&c, "beta-control");
    let tight = Cauldron::open(
        &dir,
        SpineConfig {
            retry_bound: 0,
            ..SpineConfig::default()
        },
    )
    .expect("tight handle");
    let short = Cauldron::open(
        &dir,
        SpineConfig {
            retry_bound: 3,
            lock_poll_interval: Duration::from_millis(5),
            ..SpineConfig::default()
        },
    )
    .expect("short handle");

    // ---- arm (a): X held past the bound ----
    let r0 = c.node_view(&q(ALPHA, &n)).unwrap().unwrap().record.rev;
    let x = stage_landing_set(&dir, &n, "X");
    let held = short.node_view(&q(ALPHA, &n));
    // Control, during the landing: beta is untouched by X and needs no retry;
    // alpha, at the same zero bound, does. Asserted after `held` is judged.
    let beta_ctrl = tight.node_view(&q(BETA, &shelf));
    let alpha_ctrl = tight.node_view(&q(ALPHA, &n));
    finish_landing(&dir, x);
    let x_at = c.transaction(x).expect("row").committed_at.expect("X counted");
    if let Ok(Some(v)) = &held {
        let folded = v.record.rev + if x_at > v.cut { 1 } else { 0 };
        assert_eq!(
            folded,
            r0 + 1,
            "rev moved twice: the view showed X (title {:?}) at cut {} below committed_at(X) = \
             {x_at}, so the fold guard applied X again",
            v.record.fields["title"].value,
            v.cut
        );
    }
    match &held {
        Err(SpineError::Busy { message }) => {
            assert!(message.contains(&n.0), "the Busy names the node: {message}");
            assert!(message.contains("attempt"), "and the attempts: {message}");
        }
        other => panic!("X held past the bound must be Busy, got {other:?}"),
    }
    assert!(
        matches!(beta_ctrl, Ok(Some(_))),
        "a pool X does not touch is certified at zero retries: {beta_ctrl:?}"
    );
    assert!(
        matches!(alpha_ctrl, Err(SpineError::Busy { .. })),
        "the pool X is landing in is not: {alpha_ctrl:?}"
    );
    // After X clears, alpha certifies at zero retries again (control).
    let after = tight.node_view(&q(ALPHA, &n)).expect("quiet again").unwrap();
    assert_eq!(after.record.rev, r0 + 1);
    assert!(after.cut >= x_at);

    // ---- arm (b): X2 finished by another thread during the retries ----
    let r1 = after.record.rev;
    let patient = Cauldron::open(
        &dir,
        SpineConfig {
            retry_bound: 10_000,
            lock_poll_interval: Duration::from_millis(2),
            ..SpineConfig::default()
        },
    )
    .expect("patient handle");
    let x2 = stage_landing_set(&dir, &n, "X2");
    let view = std::thread::scope(|s| {
        let d = dir.clone();
        s.spawn(move || {
            std::thread::sleep(Duration::from_millis(40));
            finish_landing(&d, x2);
        });
        patient.node_view(&q(ALPHA, &n))
    })
    .expect("the view completes once X2 is counted")
    .expect("present");
    let x2_at = c.transaction(x2).expect("row").committed_at.expect("X2 counted");
    assert_eq!(view.record.fields["title"].value, serde_json::json!("X2"));
    assert!(
        view.cut >= x2_at,
        "the view shows X2, so its cut ({}) is not below committed_at(X2) = {x2_at}",
        view.cut
    );
    let folded = view.record.rev + if x2_at > view.cut { 1 } else { 0 };
    assert_eq!(folded, r1 + 1, "the guard skips X2: rev moves exactly once");
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// 4. the cut under a real SET storm, N = 2 writers
// ---------------------------------------------------------------------------

/// Two writer threads SET `n.title` to distinct values while a reader loops
/// `node_view`. Afterwards every commit's `committed_at` is mapped to the
/// title it wrote (third exposure + `transaction`), and every view's title
/// must be the title of the LATEST commit with `committed_at <= cut`.
// RED (forcing A, as in test 3): the cut read before one bare snapshot —
// "14 of 576 views showed a title that is not the one at their cut", e.g.
// "cut 16: view "t-w0-8", expected t-w0-7".
#[test]
fn node_view_holds_its_cut_under_a_set_storm_at_two_writers() {
    const PER_WRITER: usize = 60;
    let dir = root("storm");
    let config = SpineConfig {
        retry_bound: 10_000,
        lock_poll_interval: Duration::from_millis(1),
        ..SpineConfig::default()
    };
    let c = fresh_cauldron(&dir, config);
    let n = create_doc(&c, "t-initial");
    let done = AtomicUsize::new(0);
    let mut views: Vec<(serde_json::Value, u64)> = Vec::new();
    std::thread::scope(|s| {
        for w in 0..2 {
            let (c, n, done) = (&c, &n, &done);
            s.spawn(move || {
                for i in 0..PER_WRITER {
                    let k = format!("t-w{w}-{i}");
                    c.submit(commit(
                        r#"MATCH (d: fixture-alpha/doc { id: $id }) SET d.title = $k"#,
                        &[("id", serde_json::json!(n.0)), ("k", serde_json::json!(k))],
                    ))
                    .expect("a storm SET commits");
                }
                done.fetch_add(1, Ordering::SeqCst);
            });
        }
        while done.load(Ordering::SeqCst) < 2 {
            let v = c.node_view(&q(ALPHA, &n)).expect("node_view").expect("present");
            views.push((v.record.fields["title"].value.clone(), v.cut));
        }
    });

    let counter = c.committed_commit_counter().expect("counter");
    let mut by_cut: Vec<(u64, String)> = Vec::new();
    for cc in c
        .commits_touching(&[PoolName::new(ALPHA)], 0, counter)
        .expect("third exposure")
    {
        let row = c.transaction(cc.seq).expect("row");
        let title = row
            .bindings
            .get("k")
            .or_else(|| row.bindings.get("t"))
            .and_then(|v| v.as_str())
            .expect("every alpha commit here wrote a title")
            .to_string();
        by_cut.push((cc.committed_at, title));
    }
    by_cut.sort();
    assert_eq!(by_cut.len(), 1 + 2 * PER_WRITER, "every commit is in the sweep");
    assert!(views.len() >= 2, "the reader read during the storm: {}", views.len());
    let mut wrong = Vec::new();
    for (title, cut) in &views {
        let want = by_cut
            .iter()
            .rev()
            .find(|(at, _)| at <= cut)
            .map(|(_, t)| t.clone())
            .expect("the create is at or below every cut");
        if title != &serde_json::json!(want) {
            wrong.push(format!("cut {cut}: view {title}, expected {want}"));
        }
    }
    assert!(
        wrong.is_empty(),
        "{} of {} views showed a title that is not the one at their cut: {:?}",
        wrong.len(),
        views.len(),
        &wrong[..wrong.len().min(5)]
    );
    println!("MEASUREMENT storm: {} views over {} commits", views.len(), by_cut.len());
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// 5. step 3 reads `in_landing` before the counter
// ---------------------------------------------------------------------------

/// A SOURCE-ORDER scan, and honest about it: the window (a commit counted
/// and cleared between a counter read and an `in_landing` read) is
/// microseconds wide and no storm forces it reliably; `counter_end`'s doc
/// carries the argument that this order makes the cut exact. Comments are
/// stripped before the scan, so the doc that explains the order cannot
/// satisfy it.
// RED: the two lines swapped back (counter first) — "in_landing( at byte
// N, committed_commit_counter( at byte M < N".
#[test]
fn counter_end_reads_in_landing_before_the_counter() {
    let src = include_str!("../src/read.rs");
    let body = fn_body(src, "pub fn counter_end(");
    let code = strip_comments(body);
    let landing = code.find("in_landing(").expect("counter_end reads in_landing");
    let counter = code
        .find("committed_commit_counter(")
        .expect("counter_end reads the counter");
    let touching = code.find("commits_touching(").expect("and the window");
    assert!(
        landing < counter,
        "in_landing( at byte {landing}, committed_commit_counter( at byte {counter} < \
         {landing}: a commit counted and cleared between the two reads escapes the window"
    );
    assert!(counter < touching, "the window query takes the counter it read");
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// The text of one fn, from its signature to the first line that is exactly
/// `}` (top-level fns in this crate are rustfmt'd).
fn fn_body<'a>(src: &'a str, sig: &str) -> &'a str {
    let start = src.find(sig).unwrap_or_else(|| panic!("`{sig}` not found"));
    let rest = &src[start..];
    let end = rest.find("\n}\n").expect("fn end");
    &rest[..end]
}

fn strip_comments(code: &str) -> String {
    code.lines()
        .map(|l| match l.find("//") {
            Some(i) => &l[..i],
            None => l,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn root(name: &str) -> PathBuf {
    test_scratch::tmp_path(format!("spine-node-view-{name}-{}", std::process::id()))
}

fn fresh_cauldron(dir: &Path, config: SpineConfig) -> Cauldron {
    let _ = std::fs::remove_dir_all(dir);
    Cauldron::create(
        dir,
        &[fixtures::fixture_alpha(), fixtures::fixture_beta()],
        config,
    )
    .expect("create the cauldron")
}

fn q(pool: &str, id: &NodeId) -> QualifiedNodeId {
    QualifiedNodeId {
        pool: PoolName::new(pool),
        id: id.clone(),
    }
}

fn bindings(params: &[(&str, serde_json::Value)]) -> Bindings {
    let mut b = Bindings::new();
    for (k, v) in params {
        b.insert((*k).to_string(), v.clone());
    }
    b
}

fn commit(statement: &str, params: &[(&str, serde_json::Value)]) -> Envelope {
    Envelope {
        statement: statement.to_string(),
        bindings: bindings(params),
        format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
        provenance: Provenance {
            initiator: Initiator::Agent {
                thread: "node-view".into(),
                dispatch: "node-view".into(),
            },
            instrument: Instrument::door(),
        },
        mode: Mode::Commit,
        idempotency_nonce: None,
    }
}

fn committed(out: Outcome) -> (TxHash, spine::Returned) {
    match out {
        Outcome::Committed { tx, returned, .. } => (tx, returned),
        other => panic!("expected Committed, got {other:?}"),
    }
}

fn create_doc(c: &Cauldron, title: &str) -> NodeId {
    let (_, r) = committed(
        c.submit(commit(
            r#"CREATE (d: fixture-alpha/doc { title: $t, state: "draft" }) RETURN d.id"#,
            &[("t", serde_json::json!(title))],
        ))
        .expect("create a doc"),
    );
    NodeId(r.0[0]["d.id"].as_str().expect("id").to_string())
}

fn create_shelf(c: &Cauldron, label: &str) -> NodeId {
    let (_, r) = committed(
        c.submit(commit(
            r#"CREATE (s: fixture-beta/shelf { label: $l }) RETURN s.id"#,
            &[("l", serde_json::json!(label))],
        ))
        .expect("create a shelf"),
    );
    NodeId(r.0[0]["s.id"].as_str().expect("id").to_string())
}

/// A doc with two out-edges — `supersedes` to another doc, `current_of` to
/// an author — created in ONE commit, whose tx is returned.
fn create_doc_with_two_edges(c: &Cauldron, title: &str) -> (NodeId, NodeId, NodeId, TxHash) {
    let (tx, r) = committed(
        c.submit(commit(
            r#"CREATE (d: fixture-alpha/doc { title: $t, state: "draft" })
               CREATE (o: fixture-alpha/doc { title: "older", state: "superseded" })
               CREATE (a: fixture-alpha/author { name: "ann" })
               CREATE (d) -[supersedes]-> (o)
               CREATE (d) -[current_of]-> (a)
               RETURN d.id, o.id, a.id"#,
            &[("t", serde_json::json!(title))],
        ))
        .expect("create a doc with two edges"),
    );
    let id = |k: &str| NodeId(r.0[0][k].as_str().expect("id").to_string());
    (id("d.id"), id("o.id"), id("a.id"), tx)
}

fn set_title(c: &Cauldron, doc: &NodeId, title: &str) -> TxHash {
    committed(
        c.submit(commit(
            r#"MATCH (d: fixture-alpha/doc { id: $id }) SET d.title = $k"#,
            &[("id", serde_json::json!(doc.0)), ("k", serde_json::json!(title))],
        ))
        .expect("SET commits"),
    )
    .0
}

/// Commit X, a SET of `doc.title`, taken as far as a landing gets before
/// its landed flip: appended, decided, the part APPLIED to alpha — and not
/// marked landed, so X is uncounted and in landing.
fn stage_landing_set(dir: &Path, doc: &NodeId, title: &str) -> Seq {
    let mut spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    let env = commit(&format!("-- staged landing SET: {title}"), &[]);
    let hash = env.hash_identity().expect("hash");
    let part = CompiledPart {
        pool: PoolName::new(ALPHA),
        schema_version: 1,
        deltas: vec![Delta::SetField {
            node: doc.clone(),
            path: vec!["title".to_string()],
            value: serde_json::json!(title),
        }],
        premises: Vec::new(),
        far_versions: Default::default(),
    };
    let payload = CompiledPayload {
        parts: vec![part.clone()],
        minted: std::collections::BTreeMap::new(),
    };
    let seq = spine.append_pending(&env, &hash, &payload).expect("append");
    spine.mark_decided(seq).expect("decided");
    let mut pool = Pool::open(&Pool::path_in(dir, &PoolName::new(ALPHA))).expect("alpha");
    pool.apply_part(&hash, 0, &part).expect("X's part applies to alpha");
    seq
}

/// X's landed flip (counts it, stamps committed_at) and its cleared flip.
fn finish_landing(dir: &Path, seq: Seq) {
    let mut spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    spine.mark_part_landed(seq, 0).expect("landed");
    spine
        .mark_cleared(seq, &serde_json::json!({ "staged": "cleared" }))
        .expect("clear");
}
