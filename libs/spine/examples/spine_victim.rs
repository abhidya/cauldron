//! `spine_victim` — the process that DIES, or that HOLDS, so atomicity and
//! the held-open window can be proven by a REAL process rather than by a
//! simulated error return or a hand-driven thread (the kill form and the
//! pause form; the atomicity and window-invisibility families).
//!
//! The design allowed either "a helper that spawns the current test
//! binary" or "a dedicated example". This is the example, and it was chosen
//! for one reason: the seam must be AIMED at the statement under test, and
//! the four stages include `row-appended`, which fires on the FIRST
//! statement the process runs — the fixture setup. A dedicated victim can
//! disarm the seam for its setup and re-arm it for exactly the statement
//! under test; a test binary re-entered by name cannot, because its setup and
//! its payload run under one uninterruptible environment. It is also ONE
//! victim shared by every suite instead of one child body per suite.
//!
//! # Contract with the parent (`tests/util/kill.rs`)
//!
//! Environment in, exit status out. Nothing is written beside the database
//! files (the soup idiom, inherited): the victim's only channels are its exit
//! status and its two standard streams. (The pause form's `.reached` marker
//! is the LIBRARY'S, written under the feature to the path the parent named,
//! never inside the cauldron — `src/fault.rs`'s module docs say why that is
//! not a breach.)
//!
//! - `SPINE_VICTIM_DIR` — the cauldron directory. Required.
//! - `SPINE_VICTIM_JOB` — one of the jobs below.
//! - `SPINE_FAULT` — the stage to die at, exactly as the parent set it
//!   (`recover::Stage`'s frozen names).
//! - `SPINE_PAUSE` / `SPINE_PAUSE_LATCH` — the stage to HOLD at and the
//!   latch to wait for, exactly as the parent set them.
//!
//! **The disarm / re-arm.** The victim REMOVES BOTH seam variables across
//! its own setup and RESTORES them immediately before the statement under
//! test, so the kill or the pause lands inside THAT statement's commit path
//! and nowhere else. That is the whole reason this file exists; it is stated
//! loudly here because it is the one place a reader could mistake the seam
//! for something less aimed than it is.
//!
//! # Jobs
//!
//! | job | cauldron | statement under test |
//! |---|---|---|
//! | `cross-schema` | created here, one draft doc seeded | the two-part cross-schema statement |
//! | `single-part` | created here | a one-part CREATE |
//! | `repeatable` | created here | a two-part statement with nothing to stop a second landing |
//! | `resubmit`, `resubmit-repeatable` | OPENED | the same envelope again, from a fresh process |
//! | `connect` | OPENED if present, else created | `wrote` edge from an author to a fresh doc (the count-cell job) |
//! | `windowed` | OPENED if present, else created | the bulk cross-schema statement over every draft doc (the window job) |
//!
//! `connect` takes `SPINE_VICTIM_AUTHOR` (the author's node id; if unset an
//! author named `victim-author` is seeded with the seam disarmed) and
//! `SPINE_VICTIM_DOC_TITLE` (default `connected by the victim`). It prints
//! `outcome=committed doc=<id>`.
//!
//! Exit statuses, all distinct so the parent never guesses:
//!
//! | status | meaning |
//! |---|---|
//! | killed by SIGABRT | the seam fired: a REAL process death at the stage (or an unreleased pause hit its ceiling) |
//! | 0 | the job completed; if a pause was armed, it was reached and released |
//! | 2 | the victim was called wrong (bad job, missing directory) |
//! | 3 | the DOOR REFUSED — in a build without `fault-injection` this is the
//!      production seam refusing a set `SPINE_FAULT` or `SPINE_PAUSE` by name, out of process |
//! | 4 | the statement returned where a kill or a pause was expected (armed stage never reached) |
//! | 5 | anything else failed |
//!
//! Status 3 is load-bearing: it is how the production build proves — from
//! OUTSIDE the test process, where an abort would be visible as a signal and
//! a pause as a hang — that it carries neither path.

use spine::config::SpineConfig;
use spine::envelope::{Bindings, Envelope, Initiator, Instrument, Mode, Provenance};
use spine::error::SpineError;
use spine::schema_def::fixtures;
use spine::{Cauldron, Outcome};
use std::path::{Path, PathBuf};

const FAULT_ENV: &str = spine::fault::FAULT_ENV;
const PAUSE_ENV: &str = spine::fault::PAUSE_ENV;

const EXIT_USAGE: i32 = 2;
const EXIT_REFUSED: i32 = 3;
const EXIT_UNREACHED: i32 = 4;
const EXIT_FAILED: i32 = 5;
/// This victim was built under a different feature set than the parent test
/// binary. See [`check_build`].
const EXIT_WRONG_BUILD: i32 = 7;

/// The title of the doc the setup seeds, and the doc the cross-schema statement
/// matches. The parent asserts on it by name.
const SEED_TITLE: &str = "catalogued";

