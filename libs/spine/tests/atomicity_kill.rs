//! ATOMICITY ACROSS POOLS, proven by killing the process.
//!
//! Every kill in this file is a REAL PROCESS DEATH. `examples/spine_victim.rs`
//! runs the cross-schema statement in a CHILD PROCESS with
//! `SPINE_FAULT=<stage>` set; the seam calls `std::process::abort()`; the
//! parent asserts SIGABRT with no exit code and then reads what the corpse
//! left on disk. Nothing here simulates a crash with an error return — the
//! half-landed window on disk is the thing under test, and an error return
//! would unwind past it.
//!
//! # What this file adds over `door_smoke.rs`
//!
//! `door_smoke.rs` already kills a child at ONE stage (`between-landings`)
//! and proves roll-forward from it. This suite proves the cases that one
//! cannot: **all four** named stages, the atomicity invariant asserted uniformly at
//! each (everything landed or nothing, and the ROW tells which), the
//! committed-commit counter's behaviour across the window, the seam's
//! AIMEDNESS (a one-part commit never reaches `between-landings`), the fact
//! that a killed transaction's LOCKS survive it durably and are released only
//! by recovery's cleared flip, and the production build's seam-absence proof
//! taken both in process and OUT of process.
//!
//! # And the seam's PAUSE form
//!
//! The last section proves the pause form (`SPINE_PAUSE`, `src/fault.rs`)
//! under the same two hard conditions and by the same method: sixteen
//! stage-call returns in the production build, exit status 3 out of process,
//! no marker ever written by a production library; and in the armed build,
//! a live hold released by a latch, a ceiling that aborts naming the stage,
//! aimedness, and the both-variables refusal.
//!
//! # Where the cross-file obligation is discharged
//!
//! In [`a_kill_between_alphas_landing_and_betas_leaves_alpha_landed_and_beta_not`],
//! explicitly and by name. The design requires verifying — not inheriting —
//! that a crash between two files' landings is possible in the
//! configuration actually used. It is.

test_scratch::install!();

#[cfg(all(unix, feature = "fault-injection"))]
#[path = "util/kill.rs"]
mod kill;

// ---------------------------------------------------------------------------
// the four named stages, each killed for real
// ---------------------------------------------------------------------------

/// STAGE ONE — killed AFTER THE ROW APPEND, before locks and before the
/// decided flip. The row exists with its full compiled payload; nothing else
/// happened. Recovery reads the row, sees it un-decided, and ABORTS: nothing
/// landed (the per-pool atomic step guarantees no part is half-applied), and
/// the row REMAINS forever as the record of the aborted attempt.
// RED: forced red twice against a scratch copy. (a) Making `recover` roll
// UN-decided rows forward as well as decided ones — the abandoned payload
// landed and `nothing_landed` went false. (b) Removing the `RowAppended` seam
// call from `exec::commit_after_append` — the child ran to completion, exited
// 4 (EXIT_UNREACHED) instead of dying, and `kill_at` failed on the missing
// signal.
#[cfg(all(unix, feature = "fault-injection"))]
#[test]
fn a_kill_after_the_row_append_leaves_an_undecided_row_and_recovery_aborts_it() {
    let _env = kill::env_shared();
    let dir = kill::scratch("row-appended");
    kill::kill_at("cross-schema", &dir, "row-appended");

    // THE WINDOW ON DISK. The durable record of the request exists — that is
    // the write door's first and only unfailable step — and
    // absolutely nothing else does.
    let c = kill::open(&dir);
    let row = kill::the_stuck_row(&dir);
    assert!(!row.decided, "the kill landed before the decided flip");
    assert!(!row.cleared);
    assert_eq!(row.parts.len(), 2, "both parts are enrolled at the append");
    assert!(
        row.parts.iter().all(|p| !p.landed),
        "nothing had landed when the process died"
    );
    assert!(
        !row.payload.parts.is_empty(),
        "the COMPILED PAYLOAD is durable before any landing — that is what makes \
         roll-forward possible at all"
    );
    assert!(
        kill::nothing_landed(&kill::observe(&c)),
        "no effect exists yet in either pool"
    );
    // Locks were never acquired: the kill is before phase 9.
    assert_eq!(kill::lock_rows(&dir, row.seq), 0);
    // The seed commit and nothing else.
    assert_eq!(c.committed_commit_counter().expect("counter"), 1);

    let report = c.recover().expect("recovery runs");
    assert_eq!(report.aborted, vec![row.hash], "un-decided means ABORT");
    assert!(report.rolled_forward.is_empty());
    assert!(report.cleared_only.is_empty());

    let after = kill::spine_of(&dir).row_at(row.seq).expect("the row is kept");
    kill::assert_all_or_nothing(&c, &after, "row-appended");
    assert_eq!(
        c.committed_commit_counter().expect("counter"),
        1,
        "an abort never moves the committed-commit counter"
    );
    // The abort is itself a recorded fact, forever.
    assert_eq!(after.statement, row.statement);
    assert_eq!(after.attempt, 1);
}

