//! A quiesce whose taker is dead — its token gone with its process — is lifted by a
//! NAMED, LOUD, BY-HAND act, `Cauldron::resume_all(reason)`, which names
//! every quiescer it lifted by holder and since when; never automatically.
//! And the refusal a quiesced cauldron gives names the same.
//!
//! A dropped token stands in for a dead process: `Quiesce` is not `Clone`,
//! and dropping it lifts nothing (its own docs) — after `drop(token)` there
//! is no sanctioned `resume` left for that quiescer, which is exactly the
//! dead taker's state as seen from the spine.

use spine::config::SpineConfig;
use spine::envelope::{Bindings, Envelope, Initiator, Instrument, Mode, Provenance};
use spine::error::SpineError;
use spine::schema_def::fixtures;
use spine::spine_db::SPINE_FILE;
use spine::{Cauldron, Outcome, Quiescer};
use std::path::{Path, PathBuf};

test_scratch::install!();

/// A quiesce whose token was dropped stands; nothing in the token API lifts
/// it; `resume_all` does, reporting the holder and since when the spine
/// recorded, and the cauldron takes commits again.
// RED: (a) `take_all_quiescers` listing without deleting — the cauldron was
// still quiesced after the lift and the commit after it refused; (b) the
// report built with empty holder/since — the equality against the row the
// spine recorded failed.
#[test]
fn resume_all_lifts_a_quiesce_held_by_a_dropped_token_and_names_holder_and_since() {
    let dir = root("resume-all-dropped");
    let c = fresh_cauldron(&dir);
    let token = c.quiesce().expect("an idle cauldron quiesces at once");
    drop(token);

    // THE SENTINEL: exactly one quiescer stands, and the spine says who and
    // since when — the two facts the lift must report back verbatim.
    let standing = quiescers(&dir);
    assert_eq!(standing.len(), 1, "one quiescer: {standing:?}");
    let expected = standing[0].clone();
    assert_eq!(expected.holder, format!("pid {}", std::process::id()));
    assert!(!expected.since.is_empty());
    assert!(
        matches!(create(&c, "during"), Err(SpineError::Busy { .. })),
        "the cauldron is quiesced and no token can lift it"
    );

    let lifted = c
        .resume_all("the backup-taker died holding its token; lifting by hand")
        .expect("the by-hand lift");
    assert_eq!(lifted.reason, "the backup-taker died holding its token; lifting by hand");
    assert_eq!(lifted.lifted, vec![expected.clone()], "names holder and since: {lifted:?}");
    assert_eq!(lifted.lifted[0].holder, expected.holder);
    assert_eq!(lifted.lifted[0].since, expected.since);

    assert!(quiescers(&dir).is_empty(), "nothing stands after the lift");
    assert!(
        matches!(create(&c, "after"), Ok(Outcome::Committed { .. })),
        "a commit begins again"
    );
}

/// N = 2: two quiescers, both tokens dropped. One lift takes both, reports
/// both in the order they were taken, and the cauldron is open after it —
/// a lift that took only one would leave it quiesced by count.
// RED: `take_all_quiescers` deleting with `LIMIT 1` — the report had one
// entry and the cauldron stayed quiesced.
#[test]
fn resume_all_lifts_every_quiescer_at_n_2() {
    let dir = root("resume-all-two");
    let c = fresh_cauldron(&dir);
    let first = c.quiesce().expect("first");
    let second = c.quiesce().expect("second");
    drop(first);
    drop(second);
    let standing = quiescers(&dir);
    assert_eq!(standing.len(), 2, "two quiescers: {standing:?}");

    let lifted = c.resume_all("two dead takers").expect("lift");
    assert_eq!(lifted.lifted, standing, "both, in the order they were taken");
    assert!(quiescers(&dir).is_empty());
    assert!(matches!(create(&c, "after two"), Ok(Outcome::Committed { .. })));
}

/// A reason is REQUIRED: a blank one lifts nothing and is refused as usage.
/// The quiesce still stands, and the same call with a reason lifts it.
// RED: the blank-reason check removed — the lift went through and the
/// cauldron was open after a refusal that should have left it quiesced.
#[test]
fn resume_all_with_a_blank_reason_lifts_nothing() {
    let dir = root("resume-all-blank");
    let c = fresh_cauldron(&dir);
    drop(c.quiesce().expect("quiesce"));

    for blank in ["", "   ", "\n"] {
        let err = c.resume_all(blank).expect_err("a blank reason is refused");
        assert_eq!(err.kind(), "usage", "{err:?}");
        assert_eq!(quiescers(&dir).len(), 1, "nothing was lifted for {blank:?}");
        assert!(matches!(create(&c, "still"), Err(SpineError::Busy { .. })));
    }
    assert_eq!(c.resume_all("named now").expect("lift").lifted.len(), 1);
    assert!(quiescers(&dir).is_empty());
}