/// The cross-schema statement: match a `doc` in alpha
/// with an expectation, create a `ticket` in beta, create the cross-pool
/// `catalogues` edge, set the doc's state. ONE commit, TWO pools, TWO
/// grammars — and therefore TWO parts, which is what makes the
/// `between-landings` stage a real place rather than a name.
///
/// Its RETURN also asks for the schema-version STAMP on both sides — the
/// matched doc's and the created ticket's (visible on explicit
/// ask) — so the exactly-once suite can prove the stamp survives recovery
/// exactly as live. Nothing else reads these columns.
const CROSS_SCHEMA: &str = r#"MATCH (d: fixture-alpha/doc { state: "draft" })
   EXPECT COUNT(d) = 1
   CREATE (t: fixture-beta/ticket { state: "open" })
   CREATE (t) -[catalogues]-> (d)
   SET d.state = "published"
   RETURN t.id AS ticket, d.id AS doc, d.schema_version AS doc_stamp, t.schema_version AS ticket_stamp"#;

/// The same statement in BULK form, with no count expectation: every draft
/// doc in alpha is published and catalogued by one new ticket in beta. Alpha's
/// part is then one `SetField` PER DRAFT — several deltas in one part — which
/// is what lets the window suite exhibit (or fail to exhibit) a TORN per-pool
/// landing. Parts land in the frozen ascending-pool-name order, so alpha is
/// part 0, beta is part 1, and `between-landings` is exactly the window
/// between them. Byte-for-byte the statement `tests/window_invisible.rs`
/// drove by hand before the pause seam existed.
const WINDOWED: &str = r#"MATCH (d: fixture-alpha/doc { state: "draft" })
   CREATE (t: fixture-beta/ticket { state: "open" })
   CREATE (t) -[catalogues]-> (d)
   SET d.state = "published"
   RETURN t.id AS ticket"#;

/// The count-cell job's statement: connect a `wrote` edge from a named
/// author to a fresh doc. Its landing derives ONE increment of the author's
/// `docs_written` cell inside alpha's landing transaction, and
/// its claim set — read on the author's body, link on the author's `wrote`
/// slice, never a write on the author — is the count-cell carve-out.
const CONNECT: &str = r#"MATCH (a: fixture-alpha/author { id: $a })
   CREATE (d: fixture-alpha/doc { title: $t, state: "draft" })
   CREATE (a) -[wrote]-> (d)
   RETURN d.id"#;

/// A ONE-part statement, for proving the seam is aimed and not blunt: it can
/// never reach `between-landings`, because that stage fires only before a
/// SECOND part.
const SINGLE_PART: &str = r#"CREATE (d: fixture-alpha/doc { title: "single", state: "draft" })
   RETURN d.id"#;

/// A TWO-part statement that would land AGAIN if it were submitted again —
/// no `MATCH`, no `EXPECT`, so nothing about the statement itself stops a
/// second landing. Exactly-once is the ONLY thing standing between one
/// submission and two.
///
/// This exists because the red sweep found that [`CROSS_SCHEMA`] cannot be
/// used to test "never lands twice": its own `EXPECT COUNT(d) = 1` over draft
/// docs fails on a second run, so the test passed even with all three dedup
/// belts removed. A checker that cannot be forced red does not count as
/// existing, so the family got a statement that can be.
const REPEATABLE: &str = r#"CREATE (d: fixture-alpha/doc { title: "repeatable", state: "draft" })
   CREATE (t: fixture-beta/ticket { state: "open" })
   CREATE (t) -[catalogues]-> (d)
   RETURN d.id AS doc, t.id AS ticket, d.schema_version AS doc_stamp, t.schema_version AS ticket_stamp"#;

/// What the parent armed, taken off the environment for the duration of the
/// setup and put back for the statement under test.
struct Armed {
    fault: Option<String>,
    pause: Option<String>,
}

fn main() {
    let Ok(dir) = std::env::var("SPINE_VICTIM_DIR") else {
        die(EXIT_USAGE, "SPINE_VICTIM_DIR names the cauldron directory");
    };
    let dir = PathBuf::from(dir);
    let job = std::env::var("SPINE_VICTIM_JOB").unwrap_or_else(|_| "cross-schema".to_string());
    check_build();

    // THE DISARM. Everything from here to `rearm()` runs with BOTH forms of
    // the seam unarmed, whatever the parent set, so no setup statement can be
    // the one that dies or holds. The latch path is not a trigger and stays.
    let armed = Armed {
        fault: std::env::var(FAULT_ENV).ok(),
        pause: std::env::var(PAUSE_ENV).ok(),
    };
    std::env::remove_var(FAULT_ENV);
    std::env::remove_var(PAUSE_ENV);

    match job.as_str() {
        "cross-schema" => cross_schema(&dir, armed),
        "single-part" => single_part(&dir, armed),
        "repeatable" => {
            let c = create(&dir);
            rearm(&armed);
            submit_and_report(&c, envelope(REPEATABLE), &armed)
        }
        "resubmit" => resubmit(&dir, armed, CROSS_SCHEMA),
        "resubmit-repeatable" => resubmit(&dir, armed, REPEATABLE),
        "connect" => connect(&dir, armed),
        "windowed" => windowed(&dir, armed),
        "tranche" => tranche::run(&dir, armed),
        other => die(
            EXIT_USAGE,
            &format!(
                "`{other}` is not a victim job (cross-schema, single-part, repeatable, \
                 resubmit, resubmit-repeatable, connect, windowed, tranche)"
            ),
        ),
    }
}