/// STAGE TWO — killed AFTER THE DECIDED FLIP, before the first pool landing.
/// The commit point of the whole design has passed (the commit
/// point is the row, not the storage engine) and yet NOTHING is in either
/// pool. Recovery must therefore roll BOTH parts forward from the row's own
/// payload — this is the case that shows "decided" is a promise the spine
/// keeps rather than a description of the disk.
// RED: forced red twice. (a) Making `recover` abort decided rows instead of
// rolling them forward — nothing landed, `assert_all_or_nothing` caught the
// row saying decided while the pools said empty. (b) Making roll-forward
// replay only `payload.parts[0]` — beta stayed empty and `mark_cleared`'s
// own guard refused the clear, so recovery failed loudly rather than lying.
#[cfg(all(unix, feature = "fault-injection"))]
#[test]
fn a_kill_after_the_decided_flip_leaves_no_part_landed_and_recovery_rolls_every_part_forward() {
    let _env = kill::env_shared();
    let dir = kill::scratch("decided");
    kill::kill_at("cross-schema", &dir, "decided");

    let c = kill::open(&dir);
    let row = kill::the_stuck_row(&dir);
    assert!(row.decided, "the kill landed after the decided flip");
    assert!(!row.cleared);
    assert!(
        row.parts.iter().all(|p| !p.landed),
        "decided, and not one part landed — the widest roll-forward there is"
    );
    assert!(
        kill::nothing_landed(&kill::observe(&c)),
        "the pools are empty although the transaction is COMMITTED by the row"
    );
    assert_eq!(c.committed_commit_counter().expect("counter"), 1);
    // Locks are held, and they are held DURABLY: the process that took them
    // is gone (release only at the cleared flip; the locks are tables, not
    // process state).
    assert!(
        kill::lock_rows(&dir, row.seq) > 0,
        "a killed transaction's locks survive it"
    );

    let report = c.recover().expect("recovery runs");
    assert_eq!(report.rolled_forward, vec![row.hash]);
    assert!(report.aborted.is_empty());

    let after = kill::spine_of(&dir).row_at(row.seq).expect("row");
    kill::assert_all_or_nothing(&c, &after, "decided");
    assert_eq!(
        c.committed_commit_counter().expect("counter"),
        2,
        "the counter moves when the commit's LAST part lands — under recovery too"
    );
    assert_eq!(kill::lock_rows(&dir, row.seq), 0, "recovery released them");
}

/// STAGE THREE — killed BETWEEN ALPHA'S LANDING AND BETA'S.
///
/// # THE CROSS-FILE OBLIGATION, DISCHARGED HERE
///
/// The storage engine's actual cross-file behaviour is verified by forcing the
/// failure rather than inherited as a claim: either a crash between two files'
/// landings is possible in the configuration actually used, or the reason it
/// is not is documented.
///
/// It is possible, and this test exhibits it: after a real SIGABRT taken
/// between the two landings, `fixture-alpha.db` holds its half of the commit
/// and `fixture-beta.db` holds none of it. The configuration is the one
/// actually used — one SQLite file per pool plus `spine.db`, WAL mode, NO
/// `ATTACH` anywhere, each pool part landing as its own single transaction on
/// its own file. Nothing in that arrangement can make
/// two files commit together, and the assertions below are the measurement,
/// not the claim.
///
/// It is the one window that distinguishes "the commit point is the ROW" from
/// "the commit point is the storage engine": under the second design the
/// state below could not exist, so a suite without this kill cannot tell the
/// two designs apart.
// RED: forced red twice. (a) Removing the `ordinal > 0` guard from the
// `BetweenLandings` seam call in `exec` so the stage fires before EVERY part
// — alpha then had not landed either and the half-landed assertion failed,
// which is the check that the window is genuinely BETWEEN the two files.
// (b) Making `pool::apply_part` land both parts on the first call — beta had
// its ticket in the window and the "the pools genuinely disagree" assertion
// failed.
#[cfg(all(unix, feature = "fault-injection"))]
#[test]
fn a_kill_between_alphas_landing_and_betas_leaves_alpha_landed_and_beta_not() {
    let _env = kill::env_shared();
    let dir = kill::scratch("between-landings");
    kill::kill_at("cross-schema", &dir, "between-landings");

    // The two files are genuinely two files.
    assert!(dir.join("fixture-alpha.db").is_file());
    assert!(dir.join("fixture-beta.db").is_file());

    let c = kill::open(&dir);
    let row = kill::the_stuck_row(&dir);
    assert_eq!(
        kill::part_pools(&row),
        vec![
            spine::schema_def::PoolName::new(kill::ALPHA),
            spine::schema_def::PoolName::new(kill::BETA)
        ],
        "two parts, one per pool, in the frozen landing order"
    );

    // ---- THE HALF-LANDED WINDOW, MEASURED ----
    assert!(row.decided && !row.cleared);
    assert!(row.parts[0].landed, "ALPHA'S PART LANDED");
    assert!(!row.parts[1].landed, "BETA'S PART DID NOT");
    let o = kill::observe(&c);
    assert_eq!(
        o.doc_state.as_deref(),
        Some("published"),
        "alpha's file carries its half of the commit"
    );
    assert_eq!(o.tickets, 0, "beta's file carries none of it");
    assert!(
        !kill::everything_landed(&o) && !kill::nothing_landed(&o),
        "THE STORAGE ENGINE GENUINELY ALLOWS A CRASH BETWEEN TWO FILES' LANDINGS \
         in this configuration — this is the cross-file verification, and this is its result"
    );
    // And the counter has NOT moved: it increments only when a commit's LAST
    // part lands, so a half-landed commit is not a commit to any
    // reader of that exposure.
    assert_eq!(c.committed_commit_counter().expect("counter"), 1);
    assert_eq!(
        c.in_landing(None).expect("in landing").len(),
        1,
        "the transaction is visible as in-landing while the window is open"
    );
    assert!(kill::lock_rows(&dir, row.seq) > 0, "locks are still held");

    // ---- AND THE ROW CLOSES IT ----
    let report = c.recover().expect("recovery runs");
    assert_eq!(report.rolled_forward, vec![row.hash]);
    let after = kill::spine_of(&dir).row_at(row.seq).expect("row");
    kill::assert_all_or_nothing(&c, &after, "between-landings");
    assert_eq!(c.committed_commit_counter().expect("counter"), 2);
    assert_eq!(kill::lock_rows(&dir, row.seq), 0);
    assert!(c.in_landing(None).expect("in landing").is_empty());
    // Alpha's part was NOT applied a second time: one doc, one state.
    assert_eq!(kill::docs_titled(&c, kill::SEED_TITLE), 1);
    assert_eq!(kill::ticket_count(&c), 1);
}

