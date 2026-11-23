//! **The exported delta-apply**: the SetField arithmetic as pure
//! functions (`pool::set_target`, `state_after_set`, `field_after_set`) that
//! the landing itself calls, so a fold outside the crate and the landing are
//! ONE builder.
//!
//! Each case is computed by the function AND landed through the door, then
//! read back with `node_view`, AND compared with a hand-written result. The
//! landing calls the same function, so function-vs-landing proves only that
//! nobody re-inlined the arithmetic; the hand-written result is the oracle
//! for the arithmetic itself, and the door's compile check (`set_at_path`
//! directly) is the oracle for the refusal.
//!
//! Every guard here was forced RED once (`// RED:` above each test).

use spine::config::SpineConfig;
use spine::envelope::{Bindings, Envelope, Initiator, Instrument, Mode, Provenance};
use spine::error::SpineError;
use spine::pool::{field_after_set, set_target, state_after_set, NodeId, QualifiedNodeId, SetTarget};
use spine::schema_def::{fixtures, grammar_fault_tag, PoolName};
use spine::{Cauldron, Outcome};
use std::path::{Path, PathBuf};

test_scratch::install!();

const BETA: &str = "fixture-beta";

// RED: `field_after_set` made to REPLACE a non-object instead of refusing —
// the function answered `{"sub":1}` while the door (whose compile check
// calls `set_at_path` directly) refused `not_an_object`: "the function and
// the door disagree".
// RED (opposed): `field_after_set` made to IGNORE the current value (start
// from Null always) — function and landing still agreed (one builder), and
// the hand-written result caught it: "{a:1}, [b]: not the SET's documented
// result" (`{"b":..}` against `{"a":1,"b":..}`).
#[test]
fn field_after_set_is_the_landings_own_arithmetic() {
    let dir = root("arith");
    let c = fresh_cauldron(&dir);
    let v = serde_json::json!({ "x": [1, 2], "y": "z" });

    // (current-before, sub_path)
    // `want` is written out by hand: the landing calls `field_after_set`
    // itself, so function-vs-landing alone is the function compared with
    // itself — a wrong rule both share would agree (see the second RED).
    struct Case {
        name: &'static str,
        start: Start,
        sub: Vec<&'static str>,
        want: serde_json::Value,
    }
    enum Start {
        Absent,
        Set(serde_json::Value),
    }
    let cases = vec![
        Case { name: "absent, []", start: Start::Absent, sub: vec![], want: v.clone() },
        Case {
            name: "absent, [a,b]",
            start: Start::Absent,
            sub: vec!["a", "b"],
            want: serde_json::json!({ "a": { "b": v.clone() } }),
        },
        Case {
            name: "null, [a]",
            start: Start::Set(serde_json::Value::Null),
            sub: vec!["a"],
            want: serde_json::json!({ "a": v.clone() }),
        },
        Case {
            name: "{a:1}, [b]",
            start: Start::Set(serde_json::json!({ "a": 1 })),
            sub: vec!["b"],
            want: serde_json::json!({ "a": 1, "b": v.clone() }),
        },
    ];
    for case in cases {
        let shelf = create_shelf(&c, case.name);
        if let Start::Set(start) = &case.start {
            set_capacity(&c, &shelf, &[], start).expect("seed the starting value");
        }
        let before = capacity(&c, &shelf);
        match (&case.start, &before) {
            (Start::Absent, None) => {}
            (Start::Set(s), Some(b)) => assert_eq!(b, s, "{}: seeded", case.name),
            _ => panic!("{}: the starting state did not take: {before:?}", case.name),
        }
        let sub: Vec<String> = case.sub.iter().map(|s| s.to_string()).collect();
        let computed = field_after_set(before.as_ref(), &sub, &v)
            .unwrap_or_else(|f| panic!("{}: the function refused: {f:?}", case.name));
        set_capacity(&c, &shelf, &sub, &v).expect("the door lands it");
        let landed = capacity(&c, &shelf).expect("the field is present after the SET");
        assert_eq!(
            computed, landed,
            "{}: the function and the landing disagree",
            case.name
        );
        assert_eq!(landed, case.want, "{}: not the SET's documented result", case.name);
    }

    // ("hello", [sub], 1): both refuse, by the same name.
    let shelf = create_shelf(&c, "scalar");
    set_capacity(&c, &shelf, &[], &serde_json::json!("hello")).expect("seed a scalar");
    let one = serde_json::json!(1);
    let sub = vec!["sub".to_string()];
    let function = field_after_set(Some(&serde_json::json!("hello")), &sub, &one);
    let door = set_capacity(&c, &shelf, &sub, &one);
    let door_tag = match &door {
        Err(SpineError::Refused { witnesses }) => witnesses.iter().find_map(grammar_fault_tag),
        _ => None,
    };
    assert_eq!(
        (function.as_ref().err().map(|f| f.tag), door_tag),
        (Some("not_an_object"), Some("not_an_object")),
        "the function and the door disagree: function {function:?}, door {door:?}"
    );
    assert_eq!(capacity(&c, &shelf), Some(serde_json::json!("hello")), "the scalar survives");
    let _ = std::fs::remove_dir_all(&dir);
}

