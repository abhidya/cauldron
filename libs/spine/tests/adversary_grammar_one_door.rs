//! ADVERSARY — lens: GRAMMAR, REFUSAL AND THE ONE DOOR, the
//! ONE-DOOR half: "any path by which a production consumer can change a pool
//! other than through the single door". One second write path at the crate
//! boundary (`Pool::apply_part` and the `SpineDb` lifecycle flips) was found
//! earlier and gated behind the `test-support` cargo feature, so that
//! a production consumer reaches exactly one write path, `Cauldron::submit`.
//! This file looks for more.
//!
//! THE METHOD. This crate's own test suites ALWAYS see `test-support` (the
//! self dev-dependency turns it on), so a test in this crate cannot observe
//! the production boundary by calling it. Instead each test writes a tiny
//! throwaway crate that depends on `spine` with DEFAULT features — exactly
//! what a production consumer gets — builds it in its own target directory,
//! and (if it builds) runs it against a cauldron this test staged. The probe
//! reaches ONLY items outside the gate; if the one-door law held at the
//! boundary, the probe would not compile and every test here would pass by
//! that fact. It compiles.
//!
//! RED tests are committed under `#[ignore = "RED (adversary_grammar): ..."]`;
//! run with `--ignored`. Building the probe compiles `spine` once without
//! `test-support` plus its dependencies (bundled SQLite) into
//! `target/adversary-grammar-probe/`; expect the first run to take a minute.
//! Nothing in `src/` was touched by this file's author.

use spine::config::SpineConfig;
use spine::delta::{CompiledPart, CompiledPayload};
use spine::envelope::{Bindings, Envelope, Initiator, Instrument, Mode, Provenance};
use spine::locks::{self, Claim, ClaimSet, LockMode, Resource};
use spine::pool::NodeId;
use spine::schema_def::{fixtures, PoolName};
use spine::spine_db::{Seq, SpineDb, SPINE_FILE};
use spine::{Cauldron, Outcome};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};

test_scratch::install!();

const ALPHA: &str = "fixture-alpha";