/// STAGE FOUR — killed BEFORE THE CLEARED FLIP, with every part already
/// landed. Everything is on disk and the row does not say so yet; the locks
/// are still held, because locks release only at that flip.
/// Recovery must clear it and release, and must NOT re-apply anything — the
/// distinction the report's `cleared_only` bucket exists to make.
// RED: forced red twice. (a) Making `recover` call `pool::apply_part` for
// parts whose landed flag is already true — the seeded doc's count cell
// double-incremented and `rolled_forward` came back where `cleared_only` was
// expected. (b) Removing `locks::release_at_clear` from recovery — the lock
// rows survived and the post-recovery assertion failed.
#[cfg(all(unix, feature = "fault-injection"))]
#[test]
fn a_kill_before_the_cleared_flip_leaves_everything_landed_and_recovery_only_clears_it() {
    let _env = kill::env_shared();
    let dir = kill::scratch("before-cleared");
    kill::kill_at("cross-schema", &dir, "before-cleared");

    let c = kill::open(&dir);
    let row = kill::the_stuck_row(&dir);
    assert!(row.decided && !row.cleared);
    assert!(
        row.parts.iter().all(|p| p.landed),
        "every part landed before the kill"
    );
    assert!(
        kill::everything_landed(&kill::observe(&c)),
        "both pools carry their halves already"
    );
    assert_eq!(
        c.committed_commit_counter().expect("counter"),
        2,
        "the counter moved with the LAST part, before the cleared flip"
    );
    assert!(
        kill::lock_rows(&dir, row.seq) > 0,
        "and the locks are STILL HELD — they release at the cleared flip, never per part"
    );

    let report = c.recover().expect("recovery runs");
    assert_eq!(
        report.cleared_only,
        vec![row.hash],
        "nothing to roll forward: recovery only finishes the flip"
    );
    assert!(report.rolled_forward.is_empty());
    assert!(report.aborted.is_empty());

    let after = kill::spine_of(&dir).row_at(row.seq).expect("row");
    kill::assert_all_or_nothing(&c, &after, "before-cleared");
    assert_eq!(
        c.committed_commit_counter().expect("counter"),
        2,
        "and it did not move a second time — the flip is idempotent by the landed flags"
    );
    assert_eq!(kill::lock_rows(&dir, row.seq), 0);
    assert_eq!(kill::ticket_count(&c), 1, "nothing was applied twice");
}

// ---------------------------------------------------------------------------
// the seam is AIMED
// ---------------------------------------------------------------------------

/// A kill test is only evidence if the stage names a real place in the commit
/// path. `between-landings` fires only BEFORE A SECOND PART, so a one-part
/// commit must run straight through it and complete — while the very same
/// armed environment kills the two-part statement above. If the seam were a
/// blunt kill switch aimed at nothing in particular, this test would die too
/// and every assertion in this file would be worth less.
// RED: removing the `if ordinal > 0` guard in `exec`'s landing loop — the
// single-part commit then died at `between-landings` too and this test failed
// on the missing exit status. That is the same mutation that reds the
// half-landed assertion above, from the opposite direction.
#[cfg(all(unix, feature = "fault-injection"))]
#[test]
fn the_seam_is_aimed_a_single_part_commit_never_reaches_between_landings() {
    let _env = kill::env_shared();
    let dir = kill::scratch("aimed");
    let out = kill::run("single-part", &dir, Some("between-landings"));
    assert_eq!(
        out.status.code(),
        Some(kill::EXIT_UNREACHED),
        "a ONE-part commit must complete under an armed `between-landings`.\nstderr: {}",
        kill::stderr_of(&out)
    );
    assert_eq!(kill::field(&out, "outcome").as_deref(), Some("committed"));

    // And it really did commit: the spine has a cleared row and no pending
    // work at all.
    let c = kill::open(&dir);
    assert!(kill::spine_of(&dir).pending().expect("pending").is_empty());
    assert_eq!(kill::docs_titled(&c, "single"), 1);
    assert_eq!(
        c.recover().expect("recovery"),
        spine::recover::RecoveryReport::default(),
        "there is nothing for recovery to do after a clean commit"
    );
}

// ---------------------------------------------------------------------------
// the seam is ABSENT OR REFUSING, forever
// ---------------------------------------------------------------------------

