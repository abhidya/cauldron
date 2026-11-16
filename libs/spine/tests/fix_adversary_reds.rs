//! THE FIXES' OWN PROOFS — the greens the
//! adversaries' red tests cannot force on their own.
//!
//! Every adversary red that went green is guarded by the adversary's test.
//! A few of the fixes are not fully DISCRIMINATED by those tests, and a
//! green that cannot be forced red is not evidence:
//!
//! - `RETURN d.rev` through the WRITE door is refused by name now (a
//!   mutation-free, premise-free statement is the read form), so the
//!   adversary's rev test never reaches the projection on that path; the
//!   projection is proven here at the READ door.
//! - A dotted SET into a scalar is refused at COMPILE (before any row) and
//!   again at the LANDING (defence in depth). The adversary's test asserts
//!   only "refused, nothing landed", which the landing's refusal alone would
//!   satisfy — after the decided flip, as a wedged row. The test here pins
//!   the refusal to BEFORE the decided flip, by name.
//!
//! Each test names, in a comment, the reversion that reds it.
//!
//! # Isolation is STRUCTURAL: the armed pause never runs in this process
//!
//! The armed test sets `SPINE_PAUSE` and a latch path in its process's
//! environment, and every door in a process reads those variables at every
//! stage. An earlier shape isolated it with an `RwLock` the armed test held exclusively and every other test had
//! to take shared (`env_shared_guard()`), which was opt-in: a test that
//! forgot the call reopened the race that ended in a message-less SIGABRT.
//! Now the armed bodies run only in a CHILD process — this test binary
//! re-executed as `--ignored --exact armed::child_entry --test-threads=1` —
//! and `arm_pause` refuses to arm anywhere else. So no test in this
//! binary's own process can ever see an armed pause: every test here,
//! including one added tomorrow with no guard at all, is isolated by
//! construction. The RwLock guard is gone (inert once nothing in this
//! process arms). `armed::a_test_with_no_guard_runs_its_door_while_a_sibling_is_held_armed`
//! is that tomorrow's test, written now: no guard, its door run while a
//! child is held at the armed pause.
//!
//! Measured (the whole binary 32 times, 8 at a time, fault build):
//! this shape 0/32 failing; the transient-hold body put back IN THIS
//! PROCESS with no guard anywhere (the boundary removed) 26/32 (24 at the
//! stuck-row assertions, 2 SIGABRT) — the old race, reopened. The old
//! opt-in guard measured 0/32 with ONE call deleted and 14/32 with all
//! three deleted: a single forgotten call was a silent hole.

use spine::config::SpineConfig;
use spine::envelope::{Bindings, Envelope, Initiator, Instrument, Mode, Provenance};
use spine::error::SpineError;
use spine::pool::NodeId;
use spine::schema_def::{fixtures, grammar_fault_tag, PoolName};
use spine::spine_db::{SpineDb, SPINE_FILE};
use spine::{Cauldron, Outcome, ReadRequest};
use std::path::{Path, PathBuf};

test_scratch::install!();

/// `rev` is a machine-owned name every node has a value for; asked by field
/// path it projects the whole-node revision — the same number `REV(d)`
/// projects — never `null`.
// RED: the `FIELD_REV` arm of `ladle::compile::read_path` answering
// `Value::Null` — `d.rev` then reads as "no such field".
#[test]
fn return_rev_by_field_path_projects_the_revision_at_the_read_door() {
    let dir = root("rev-read-door");
    let c = fresh_cauldron(&dir);
    let doc = create_doc(&c, "rev");
    // Move the revision once so 1 is not the only number that could appear.
    c.submit(commit(
        r#"MATCH (d: fixture-alpha/doc { id: $id }) SET d.title = "moved""#,
        &[("id", serde_json::json!(doc.0))],
    ))
    .expect("a SET lands");

    let out = c
        .read(&ReadRequest {
            statement: r#"MATCH (d: fixture-alpha/doc { id: $id }) RETURN d.rev, REV(d) AS whole"#
                .into(),
            bindings: bindings(&[("id", serde_json::json!(doc.0))]),
            format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
        })
        .expect("the read completes");
    let row = &out.rows.0[0];
    assert_eq!(row["whole"], serde_json::json!(2), "REV(d) moved to 2");
    assert_eq!(
        row["d.rev"], row["whole"],
        "RETURN d.rev projects the same revision REV(d) does: {row:?}"
    );
}

