//! **`Cauldron::transaction` / `transaction_by_hash`**: a spine row
//! read back whole, by sequence number and by hash identity. Exposures of a
//! fact the spine already owns — what the soup daemon's feed
//! composes with the third exposure (`commits_touching`).
//!
//! Every guard here was forced RED once (`// RED:` above each test).

use spine::config::SpineConfig;
use spine::envelope::{Bindings, Envelope, Initiator, Instrument, Mode, Provenance, TxHash};
use spine::error::SpineError;
use spine::schema_def::{fixtures, PoolName};
use spine::spine_db::Seq;
use spine::{Cauldron, Outcome};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

test_scratch::install!();

const ALPHA: &str = "fixture-alpha";
const BETA: &str = "fixture-beta";

/// Two writer threads commit across alpha and beta (single-pool commits in
/// each, and cross-pool ones). A feed sweep — `commits_touching(all pools,
/// 0, counter)` then `transaction(seq)` — yields every commit exactly once,
/// in `committed_at` order, and each row is THAT commit: same seq, same
/// hash, same `committed_at` as the third exposure reported.
// RED: `transaction` returned `row_at(Seq(seq.0 - 1))` — "row for seq N is
// seq N-1" on the first commit swept.
#[test]
fn a_feed_sweep_composes_from_the_exposures_at_two_writers() {
    const PER_WRITER: usize = 15;
    let dir = root("sweep");
    let c = fresh_cauldron(&dir);
    let submitted: Vec<(Seq, TxHash)> = std::thread::scope(|s| {
        let hs: Vec<_> = (0..2)
            .map(|w| {
                let c = &c;
                s.spawn(move || {
                    let mut out = Vec::new();
                    for i in 0..PER_WRITER {
                        let t = format!("w{w}-{i}");
                        let stmt = match i % 3 {
                            0 => r#"CREATE (d: fixture-alpha/doc { title: $t, state: "draft" })"#,
                            1 => r#"CREATE (s: fixture-beta/shelf { label: $t })"#,
                            _ => {
                                r#"CREATE (d: fixture-alpha/doc { title: $t, state: "draft" })
                                   CREATE (s: fixture-beta/shelf { label: $t })"#
                            }
                        };
                        match c
                            .submit(commit(stmt, &[("t", serde_json::json!(t))]))
                            .expect("a writer commits")
                        {
                            Outcome::Committed { seq, tx, deduplicated, .. } => {
                                assert!(!deduplicated, "every commit is a fresh identity");
                                out.push((seq, tx));
                            }
                            other => panic!("expected Committed, got {other:?}"),
                        }
                    }
                    out
                })
            })
            .collect();
        hs.into_iter().flat_map(|h| h.join().expect("writer")).collect()
    });
    assert_eq!(submitted.len(), 2 * PER_WRITER);

    let counter = c.committed_commit_counter().expect("counter");
    let swept = c
        .commits_touching(&[PoolName::new(ALPHA), PoolName::new(BETA)], 0, counter)
        .expect("third exposure");
    let mut last_at = 0u64;
    let mut seen: BTreeMap<i64, TxHash> = BTreeMap::new();
    for cc in &swept {
        assert!(cc.committed_at > last_at, "committed_at order, strictly: {swept:?}");
        last_at = cc.committed_at;
        let row = c.transaction(cc.seq).expect("the row the sweep named");
        assert_eq!(row.seq, cc.seq, "row for seq {} is seq {}", cc.seq.0, row.seq.0);
        assert_eq!(row.hash, cc.hash, "row for seq {} carries its hash", cc.seq.0);
        assert_eq!(row.committed_at, Some(cc.committed_at), "and its committed_at");
        assert!(seen.insert(row.seq.0, row.hash.clone()).is_none(), "exactly once");
    }
    let want: BTreeMap<i64, TxHash> = submitted.into_iter().map(|(s, h)| (s.0, h)).collect();
    assert_eq!(seen, want, "the sweep is exactly the commits the writers made");
    let _ = std::fs::remove_dir_all(&dir);
}

// RED: `transaction` returned `row_at`'s error unmapped — the unknown seq
// arrived as `Hard { "storage failure: Query returned no rows" }`.
#[test]
fn transaction_of_an_unknown_seq_is_not_found() {
    let dir = root("unknown-seq");
    let c = fresh_cauldron(&dir);
    let seq = match c
        .submit(commit(
            r#"CREATE (d: fixture-alpha/doc { title: "one", state: "draft" })"#,
            &[],
        ))
        .expect("commit")
    {
        Outcome::Committed { seq, .. } => seq,
        other => panic!("expected Committed, got {other:?}"),
    };
    // Counterpart: the known seq is a row, so NotFound below is not constant.
    assert_eq!(c.transaction(seq).expect("a known seq").seq, seq);
    match c.transaction(Seq(seq.0 + 1_000)) {
        Err(SpineError::NotFound { message }) => {
            assert!(message.contains(&(seq.0 + 1_000).to_string()), "names the seq: {message}")
        }
        other => panic!("an unknown seq must be NotFound, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// RED: `transaction_by_hash` returned `Ok(None)` always — the committed
// hash came back None.
#[test]
fn transaction_by_hash_returns_the_decided_row_and_none_for_an_unknown_hash() {
    let dir = root("by-hash");
    let c = fresh_cauldron(&dir);
    let (seq, tx) = match c
        .submit(commit(
            r#"CREATE (d: fixture-alpha/doc { title: "one", state: "draft" })"#,
            &[],
        ))
        .expect("commit")
    {
        Outcome::Committed { seq, tx, .. } => (seq, tx),
        other => panic!("expected Committed, got {other:?}"),
    };
    let row = c
        .transaction_by_hash(&tx)
        .expect("by hash")
        .expect("the committed identity has a row");
    assert_eq!((row.seq, row.hash.clone()), (seq, tx.clone()));
    assert!(row.decided && row.cleared, "the decided, cleared commit row");
    assert!(row.committed_at.is_some());
    let unknown = TxHash::from_hex(&"ab".repeat(32)).expect("a well-formed hash");
    assert_ne!(unknown, tx, "sentinel: the unknown hash is a different identity");
    assert!(c.transaction_by_hash(&unknown).expect("an answer").is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn root(name: &str) -> PathBuf {
    test_scratch::tmp_path(format!("spine-tx-exposure-{name}-{}", std::process::id()))
}

fn fresh_cauldron(dir: &Path) -> Cauldron {
    let _ = std::fs::remove_dir_all(dir);
    Cauldron::create(
        dir,
        &[fixtures::fixture_alpha(), fixtures::fixture_beta()],
        SpineConfig::default(),
    )
    .expect("create the cauldron")
}

fn commit(statement: &str, params: &[(&str, serde_json::Value)]) -> Envelope {
    let mut bindings = Bindings::new();
    for (k, v) in params {
        bindings.insert((*k).to_string(), v.clone());
    }
    Envelope {
        statement: statement.to_string(),
        bindings,
        format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
        provenance: Provenance {
            initiator: Initiator::Agent {
                thread: "tx-exposure".into(),
                dispatch: "tx-exposure-test".into(),
            },
            instrument: Instrument::door(),
        },
        mode: Mode::Commit,
        idempotency_nonce: None,
    }
}