/// THE STANDING OBLIGATION: every production build must PROVE the seam
/// absent — compiled out or refusing — forever.
///
/// # How absence-of-abort is established
///
/// Not by grepping and not by trusting the `#[cfg]`. By EXECUTION: with
/// `SPINE_FAULT` set to a real stage name, every one of the sixteen
/// (armed stage, called stage) pairs of `fault::at_stage` is CALLED, in this
/// process, and every one is observed to RETURN a value. An abort path is not
/// a thing a test can fail to notice — a single surviving `process::abort()`
/// would kill this test binary outright and the harness would report a signal
/// rather than a failure. Sixteen returns is the absence.
///
/// The companion out-of-process proof is
/// [`the_production_build_refuses_a_set_spine_fault_out_of_process_too`],
/// which runs the whole door in a CHILD and asserts the child exited with a
/// status rather than dying of a signal.
// RED: replacing the `#[cfg(not(feature))] gate_with` body with `Ok(())` —
// the production door then ignored a set SPINE_FAULT and the refusal
// assertions failed. Also red by replacing the non-feature `at_stage_with`
// body with `Ok(())` — the sixteen-pair sweep stopped seeing refusals.
#[test]
fn the_production_build_carries_no_abort_path_and_refuses_a_set_spine_fault() {
    let _env = env_exclusive();
    let _clean = SeamEnvClean;
    let stages = [
        spine::recover::Stage::RowAppended,
        spine::recover::Stage::Decided,
        spine::recover::Stage::BetweenLandings,
        spine::recover::Stage::BeforeCleared,
    ];

    #[cfg(not(feature = "fault-injection"))]
    {
        use spine::error::SpineError;
        // SIXTEEN CALLS, sixteen returns. Any one of them would abort in the
        // armed build; none of them can here.
        for armed in stages.iter() {
            std::env::set_var("SPINE_FAULT", armed.name());
            for called in stages.iter() {
                match spine::fault::at_stage(*called) {
                    Err(SpineError::FaultSeamRefused { variable, value }) => {
                        assert_eq!(variable, spine::fault::FAULT_ENV, "and WHICH variable");
                        assert_eq!(value, armed.name(), "the refusal names what was set");
                    }
                    other => panic!(
                        "the production seam must REFUSE (armed {}, called {}), got {other:?}",
                        armed.name(),
                        called.name()
                    ),
                }
            }
            // The door's own gate refuses the same value, before anything is
            // parsed or appended.
            match spine::fault::gate() {
                Err(SpineError::FaultSeamRefused { variable, value }) => {
                    assert_eq!(variable, spine::fault::FAULT_ENV);
                    assert_eq!(value, armed.name());
                }
                other => panic!("the production gate must refuse, got {other:?}"),
            }
        }
        // A value naming NO stage refuses too: the production build refuses by
        // PRESENCE, never by parsing — it has no stage machinery to consult.
        std::env::set_var("SPINE_FAULT", "not-a-stage-at-all");
        match spine::fault::gate() {
            Err(SpineError::FaultSeamRefused { variable, value }) => {
                assert_eq!(variable, spine::fault::FAULT_ENV);
                assert_eq!(value, "not-a-stage-at-all");
            }
            other => panic!("expected FaultSeamRefused, got {other:?}"),
        }
        // Unset: silent in both builds, and only in that case.
        std::env::remove_var("SPINE_FAULT");
        assert!(spine::fault::gate().is_ok());
        for called in stages.iter() {
            assert!(spine::fault::at_stage(*called).is_ok());
        }
    }

    #[cfg(feature = "fault-injection")]
    {
        // The armed build cannot call the MATCHING pair in process — that is
        // the point of the whole accommodation — so it asserts the other
        // half: the seam is aimed. Twelve non-matching pairs pass through.
        for armed in stages.iter() {
            std::env::set_var("SPINE_FAULT", armed.name());
            for called in stages.iter().filter(|c| *c != armed) {
                assert!(
                    spine::fault::at_stage(*called).is_ok(),
                    "a non-matching stage passes through (armed {}, called {})",
                    armed.name(),
                    called.name()
                );
            }
        }
        // A misspelled stage is a LOUD usage error at every seam call, never a
        // silent no-op — a silent no-op would make every kill test in this
        // file a fake green.
        std::env::set_var("SPINE_FAULT", "not-a-stage-at-all");
        for called in stages.iter() {
            kill::assert_seam_usage(
                spine::fault::at_stage(*called),
                spine::fault::USAGE_NO_SUCH_STAGE,
                &format!("a misspelled SPINE_FAULT must be loud at `{}`", called.name()),
            );
        }
        kill::assert_seam_usage(
            spine::fault::gate(),
            spine::fault::USAGE_NO_SUCH_STAGE,
            "a misspelled SPINE_FAULT is loud at the gate",
        );
        std::env::remove_var("SPINE_FAULT");
        for called in stages.iter() {
            assert!(spine::fault::at_stage(*called).is_ok());
        }
    }

    std::env::remove_var("SPINE_FAULT");
}