/// The production-feature-set probe. Every arm below is a `pub` item a
/// consumer of `spine = { path = ".." }` (no features) can reach, and every
/// arm WRITES the spine or the cauldron directory without going through
/// `Cauldron::submit`.
///
/// ONE BUILD PER ARM. As first written the
/// probe was one binary holding all five arms, so ANY one gate left in
/// place kept the whole probe from compiling and the suite green — a green
/// that could not be forced red by dropping a single gate. Each arm is now
/// behind a cargo feature of the probe crate (`arm-<mode>`), built
/// separately into the same target directory (`spine` compiles once; only
/// the probe's `main.rs` rebuilds), so every writer is judged on its own and
/// un-gating exactly one of them reds exactly its tests.
const PROBE_MAIN: &str = r#"
#![allow(unused_imports, unused_variables, unreachable_code)]
use spine::spine_db::{Seq, SpineDb, SPINE_FILE};
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(String::as_str).unwrap_or("");
    let dir = PathBuf::from(args.get(2).cloned().unwrap_or_default());
    let seq = args.get(3).and_then(|s| s.parse::<i64>().ok()).map(Seq);
    let spine_path = dir.join(SPINE_FILE);
    let result: spine::error::Result<()> = match mode {
        // locks::release_at_clear is pub and takes any seq.
        #[cfg(feature = "arm-strip-locks")]
        "strip-locks" => {
            let s = SpineDb::open(&spine_path).expect("open");
            spine::locks::release_at_clear(&s, seq.expect("seq"))
        }
        // locks::acquire is pub: enrol lock rows for a seq no row issued.
        #[cfg(feature = "arm-grab-lock")]
        "grab-lock" => {
            use spine::locks::{Claim, ClaimSet, LockMode, Resource};
            let s = SpineDb::open(&spine_path).expect("open");
            let mut claims = ClaimSet::new();
            claims.insert(Claim {
                pool: spine::schema_def::PoolName::new("fixture-alpha"),
                resource: Resource::Body { node: spine::pool::NodeId(args[4].clone()) },
                mode: LockMode::Write,
            });
            spine::locks::acquire(&s, seq.expect("seq"), &claims, &spine::config::SpineConfig::default()).map(|_| ())
        }
        // recover::abort_row is pub: abort somebody else's live attempt.
        #[cfg(feature = "arm-abort-row")]
        "abort-row" => {
            let mut s = SpineDb::open(&spine_path).expect("open");
            let row = s.row_at(seq.expect("seq")).expect("row");
            spine::recover::abort_row(&mut s, &row).map(|_| ())
        }
        // recover::halt / SpineDb::set_halted are pub: halt the cauldron by hand.
        #[cfg(feature = "arm-halt")]
        "halt" => {
            let mut s = SpineDb::open(&spine_path).expect("open");
            spine::recover::halt(&mut s, "adversary probe: halted by a production consumer")
        }
        // Pool::create is pub and ungated: add a pool file beside the door.
        #[cfg(feature = "arm-add-pool")]
        "add-pool" => {
            use spine::schema_def::{EdgeKindDef, FieldDef, NodeTypeDef, PoolGrammar, PoolKind, PoolName};
            use std::collections::BTreeMap;
            let mut node_types = BTreeMap::new();
            node_types.insert(
                "thing".to_string(),
                NodeTypeDef {
                    fields: BTreeMap::from([("name".to_string(), FieldDef::required())]),
                    states: vec![],
                    count_cells: BTreeMap::new(),
                    ..Default::default()
                },
            );
            let mut edge_kinds = BTreeMap::new();
            edge_kinds.insert("near".to_string(), EdgeKindDef::local("thing", "thing"));
            let g = PoolGrammar {
                pool: PoolName::new("smuggled"),
                kind: PoolKind::new("smuggled"),
                version: 1,
                node_types,
                edge_kinds,
            };
            spine::pool::Pool::create(&dir, &g).map(|_| ())
        }
        // Cauldron::open_unrecovered is the old open, from before `open`
        // ran recovery: a handle onto a cauldron whose
        // decided, un-cleared rows nobody finished. Test-support ONLY, by
        // `cfg` and with no ungated twin; this arm proves a production
        // consumer cannot name it.
        #[cfg(feature = "arm-open-unrecovered")]
        "open-unrecovered" => {
            spine::Cauldron::open_unrecovered(&dir, spine::config::SpineConfig::default()).map(|_| ())
        }
        other => panic!("unknown probe mode `{other}`"),
    };
    match result {
        Ok(()) => println!("probe: {mode} succeeded"),
        Err(e) => {
            eprintln!("probe: {mode} refused: {e}");
            std::process::exit(2);
        }
    }
}
"#;

/// The six arms, one build each. `open-unrecovered` was a late addition:
/// the gate on `Cauldron::open_unrecovered` had been argued from
/// its `cfg`, never measured by a build.
const ARMS: [&str; 6] = ["strip-locks", "grab-lock", "abort-row", "halt", "add-pool", "open-unrecovered"];

// ===========================================================================
// THE LAW AT THE BOUNDARY
// ===========================================================================

/// SHOULD HOLD (there is exactly ONE write path; a second import path or
/// side door is a defect by construction: a production consumer reaches
/// exactly one write path, `Cauldron::submit`). A crate that depends on `spine` with default
/// features and tries to call `locks::release_at_clear`, `locks::acquire`,
/// `recover::abort_row`, `recover::halt` / `SpineDb::set_halted`, or
/// `Pool::create` against a live cauldron must NOT COMPILE, because each of
/// those is a write into the spine's tables or the cauldron directory that
/// no spine row records and no door judged.
///
/// WAS RED: it compiled. `SpineDb::open` was `pub`, and so
/// were all of the above — none carried the `test-support` gate already
/// on `apply_part` and the lifecycle flips. FIXED:
/// every one of the five now carries the same `cfg` pair, and this test
/// judges each arm on its own build.
// RED: un-gating any ONE of the six (a plain `pub` on
// `locks::release_at_clear`, say) makes exactly that arm compile and fails
// this test naming it; the arm's own effect test then fails too.
#[test]
fn a_default_features_consumer_cannot_compile_a_second_write_path() {
    let mut open: Vec<String> = Vec::new();
    for arm in ARMS {
        match probe(arm) {
            Err(log) => {
                // `open_unrecovered` is gated by `cfg` alone (no `pub(crate)`
                // twin), so without `test-support` it does not exist at all:
                // its build fails E0599 NAMING it, not E0603. Any other
                // failure (a probe typo, a moved path) is not the gate.
                //
                // Matched on the code and the NAME, not on the sentence
                // around it: rustc 1.95 says "no function or associated item
                // named `open_unrecovered`", rustc 1.99 says "no associated
                // function or constant named `open_unrecovered`" (measured;
                // cauldron pins no toolchain). The closing
                // backtick keeps a misspelt probe call from matching.
                let gated = if arm == "open-unrecovered" {
                    log.contains("error[E0599]")
                        && log.contains("named `open_unrecovered`")
                } else {
                    log.contains("error[E0603]") || log.contains("is private") || log.contains("private")
                };
                assert!(
                    gated,
                    "the `{arm}` probe failed to build for a reason other than the gate:\n{log}"
                );
            }
            Ok(bin) => open.push(format!("{arm} ({})", bin.display())),
        }
    }
    assert!(
        open.is_empty(),
        "the production-feature-set probe BUILT for {open:?}: a consumer with no `test-support` \
         can reach these writers beside the door"
    );
}