/// A dotted SET through a scalar refuses `not_an_object` at COMPILE — before
/// any row is appended, so nothing is decided and nothing is wedged. The
/// landing refuses the same fact by the same name as defence in depth, but a
/// refusal there would come AFTER the decided flip.
// RED: `check_settable`'s dotted-path walk removed (returning `Ok(())` for
// any declared field) — the landing then refuses instead, after the decided
// flip, and the row is left decided under a loud invariant error.
#[test]
fn a_dotted_set_into_a_scalar_is_refused_by_name_before_any_row_is_decided() {
    let dir = root("dotted-scalar-pre-decided");
    let c = fresh_cauldron(&dir);
    let doc = create_doc(&c, "hello");
    let env = commit(
        r#"MATCH (d: fixture-alpha/doc { id: $id }) SET d.title.sub = 1"#,
        &[("id", serde_json::json!(doc.0))],
    );
    let hash = env.hash_identity().expect("hash");
    let out = c.submit(env);
    match &out {
        Err(SpineError::Refused { witnesses }) => assert!(
            witnesses.iter().any(|w| grammar_fault_tag(w) == Some("not_an_object")),
            "refused by the grammar's own name: {witnesses:?}"
        ),
        other => panic!("expected a structured refusal, got {other:?}"),
    }
    let spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    let row = spine.find_by_hash(&hash).expect("lookup");
    assert!(
        row.as_ref().map(|r| !r.decided && r.cleared).unwrap_or(true),
        "a refusal is whole: no decided row, no wedge: {row:?}"
    );
    // And the scalar is intact.
    let title = c
        .read(&ReadRequest {
            statement: r#"MATCH (d: fixture-alpha/doc { id: $id }) RETURN d.title"#.into(),
            bindings: bindings(&[("id", serde_json::json!(doc.0))]),
            format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
        })
        .expect("read");
    assert_eq!(title.rows.0[0]["d.title"], serde_json::json!("hello"));
}

/// `quiesce()` answering `Ok` means "quiesced, by you, and drained". If the
/// caller's own quiescer is lifted while it is still draining — here by the
/// test-support repair that lifts every quiescer, the shape of a by-hand
/// `DELETE FROM quiescers` — the drain loop notices and answers `Busy`, not
/// `Ok` (the concurrency adversary's shape, whose own test is carried
/// by ownership alone: a second actor's `resume()` lifts nothing).
// RED: the drain loop's `holds_quiescer` re-check removed — it then sees
// `pending()` empty once the staged row finishes and answers `Ok` for a
// cauldron that is no longer quiesced by anyone.
#[test]
fn a_quiesce_lifted_out_from_under_its_caller_mid_drain_does_not_answer_ok() {
    use spine::delta::{CompiledPart, CompiledPayload};
    use spine::envelope::TxHash;
    use spine::spine_db::AppendOutcome;
    use std::time::{Duration, Instant};

    let dir = root("quiesce-lifted-mid-drain");
    let c = fresh_cauldron(&dir);
    // One decided, un-cleared row, so the drain has to wait.
    let seq = {
        let mut spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
        let payload = CompiledPayload {
            parts: vec![CompiledPart {
                pool: PoolName::new("fixture-alpha"),
                schema_version: fixtures::fixture_alpha().version,
                deltas: vec![],
                premises: vec![],
                far_versions: Default::default(),
            }],
            minted: Default::default(),
        };
        let seq = match spine
            .append_or_existing(&commit("-- staged by the fixing pass", &[]), &TxHash([0x5a; 32]), &payload)
            .expect("append")
        {
            AppendOutcome::Appended(seq) => seq,
            other => panic!("a fresh append, got {other:?}"),
        };
        spine.mark_decided(seq).expect("decide");
        seq
    };

    let quiescer = {
        let dir = dir.clone();
        std::thread::spawn(move || {
            Cauldron::open_unrecovered(&dir, SpineConfig::default())
                .expect("open")
                .quiesce()
        })
    };
    let started = Instant::now();
    while !spine::recover::is_quiesced(&SpineDb::open(&dir.join(SPINE_FILE)).expect("spine"))
        .expect("flag")
    {
        assert!(started.elapsed() < Duration::from_secs(5), "the quiescer goes up first");
        std::thread::sleep(Duration::from_millis(5));
    }
    std::thread::sleep(Duration::from_millis(150));
    assert!(!quiescer.is_finished(), "the quiesce is draining behind the staged row");

    // LIFT EVERY QUIESCER, by hand, mid-drain.
    SpineDb::open(&dir.join(SPINE_FILE))
        .expect("spine")
        .set_quiesced(false)
        .expect("lift by hand");

    // Finish the staged row so the drain loop would see `pending()` empty.
    {
        let mut spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
        spine.mark_part_landed(seq, 0).expect("alpha");
        spine.mark_cleared(seq, &serde_json::json!({ "staged": true })).expect("cleared");
    }
    let outcome = quiescer.join().expect("the quiescing thread did not panic");
    // And nothing of that call stands, so no token comes back with the
    // error (fixing pass two: `QuiesceError::standing`).
    assert!(
        matches!(
            &outcome,
            Err(spine::QuiesceError {
                error: SpineError::Busy { .. },
                standing: None
            })
        ),
        "a quiesce lifted out from under its caller must not answer Ok: {outcome:?}"
    );
    let _ = c;
}