// RED: `set_target` routed any path whose HEAD is `state` to State —
// `["state","x"]` came back `State`.
#[test]
fn set_target_routes_only_the_bare_state_path() {
    let p = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<String>>();
    let bare = p(&["state"]);
    assert_eq!(set_target(&bare), Some(SetTarget::State));
    let dotted = p(&["state", "x"]);
    assert_eq!(
        set_target(&dotted),
        Some(SetTarget::Field { name: "state", sub_path: &dotted[1..] })
    );
    let field = p(&["title"]);
    assert_eq!(set_target(&field), Some(SetTarget::Field { name: "title", sub_path: &[] }));
    assert_eq!(set_target(&[]), None);
}

// RED: `state_after_set` stringified a non-string (`value.to_string()`) —
// `7` came back `Ok("7")`.
#[test]
fn state_after_set_refuses_a_non_string_as_unknown_state() {
    assert_eq!(state_after_set(&serde_json::json!("open")).expect("a string"), "open");
    for bad in [serde_json::json!(7), serde_json::json!(null), serde_json::json!({ "s": 1 })] {
        let f = state_after_set(&bad).expect_err("a non-string state refuses");
        assert_eq!(f.tag, "unknown_state", "{bad}");
    }
}

/// A SOURCE scan of `Pool::apply_part`'s SetField arm (comments stripped, so
/// the arm's own comments cannot satisfy it): it calls the three exported
/// functions and never `set_at_path` itself — one builder.
// RED: the arm's `field_after_set(...)` call replaced by an inline
// `set_at_path(&mut doc, sub_path, value.clone())` — "the SetField arm calls
// `field_after_set(`" failed first. RED (opposed): the `state_after_set`
// call inlined as `value.as_str()...` — "the SetField arm calls
// `state_after_set(`".
#[test]
fn the_landing_calls_the_exported_functions() {
    let src = include_str!("../src/pool.rs");
    let interior = &src[src.find("fn apply_part_interior(").expect("the landing")..];
    let start = interior.find("Delta::SetField {").expect("the SetField arm");
    let end = interior[start..].find("Delta::AddEdge {").expect("the next arm") + start;
    let arm: String = interior[start..end]
        .lines()
        .map(|l| match l.find("//") {
            Some(i) => &l[..i],
            None => l,
        })
        .collect::<Vec<_>>()
        .join("\n");
    for f in ["set_target(", "state_after_set(", "field_after_set("] {
        assert!(arm.contains(f), "the SetField arm calls `{f}`:\n{arm}");
    }
    assert!(!arm.contains("set_at_path("), "the SetField arm calls set_at_path directly:\n{arm}");
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn root(name: &str) -> PathBuf {
    test_scratch::tmp_path(format!("spine-delta-apply-{name}-{}", std::process::id()))
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
                thread: "delta-apply".into(),
                dispatch: "delta-apply".into(),
            },
            instrument: Instrument::door(),
        },
        mode: Mode::Commit,
        idempotency_nonce: None,
    }
}

fn create_shelf(c: &Cauldron, label: &str) -> NodeId {
    match c
        .submit(commit(
            r#"CREATE (s: fixture-beta/shelf { label: $l }) RETURN s.id"#,
            &[("l", serde_json::json!(label))],
        ))
        .expect("create a shelf")
    {
        Outcome::Committed { returned, .. } => {
            NodeId(returned.0[0]["s.id"].as_str().expect("id").to_string())
        }
        other => panic!("expected Committed, got {other:?}"),
    }
}

/// `SET s.capacity[.sub...] = $v` through the door.
fn set_capacity(
    c: &Cauldron,
    shelf: &NodeId,
    sub: &[String],
    v: &serde_json::Value,
) -> Result<Outcome, SpineError> {
    let path: String = sub.iter().map(|k| format!(".{k}")).collect();
    c.submit(commit(
        &format!(r#"MATCH (s: fixture-beta/shelf {{ id: $id }}) SET s.capacity{path} = $v"#),
        &[("id", serde_json::json!(shelf.0)), ("v", v.clone())],
    ))
}

/// `capacity` as `node_view` reads it; `None` when absent.
fn capacity(c: &Cauldron, shelf: &NodeId) -> Option<serde_json::Value> {
    let view = c
        .node_view(&QualifiedNodeId {
            pool: PoolName::new(BETA),
            id: shelf.clone(),
        })
        .expect("node_view")
        .expect("the shelf exists");
    view.record.fields.get("capacity").map(|f| f.value.clone())
}