// ===========================================================================
// THE EFFECTS, ONE PER WRITER
// ===========================================================================

/// SHOULD HOLD (locks release ONLY at the cleared flip; the
/// half-landed window is invisible to lock-takers by that rule). No consumer
/// outside the door can release a decided, un-cleared transaction's locks.
///
/// ACTUAL: the probe's `strip-locks` deletes them through the public
/// `locks::release_at_clear`. The window that must be invisible is now
/// open to every lock-taker; the recovery abort race
/// was this same leak from inside the crate, and it is available from
/// outside it.
#[test]
fn a_production_consumer_cannot_release_a_landing_transactions_locks() {
    let Ok(bin) = probe("strip-locks") else {
        return; // the path is closed at the boundary: the law holds
    };
    let dir = root("strip-locks");
    let _c = fresh_cauldron(&dir);
    let node = NodeId("n-held".into());
    // A DECIDED, UN-CLEARED row holding a write lock on a body: the shape of
    // a transaction between two pools' landings (staged with the
    // test-support surface, which is what it exists for).
    let seq = stage_holder(&dir, &node, true);
    let spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    let before = locks::queue(&spine, &PoolName::new(ALPHA), &body(&node)).expect("queue");
    assert!(
        before.iter().any(|e| e.seq == seq && e.granted),
        "staged: seq {seq} holds the body write lock: {before:?}"
    );

    let out = run(&bin, &["strip-locks", dir.to_str().unwrap(), &seq.0.to_string()]);
    let after = locks::queue(&spine, &PoolName::new(ALPHA), &body(&node)).expect("queue");
    assert!(
        after.iter().any(|e| e.seq == seq && e.granted),
        "a default-features consumer released seq {seq}'s locks while its row is decided and \
         un-cleared (probe said: {out}); the lock queue is now {after:?} — the half-landed window \
         is visible to every lock-taker"
    );
}

/// SHOULD HOLD: every lock row belongs to a sequence number the spine
/// issued (the number is the row's insertion order). No
/// consumer can enrol lock rows for a number no row has — such rows block
/// every door on that resource until the wait ceiling, and the orphan sweep
/// (`release_cleared_orphans`) cannot reach them because no cleared row
/// owns them.
///
/// ACTUAL: `locks::acquire` is public; the probe grants itself a body write
/// lock at seq 999999 and the door's next SET on that body waits out the
/// lock ceiling and refuses `Busy`.
#[test]
fn a_production_consumer_cannot_enrol_locks_for_a_number_the_spine_never_issued() {
    let Ok(bin) = probe("grab-lock") else {
        return;
    };
    let dir = root("grab-lock");
    let c = fresh_cauldron(&dir);
    let doc = create_doc(&c, "dammed");
    let out = run(&bin, &["grab-lock", dir.to_str().unwrap(), "999999", &doc.0]);
    let spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    let q = locks::queue(&spine, &PoolName::new(ALPHA), &body(&doc)).expect("queue");
    assert!(
        !q.iter().any(|e| e.seq == Seq(999999)),
        "a default-features consumer holds a granted write lock at seq 999999 on `{}` (probe \
         said: {out}); the lock table is a second write path and the door on that body is \
         dammed until the wait ceiling",
        doc.0
    );
}