/// The out-of-process half of the same obligation, and the one that cannot be
/// argued with: the production build runs the WHOLE cross-schema statement in a child
/// with `SPINE_FAULT=between-landings` set, and the child comes back with an
/// EXIT STATUS. A build carrying an abort path would come back with a signal.
/// Nothing lands, and no spine row is even appended, because the refusal is at
/// the door.
// RED: replacing BOTH `#[cfg(not(feature))]` bodies in `fault.rs` —
// `gate_with` AND `at_stage_with` — with `Ok(())`: the child then ran the
// cross-schema statement to completion and came back 4 (EXIT_UNREACHED) with a ticket
// in beta.
//
// NEGATIVE RESULT, recorded: removing EITHER ONE ALONE leaves this test
// green. The production seam is defended twice — at the door's gate and again
// at every stage call — so the out-of-process proof survives losing either.
// That is genuine defence in depth, and the sibling test above is the one
// that pins each layer separately.
#[cfg(not(feature = "fault-injection"))]
#[test]
fn the_production_build_refuses_a_set_spine_fault_out_of_process_too() {
    let _env = env_shared_only();
    let dir = test_scratch::tmp_path(format!(
        "spine-kill-prod-seam-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);

    let mut cmd = std::process::Command::new(production_victim_binary());
    cmd.env("SPINE_VICTIM_DIR", dir.as_os_str())
        .env("SPINE_VICTIM_JOB", "cross-schema")
        .env("SPINE_VICTIM_EXPECT_FEATURE", "0")
        .env("SPINE_FAULT", "between-landings");
    let out = cmd.output().expect("spawn the victim");
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    // A victim left over from an armed build would abort here and be
    // mistaken for a production build carrying an abort path. It refuses by
    // name instead (see `examples/spine_victim.rs::check_build`), and this is
    // the one place where reading that wrong would be a catastrophic
    // false conclusion rather than a confusing one.
    assert_ne!(
        out.status.code(),
        Some(7),
        "the `spine_victim` on disk was built in the other configuration:\n{stderr}"
    );
    assert_eq!(
        out.status.code(),
        Some(3),
        "the production door refuses and the process EXITS; a signal here would be an \
         abort path surviving into a production build.\nstderr: {stderr}"
    );
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        stdout.contains("kind=fault_seam_refused") && stdout.contains("variable=SPINE_FAULT"),
        "and it refuses BY NAME, naming the KILL variable; stdout was: {stdout}"
    );

    // Nothing landed, and the door refused before appending anything: the
    // cauldron holds its seeded doc and no pending row at all.
    let c = spine::Cauldron::open(&dir, spine::config::SpineConfig::default())
        .expect("the victim still built the cauldron");
    assert!(
        spine::spine_db::SpineDb::open(&dir.join(spine::spine_db::SPINE_FILE))
            .expect("spine")
            .pending()
            .expect("pending")
            .is_empty(),
        "a door refusal at phase 2 appends no row"
    );
    assert_eq!(
        c.committed_commit_counter().expect("counter"),
        1,
        "only the victim's seed commit"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// the seam's PAUSE form: the SAME two
// hard conditions, proven the SAME way
// ---------------------------------------------------------------------------

/// THE STANDING OBLIGATION, for the second variable. Exactly the shape of
/// [`the_production_build_carries_no_abort_path_and_refuses_a_set_spine_fault`]:
/// with `SPINE_PAUSE` set to a real stage name — and NO latch, and a
/// hundred-millisecond ceiling — every one of the sixteen (armed stage,
/// called stage) pairs of `fault::at_stage` is CALLED in this process and
/// observed to RETURN. A pause body surviving into the production build
/// could not return on the matching pair: it would write a marker, wait
/// for a latch nobody creates, and abort at its ceiling — a signal, not a
/// failure, and one this harness cannot miss. Sixteen returns is the
/// absence; and NO `.reached` marker on disk afterwards is the absence of
/// the one write the pause form makes.
///
/// The companion out-of-process proof is
/// [`the_production_build_refuses_a_set_spine_pause_out_of_process_too`].
// RED: replacing the `#[cfg(not(feature))] pause_at_stage_with` body with
// `Ok(())` — the sixteen-pair sweep stopped seeing refusals; replacing the
// non-feature `pause_gate_with` body with `Ok(())` — the gate assertion
// failed. In the armed build: `pause_gate_with`'s `pause_latch()?` removed —
// "no latch path" stopped refusing at the gate.
#[test]
fn the_production_build_carries_no_pause_path_and_refuses_a_set_spine_pause() {
    let _env = env_exclusive();
    let _clean = SeamEnvClean;
    let stages = [
        spine::recover::Stage::RowAppended,
        spine::recover::Stage::Decided,
        spine::recover::Stage::BetweenLandings,
        spine::recover::Stage::BeforeCleared,
    ];
    let latch = test_scratch::tmp_path(format!(
        "spine-pause-absence-{}-latch",
        std::process::id()
    ));
    let reached = spine::fault::reached_path(&latch);
    let _ = std::fs::remove_file(&reached);
    std::env::remove_var("SPINE_FAULT");
    std::env::remove_var("SPINE_PAUSE_LATCH");
    std::env::set_var("SPINE_PAUSE_CEILING_MS", "100");

    #[cfg(not(feature = "fault-injection"))]
    {
        use spine::error::SpineError;
        // SIXTEEN CALLS, sixteen returns — INCLUDING the four matching pairs,
        // which would HOLD in the armed build and cannot here.
        for armed in stages.iter() {
            std::env::set_var("SPINE_PAUSE", armed.name());
            for called in stages.iter() {
                match spine::fault::at_stage(*called) {
                    // ONE variant for both forms: the PAUSE form is
                    // told apart by `variable`, structurally.
                    Err(SpineError::FaultSeamRefused { variable, value }) => {
                        assert_eq!(variable, spine::fault::PAUSE_ENV, "it names the PAUSE variable");
                        assert_eq!(value, armed.name(), "the refusal names what was set");
                    }
                    other => panic!(
                        "the production seam must REFUSE a set SPINE_PAUSE (armed {}, called {}), \
                         got {other:?}",
                        armed.name(),
                        called.name()
                    ),
                }
            }
            match spine::fault::gate() {
                Err(SpineError::FaultSeamRefused { variable, value }) => {
                    assert_eq!(variable, spine::fault::PAUSE_ENV);
                    assert_eq!(value, armed.name());
                }
                other => panic!("the production gate must refuse, got {other:?}"),
            }
        }
        // Refusal is by PRESENCE, never by parsing — even with a latch path
        // supplied, and even for a value naming no stage.
        std::env::set_var("SPINE_PAUSE_LATCH", latch.as_os_str());
        std::env::set_var("SPINE_PAUSE", "decided");
        match spine::fault::at_stage(spine::recover::Stage::Decided) {
            Err(SpineError::FaultSeamRefused { variable, value }) => {
                assert_eq!(variable, spine::fault::PAUSE_ENV);
                assert_eq!(value, "decided");
            }
            other => panic!("expected FaultSeamRefused naming SPINE_PAUSE, got {other:?}"),
        }
        std::env::set_var("SPINE_PAUSE", "not-a-stage-at-all");
        match spine::fault::gate() {
            Err(SpineError::FaultSeamRefused { variable, value }) => {
                assert_eq!(variable, spine::fault::PAUSE_ENV);
                assert_eq!(value, "not-a-stage-at-all");
            }
            other => panic!("expected FaultSeamRefused naming SPINE_PAUSE, got {other:?}"),
        }
        // Both variables set: still a refusal by name, never an arm — and it
        // names the KILL variable, the first one checked.
        std::env::set_var("SPINE_FAULT", "decided");
        match spine::fault::gate() {
            Err(SpineError::FaultSeamRefused { variable, value }) => {
                assert_eq!(variable, spine::fault::FAULT_ENV);
                assert_eq!(value, "decided");
            }
            other => panic!("both set: expected FaultSeamRefused naming SPINE_FAULT, got {other:?}"),
        }
        std::env::remove_var("SPINE_FAULT");
        // THE PRODUCTION LIBRARY WROTE NOTHING: no marker exists, because no
        // code that writes one was compiled.
        assert!(
            !reached.exists(),
            "the production build must never write a `.reached` marker: {}",
            reached.display()
        );
        // Unset: silent.
        std::env::remove_var("SPINE_PAUSE");
        assert!(spine::fault::gate().is_ok());
        for called in stages.iter() {
            assert!(spine::fault::at_stage(*called).is_ok());
        }
    }

    #[cfg(feature = "fault-injection")]
    {
        // The armed build cannot call the MATCHING pair without a latch —
        // it would hold to the ceiling and abort — so it asserts the other
        // half here: the seam is aimed (twelve non-matching pairs pass), a
        // misspelled stage is loud everywhere, and the GATE demands a latch.
        // The matching pair, with a latch, is exercised live by
        // `the_pause_form_holds_a_live_stage_call_in_process_until_the_latch`.
        for armed in stages.iter() {
            std::env::set_var("SPINE_PAUSE", armed.name());
            for called in stages.iter().filter(|c| *c != armed) {
                assert!(
                    spine::fault::at_stage(*called).is_ok(),
                    "a non-matching stage passes through (armed {}, called {})",
                    armed.name(),
                    called.name()
                );
            }
            // Every refusal below is asserted BY ITS FROZEN TAG: a
            // bare `is_err()` would let a latch refusal satisfy the
            // misspelled-stage line and a misspelled stage satisfy this one.
            kill::assert_seam_usage(
                spine::fault::gate(),
                spine::fault::USAGE_NO_LATCH,
                &format!("no latch path: the gate refuses (armed {})", armed.name()),
            );
            std::env::set_var("SPINE_PAUSE_LATCH", latch.as_os_str());
            assert!(spine::fault::gate().is_ok(), "with a latch path the gate arms");
            std::env::remove_var("SPINE_PAUSE_LATCH");
        }
        std::env::set_var("SPINE_PAUSE", "not-a-stage-at-all");
        std::env::set_var("SPINE_PAUSE_LATCH", latch.as_os_str());
        for called in stages.iter() {
            kill::assert_seam_usage(
                spine::fault::at_stage(*called),
                spine::fault::USAGE_NO_SUCH_STAGE,
                &format!("a misspelled SPINE_PAUSE must be loud at `{}`", called.name()),
            );
        }
        kill::assert_seam_usage(
            spine::fault::gate(),
            spine::fault::USAGE_NO_SUCH_STAGE,
            "a misspelled SPINE_PAUSE is loud at the gate",
        );
        // A ceiling that does not parse is loud at the gate, before any row.
        std::env::set_var("SPINE_PAUSE", "decided");
        std::env::set_var("SPINE_PAUSE_CEILING_MS", "soon");
        kill::assert_seam_usage(
            spine::fault::gate(),
            spine::fault::USAGE_BAD_CEILING,
            "an unparseable ceiling refuses at the gate",
        );
        std::env::set_var("SPINE_PAUSE_CEILING_MS", "100");
        // Both set: one seam, one aim.
        std::env::set_var("SPINE_FAULT", "before-cleared");
        kill::assert_seam_usage(
            spine::fault::gate(),
            spine::fault::USAGE_BOTH_SET,
            "both set: the gate refuses",
        );
        for called in stages.iter() {
            kill::assert_seam_usage(
                spine::fault::at_stage(*called),
                spine::fault::USAGE_BOTH_SET,
                &format!("both set: every stage call refuses (`{}`)", called.name()),
            );
        }
        std::env::remove_var("SPINE_FAULT");
        std::env::remove_var("SPINE_PAUSE_LATCH");
        std::env::remove_var("SPINE_PAUSE");
        for called in stages.iter() {
            assert!(spine::fault::at_stage(*called).is_ok());
        }
        assert!(!reached.exists(), "nothing paused, so nothing was marked");
    }

    std::env::remove_var("SPINE_PAUSE");
    std::env::remove_var("SPINE_PAUSE_LATCH");
    std::env::remove_var("SPINE_PAUSE_CEILING_MS");
    std::env::remove_var("SPINE_FAULT");
}

/// The out-of-process half for the pause form: the production build runs a
/// whole door job in a child with `SPINE_PAUSE=decided` and a real latch
/// path, and the child comes back with EXIT STATUS 3 — never a hang (a pause
/// path surviving would hold to its ceiling and die of a signal), never a
/// marker on disk. Nothing lands and no row is appended: the refusal is at
/// the door.
// RED: replacing BOTH `#[cfg(not(feature))]` pause bodies in `fault.rs` —
// `pause_gate_with` AND `pause_at_stage_with` — with `Ok(())`: the child ran
// the connect instead of refusing at the door, and the exit-status-3
// assertion failed.
#[cfg(not(feature = "fault-injection"))]
#[test]
fn the_production_build_refuses_a_set_spine_pause_out_of_process_too() {
    let _env = env_shared_only();
    let dir = test_scratch::tmp_path(format!(
        "spine-pause-prod-seam-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let latch = test_scratch::tmp_path(format!(
        "spine-pause-prod-seam-{}-latch",
        std::process::id()
    ));
    let reached = spine::fault::reached_path(&latch);
    let _ = std::fs::remove_file(&reached);

    let mut cmd = std::process::Command::new(production_victim_binary());
    cmd.env("SPINE_VICTIM_DIR", dir.as_os_str())
        .env("SPINE_VICTIM_JOB", "connect")
        .env("SPINE_VICTIM_EXPECT_FEATURE", "0")
        .env_remove("SPINE_FAULT")
        .env("SPINE_PAUSE", "decided")
        .env("SPINE_PAUSE_LATCH", latch.as_os_str())
        // If a pause path survived, die inside a second rather than hang.
        .env("SPINE_PAUSE_CEILING_MS", "1000");
    let out = cmd.output().expect("spawn the victim");
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert_ne!(
        out.status.code(),
        Some(7),
        "the `spine_victim` on disk was built in the other configuration:\n{stderr}"
    );
    assert_eq!(
        out.status.code(),
        Some(3),
        "the production door refuses a set SPINE_PAUSE and the process EXITS; a signal or a \
         hang here would be a pause path surviving into a production build.\nstderr: {stderr}"
    );
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        stdout.contains("kind=fault_seam_refused") && stdout.contains("variable=SPINE_PAUSE"),
        "and it refuses BY NAME, naming the PAUSE variable (one variant for both forms); \
         stdout was: {stdout}"
    );
    assert!(
        !reached.exists(),
        "the production library never writes the marker: {}",
        reached.display()
    );

    // Nothing landed, and the door refused before appending anything: the
    // cauldron holds the victim's seeded author and no pending row at all.
    let c = spine::Cauldron::open(&dir, spine::config::SpineConfig::default())
        .expect("the victim still built the cauldron");
    assert!(
        spine::spine_db::SpineDb::open(&dir.join(spine::spine_db::SPINE_FILE))
            .expect("spine")
            .pending()
            .expect("pending")
            .is_empty(),
        "a door refusal at phase 2 appends no row"
    );
    assert_eq!(
        c.committed_commit_counter().expect("counter"),
        1,
        "only the victim's seeded author"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// THE PAUSE, LIVE, IN PROCESS (armed build): the matching pair HOLDS. A
/// thread calls `fault::at_stage(Decided)` under `SPINE_PAUSE=decided`; the
/// test waits for the library's `.reached` marker (never a timer), observes
/// the thread still running, creates the latch, and joins it to an `Ok`.
/// Then the same armed environment at a NON-matching stage returns at once.
/// This is the contract every pause-form test in the crate rests on, taken
/// at the seam itself with no door in the way.
// RED: `pause_here`'s wait loop deleted (return `Ok` after the marker) —
// "still held after the marker" failed; the marker write deleted — the
// wait for `.reached` hit its bound.
#[cfg(feature = "fault-injection")]
#[test]
fn the_pause_form_holds_a_live_stage_call_in_process_until_the_latch() {
    let _env = env_exclusive();
    let _clean = SeamEnvClean;
    let latch_dir = test_scratch::tmp_path(format!(
        "spine-pause-inproc-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&latch_dir);
    std::fs::create_dir_all(&latch_dir).expect("latch dir");
    let latch = latch_dir.join("latch");
    let reached = spine::fault::reached_path(&latch);
    std::env::remove_var("SPINE_FAULT");
    std::env::set_var("SPINE_PAUSE", "decided");
    std::env::set_var("SPINE_PAUSE_LATCH", latch.as_os_str());
    std::env::remove_var("SPINE_PAUSE_CEILING_MS");

    let held = std::thread::spawn(|| spine::fault::at_stage(spine::recover::Stage::Decided));
    let started = std::time::Instant::now();
    while !reached.is_file() {
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "the seam never wrote its reached-marker"
        );
        assert!(!held.is_finished(), "the seam returned before it was released");
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert_eq!(
        std::fs::read_to_string(&reached).expect("marker").trim(),
        "decided",
        "the marker names the stage, in recovery's vocabulary"
    );
    std::thread::sleep(std::time::Duration::from_millis(80));
    assert!(!held.is_finished(), "still held after the marker: the latch is what releases it");
    // A non-matching stage under the same armed environment returns at once.
    assert!(spine::fault::at_stage(spine::recover::Stage::BeforeCleared).is_ok());

    std::fs::write(&latch, b"go").expect("latch");
    let out = held.join().expect("join");
    assert!(out.is_ok(), "released: the stage call returns Ok, got {out:?}");

    std::env::remove_var("SPINE_PAUSE");
    std::env::remove_var("SPINE_PAUSE_LATCH");
    let _ = std::fs::remove_dir_all(&latch_dir);
}

/// A PAUSE NOBODY RELEASES MUST KILL THE TEST VISIBLY, NEVER HANG IT: with
/// `SPINE_PAUSE_CEILING_MS=300` and no latch ever written, the held victim
/// dies of SIGABRT naming the stage — a signal the parent cannot mistake for
/// a completed commit, and a wait bounded by configuration rather than by a
/// constant.
// RED: the ceiling check in `pause_here` deleted — the child sat past the
// bound, `wait_unreleased_within(8s)` killed it and failed: "a pause nobody
// releases must abort at its ceiling, never hang". That bounded wait is what
// keeps this checker from hanging the suite under the very mutation it
// exists to catch.
#[cfg(all(unix, feature = "fault-injection"))]
#[test]
fn an_unreleased_pause_aborts_at_its_ceiling_naming_the_stage() {
    use std::os::unix::process::ExitStatusExt;
    let _env = kill::env_shared();
    let dir = kill::scratch("pause-ceiling");
    let held = kill::pause_at(
        "decided",
        "connect",
        &dir,
        "ceiling",
        &[("SPINE_PAUSE_CEILING_MS", "300")],
    );
    let started = std::time::Instant::now();
    // Bounded well under the DEFAULT ceiling (30s): a child that ignored the
    // configured 300ms and sat to the default would fail here, not pass late.
    let out = held.wait_unreleased_within(std::time::Duration::from_secs(8));
    let waited = started.elapsed();
    let stderr = kill::stderr_of(&out);
    assert_eq!(out.status.code(), None, "an unreleased pause is a DEATH, not an exit");
    assert_eq!(
        out.status.signal(),
        Some(kill::SIGABRT),
        "it aborts at the ceiling.\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("pause at stage `decided` was never released"),
        "and it names the stage; stderr was:\n{stderr}"
    );
    assert!(
        waited < std::time::Duration::from_secs(8),
        "the ceiling is the CONFIGURED one, not the default: waited {waited:?}"
    );
    // The transaction it was holding is left decided and un-landed — exactly
    // the kill form's window — and recovery closes it.
    let c = kill::open(&dir);
    let row = kill::the_stuck_row(&dir);
    assert!(row.decided && !row.cleared);
    let report = c.recover().expect("recovery");
    assert_eq!(report.rolled_forward, vec![row.hash]);
}

/// The pause form is AIMED, as the kill form is: `between-landings` fires
/// only before a SECOND part, so a one-part commit under
/// `SPINE_PAUSE=between-landings` runs straight through, never writes a
/// marker, and exits 4 — the victim's "armed and never reached" status.
// RED: removing the `if ordinal > 0` guard in `exec`'s landing loop — the
// single-part commit paused, wrote a marker, and hit its ceiling.
#[cfg(all(unix, feature = "fault-injection"))]
#[test]
fn the_pause_form_is_aimed_a_single_part_commit_never_reaches_between_landings() {
    let _env = kill::env_shared();
    let dir = kill::scratch("pause-aimed");
    let latch = test_scratch::tmp_path(format!(
        "spine-pause-aimed-{}-latch",
        std::process::id()
    ));
    let reached = spine::fault::reached_path(&latch);
    let _ = std::fs::remove_file(&reached);
    let mut cmd = kill::victim("single-part", &dir, None);
    cmd.env("SPINE_PAUSE", "between-landings")
        .env("SPINE_PAUSE_LATCH", latch.as_os_str())
        .env("SPINE_PAUSE_CEILING_MS", "2000");
    let out = cmd.output().expect("spawn the victim");
    kill::reject_a_stale_victim(&out);
    assert_eq!(
        out.status.code(),
        Some(kill::EXIT_UNREACHED),
        "a ONE-part commit must complete under an armed `between-landings` pause.\nstderr: {}",
        kill::stderr_of(&out)
    );
    assert!(!reached.exists(), "no stage was reached, so no marker was written");
    assert_eq!(kill::docs_titled(&kill::open(&dir), "single"), 1);
}

/// BOTH VARIABLES SET AT ONCE: one seam, one aim. The door refuses at its
/// gate, out of process, before any row is appended — a victim armed both
/// ways exits 3 with a usage refusal, and neither the kill nor the pause
/// ever runs.
// RED: `both_set` returning `Ok(())` in the armed build — the child died of
// the kill at `decided` instead of exiting 3.
#[cfg(all(unix, feature = "fault-injection"))]
#[test]
fn both_seam_variables_set_at_once_refuse_at_the_door() {
    let _env = kill::env_shared();
    let dir = kill::scratch("both-set");
    let latch = test_scratch::tmp_path(format!(
        "spine-pause-both-{}-latch",
        std::process::id()
    ));
    let mut cmd = kill::victim("cross-schema", &dir, Some("decided"));
    cmd.env("SPINE_PAUSE", "decided")
        .env("SPINE_PAUSE_LATCH", latch.as_os_str());
    let out = cmd.output().expect("spawn the victim");
    kill::reject_a_stale_victim(&out);
    assert_eq!(
        out.status.code(),
        Some(kill::EXIT_REFUSED),
        "both set: the door refuses and the process exits.\nstderr: {}",
        kill::stderr_of(&out)
    );
    assert_eq!(kill::field(&out, "kind").as_deref(), Some("usage"));
    assert!(
        kill::spine_of(&dir).pending().expect("pending").is_empty(),
        "refused at the gate: no row appended"
    );
}

// ---------------------------------------------------------------------------
// local plumbing — the seam-absence tests run in BOTH builds, so they cannot
// borrow `kill.rs`, which is compiled only in the armed build.
// ---------------------------------------------------------------------------

// THE ENV LOCK IS `kill.rs`'S, WHEREVER `kill.rs` EXISTS — and this file used
// to declare a SECOND `RwLock<()>` beside it instead.
// Two locks interlock nothing: in the armed build every kill test above takes
// `kill::env_shared()`, so a write guard taken on a different static excluded
// none of them, and the one test in this file that MUTATES the process-global
// `SPINE_FAULT` ran concurrently with five tests that spawn children and read
// the disk. The armed build now takes the same static they do.
//
// A local static survives ONLY where `kill.rs` is not compiled at all (the
// production build, and non-unix), because the seam-absence tests run in BOTH
// builds and `kill.rs` is armed-and-unix only.
#[cfg(all(unix, feature = "fault-injection"))]
use kill::env_exclusive;

/// Removes every seam variable on drop. Declared AFTER the env lock in each
/// env-mutating test so it drops BEFORE the lock releases: a panic mid-test
/// then cannot leave `SPINE_FAULT` or `SPINE_PAUSE` set for whoever runs
/// next (a red sweep saw exactly that cascade in `smoke.rs`).
struct SeamEnvClean;

impl Drop for SeamEnvClean {
    fn drop(&mut self) {
        for v in [
            spine::fault::FAULT_ENV,
            spine::fault::PAUSE_ENV,
            spine::fault::PAUSE_LATCH_ENV,
            spine::fault::PAUSE_CEILING_ENV,
        ] {
            std::env::remove_var(v);
        }
    }
}

#[cfg(not(all(unix, feature = "fault-injection")))]
static ENV: std::sync::RwLock<()> = std::sync::RwLock::new(());

#[cfg(not(all(unix, feature = "fault-injection")))]
fn env_exclusive() -> std::sync::RwLockWriteGuard<'static, ()> {
    ENV.write().unwrap_or_else(|e| e.into_inner())
}

#[cfg(not(feature = "fault-injection"))]
fn env_shared_only() -> std::sync::RwLockReadGuard<'static, ()> {
    ENV.read().unwrap_or_else(|e| e.into_inner())
}

/// The same resolution [`kill::victim_binary`] does — the same file, for the
/// production build where `kill.rs` is not compiled at all.
#[cfg(not(feature = "fault-injection"))]
#[path = "util/victim.rs"]
mod victim_path;
#[cfg(not(feature = "fault-injection"))]
use victim_path::victim_binary as production_victim_binary;