// ---------------------------------------------------------------------------
// plumbing
// ---------------------------------------------------------------------------

fn root(name: &str) -> PathBuf {
    test_scratch::tmp_path(format!("spine-fix-reds-{name}-{}", std::process::id()))
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
                thread: "fix-adversary-reds".into(),
                dispatch: "fix-pass".into(),
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

#[allow(dead_code)]
const _ALPHA: PoolName = PoolName(String::new());

// ===========================================================================
// ARMED BUILD — the seam's pause form, in-process
// ===========================================================================

// The armed child's spine helpers (`the_stuck_row`, `spine_sql`, ...). Only
// the armed build uses it since the `env_shared` guard was removed
// from the unarmed tests (isolation is the child process now; module doc).
#[cfg(all(unix, feature = "fault-injection"))]
#[allow(dead_code)]
#[path = "util/kill.rs"]
mod kill;

#[cfg(all(unix, feature = "fault-injection"))]
mod armed {
    use super::*;
    use std::process::{Child, Command, Output, Stdio};
    use std::thread::ScopedJoinHandle;
    use std::time::{Duration, Instant};

    /// Set ONLY on the child `Command` (never in this process): names the
    /// armed body [`child_entry`] runs. `arm_pause` refuses without it.
    const ARMED_CHILD_ENV: &str = "R3S8_ARMED_CHILD";

    const CROSS_BY_ID: &str = r#"MATCH (d: fixture-alpha/doc { id: $id })
       EXPECT d.state = "draft"
       CREATE (t: fixture-beta/ticket { state: "open" })
       CREATE (t) -[catalogues]-> (d)
       SET d.state = "published"
       RETURN d.title AS title, t.id AS ticket"#;

    /// Mechanical writes wait their turn; they are never refused. A foreign writer holds `spine.db`'s write lock across the
    /// door's cleared flip for LONGER than the per-statement busy timeout
    /// and SHORTER than the lock ceiling: the door finishes its commit
    /// (Committed, row cleared, no lock left), and the next statement on the
    /// doc lands. Before this fix the busy timeout was a hidden 5 s
    /// constant and the door abandoned the decided row on the first `Busy`
    /// (the concurrency adversary's shape, with a hold the ceiling can
    /// absorb).
    // RED: `exec::wait_turn` returning the first `Busy` without retrying —
    // the door answers `Busy`, the row stays decided and un-cleared with its
    // locks, and the later statement is `Busy` at the ceiling.
    //
    // Runs in a CHILD process (module doc): this parent runs
    // `transient_hold_body` there and requires the child's one test to pass.
    #[test]
    fn a_transient_spine_hold_shorter_than_the_ceiling_never_strands_a_decided_commit() {
        let out = armed_child("transient-hold").output().expect("run the armed child");
        assert_child_passed(&out, "transient-hold");
    }

    /// The body of the test above, run only inside the armed child.
    fn transient_hold_body() {
        let name = "fix-transient-hold";
        let dir = root(name);
        let _ = std::fs::remove_dir_all(&dir);
        let c = Cauldron::create(
            &dir,
            &[fixtures::fixture_alpha(), fixtures::fixture_beta()],
            SpineConfig {
                busy_timeout: Duration::from_millis(50),
                lock_wait_ceiling: Duration::from_secs(3),
                ..SpineConfig::default()
            },
        )
        .expect("create the cauldron");
        let id = create_doc(&c, "held across a transient hold");
        let env = commit(CROSS_BY_ID, &[("id", serde_json::json!(id.0))]);
        let pause = arm_pause("before-cleared", name);

        let (door_outcome, seq) = std::thread::scope(|scope| {
            let door = scope.spawn(|| c.submit(env));
            // A panic in this body would otherwise DEADLOCK: `scope` joins
            // the door before it propagates the panic, the door is held on
            // a latch only `pause`'s drop (after the scope) would write, and
            // the pause ends at its 30 s ceiling as a message-less abort
            // instead of this test's own failure message.
            // RED: delete this line and fail any assertion below — the run
            // hangs 30 s and dies by SIGABRT with the panic unprinted.
            let _unstick = pause.release_on_unwind();
            pause.wait_reached(&door);
            pause.disarm();
            let row = super::kill::the_stuck_row(&dir);
            assert!(row.decided && !row.cleared && row.parts.iter().all(|p| p.landed));

            // The foreign hold: longer than busy_timeout (50 ms), shorter
            // than the ceiling (3 s), and let go on a timer from another
            // thread — the door must still be waiting when it ends.
            let holder = super::kill::spine_sql(&dir);
            holder
                .execute_batch("BEGIN IMMEDIATE")
                .expect("hold the spine's write lock");
            let letting_go = std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(600));
                holder.execute_batch("COMMIT").expect("let go of the spine");
            });
            pause.release();
            let out = door.join().expect("the door thread did not panic");
            letting_go.join().expect("the holder thread did not panic");
            (out, row.seq)
        });

        assert!(
            matches!(door_outcome, Ok(Outcome::Committed { .. })),
            "a door one flip from cleared waits its turn and finishes: {:?}",
            door_outcome.as_ref().map_err(|e| e.message())
        );
        let after = super::kill::spine_of(&dir).row_at(seq).expect("row");
        assert!(after.cleared, "the commit cleared");
        assert_eq!(super::kill::lock_rows(&dir, seq), 0, "no lock survives the return");
        let later = c.submit(commit(
            r#"MATCH (d: fixture-alpha/doc { id: $id }) SET d.title = "after the hold""#,
            &[("id", serde_json::json!(id.0))],
        ));
        assert!(
            matches!(later, Ok(Outcome::Committed { .. })),
            "the next statement on the doc lands: {:?}",
            later.as_ref().map_err(|e| e.message())
        );
    }

    /// ISOLATION BY CONSTRUCTION, PROVED. This test
    /// takes NO guard of any kind, exactly as a test added tomorrow by
    /// someone who never read this file would not. It starts an armed child
    /// held at `before-cleared` (its `.reached` marker up, the child alive,
    /// its latch unwritten), and WHILE that child is held it runs the door in
    /// this process — create and SET on its own cauldron — and requires both
    /// to land inside a bound far below the 30 s pause ceiling. Then it
    /// releases the child, which must finish its own held commit.
    ///
    /// Under the old opt-in shape an unguarded door in this process could be
    /// paused at a sibling's latch; here it cannot, because no sibling arms
    /// THIS process. The child is checked to be still held after this
    /// process's door returned, so the two genuinely overlapped.
    // RED (seen): the armed body run in THIS process instead of a
    // child (`ARMED_CHILD_ENV` set here and `held_door_body` on a thread —
    // the structural boundary removed) — this test's door is paused at the
    // armed latch and the bound fires: "a door in this process did not
    // return within 10 s".
    #[test]
    fn a_test_with_no_guard_runs_its_door_while_a_sibling_is_held_armed() {
        let child = armed_child("held-door")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn the armed child");
        let mut held = HeldChild::new(child, "held-door");
        let started = Instant::now();
        while !held.reached.is_file() {
            assert!(
                held.child.try_wait().expect("try_wait").is_none(),
                "the armed child exited before reaching its pause"
            );
            assert!(started.elapsed() < Duration::from_secs(20), "the armed child never paused");
            std::thread::sleep(Duration::from_millis(2));
        }

        // The child is armed and HELD right now. This process's door, unguarded:
        let dir = root("unguarded-while-held");
        let (tx, rx) = std::sync::mpsc::channel();
        let door_dir = dir.clone();
        std::thread::spawn(move || {
            let c = fresh_cauldron(&door_dir);
            let id = create_doc(&c, "unguarded");
            let set = c.submit(commit(
                r#"MATCH (d: fixture-alpha/doc { id: $id }) SET d.title = "landed while a sibling was held""#,
                &[("id", serde_json::json!(id.0))],
            ));
            let _ = tx.send(set.map_err(|e| e.message()));
        });
        let landed = rx.recv_timeout(Duration::from_secs(10)).unwrap_or_else(|_| {
            panic!(
                "a door in this process did not return within 10 s while a sibling was held \
                 armed: it is paused at an arm this test never set — isolation is not structural"
            )
        });
        assert!(
            matches!(landed, Ok(Outcome::Committed { .. })),
            "this process's door lands: {landed:?}"
        );
        assert!(
            held.child.try_wait().expect("try_wait").is_none() && !held.latch.exists(),
            "the sibling was still held when this process's door returned (they overlapped)"
        );

        let out = held.release_and_wait();
        assert_child_passed(&out, "held-door");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The child half of the sentinel above: arm, submit, and stay held at
    /// `before-cleared` until the PARENT writes the latch; then the commit
    /// must finish.
    fn held_door_body() {
        let name = "held-door";
        let dir = root(name);
        let c = fresh_cauldron(&dir);
        let id = create_doc(&c, "held in the child");
        let env = commit(
            r#"MATCH (d: fixture-alpha/doc { id: $id }) SET d.title = "held then released""#,
            &[("id", serde_json::json!(id.0))],
        );
        let pause = arm_pause("before-cleared", name);
        let out = std::thread::scope(|scope| {
            let door = scope.spawn(|| c.submit(env));
            let _unstick = pause.release_on_unwind();
            pause.wait_reached(&door);
            door.join().expect("the door thread did not panic")
        });
        assert!(
            matches!(out, Ok(Outcome::Committed { .. })),
            "the held door commits once the parent releases it: {:?}",
            out.as_ref().map_err(|e| e.message())
        );
    }

    /// The child half: never a test of its own. A top-level run
    /// lists it `ignored`; the parents above run it with `--ignored --exact`
    /// and [`ARMED_CHILD_ENV`] naming the body; reached without that
    /// variable it FAILS by name rather than pass.
    #[test]
    #[ignore = "armed child entry: run only by this module's parent tests (--ignored --exact armed::child_entry)"]
    fn child_entry() {
        let mode = std::env::var(ARMED_CHILD_ENV).unwrap_or_else(|_| {
            panic!("armed::child_entry is run only by its parent tests, with {ARMED_CHILD_ENV} set")
        });
        match mode.as_str() {
            "transient-hold" => transient_hold_body(),
            "held-door" => held_door_body(),
            other => panic!("unknown armed child mode `{other}`"),
        }
    }

    /// This test binary, re-executed to run exactly `armed::child_entry`
    /// with capture on, the armed body named on the `Command`, and every
    /// seam variable removed so only the child's own `arm_pause` arms it.
    fn armed_child(mode: &str) -> Command {
        let mut cmd = Command::new(std::env::current_exe().expect("this test binary"));
        cmd.args(["--ignored", "--exact", "armed::child_entry", "--test-threads=1"])
            .env(ARMED_CHILD_ENV, mode)
            .env_remove(spine::fault::FAULT_ENV)
            .env_remove(spine::fault::PAUSE_ENV)
            .env_remove(spine::fault::PAUSE_LATCH_ENV)
            .env_remove(spine::fault::PAUSE_CEILING_ENV);
        cmd
    }

    /// The child ran exactly its one test and it passed; otherwise fail with
    /// everything the child said.
    fn assert_child_passed(out: &Output, mode: &str) {
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            out.status.success()
                && stdout.contains("running 1 test")
                && stdout.contains("test result: ok. 1 passed"),
            "the armed child `{mode}` did not run and pass its one test: {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
            out.status
        );
    }

    /// A held armed child, released (latch written) and killed if the parent
    /// unwinds, so a failing parent never leaves it waiting out its ceiling.
    struct HeldChild {
        child: Child,
        latch: PathBuf,
        reached: PathBuf,
    }

    impl HeldChild {
        fn new(child: Child, name: &str) -> HeldChild {
            // `arm_pause`'s latch path, computed for the CHILD's pid.
            let latch = latch_dir(name, child.id()).join("latch");
            let reached = spine::fault::reached_path(&latch);
            HeldChild { child, latch, reached }
        }

        fn release_and_wait(mut self) -> Output {
            std::fs::write(&self.latch, b"released by the parent").expect("write the latch");
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            use std::io::Read;
            if let Some(mut o) = self.child.stdout.take() {
                o.read_to_end(&mut stdout).expect("child stdout");
            }
            if let Some(mut e) = self.child.stderr.take() {
                e.read_to_end(&mut stderr).expect("child stderr");
            }
            let status = self.child.wait().expect("wait for the armed child");
            Output { status, stdout, stderr }
        }
    }

    impl Drop for HeldChild {
        fn drop(&mut self) {
            if std::thread::panicking() {
                let _ = std::fs::write(&self.latch, b"released on unwind");
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
        }
    }

    fn latch_dir(name: &str, pid: u32) -> PathBuf {
        test_scratch::tmp_path(format!("spine-fix-pause-{name}-{pid}"))
    }

    /// The seam's PAUSE form, armed in THIS process (the shape the
    /// concurrency adversary's file uses; copied rather than shared because
    /// that file is the adversary's).
    struct InProcessPause {
        stage: &'static str,
        latch_dir: PathBuf,
        latch: PathBuf,
        reached: PathBuf,
    }

    fn arm_pause(stage: &'static str, name: &str) -> InProcessPause {
        // THE STRUCTURAL BOUNDARY: arming sets process-global
        // variables every door in the process reads, so it happens only in
        // an armed child — never in the process the other tests share.
        assert!(
            std::env::var_os(ARMED_CHILD_ENV).is_some(),
            "arm_pause sets SPINE_PAUSE for the WHOLE process; it runs only in an armed child \
             (`armed_child`), never beside other tests"
        );
        let latch_dir = latch_dir(name, std::process::id());
        let _ = std::fs::remove_dir_all(&latch_dir);
        std::fs::create_dir_all(&latch_dir).expect("latch dir");
        let latch = latch_dir.join("latch");
        let reached = spine::fault::reached_path(&latch);
        std::env::remove_var(spine::fault::FAULT_ENV);
        std::env::remove_var(spine::fault::PAUSE_CEILING_ENV);
        std::env::set_var(spine::fault::PAUSE_LATCH_ENV, latch.as_os_str());
        std::env::set_var(spine::fault::PAUSE_ENV, stage);
        InProcessPause {
            stage,
            latch_dir,
            latch,
            reached,
        }
    }

    impl InProcessPause {
        fn wait_reached<T>(&self, held: &ScopedJoinHandle<'_, T>) {
            let started = Instant::now();
            while !self.reached.is_file() {
                assert!(
                    !held.is_finished(),
                    "the door returned before reaching `{}`; the pause never opened",
                    self.stage
                );
                assert!(
                    started.elapsed() < Duration::from_secs(20),
                    "the door never reached `{}`",
                    self.stage
                );
                std::thread::sleep(Duration::from_millis(2));
            }
            assert!(!held.is_finished(), "held after the marker");
        }

        fn disarm(&self) {
            std::env::remove_var(spine::fault::PAUSE_ENV);
        }

        fn release(&self) {
            std::fs::write(&self.latch, b"released").expect("write the latch");
        }
    }

    /// Writes the latch when dropped — on the scope body's unwind, BEFORE
    /// `thread::scope` joins the held door, so a failing assertion inside the
    /// scope surfaces as that assertion and not as the pause ceiling's abort.
    struct ReleaseOnUnwind<'a>(&'a InProcessPause);

    impl InProcessPause {
        fn release_on_unwind(&self) -> ReleaseOnUnwind<'_> {
            ReleaseOnUnwind(self)
        }
    }

    impl Drop for ReleaseOnUnwind<'_> {
        fn drop(&mut self) {
            if std::thread::panicking() {
                let _ = std::fs::write(&self.0.latch, b"released on unwind");
            }
        }
    }

    impl Drop for InProcessPause {
        fn drop(&mut self) {
            let _ = std::fs::write(&self.latch, b"released by drop");
            std::env::remove_var(spine::fault::PAUSE_ENV);
            std::env::remove_var(spine::fault::PAUSE_LATCH_ENV);
            std::env::remove_var(spine::fault::PAUSE_CEILING_ENV);
            let _ = std::fs::remove_dir_all(&self.latch_dir);
        }
    }
}