/// SHOULD HOLD: an attempt's row and locks belong to the door that opened
/// it; only that door or recovery (which consults the row) may abort it. No
/// consumer can abort a live attempt from outside.
///
/// ACTUAL: `recover::abort_row` is public and takes any row; the probe
/// clears the staged attempt and releases its locks. The door that owned it
/// would then fail its decided flip with a Usage error ("already cleared")
/// and report a refusal for an ask a third party cancelled.
#[test]
fn a_production_consumer_cannot_abort_a_live_attempt() {
    let Ok(bin) = probe("abort-row") else {
        return;
    };
    let dir = root("abort-row");
    let _c = fresh_cauldron(&dir);
    let node = NodeId("n-live".into());
    // An APPENDED, UN-DECIDED row holding its claims — phase 9/10 of a door.
    let seq = stage_holder(&dir, &node, false);
    let out = run(&bin, &["abort-row", dir.to_str().unwrap(), &seq.0.to_string()]);
    let spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    let row = spine.row_at(seq).expect("row");
    assert!(
        !row.cleared,
        "a default-features consumer aborted seq {seq} (probe said: {out}): cleared={} \
         decided={}; the attempt's locks are {:?}",
        row.cleared,
        row.decided,
        locks::queue(&spine, &PoolName::new(ALPHA), &body(&node)).expect("queue")
    );
}

/// SHOULD HOLD: the fail-closed halt is entered when PHYSICS
/// refuses the spine append — "an error with the system itself". It is a
/// state the machinery enters, not an operation a consumer performs; the
/// `Cauldron` handle exposes `quiesce`/`resume` and
/// deliberately no `halt`. No consumer can halt the cauldron by hand.
///
/// ACTUAL: `recover::halt` and `SpineDb::set_halted` are public; the probe
/// halts the cauldron and every later submit refuses `Halted`, naming a
/// reason nobody's physics produced.
#[test]
fn a_production_consumer_cannot_halt_the_cauldron_by_hand() {
    let Ok(bin) = probe("halt") else {
        return;
    };
    let dir = root("halt");
    let c = fresh_cauldron(&dir);
    let out = run(&bin, &["halt", dir.to_str().unwrap()]);
    let halted = c.halted().expect("halted?");
    assert!(
        halted.is_none(),
        "a default-features consumer halted the cauldron (probe said: {out}); halted = {halted:?}; \
         the next door call answers: {:?}",
        c.submit(commit(
            r#"CREATE (d: fixture-alpha/doc { title: "after", state: "draft" })"#,
            &[]
        ))
        .err()
    );
}

/// SHOULD HOLD (the cauldron is a directory of pool files;
/// `Cauldron::create` judges the whole grammar set before writing any of
/// them, and "nothing in this build changes a pin"). The shape of a live
/// cauldron — which pools exist — changes through no consumer-reachable
/// call; a pool that appears beside the door has no spine record and was
/// judged against no cauldron.
///
/// ACTUAL: `Pool::create(dir, &grammar)` is public and ungated; the probe
/// drops `smuggled.db` into the live cauldron directory, and the next
/// `Cauldron::open` discovers it as a pool like any other.
#[test]
fn a_production_consumer_cannot_add_a_pool_beside_the_door() {
    let Ok(bin) = probe("add-pool") else {
        return;
    };
    let dir = root("add-pool");
    let _c = fresh_cauldron(&dir);
    let out = run(&bin, &["add-pool", dir.to_str().unwrap()]);
    let reopened = Cauldron::open(&dir, SpineConfig::default()).expect("reopen");
    let pools: Vec<String> = reopened.pools().map(|p| p.to_string()).collect();
    assert!(
        !pools.iter().any(|p| p == "smuggled"),
        "a default-features consumer created pool `smuggled` inside the live cauldron (probe \
         said: {out}); Cauldron::open now lists {pools:?}"
    );
}