/// THE TRANCHE JOB (tranche tests 27–32):
/// the process that dies at a named point of a TRANCHE's life, so that the
/// healing `Cauldron::open` in the parent — its ordinary pass, its tranche
/// pass, its first `tranche_tick` — is proven against a real process death
/// and a real record on disk. The parent is `tests/tranche/crash.rs`.
///
/// `SPINE_VICTIM_JOB=tranche`, with:
///
/// | variable | meaning |
/// |---|---|
/// | `SPINE_VICTIM_TRANCHE` | the scenario: `queued`, `staging`, `commit`, `rollback`, `draining` |
/// | `SPINE_VICTIM_MODE` | `touched` (default) or `whole` |
/// | `SPINE_VICTIM_CHILDREN` | `k`, how many children the scenario stages (default 2) |
/// | `SPINE_VICTIM_FOREIGN` | `parked`: a foreign granted writer row is parked (test-support) before the tranche acts, so recovery has both kinds of row to classify |
/// | `SPINE_VICTIM_TIMEOUT_MS` | the tranche heartbeat timeout (`heartbeat_interval` = a third of it) |
/// | `SPINE_VICTIM_CEILING_MS` | `lock_wait_ceiling` |
/// | `SPINE_VICTIM_NOW_MS` | the daemon clock every act is given (default: the wall) |
///
/// Every scenario CREATES the cauldron (alpha + beta fixtures), seeds an
/// author and a draft doc with the seam disarmed, runs its unarmed acts,
/// prints what the parent needs as `victim: key=value` lines (one per line,
/// flushed — a later abort loses nothing), RE-ARMS, and runs the one act
/// the kill is aimed at:
///
/// - `queued`: `T0` begins (`touched`, opens inline); then `T1` begins →
///   `after-begin` dies with `T1` queued at position 2 (test 27);
/// - `staging`: `T` begins in `MODE`; children `0..k-1` stage unarmed;
///   child `k-1` stages armed → `after-stage` dies with `staged = k`
///   (tests 28, 32; `MODE=whole` is the gate-up-at-boot shape);
/// - `commit`: `T` begins, `k` children stage unarmed; armed, `commit` runs
///   on a thread while this thread ticks → the LANDER dies at any of its
///   four stages or the two child-landing stages (tests 29, 30);
/// - `rollback`: `T` begins, `k` children stage unarmed; armed, `rollback`
///   → `in-rollback` dies inside the rollback's transaction (test 31);
/// - `draining`: the beta pool file is held; a keyless two-part writer `W`
///   (alpha, then beta) is submitted on a thread and blocks on beta after
///   its alpha part landed; `T1` begins in `MODE` (`whole` → `queued,
///   draining`); the hold is released and `W` dies at the DOOR stage the
///   parent armed (`before-cleared`), its row decided and un-cleared with
///   its lock rows held.
///
/// Children alternate two shapes so a landing crosses both pools: even
/// ordinals connect a `wrote` edge from the author to a fresh doc (ONE part;
/// the author's `docs_written` count cell is what a part applied twice would
/// double — test 30's red); odd ordinals catalogue the previous child's doc
/// from a fresh beta ticket and publish it (TWO parts, alpha then beta —
/// `between-child-parts` is a real place).
mod tranche {
    use super::*;
    use spine::envelope::{RippleKind, TxHash};
    use spine::locks::{self, Claim, ClaimSet, LockMode, Resource};
    use spine::pool::{NodeId, Pool};
    use spine::schema_def::PoolName;
    use spine::spine_db::{SpineDb, SPINE_FILE};
    use spine::tranche::{ChildOutcome, QuietLevels, TrancheActor, TrancheBegin, TrancheLockMode};
    use std::time::{Duration, Instant};

    const QUIET: QuietLevels = QuietLevels::BusNotConfigured;
    const SERVICE: &str = "runner";
    const ALPHA: &str = "fixture-alpha";
    const BETA: &str = "fixture-beta";
    /// The keys the parent knows: `T0` = key 1, `T1`/`T` = key 2.
    pub const KEY_T0: &str = "0101010101010101010101010101010101010101010101010101010101010101";
    pub const KEY_T: &str = "0202020202020202020202020202020202020202020202020202020202020202";

    const WROTE: &str = r#"MATCH (a: fixture-alpha/author { id: $a })
   CREATE (d: fixture-alpha/doc { title: $t, state: "draft" })
   CREATE (a) -[wrote]-> (d)
   RETURN d.id"#;

    const CATALOGUE: &str = r#"MATCH (d: fixture-alpha/doc { id: $d })
   CREATE (t: fixture-beta/ticket { state: "open" })
   CREATE (t) -[catalogues]-> (d)
   SET d.state = "published"
   RETURN t.id"#;

    fn var(k: &str) -> Option<String> {
        std::env::var(format!("SPINE_VICTIM_{k}")).ok().filter(|v| !v.is_empty())
    }

    fn say(line: &str) {
        println!("victim: {line}");
    }

    fn config() -> SpineConfig {
        let mut cfg = SpineConfig::default();
        cfg.busy_timeout = Duration::from_secs(5);
        if let Some(ms) = var("TIMEOUT_MS") {
            let t: u64 = ms.parse().unwrap_or_else(|_| die(EXIT_USAGE, "SPINE_VICTIM_TIMEOUT_MS is not a number"));
            cfg.tranche.timeout = Duration::from_millis(t);
            cfg.tranche.heartbeat_interval = Duration::from_millis(t / 3);
        }
        if let Some(ms) = var("CEILING_MS") {
            let t: u64 = ms.parse().unwrap_or_else(|_| die(EXIT_USAGE, "SPINE_VICTIM_CEILING_MS is not a number"));
            cfg.lock_wait_ceiling = Duration::from_millis(t);
        }
        cfg
    }