/// THE OPPOSITE: with nothing standing, `resume_all` lifts nothing and says
/// so with an empty list — never an error, never a fabricated entry.
// RED: `resume_all` returning a fabricated entry when nothing stood — the
// emptiness assertion.
#[test]
fn resume_all_with_nothing_standing_reports_an_empty_lift() {
    let dir = root("resume-all-empty");
    let c = fresh_cauldron(&dir);
    assert!(quiescers(&dir).is_empty());
    let lifted = c.resume_all("checking").expect("nothing to lift is not an error");
    assert_eq!(lifted.reason, "checking");
    assert!(lifted.lifted.is_empty(), "{lifted:?}");
    assert!(matches!(create(&c, "open"), Ok(Outcome::Committed { .. })));
}

/// The quiesced refusal NAMES the holder and since when, as the spine
/// recorded them, and names both lifts — so a pause whose taker is dead is
/// diagnosable from the refusal alone.
// RED: the door's phase-1 gate restored to its old constant message — none
// of holder, since, `resume_all` was in it.
// RED (seen): "Each is lifted by its own token (`resume`); " cut from
// `quiesced_busy` — the token-lift assertion; the old bare
// `contains("resume")` stayed ok under that same cut. And `resume_all(reason)`
// renamed in the message — the by-hand-lift assertion. (Seen) the
// token sentence cut — the stripped-message token check, with the by-hand
// comparison before it passing; `resume_all(reason)` renamed
// `lift_all(reason)` in `quiesced_busy` — the runtime by-hand comparison.
#[test]
fn the_quiesced_refusal_names_holder_and_since_and_the_by_hand_lift() {
    let dir = root("quiesced-message");
    let c = fresh_cauldron(&dir);
    let token = c.quiesce().expect("quiesce");
    let standing = quiescers(&dir);
    assert_eq!(standing.len(), 1);

    let message = match create(&c, "during") {
        Err(SpineError::Busy { message }) => message,
        other => panic!("a quiesced cauldron refuses Busy, got {other:?}"),
    };
    assert!(message.contains("quiesced"), "{message}");
    assert!(message.contains("1 quiescer(s)"), "counts them: {message}");
    assert!(
        message.contains(&format!("{} since {}", standing[0].holder, standing[0].since)),
        "names holder and since as recorded ({:?}): {message}",
        standing[0]
    );
    const BY_HAND: &str = "`resume_all(reason)`";
    const TOKEN: &str = "`resume`";
    // The token lift is named by its OWN backticked name. A bare
    // `contains("resume")` was satisfied by the
    // `resume_all` the by-hand check requires, so it could not fail on its
    // own. A later sentinel compared two string
    // LITERALS — a compile-time fact, true on every run, that could not fail.
    // Now both checks are asked of RUNTIME values: every
    // by-hand mention is cut out of the message the spine returned, the
    // by-hand check is that the cut removed something (message vs. message
    // minus the by-hand name), and the token check is asked of what is left
    // — so the by-hand lift's name cannot witness it by construction.
    let without_by_hand = message.replace(BY_HAND, "");
    assert_ne!(
        without_by_hand, message,
        "names the by-hand lift {BY_HAND}: cutting it out of the message changed nothing"
    );
    assert!(
        without_by_hand.contains(TOKEN),
        "and the token lift {TOKEN}, outside the by-hand lift's name: {message}"
    );

    assert!(c.resume(&token).expect("resume"), "the token lift still works");
    assert!(quiescers(&dir).is_empty());
}

// ===========================================================================
// helpers
// ===========================================================================

fn root(name: &str) -> PathBuf {
    let dir = test_scratch::tmp_path(format!("spine-seams-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn fresh_cauldron(dir: &Path) -> Cauldron {
    Cauldron::create(
        dir,
        &[fixtures::fixture_alpha(), fixtures::fixture_beta()],
        SpineConfig::default(),
    )
    .expect("create the cauldron")
}

/// Read the quiescers table directly — the spine's own record of who paused
/// and since when, independent of anything `resume_all` reports.
fn quiescers(dir: &Path) -> Vec<Quiescer> {
    let conn = rusqlite::Connection::open(dir.join(SPINE_FILE)).expect("spine");
    let mut stmt = conn
        .prepare("SELECT nonce, holder, since FROM quiescers ORDER BY since, nonce")
        .expect("prepare");
    let rows = stmt
        .query_map([], |r| {
            Ok(Quiescer {
                nonce: r.get(0)?,
                holder: r.get(1)?,
                since: r.get(2)?,
            })
        })
        .expect("query");
    rows.map(|r| r.expect("row")).collect()
}

fn create(c: &Cauldron, title: &str) -> Result<Outcome, SpineError> {
    c.submit(Envelope {
        statement: format!(r#"CREATE (d: fixture-alpha/doc {{ title: "{title}", state: "draft" }})"#),
        bindings: Bindings::new(),
        format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
        provenance: Provenance {
            initiator: Initiator::Agent {
                thread: "seams".into(),
                dispatch: "seams-test".into(),
            },
            instrument: Instrument::door(),
        },
        mode: Mode::Commit,
        idempotency_nonce: None,
    })
}