/// SHOULD HOLD: every open a production consumer can reach
/// runs recovery before it hands back a handle, so a decided, un-cleared row
/// left by a crash is finished (landed, cleared, its locks released) by the
/// open itself. `Cauldron::open_unrecovered` — the old open, which does not —
/// is test-support only.
///
/// ACTUAL if un-gated: the probe opens the cauldron and the staged decided
/// row is still un-cleared, still holding its body lock, after a consumer's
/// open returned a handle.
// RED (seen): `#[cfg(feature = "test-support")]` removed from
// `Cauldron::open_unrecovered` — the boundary test above names
// `open-unrecovered`, and this test finds the row un-cleared.
#[test]
fn a_production_consumer_cannot_open_without_recovering() {
    let Ok(bin) = probe("open-unrecovered") else {
        return; // the path is closed at the boundary: the law holds
    };
    let dir = root("open-unrecovered");
    let c = fresh_cauldron(&dir);
    drop(c);
    let node = NodeId("n-stranded".into());
    // Text that PARSES, so the recovering open can finish this row: the
    // assertion below then tells an open that heals (green — seen,
    // with the arm pointed at `Cauldron::open`) from one that does not.
    let seq = stage_holder_with(
        &dir,
        &node,
        true,
        r#"CREATE (d: fixture-alpha/doc { title: "stranded", state: "draft" })"#,
    );
    let out = run(&bin, &["open-unrecovered", dir.to_str().unwrap()]);
    let spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    let row = spine.row_at(seq).expect("row");
    assert!(
        row.cleared,
        "a default-features consumer opened the cauldron (probe said: {out}) and seq {seq} is \
         still decided and un-cleared: decided={} cleared={}; its locks are {:?} — an open that \
         does not recover is reachable beside the one that does",
        row.decided,
        row.cleared,
        locks::queue(&spine, &PoolName::new(ALPHA), &body(&node)).expect("queue")
    );
}

// ===========================================================================
// building and running the probe
// ===========================================================================

/// Build the production-feature-set probe for ONE arm, once per test binary
/// per arm. `Ok(path)` is the built binary; `Err(log)` is cargo's output
/// when it did not build. Builds are serialized through the cache lock so
/// two tests never race cargo on the probe directory.
fn probe(arm: &str) -> Result<PathBuf, String> {
    static BUILT: OnceLock<Mutex<BTreeMap<String, Result<PathBuf, String>>>> = OnceLock::new();
    let cache = BUILT.get_or_init(|| Mutex::new(BTreeMap::new()));
    let mut cache = cache.lock().expect("probe cache");
    if let Some(r) = cache.get(arm) {
        return r.clone();
    }
    let r = build_probe(arm);
    cache.insert(arm.to_string(), r.clone());
    r
}