    fn now_ms() -> i64 {
        match var("NOW_MS") {
            Some(ms) => ms.parse().unwrap_or_else(|_| die(EXIT_USAGE, "SPINE_VICTIM_NOW_MS is not a number")),
            None => std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0),
        }
    }

    fn mode() -> TrancheLockMode {
        match var("MODE").as_deref() {
            None | Some("touched") => TrancheLockMode::Touched,
            Some("whole") => TrancheLockMode::Whole,
            Some(other) => die(EXIT_USAGE, &format!("SPINE_VICTIM_MODE={other} is neither touched nor whole")),
        }
    }

    fn children() -> u32 {
        var("CHILDREN").map(|k| k.parse().unwrap_or_else(|_| die(EXIT_USAGE, "SPINE_VICTIM_CHILDREN is not a number"))).unwrap_or(2)
    }

    fn actor(t: &str, key: &str) -> TrancheActor {
        TrancheActor {
            tranche: t.into(),
            key: key.into(),
            holder_service: SERVICE.into(),
        }
    }

    fn begin_req(t: &str, key: &str, mode: TrancheLockMode) -> TrancheBegin {
        TrancheBegin {
            tranche: t.into(),
            key: key.into(),
            holder_service: SERVICE.into(),
            instrument: Instrument {
                name: "runner".into(),
                version: "test-build".into(),
            },
            lock_mode: mode,
            regions: vec![ALPHA.into()],
        }
    }

    /// A type-one child under run `T`, nonce = the step.
    fn child(step: &str, statement: &str, params: &[(&str, serde_json::Value)]) -> Envelope {
        let mut e = envelope_with(statement, params);
        e.provenance.initiator = Initiator::Ripple {
            kind: RippleKind::TypeOne,
            run_id: "T".into(),
            step_id: step.into(),
            scope: vec![],
        };
        e.provenance.instrument = Instrument {
            name: "runner".into(),
            version: "test-build".into(),
        };
        e.idempotency_nonce = Some(step.into());
        e
    }

    /// The statement and params of child `i`, given the doc the previous
    /// child minted.
    fn child_i(i: u32, author: &str, prev_doc: Option<&str>) -> Envelope {
        if i % 2 == 0 {
            child(
                &format!("step-{i}"),
                WROTE,
                &[("a", serde_json::json!(author)), ("t", serde_json::json!(format!("child {i}")))],
            )
        } else {
            let d = prev_doc.unwrap_or_else(|| die(EXIT_FAILED, "an odd child needs the previous child's doc"));
            child(&format!("step-{i}"), CATALOGUE, &[("d", serde_json::json!(d))])
        }
    }

    /// What the parent asserts a refused or completed tranche act as: the
    /// production build's by-name refusal (exit 3), any other error (3), a
    /// completion where a kill was armed (4), or a plain completion (0).
    fn finish(result: Result<String, SpineError>, armed: &Armed) -> ! {
        match result {
            Ok(what) => {
                say(&format!("outcome=completed what={what}"));
                println!("outcome=completed");
                if armed.fault.is_some() {
                    eprintln!("spine_victim: a kill was armed and the tranche act RETURNED; no stage aborted this process");
                    std::process::exit(EXIT_UNREACHED);
                }
                std::process::exit(0);
            }
            Err(SpineError::FaultSeamRefused { variable, value }) => {
                println!("outcome=error kind=fault_seam_refused variable={variable} value={value}");
                std::process::exit(EXIT_REFUSED);
            }
            Err(e) => {
                println!("outcome=error kind={}", e.kind());
                eprintln!("spine_victim: {}", e.message());
                std::process::exit(EXIT_REFUSED);
            }
        }
    }

    fn stage_child(c: &Cauldron, a: &TrancheActor, i: u32, author: &str, prev_doc: Option<&str>) -> Result<Option<String>, SpineError> {
        match c.tranche_stage(a, child_i(i, author, prev_doc), now_ms())? {
            ChildOutcome::Staged { ordinal, minted, .. } => {
                // `d` for an even child, `t` for an odd one; the parent reads
                // `child<i>=<pool>/<id>`.
                let key = if i % 2 == 0 { "d" } else { "t" };
                let id = minted.get(key).cloned().unwrap_or_default();
                say(&format!("child{ordinal}={id}"));
                Ok(if i % 2 == 0 { id.split_once('/').map(|(_, id)| id.to_string()) } else { prev_doc.map(str::to_string) })
            }
            other => die(EXIT_FAILED, &format!("child {i} did not stage: {other:?}")),
        }
    }

    /// PARK a foreign granted writer (test-support): a real pending row under
    /// a real `seq` with a granted `write` on the seeded doc's body — the
    /// shape `tests/tranche/locks.rs` parks. Recovery's ordinary pass finds
    /// it un-decided and aborts it, releasing its locks (the ordinary
    /// pass). Parked BEFORE the tranche begins (under `whole` the gate
    /// would refuse its grant; under `touched` it coexists either way).
    fn park_foreign(dir: &Path, doc: &str, cfg: &SpineConfig) -> i64 {
        let mut spine = SpineDb::open(&dir.join(SPINE_FILE)).unwrap_or_else(|e| die(EXIT_FAILED, &format!("spine: {e}")));
        let env = envelope_with("-- parked foreign writer", &[]);
        let hash = env.hash_identity().unwrap_or_else(|e| die(EXIT_FAILED, &format!("hash: {e}")));
        let seq = spine
            .append_pending(&env, &hash, &spine::delta::CompiledPayload { parts: vec![], minted: Default::default() })
            .unwrap_or_else(|e| die(EXIT_FAILED, &format!("park: {e}")));
        let mut set = ClaimSet::new();
        set.insert(Claim {
            pool: PoolName::new(ALPHA),
            resource: Resource::Body { node: NodeId(doc.to_string()) },
            mode: LockMode::Write,
        });
        locks::acquire(&spine, seq, &set, cfg).unwrap_or_else(|e| die(EXIT_FAILED, &format!("park grant: {e}")));
        seq.0
    }

    fn touched_json(c: &Cauldron, t: &str) -> String {
        let rows = c.tranche_touched(t, 1).unwrap_or_else(|e| die(EXIT_FAILED, &format!("touched: {e}")));
        serde_json::to_string(&rows).unwrap_or_default()
    }

    pub fn run(dir: &Path, armed: Armed) -> ! {
        let scenario = var("TRANCHE").unwrap_or_else(|| die(EXIT_USAGE, "SPINE_VICTIM_TRANCHE names the scenario"));
        let cfg = config();
        let _ = std::fs::remove_dir_all(dir);
        let c = Cauldron::create(dir, &[fixtures::fixture_alpha(), fixtures::fixture_beta()], cfg.clone())
            .unwrap_or_else(|e| die(EXIT_FAILED, &format!("cannot create the cauldron: {e}")));
        let author = mint(&c, r#"CREATE (a: fixture-alpha/author { name: "victim-author" }) RETURN a.id"#, &[], "a.id");
        say(&format!("author={author}"));
        // The seeded draft doc (the one the cross-schema statement matches, for W).
        let seeded = mint(
            &c,
            &format!(r#"CREATE (d: fixture-alpha/doc {{ title: "{SEED_TITLE}", state: "draft" }}) RETURN d.id"#),
            &[],
            "d.id",
        );
        say(&format!("seed_doc={seeded}"));
        if var("FOREIGN").as_deref() == Some("parked") {
            let seq = park_foreign(dir, &seeded, &cfg);
            say(&format!("foreign_seq={seq}"));
        }
        let k = children();
        match scenario.as_str() {
            "queued" => {
                let b0 = c.tranche_begin(&begin_req("T0", KEY_T0, TrancheLockMode::Touched), &QUIET, now_ms())
                    .unwrap_or_else(|e| die(EXIT_FAILED, &format!("begin T0: {e}")));
                say(&format!("t0_state={}", b0.state.as_str()));
                rearm(&armed);
                finish(
                    c.tranche_begin(&begin_req("T1", KEY_T, mode()), &QUIET, now_ms())
                        .map(|b| format!("begin T1 state={} position={} draining={}", b.state.as_str(), b.position, b.draining)),
                    &armed,
                );
            }
            "staging" => {
                let b = c.tranche_begin(&begin_req("T", KEY_T, mode()), &QUIET, now_ms())
                    .unwrap_or_else(|e| die(EXIT_FAILED, &format!("begin T: {e}")));
                say(&format!("begun state={} position={} draining={}", b.state.as_str(), b.position, b.draining));
                let a = actor("T", KEY_T);
                let mut prev: Option<String> = None;
                for i in 0..k.saturating_sub(1) {
                    prev = stage_child(&c, &a, i, &author, prev.as_deref()).unwrap_or_else(|e| die(EXIT_FAILED, &format!("child {i}: {e}")));
                }
                say(&format!("touched_before={}", touched_json(&c, "T")));
                rearm(&armed);
                let last = k.saturating_sub(1);
                finish(
                    stage_child(&c, &a, last, &author, prev.as_deref()).map(|_| format!("child {last} staged")),
                    &armed,
                );
            }
            "commit" => {
                let b = c.tranche_begin(&begin_req("T", KEY_T, mode()), &QUIET, now_ms())
                    .unwrap_or_else(|e| die(EXIT_FAILED, &format!("begin T: {e}")));
                say(&format!("begun state={} position={} draining={}", b.state.as_str(), b.position, b.draining));
                let a = actor("T", KEY_T);
                let mut prev: Option<String> = None;
                for i in 0..k {
                    prev = stage_child(&c, &a, i, &author, prev.as_deref()).unwrap_or_else(|e| die(EXIT_FAILED, &format!("child {i}: {e}")));
                }
                say(&format!("touched_before={}", touched_json(&c, "T")));
                rearm(&armed);
                // THE LANDING IS THE TICK'S: `commit` flips and joins
                // on a thread; this thread ticks until the join returns or a
                // seam kills the process.
                let (c2, a2) = (c.clone(), a.clone());
                let h = std::thread::spawn(move || c2.tranche_commit(&a2, now_ms()));
                let started = Instant::now();
                while !h.is_finished() {
                    if let Err(e) = c.tranche_tick(&QUIET, now_ms()) {
                        if !matches!(e, SpineError::Busy { .. }) {
                            finish(Err(e), &armed);
                        }
                    }
                    if started.elapsed() > Duration::from_secs(60) {
                        die(EXIT_FAILED, "the landing never finished within 60 s");
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                let r = h.join().unwrap_or_else(|_| die(EXIT_FAILED, "the commit thread panicked"));
                finish(r.map(|c| format!("landed children={}", c.children.len())), &armed);
            }
            "rollback" => {
                let b = c.tranche_begin(&begin_req("T", KEY_T, mode()), &QUIET, now_ms())
                    .unwrap_or_else(|e| die(EXIT_FAILED, &format!("begin T: {e}")));
                say(&format!("begun state={} position={} draining={}", b.state.as_str(), b.position, b.draining));
                let a = actor("T", KEY_T);
                let mut prev: Option<String> = None;
                for i in 0..k {
                    prev = stage_child(&c, &a, i, &author, prev.as_deref()).unwrap_or_else(|e| die(EXIT_FAILED, &format!("child {i}: {e}")));
                }
                say(&format!("touched_before={}", touched_json(&c, "T")));
                rearm(&armed);
                finish(c.tranche_rollback(&a, now_ms()).map(|r| format!("rolled back stored={}", r.stored)), &armed);
            }
            "draining" => {
                // HOLD BETA: `BEGIN IMMEDIATE` on the pool file from a side
                // connection. W's alpha part lands; its beta part waits its
                // turn up to `lock_wait_ceiling`.
                let hold = rusqlite::Connection::open(Pool::path_in(dir, &PoolName::new(BETA)))
                    .unwrap_or_else(|e| die(EXIT_FAILED, &format!("open beta: {e}")));
                hold.execute_batch("BEGIN IMMEDIATE").unwrap_or_else(|e| die(EXIT_FAILED, &format!("hold beta: {e}")));
                // W is ARMED from the start: its door passes the gate (a real
                // stage name) and every stage but the parent's — which it
                // reaches only after this thread released the hold.
                rearm(&armed);
                let w_env = envelope(CROSS_SCHEMA);
                let w_hash: TxHash = w_env.hash_identity().unwrap_or_else(|e| die(EXIT_FAILED, &format!("hash: {e}")));
                let c2 = c.clone();
                let w = std::thread::spawn(move || c2.submit(w_env));
                // Wait until W's row is DECIDED and its alpha part landed —
                // W is granted and inside its landing: a writer that
                // holds a granted write claim when the gate rises is landing.
                let started = Instant::now();
                loop {
                    if w.is_finished() {
                        // The production build: W's door refused the armed
                        // seam by name before any row. Report it.
                        let r = w.join().unwrap_or_else(|_| die(EXIT_FAILED, "W panicked"));
                        finish(r.map(|_| "W landed before T1 began".to_string()), &armed);
                    }
                    let row = c.transaction_by_hash(&w_hash).unwrap_or_else(|e| die(EXIT_FAILED, &format!("row: {e}")));
                    if let Some(row) = row {
                        if row.decided && row.parts.first().map(|p| p.landed).unwrap_or(false) {
                            say(&format!("w_seq={} w_decided=1 w_alpha_landed=1", row.seq.0));
                            break;
                        }
                    }
                    if started.elapsed() > Duration::from_secs(20) {
                        die(EXIT_FAILED, "W never reached its beta part within 20 s");
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                let b = c.tranche_begin(&begin_req("T1", KEY_T, mode()), &QUIET, now_ms())
                    .unwrap_or_else(|e| die(EXIT_FAILED, &format!("begin T1: {e}")));
                say(&format!("begun state={} position={} draining={}", b.state.as_str(), b.position, b.draining));
                // Release the hold: W lands beta and dies at the parent's stage.
                hold.execute_batch("COMMIT").unwrap_or_else(|e| die(EXIT_FAILED, &format!("release beta: {e}")));
                let started = Instant::now();
                while !w.is_finished() {
                    if started.elapsed() > Duration::from_secs(30) {
                        die(EXIT_FAILED, "W never finished within 30 s of the release");
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                let r = w.join().unwrap_or_else(|_| die(EXIT_FAILED, "W panicked"));
                finish(r.map(|_| "W landed".to_string()), &armed);
            }
            other => die(EXIT_USAGE, &format!("SPINE_VICTIM_TRANCHE={other} is not a scenario (queued, staging, commit, rollback, draining)")),
        }
    }
}

/// Create the cauldron, seed one draft doc, then run the cross-schema
/// statement with the seam armed. Expected to be killed at every one of the
/// four stages.
fn cross_schema(dir: &Path, armed: Armed) -> ! {
    let c = create(dir);
    seed(&c);
    rearm(&armed);
    submit_and_report(&c, envelope(CROSS_SCHEMA), &armed)
}

/// Create the cauldron and run a ONE-part statement with the seam armed.
/// Under `between-landings` this must complete normally (exit 0): the stage
/// names a window that a single-part commit never enters.
fn single_part(dir: &Path, armed: Armed) -> ! {
    let c = create(dir);
    rearm(&armed);
    submit_and_report(&c, envelope(SINGLE_PART), &armed)
}

/// THE COUNT-CELL JOB. Open the parent's cauldron (or create one
/// and seed an author, when run alone), then connect a `wrote` edge from the
/// named author to a fresh doc with the seam armed. Under
/// `SPINE_PAUSE=decided` this process holds a DECIDED, un-landed transaction
/// whose snapshot saw the author's cell at some value — and the parent lands
/// its own connect to the same author in that window. What this process
/// then writes to the cell is the whole question the count-cell rule asks.
fn connect(dir: &Path, armed: Armed) -> ! {
    let c = open_or_create(dir);
    let author = match std::env::var("SPINE_VICTIM_AUTHOR") {
        Ok(id) if !id.is_empty() => id,
        _ => mint(
            &c,
            r#"CREATE (a: fixture-alpha/author { name: "victim-author" }) RETURN a.id"#,
            &[],
            "a.id",
        ),
    };
    let title = std::env::var("SPINE_VICTIM_DOC_TITLE")
        .unwrap_or_else(|_| "connected by the victim".to_string());
    rearm(&armed);
    let env = envelope_with(
        CONNECT,
        &[
            ("a", serde_json::json!(author)),
            ("t", serde_json::json!(title)),
        ],
    );
    submit_and_report(&c, env, &armed)
}

/// THE WINDOW JOB. Open the parent's cauldron (which the parent
/// seeded with its draft docs; when run alone, create one and seed a single
/// draft), then run the bulk cross-schema statement with the seam armed.
/// Under `SPINE_PAUSE=between-landings` this process stands in the
/// half-landed window — alpha landed, beta not, every lock held, the row
/// decided and un-cleared — for as long as the parent wants it held. It is
/// the real pipeline's window, produced by `Cauldron::submit` itself.
fn windowed(dir: &Path, armed: Armed) -> ! {
    let c = if dir.join(spine::spine_db::SPINE_FILE).is_file() {
        open(dir)
    } else {
        let c = create(dir);
        seed(&c);
        c
    };
    rearm(&armed);
    submit_and_report(&c, envelope(WINDOWED), &armed)
}

/// OPEN an existing cauldron and submit the cross-schema envelope AGAIN, from a
/// process that shares no memory with whoever landed it. Exactly-once must
/// be answered from the spine ROW alone, and this is the
/// strongest form of "alone" available: a fresh process, and a pool state
/// that has since moved so far that re-executing the statement would refuse.
///
/// Prints one machine-readable line to stdout:
/// `outcome=committed deduplicated=<bool> seq=<n> hash=<hex>`.
fn resubmit(dir: &Path, armed: Armed, statement: &str) -> ! {
    let c = open(dir);
    rearm(&armed);
    let env = envelope(statement);
    let hash = match env.hash_identity() {
        Ok(h) => h,
        Err(e) => die(EXIT_FAILED, &format!("hash: {e}")),
    };
    match c.submit(env) {
        Ok(Outcome::Committed {
            tx,
            seq,
            deduplicated,
            ..
        }) => {
            println!(
                "outcome=committed deduplicated={deduplicated} seq={} hash={}",
                seq.0,
                tx.to_hex()
            );
            std::process::exit(0);
        }
        Ok(Outcome::Validated { .. }) => die(EXIT_FAILED, "a commit envelope came back Validated"),
        Err(e) => {
            println!(
                "outcome=error kind={} hash={} message={}",
                e.kind(),
                hash.to_hex(),
                e.message()
            );
            std::process::exit(EXIT_REFUSED);
        }
    }
}

// ---------------------------------------------------------------------------
// plumbing
// ---------------------------------------------------------------------------

/// GUARD AGAINST A STALE VICTIM, and it is not paranoia — it is a real trap
/// that bit this suite once.
///
/// Cargo "uplifts" an example to `target/<profile>/examples/spine_victim`,
/// ONE path shared by every feature configuration, and it only rebuilds
/// examples when the test run is not narrowed with `--test <name>`. So
/// `cargo test -p spine --features fault-injection --test atomicity_kill`
/// followed by `cargo test -p spine --test atomicity_kill` leaves the ARMED
/// victim sitting at the path the production-build test spawns — and that
/// test would then watch a child abort and conclude the production build has
/// an abort path. A false RED there is survivable; the same staleness in the
/// other direction is a false GREEN, and neither is acceptable in the suite
/// whose whole job is to prove which build carries the seam.
///
/// The parent states which build it is, the victim compares, and a mismatch
/// exits [`EXIT_WRONG_BUILD`] with the remedy in the message.
fn check_build() {
    let Ok(want) = std::env::var("SPINE_VICTIM_EXPECT_FEATURE") else {
        return;
    };
    let have = cfg!(feature = "fault-injection");
    if (want == "1") != have {
        die(
            EXIT_WRONG_BUILD,
            &format!(
                "this `spine_victim` was built with fault-injection={have}, but the parent test \
                 binary is fault-injection={want}. Cargo uplifts every configuration of an \
                 example to ONE path and only rebuilds examples when the run is not narrowed \
                 with `--test <name>`. Re-run without the `--test` filter (`cargo test -p spine \
                 [--features fault-injection]`) so the example is rebuilt in this configuration."
            ),
        );
    }
}

/// THE RE-ARM. From this call the seam — whichever form the parent armed —
/// is live for exactly the statement the caller is about to submit.
fn rearm(armed: &Armed) {
    if let Some(stage) = &armed.fault {
        eprintln!("spine_victim: arming {FAULT_ENV}={stage}");
        std::env::set_var(FAULT_ENV, stage);
    }
    if let Some(stage) = &armed.pause {
        eprintln!("spine_victim: arming {PAUSE_ENV}={stage}");
        std::env::set_var(PAUSE_ENV, stage);
    }
}

fn create(dir: &Path) -> Cauldron {
    let _ = std::fs::remove_dir_all(dir);
    match Cauldron::create(
        dir,
        &[fixtures::fixture_alpha(), fixtures::fixture_beta()],
        SpineConfig::default(),
    ) {
        Ok(c) => c,
        Err(e) => die(EXIT_FAILED, &format!("cannot create the cauldron: {e}")),
    }
}

/// The victim opens WITHOUT the recovery pass `Cauldron::open` runs: a second victim opening beside a paused sibling would
/// otherwise finish the sibling's held window before the parent could act
/// in it, and a resubmitting victim would heal the row the parent is about
/// to recover by hand. The victim is the harness's child and its job is to
/// hold and to die in windows, never to heal them.
fn open(dir: &Path) -> Cauldron {
    match Cauldron::open_unrecovered(dir, SpineConfig::default()) {
        Ok(c) => c,
        Err(e) => die(EXIT_FAILED, &format!("cannot open the cauldron: {e}")),
    }
}

/// The parent's cauldron if there is one, else a fresh one — so the jobs
/// that share a cauldron with the parent can also be run alone.
fn open_or_create(dir: &Path) -> Cauldron {
    if dir.join(spine::spine_db::SPINE_FILE).is_file() {
        open(dir)
    } else {
        create(dir)
    }
}

fn seed(c: &Cauldron) {
    let stmt = format!(
        r#"CREATE (d: fixture-alpha/doc {{ title: "{SEED_TITLE}", state: "draft" }}) RETURN d.id"#
    );
    if let Err(e) = c.submit(envelope(&stmt)) {
        die(EXIT_FAILED, &format!("cannot seed the fixture doc: {e}"));
    }
}

/// Commit a minting statement and hand back the id it RETURNed under `key`.
fn mint(c: &Cauldron, statement: &str, params: &[(&str, serde_json::Value)], key: &str) -> String {
    match c.submit(envelope_with(statement, params)) {
        Ok(Outcome::Committed { returned, .. }) => returned
            .0
            .first()
            .and_then(|r| r.get(key))
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| die(EXIT_FAILED, &format!("the mint returned no `{key}`"))),
        Ok(other) => die(EXIT_FAILED, &format!("the mint came back {other:?}")),
        Err(e) => die(EXIT_FAILED, &format!("cannot mint: {e}")),
    }
}

/// Submit, and report what happened in the vocabulary the parent asserts on.
/// Reaching the END of this function at all is a result: it means the armed
/// stage did NOT abort the process — which in a build without the feature is
/// exactly the proof the production seam owes, and which under an armed
/// PAUSE means the pause was reached and released (exit 0) or never reached
/// at all (exit 4, told apart by the library's own `.reached` marker).
fn submit_and_report(c: &Cauldron, env: Envelope, armed: &Armed) -> ! {
    match c.submit(env) {
        Ok(outcome) => {
            let dedup = matches!(
                outcome,
                Outcome::Committed {
                    deduplicated: true,
                    ..
                }
            );
            let doc = match &outcome {
                Outcome::Committed { returned, .. } => returned
                    .0
                    .first()
                    .and_then(|r| r.get("d.id"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                _ => None,
            };
            match doc {
                Some(id) => println!("outcome=committed deduplicated={dedup} doc={id}"),
                None => println!("outcome=committed deduplicated={dedup}"),
            }
            eprintln!("spine_victim: the statement RETURNED; no stage aborted this process");
            // If nothing was armed this is the ordinary success path. If a
            // KILL stage was armed and the process still got here, the
            // parent needs to know which, so the two are distinct statuses.
            if armed.fault.is_some() {
                std::process::exit(EXIT_UNREACHED);
            }
            // If a PAUSE stage was armed: reached-and-released is the
            // ordinary success path; never reached is distinct, and the
            // library's own marker is the evidence.
            if armed.pause.is_some() {
                let reached = std::env::var(spine::fault::PAUSE_LATCH_ENV)
                    .map(|p| spine::fault::reached_path(Path::new(&p)).is_file())
                    .unwrap_or(false);
                if !reached {
                    eprintln!("spine_victim: a pause was armed and its stage was never reached");
                    std::process::exit(EXIT_UNREACHED);
                }
            }
            std::process::exit(0);
        }
        Err(SpineError::FaultSeamRefused { variable, value }) => {
            // The production build, refusing a set seam variable BY NAME —
            // `SPINE_FAULT` (the kill form) or `SPINE_PAUSE` (the pause form),
            // one variant naming which — out of process. No abort
            // path and no pause path ran; the door turned the ask away.
            println!("outcome=error kind=fault_seam_refused variable={variable} value={value}");
            std::process::exit(EXIT_REFUSED);
        }
        Err(e) => {
            println!("outcome=error kind={}", e.kind());
            eprintln!("spine_victim: {}", e.message());
            std::process::exit(EXIT_REFUSED);
        }
    }
}

/// The victim's envelope shape, FROZEN: the parent's `resubmit` job must
/// produce byte-identical provenance or it would be a different hash
/// identity, and the whole exactly-once test would prove nothing.
fn envelope(statement: &str) -> Envelope {
    envelope_with(statement, &[])
}

fn envelope_with(statement: &str, params: &[(&str, serde_json::Value)]) -> Envelope {
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
                thread: "spine-victim".into(),
                dispatch: "spine-victim".into(),
            },
            instrument: Instrument::door(),
        },
        mode: Mode::Commit,
        idempotency_nonce: None,
    }
}

fn die(code: i32, message: &str) -> ! {
    eprintln!("spine_victim: {message}");
    std::process::exit(code);
}