fn build_probe(arm: &str) -> Result<PathBuf, String> {
    assert!(ARMS.contains(&arm), "`{arm}` is not a probe arm");
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("lib/spine sits two below the workspace root")
        .to_path_buf();
    let probe_dir = workspace.join("target").join("adversary-grammar-probe");
    let target_dir = probe_dir.join("target");
    std::fs::create_dir_all(probe_dir.join("src")).expect("probe dir");
    let features: String = ARMS
        .iter()
        .map(|a| format!("arm-{a} = []\n"))
        .collect();
    std::fs::write(
        probe_dir.join("Cargo.toml"),
        format!(
            r#"[package]
name = "adversary_grammar_probe"
version = "0.0.0"
edition = "2021"
publish = false

# DEFAULT FEATURES ONLY on spine: what every production consumer gets. The
# probe's OWN features select which arm is compiled — one build per writer.
[dependencies]
spine = {{ path = {:?} }}

[features]
{features}
[workspace]
"#,
            manifest_dir.to_str().expect("utf-8 path")
        ),
    )
    .expect("probe Cargo.toml");
    std::fs::write(probe_dir.join("src").join("main.rs"), PROBE_MAIN).expect("probe main.rs");

    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let mut attempts = vec![vec!["--offline"], vec![]];
    let mut last_log = String::new();
    while let Some(extra) = attempts.pop() {
        let out = Command::new(&cargo)
            .arg("build")
            .args(&extra)
            .arg("--manifest-path")
            .arg(probe_dir.join("Cargo.toml"))
            .arg("--features")
            .arg(format!("arm-{arm}"))
            .env("CARGO_TARGET_DIR", &target_dir)
            .env_remove("RUSTFLAGS")
            .output()
            .expect("run cargo");
        last_log = format!(
            "status: {}\nstdout:\n{}\nstderr:\n{}",
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        if out.status.success() {
            let built = target_dir.join("debug").join("adversary_grammar_probe");
            assert!(built.is_file(), "cargo said success but {} is missing", built.display());
            // Each arm's build overwrites the one output path; keep a copy
            // per arm so a later build cannot swap it out from under a test.
            let bin = target_dir.join("debug").join(format!("probe-{arm}"));
            std::fs::copy(&built, &bin).expect("copy the arm's binary");
            return Ok(bin);
        }
        // A compile error is the answer we came for; only retry an OFFLINE
        // resolution failure online.
        if !(last_log.contains("--offline") || last_log.contains("offline mode")) {
            break;
        }
    }
    Err(last_log)
}

/// Run one probe mode; the combined output is returned for the assertion
/// message. A refusal by the probe exits 2 and is reported, not panicked —
/// the assertion decides.
fn run(bin: &Path, args: &[&str]) -> String {
    let out = Command::new(bin).args(args).output().expect("run the probe");
    format!(
        "exit {} / {}{}",
        out.status,
        String::from_utf8_lossy(&out.stdout).trim(),
        String::from_utf8_lossy(&out.stderr).trim()
    )
}

// ===========================================================================
// staging (test-support surface — what it exists for)
// ===========================================================================

/// Append a row with one alpha part (no deltas), acquire a write lock on
/// `node`'s body for it, and — if `decided` — flip the decided marker. The
/// result is a transaction inside the window the lock rule makes invisible.
fn stage_holder(dir: &Path, node: &NodeId, decided: bool) -> Seq {
    stage_holder_with(dir, node, decided, &format!("-- adversary staged holder on {}", node.0))
}

/// `stage_holder` with the row's statement text chosen. Recovery parses a
/// decided row's text to project its RETURN, so a row recovery must be ABLE
/// to finish needs text that parses (the comment-only text above does not —
/// an open over it refuses, which is a different law).
fn stage_holder_with(dir: &Path, node: &NodeId, decided: bool, statement: &str) -> Seq {
    let mut spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    let env = commit(statement, &[]);
    let hash = env.hash_identity().expect("hash");
    let payload = CompiledPayload {
        parts: vec![CompiledPart {
            pool: PoolName::new(ALPHA),
            schema_version: fixtures::fixture_alpha().version,
            deltas: Vec::new(),
            premises: Vec::new(),
            far_versions: Default::default(),
        }],
        minted: BTreeMap::new(),
    };
    let seq = spine.append_pending(&env, &hash, &payload).expect("append");
    let mut claims = ClaimSet::new();
    claims.insert(Claim {
        pool: PoolName::new(ALPHA),
        resource: body(node),
        mode: LockMode::Write,
    });
    locks::acquire(&spine, seq, &claims, &SpineConfig::default()).expect("acquire");
    if decided {
        spine.mark_decided(seq).expect("decided");
    }
    seq
}

fn body(node: &NodeId) -> Resource {
    Resource::Body { node: node.clone() }
}

fn root(name: &str) -> PathBuf {
    test_scratch::tmp_path(format!("spine-adv-door-{name}-{}", std::process::id()))
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
                thread: "adversary-grammar".into(),
                dispatch: "adversary-grammar".into(),
            },
            instrument: Instrument::door(),
        },
        mode: Mode::Commit,
        idempotency_nonce: None,
    }
}

fn create_doc(c: &Cauldron, title: &str) -> NodeId {
    match c
        .submit(commit(
            r#"CREATE (d: fixture-alpha/doc { title: $t, state: "draft" }) RETURN d.id"#,
            &[("t", serde_json::json!(title))],
        ))
        .expect("create a doc")
    {
        Outcome::Committed { returned, .. } => {
            NodeId(returned.0[0]["d.id"].as_str().expect("id").to_string())
        }
        other => panic!("expected Committed, got {other:?}"),
    }
}

// Conceptual, not executable here:
//
// - GIVEN `Cauldron::quiesce`/`resume` on the public handle
//   (a sanctioned hook), WHEN any consumer calls them, THEN the spine_state
//   row changes — sanctioned by design, so not asserted; noted because `set_quiesced`
//   on `SpineDb` is the same write one layer down and is ALSO public.
// - GIVEN `SpineDb::conn()` is `pub(crate)` and every `Connection` is
//   private, WHEN a consumer wants raw SQL on `spine.db`, THEN it opens the
//   file with rusqlite itself — no crate boundary stops a process that can
//   read the directory; the one-door law is a law about the CRATE's
//   surface, which is what these tests measure.
