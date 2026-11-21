//! Records a ripple wrote: every read shows them marked `unreviewed: true`,
//! `review` filters on that, only the owner's side approves them (the seam
//! verbs with the approval key, or the review tools where the owner opened
//! `approve`; never a ripple),
//! and a store with no ripple record reads exactly as before.

use serde_json::{json, Value};
use soup_door::grants::Scope;
use soup_door::reads::{self, ReadOpts};
use spine::envelope::{Envelope, Initiator, Instrument, Mode, Provenance, RippleKind, ENVELOPE_FORMAT_VERSION};

use crate::support::*;

/// One idea in R-1, written by a ripple straight through the spine (what
/// the door's ripple seam does, without the door). Answers its id.
pub fn ripple_idea(s: &Scratch, c: &str, handle: &str) -> String {
    let cauldron = spine::Cauldron::open(&s.soup(c), spine::config::SpineConfig::default()).unwrap();
    let env = Envelope {
        statement: "MATCH (r: ws/region { id: $r }) CREATE (i: ws/idea { state: \"idea\", handle: $h, meta: $m }) CREATE (i) -[in_region]-> (r) RETURN i.id".into(),
        bindings: [("r".to_string(), json!("R-1")), ("h".to_string(), json!(handle)), ("m".to_string(), json!({"cites": [{"kind": "dump", "id": "DM-1"}]}))]
            .into_iter()
            .collect(),
        format_version: ENVELOPE_FORMAT_VERSION,
        provenance: Provenance {
            initiator: Initiator::Ripple { kind: RippleKind::TypeTwo, run_id: format!("run-{handle}"), step_id: "put".into(), scope: vec![] },
            instrument: Instrument { name: "test".into(), version: "0".into() },
        },
        mode: Mode::Commit,
        idempotency_nonce: Some(format!("review-{handle}")),
    };
    match cauldron.submit(env).unwrap() {
        spine::Outcome::Committed { returned, .. } => returned.0[0]["i.id"].as_str().unwrap().to_string(),
        other => panic!("{other:?}"),
    }
}

fn ids(list: &Value) -> Vec<String> {
    list.as_array().unwrap().iter().map(|n| n["id"].as_str().unwrap().to_string()).collect()
}

#[test]
fn rv1_a_ripple_record_is_shown_marked_filtered_and_approved_by_the_owner_side() {
    let s = Scratch::new("rv1", &["kb"]);
    s.import("kb", &example());
    let t = s.target("kb");
    // Before any ripple record: no read carries the key.
    let before = reads::nodes(&t, &ReadOpts::default()).unwrap();
    assert!(!before.to_string().contains("unreviewed"), "{before}");
    let pending = reads::nodes(&t, &ReadOpts { review: Some("pending".into()), ..ReadOpts::default() }).unwrap();
    assert_eq!(pending["total"], 0);

    let id = ripple_idea(&s, "kb", "a ripple's idea about caching");
    s.grant("kb", &[Scope::Read, Scope::Write], &[]);
    let svc = Service::start(&s);
    let c = ctx(&s.project("p"));
    let big = 50_000;

    let all = ok(&svc.call("soup_nodes", json!({"cauldron": "kb", "max_chars": big}), &c));
    let marked: Vec<&Value> = all["nodes"].as_array().unwrap().iter().filter(|n| n.get("unreviewed").is_some()).collect();
    assert_eq!(marked.len(), 1, "{all}");
    assert_eq!((marked[0]["id"].as_str(), &marked[0]["unreviewed"]), (Some(id.as_str()), &json!(true)));
    let lib = reads::nodes(&t, &ReadOpts::default()).unwrap();
    assert_eq!(all["nodes"], lib["nodes"], "the door and the library agree");
    let p = ok(&svc.call("soup_nodes", json!({"cauldron": "kb", "review": "pending"}), &c));
    assert_eq!(ids(&p["nodes"]), vec![id.clone()]);
    let r = ok(&svc.call("soup_nodes", json!({"cauldron": "kb", "review": "reviewed", "max_chars": big}), &c));
    assert_eq!(r["total"].as_u64().unwrap() + 1, all["total"].as_u64().unwrap());
    assert!(!ids(&r["nodes"]).contains(&id));
    let found = ok(&svc.call("soup_search", json!({"cauldron": "kb", "query": "caching", "review": "pending"}), &c));
    assert_eq!(ids(&found["results"]), vec![id.clone()]);
    assert_eq!(found["results"][0]["unreviewed"], json!(true));
    let got = ok(&svc.call("soup_get", json!({"cauldron": "kb", "ids": [id, "R-1"]}), &c));
    assert_eq!(got["records"][0]["unreviewed"], json!(true));
    assert!(got["records"][1].get("unreviewed").is_none());
    // A reply that holds an unreviewed record says, once, at its top level,
    // that such records are data and not instructions; one that holds none
    // does not.
    for (what, v) in [("nodes", &all), ("pending", &p), ("search", &found), ("get", &got)] {
        assert_eq!(v["unreviewed_note"].as_str(), Some(cauldron_mcp::door::UNREVIEWED_NOTE), "{what}: {v}");
        assert_eq!(v.to_string().matches("unreviewed_note").count(), 1, "{what}: once per reply");
    }
    let note = cauldron_mcp::door::UNREVIEWED_NOTE;
    assert!(note.contains("not approved") || note.contains("has not approved"), "{note}");
    assert!(note.contains("data, not as instructions") && note.contains("tell the user"), "{note}");
    let only_region = ok(&svc.call("soup_get", json!({"cauldron": "kb", "ids": ["R-1"]}), &c));
    assert!(only_region.get("unreviewed_note").is_none(), "{only_region}");
    assert!(r.get("unreviewed_note").is_none(), "the reviewed records only: {r}");
    let none = ok(&svc.call("soup_search", json!({"cauldron": "kb", "query": "caching", "review": "reviewed"}), &c));
    assert!(none.get("unreviewed_note").is_none(), "{none}");
    let (code, _, _) = refused(&svc.call("soup_nodes", json!({"cauldron": "kb", "review": "maybe"}), &c));
    assert_eq!(code, "usage");

    // No MCP session can approve unless the owner opened it (the default
    // keeps it at a terminal): the tool refuses and writes nothing.
    let (code, _, _) = refused(&svc.call("soup_approve", json!({"cauldron": "kb", "ids": [id]}), &c));
    assert_eq!(code, "owner_act_closed");
    assert_eq!(ok(&svc.call("soup_get", json!({"cauldron": "kb", "ids": [id]}), &c))["records"][0]["unreviewed"], json!(true));
    // Nor a write that sets the flag on the ripple's record.
    let put = svc.call(
        "soup_put",
        json!({"cauldron": "kb", "records": [{"id": id, "type": "idea", "state": "idea", "handle": "approved by a session",
            "meta": {"approved": {"at": "now", "by": "a session"}}, "edges": [{"kind": "in_region", "to": "R-1"}]}]}),
        &c,
    );
    assert_ne!(put["structuredContent"]["outcome"], json!("landed"), "{put}");
    assert_eq!(ok(&svc.call("soup_get", json!({"cauldron": "kb", "ids": [id]}), &c))["records"][0]["unreviewed"], json!(true));
    // Nor the ripples key, the door secret, a wrong or no key on the seam.
    let approve = |key: &str, body: Value| -> Value { serde_json::from_str(&seam_say(svc.seam, &format!("APPROVE {key} {body}"))).unwrap() };
    let owner = std::fs::read_to_string(s.mcp_dir().join("approve.secret")).unwrap().trim().to_string();
    let ripples = std::fs::read_to_string(s.mcp_dir().join("ripples.secret")).unwrap().trim().to_string();
    let door = std::fs::read_to_string(s.mcp_dir().join("door.secret")).unwrap().trim().to_string();
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(s.mcp_dir().join("approve.secret")).unwrap().permissions().mode() & 0o777, 0o600);
    }
    assert!(owner != ripples && owner != door);
    for k in [ripples.as_str(), door.as_str(), "0000", ""] {
        let v = approve(k, json!({"cauldron": "kb", "ids": [id]}));
        assert_eq!((v["ok"].clone(), v["code"].clone()), (json!(false), json!("unauthorized")), "{v}");
    }
    assert_eq!(ok(&svc.call("soup_get", json!({"cauldron": "kb", "ids": [id]}), &c))["records"][0]["unreviewed"], json!(true));

    // The owner's key approves it; a non-ripple record and an unknown id are skipped.
    let a = approve(&owner, json!({"cauldron": "kb", "ids": [id, "R-1", "C-999"]}));
    assert_eq!((a["ok"].clone(), a["approved"].clone()), (json!(true), json!([id])), "{a}");
    assert_eq!(a["skipped"].as_array().unwrap().len(), 2, "{a}");
    let got = ok(&svc.call("soup_get", json!({"cauldron": "kb", "ids": [id]}), &c));
    assert!(got["records"][0].get("unreviewed").is_none(), "{got}");
    assert_eq!(got["records"][0]["meta"]["approved"]["by"], json!("tty"));
    let p = ok(&svc.call("soup_nodes", json!({"cauldron": "kb", "review": "pending"}), &c));
    assert_eq!(p["total"], 0);
    let again = approve(&owner, json!({"cauldron": "kb", "ids": [id]}));
    assert_eq!(again["outcome"], "nothing-to-approve");
    // Audited, as the owner's command line.
    let line = s.calls().into_iter().rev().find(|l| l["tool"] == "soup_approve" && l["outcome"] == "approved").expect("audited");
    assert_eq!((line["client"].clone(), line["counts"]["approved"].clone(), line["counts"]["skipped"].clone()), (json!("cli"), json!(1), json!(2)), "{line}");
    assert!(line["tx"].is_string(), "{line}");
    // Unapproved: unreviewed again, and audited.
    let u = approve(&owner, json!({"cauldron": "kb", "ids": [id], "unapprove": true}));
    assert_eq!((u["ok"].clone(), u["unapproved"].clone()), (json!(true), json!([id])), "{u}");
    assert_eq!(ok(&svc.call("soup_get", json!({"cauldron": "kb", "ids": [id]}), &c))["records"][0]["unreviewed"], json!(true));
    assert!(s.calls().iter().any(|l| l["tool"] == "soup_unapprove" && l["outcome"] == "unapproved"));
    let u = approve(&owner, json!({"cauldron": "kb", "ids": [id], "unapprove": true}));
    assert_eq!(u["outcome"], "nothing-to-unapprove");
    // A cauldron not granted write cannot be approved in.
    s.grant("kb", &[Scope::Read], &[]);
    let v = approve(&owner, json!({"cauldron": "kb", "ids": [id]}));
    assert_eq!(v["code"], "not_granted", "{v}");
}

/// Records written by one ripple run, straight through the spine, each
/// citing `dump` and carrying `meta.ripple` (as the door's `RPUT` lands
/// them). Answers their ids in order.
pub fn ripple_batch(s: &Scratch, c: &str, run: &str, dump: &str, handles: &[&str]) -> Vec<String> {
    let cauldron = spine::Cauldron::open(&s.soup(c), spine::config::SpineConfig::default()).unwrap();
    let mut st = vec!["MATCH (r: ws/region { id: $r })".to_string()];
    let mut b: std::collections::BTreeMap<String, Value> = std::collections::BTreeMap::new();
    b.insert("r".into(), json!("R-1"));
    b.insert("m".into(), json!({"cites": [{"kind": "dump", "id": dump}], "ripple": {"package": "dump", "version": "v1", "run": run, "model": "sonnet"}}));
    let mut ret = vec![];
    for (i, h) in handles.iter().enumerate() {
        st.push(format!("CREATE (n{i}: ws/idea {{ state: \"idea\", handle: $h{i}, meta: $m }})"));
        st.push(format!("CREATE (n{i}) -[in_region]-> (r)"));
        b.insert(format!("h{i}"), json!(h));
        ret.push(format!("n{i}.id"));
    }
    st.push(format!("RETURN {}", ret.join(", ")));
    let env = Envelope {
        statement: st.join("\n"),
        bindings: b,
        format_version: ENVELOPE_FORMAT_VERSION,
        provenance: Provenance {
            initiator: Initiator::Ripple { kind: RippleKind::TypeTwo, run_id: run.into(), step_id: "put".into(), scope: vec![] },
            instrument: Instrument { name: "test".into(), version: "0".into() },
        },
        mode: Mode::Commit,
        idempotency_nonce: Some(format!("batch-{run}")),
    };
    match cauldron.submit(env).unwrap() {
        spine::Outcome::Committed { returned, .. } => ret.iter().map(|k| returned.0[0][k.as_str()].as_str().unwrap().to_string()).collect(),
        other => panic!("{other:?}"),
    }
}

/// **Approve and reject, in bulk, through the owner's verb.** By dump, by
/// run and by id; a rejected record is kept (get answers it, marked, with
/// its cite and the reason), left out of nodes, search and a region's
/// members (counted in `rejected_hidden`, listed with `include_rejected`)
/// and out of the queue; both decisions are idempotent and exclude each
/// other; every decision is audited without its text; only the owner's key
/// decides, and no session writes a review flag.
#[test]
fn rv2_bulk_approve_and_reject_through_the_owners_verb() {
    let s = Scratch::new("rv2", &["kb"]);
    s.import("kb", &example());
    let a = ripple_batch(&s, "kb", "run-a", "DM-1", &["alpha caching idea", "beta caching idea"]);
    let b = ripple_batch(&s, "kb", "run-b", "DM-2", &["gamma caching idea", "delta caching idea"]);
    s.grant("kb", &[Scope::Read, Scope::Write], &[]);
    let svc = Service::start(&s);
    let c = ctx(&s.project("p"));
    let owner = std::fs::read_to_string(s.mcp_dir().join("approve.secret")).unwrap().trim().to_string();
    let say = |key: &str, body: Value| -> Value { serde_json::from_str(&seam_say(svc.seam, &format!("DECIDE {key} {body}"))).unwrap() };
    let earlier = |key: &str, body: Value| -> Value { serde_json::from_str(&seam_say(svc.seam, &format!("APPROVE {key} {body}"))).unwrap() };
    let get = |id: &str| ok(&svc.call("soup_get", json!({"cauldron": "kb", "ids": [id]}), &c))["records"][0].clone();
    let pending = || ids(&ok(&svc.call("soup_nodes", json!({"cauldron": "kb", "review": "pending"}), &c))["nodes"]);

    // Reject a whole dump, with a reason.
    let r = say(&owner, json!({"cauldron": "kb", "dump": "DM-1", "action": "reject", "reason": "not mine\nat all"}));
    assert_eq!((r["ok"].clone(), r["outcome"].clone(), r["selected"].clone()), (json!(true), json!("rejected"), json!(2)), "{r}");
    let mut got: Vec<String> = r["rejected"].as_array().unwrap().iter().map(|x| x.as_str().unwrap().to_string()).collect();
    got.sort();
    let mut want = a.clone();
    want.sort();
    assert_eq!(got, want);
    // Kept: get answers it, marked, with its cite and the decision.
    let rec = get(&a[0]);
    assert_eq!(rec["rejected"], json!(true), "{rec}");
    assert!(rec.get("unreviewed").is_none(), "{rec}");
    assert_eq!(rec["meta"]["rejected"]["by"], json!("tty"));
    assert_eq!(rec["meta"]["rejected"]["reason"], json!("not mine at all"), "one line");
    assert!(rec["meta"]["cites"].as_array().unwrap().iter().any(|x| x["kind"] == "dump" && x["id"] == "DM-1"), "{rec}");
    // Out of the listing reads, counted; listed when asked for.
    let all = ok(&svc.call("soup_nodes", json!({"cauldron": "kb", "max_chars": 50000}), &c));
    assert!(!ids(&all["nodes"]).contains(&a[0]), "{all}");
    assert_eq!(all["rejected_hidden"], json!(2), "{all}");
    let with = ok(&svc.call("soup_nodes", json!({"cauldron": "kb", "include_rejected": true, "max_chars": 50000}), &c));
    let listed = with["nodes"].as_array().unwrap().iter().find(|n| n["id"] == a[0].as_str()).expect("listed");
    assert_eq!(listed["rejected"], json!(true));
    assert!(with.get("rejected_hidden").is_none());
    let found = ok(&svc.call("soup_search", json!({"cauldron": "kb", "query": "caching"}), &c));
    assert_eq!((found["total"].clone(), found["rejected_hidden"].clone()), (json!(2), json!(2)), "{found}");
    let region = ok(&svc.call("soup_region", json!({"cauldron": "kb", "id": "R-1", "limit": 100}), &c));
    assert_eq!(region["rejected_hidden"], json!(2), "{region}");
    assert_eq!(region["members"].as_array().unwrap().iter().filter(|m| m["rejected"] == json!(true)).count(), 0);
    let mut p = pending();
    p.sort();
    let mut bs = b.clone();
    bs.sort();
    assert_eq!(p, bs, "the review filter no longer counts them");
    // Idempotent.
    let again = say(&owner, json!({"cauldron": "kb", "dump": "DM-1", "action": "reject"}));
    assert_eq!((again["outcome"].clone(), again["skipped"].as_array().unwrap().len()), (json!("nothing-to-reject"), 2), "{again}");
    assert!(again["skipped"].as_array().unwrap().iter().all(|x| x["why"] == "already rejected"));
    // Not approvable by accident: refused until unrejected.
    let ap = say(&owner, json!({"cauldron": "kb", "ids": [a[0]], "action": "approve"}));
    assert_eq!(ap["outcome"], json!("nothing-to-approve"), "{ap}");
    assert!(ap["skipped"][0]["why"].as_str().unwrap().contains("--unreject"), "{ap}");
    let ap = earlier(&owner, json!({"cauldron": "kb", "ids": [a[0]]}));
    assert_eq!(ap["outcome"], json!("nothing-to-approve"), "an earlier command line's body too: {ap}");
    assert_eq!(get(&a[0])["rejected"], json!(true));

    // Approve a run; then a rejection of it is refused until unapproved.
    let ap = say(&owner, json!({"cauldron": "kb", "run": "run-b", "action": "approve"}));
    assert_eq!((ap["outcome"].clone(), ap["approved"].as_array().unwrap().len()), (json!("approved"), 2), "{ap}");
    assert!(pending().is_empty());
    let rj = say(&owner, json!({"cauldron": "kb", "ids": [b[0]], "action": "reject"}));
    assert_eq!(rj["outcome"], json!("nothing-to-reject"), "{rj}");
    assert!(rj["skipped"][0]["why"].as_str().unwrap().contains("--unapprove"), "{rj}");
    // Unreject one: unreviewed again, back in the reads; then approvable.
    let un = say(&owner, json!({"cauldron": "kb", "ids": [a[0]], "action": "unreject"}));
    assert_eq!((un["outcome"].clone(), un["unrejected"].clone()), (json!("unrejected"), json!([a[0]])), "{un}");
    assert_eq!(get(&a[0])["unreviewed"], json!(true));
    assert_eq!(pending(), vec![a[0].clone()]);
    let un = say(&owner, json!({"cauldron": "kb", "ids": [a[0]], "action": "unreject"}));
    assert_eq!(un["outcome"], json!("nothing-to-unreject"));
    assert_eq!(say(&owner, json!({"cauldron": "kb", "ids": [a[0]], "action": "approve"}))["outcome"], json!("approved"));
    // Unapprove by dump.
    let ua = say(&owner, json!({"cauldron": "kb", "dump": "DM-2", "action": "unapprove"}));
    assert_eq!(ua["unapproved"].as_array().unwrap().len(), 2, "{ua}");
    // A dump or run with nothing: said so.
    let none = say(&owner, json!({"cauldron": "kb", "dump": "DM-9", "action": "approve"}));
    assert_eq!((none["outcome"].clone(), none["selected"].clone()), (json!("nothing-to-approve"), json!(0)), "{none}");
    assert!(none["note"].as_str().unwrap().contains("no record a ripple wrote"), "{none}");

    // Usage: one selector, a known action, a reason only on reject.
    for (body, says) in [
        (json!({"cauldron": "kb", "dump": "DM-1", "run": "run-a", "action": "approve"}), "one way"),
        (json!({"cauldron": "kb", "action": "approve"}), "one way"),
        (json!({"cauldron": "kb", "dump": "DM-1"}), "action"),
        (json!({"cauldron": "kb", "dump": "DM-1", "action": "delete"}), "action"),
        (json!({"cauldron": "kb", "dump": "D1", "action": "approve"}), "DM-<n>"),
        (json!({"cauldron": "kb", "run": "a b", "action": "approve"}), "plain word"),
        (json!({"cauldron": "kb", "dump": "DM-1", "action": "approve", "reason": "x"}), "with a rejection only"),
        (json!({"cauldron": "kb", "dump": "DM-1", "action": "reject", "reason": "x".repeat(501)}), "500"),
    ] {
        let v = say(&owner, body.clone());
        assert_eq!((v["ok"].clone(), v["code"].clone()), (json!(false), json!("usage")), "{body}: {v}");
        assert!(v["message"].as_str().unwrap().contains(says), "{body}: {v}");
    }
    // Only the owner's key.
    let ripples = std::fs::read_to_string(s.mcp_dir().join("ripples.secret")).unwrap().trim().to_string();
    for k in [ripples.as_str(), "", "0000"] {
        let v = say(k, json!({"cauldron": "kb", "dump": "DM-2", "action": "reject"}));
        assert_eq!(v["code"], json!("unauthorized"), "{v}");
    }
    // Audited: the tool, the selector and the counts, never the reason.
    let calls = s.calls();
    let line = calls.iter().find(|l| l["tool"] == "soup_reject" && l["outcome"] == "rejected").expect("audited");
    assert_eq!((line["client"].clone(), line["counts"]["selector"].clone(), line["counts"]["rejected"].clone(), line["dump"].clone()), (json!("cli"), json!("dump"), json!(2), json!("DM-1")), "{line}");
    assert!(line["tx"].is_string());
    for t in ["soup_approve", "soup_unapprove", "soup_unreject"] {
        assert!(calls.iter().any(|l| l["tool"] == t), "{t}");
    }
    let audit = std::fs::read_to_string(s.mcp_dir().join("calls.jsonl")).unwrap();
    assert!(!audit.contains("not mine"), "the reason is not audited");

    // No session writes a review flag, on any record.
    for k in ["approved", "rejected"] {
        let put = svc.call(
            "soup_put",
            json!({"cauldron": "kb", "records": [{"id": "I-900", "type": "idea", "state": "idea", "handle": "a session's own",
                "meta": {k: true}, "edges": [{"kind": "in_region", "to": "R-1"}]}], "remint": true}),
            &c,
        );
        let (code, msg, _) = refused(&put);
        assert_eq!(code, "usage", "{put}");
        assert!(msg.contains(&format!("meta.{k}")), "{msg}");
    }
    let (code, _, _) = refused(&svc.call("soup_reject", json!({"cauldron": "kb", "dump": "DM-2"}), &c));
    assert_eq!(code, "owner_act_closed", "closed by default");
}

/// **`soup_review`**, read-only: the same queue as the library (and the
/// CLI), with the unreviewed note, bounded and paged; it needs only `read`;
/// no tool approves or rejects.
#[test]
fn rv3_soup_review_shows_the_queue_and_decides_nothing() {
    let s = Scratch::new("rv3", &["kb"]);
    s.import("kb", &example());
    let a = ripple_batch(&s, "kb", "run-a", "DM-1", &["alpha", "beta"]);
    let b = ripple_batch(&s, "kb", "run-b", "DM-2", &["gamma"]);
    s.grant("kb", &[Scope::Read], &[]);
    let svc = Service::start(&s);
    let c = ctx(&s.project("p"));
    let q = ok(&svc.call("soup_review", json!({"cauldron": "kb"}), &c));
    assert_eq!((q["counts"]["unreviewed"].clone(), q["counts"]["groups"].clone()), (json!(3), json!(2)), "{q}");
    assert_eq!((q["groups"][0]["dump"].clone(), q["groups"][0]["records"][0]["id"].clone()), (json!("DM-2"), json!(b[0])), "{q}");
    assert_eq!(q["unreviewed_note"].as_str(), Some(cauldron_mcp::door::UNREVIEWED_NOTE));
    assert_eq!(q["groups"][1]["commands"]["approve"], json!("cauldron ripples approve --dump DM-1 --env dev --cauldron kb"));
    let lib = soup_door::review::queue(&s.target("kb"), &soup_door::review::QueueOpts::default()).unwrap();
    assert_eq!(q["groups"], lib["groups"], "the door and the library agree");
    let f = ok(&svc.call("soup_review", json!({"cauldron": "kb", "dump": "DM-1"}), &c));
    assert_eq!(f["counts"]["unreviewed"], 2);
    // Bounded and paged.
    let mut seen = vec![];
    let mut after: Option<String> = None;
    loop {
        let mut args = json!({"cauldron": "kb", "max_chars": 2000});
        if let Some(a) = &after {
            args["after"] = json!(a);
        }
        let p = ok(&svc.call("soup_review", args, &c));
        assert!(serde_json::to_string(&p).unwrap().chars().count() <= 2000, "{p}");
        for g in p["groups"].as_array().unwrap() {
            for r in g["records"].as_array().unwrap() {
                seen.push(r["id"].as_str().unwrap().to_string());
            }
        }
        match p["next_after"].as_str() {
            Some(n) => after = Some(n.to_string()),
            None => break,
        }
    }
    assert_eq!(seen.len(), 3, "{seen:?}");
    let (code, msg, _) = refused(&svc.call("soup_review", json!({"cauldron": "kb", "after": "C-999"}), &c));
    assert_eq!(code, "usage", "{msg}");
    let (code, _, _) = refused(&svc.call("soup_review", json!({"cauldron": "kb", "offset": 1}), &c));
    assert_eq!(code, "usage", "no offset: the cursor is `after`");
    assert!(seen.contains(&a[0]) && seen.contains(&a[1]));
    let (code, msg, _) = refused(&svc.call("soup_review", json!({"cauldron": "kb", "max_chars": 1999}), &c));
    assert_eq!(code, "usage", "under the queue's floor: {msg}");
    assert!(msg.contains("under 2000"), "{msg}");
    let (code, _, _) = refused(&svc.call("soup_review", json!({"cauldron": "kb", "since": "soon"}), &c));
    assert_eq!(code, "usage");
    // A session decides only through the two review tools (scope `write`,
    // refused unless the owner opened `approve`; rv6), never through a read.
    for name in ["soup_unreject", "soup_unapprove"] {
        assert!(soup_door::catalogue::find(name).is_none(), "{name}");
    }
    for name in ["soup_approve", "soup_reject"] {
        assert_eq!(soup_door::catalogue::find(name).unwrap().needs, soup_door::catalogue::Needs::Scope(Scope::Write), "{name}");
    }
    assert_eq!(soup_door::catalogue::find("soup_review").unwrap().needs, soup_door::catalogue::Needs::Scope(Scope::Read));
    assert_eq!(ok(&svc.call("soup_review", json!({"cauldron": "kb"}), &c))["counts"]["unreviewed"], 3);
}

/// **Two verbs, each read for what it is.** `APPROVE` is the earlier
/// command line's body only (`cauldron`, `ids`, `unapprove`): a body that
/// carries anything a later command line adds (`action`, `dump`, `run`,
/// `reason`, ...) is refused and writes nothing, so it is never read as an
/// approval. `DECIDE` refuses a key it does not know. One `DECIDE` can
/// approve some records and reject others in one statement, and with
/// `all_or_nothing` writes nothing when any record named is not as listed.
/// Every audit line names the act it was.
#[test]
fn rv4_approve_is_the_earlier_body_only_and_decide_names_its_act() {
    let s = Scratch::new("rv4", &["kb"]);
    s.import("kb", &example());
    let a = ripple_batch(&s, "kb", "run-a", "DM-1", &["alpha", "beta", "gamma"]);
    s.grant("kb", &[Scope::Read, Scope::Write], &[]);
    let svc = Service::start(&s);
    let c = ctx(&s.project("p"));
    let owner = std::fs::read_to_string(s.mcp_dir().join("approve.secret")).unwrap().trim().to_string();
    let line = |verb: &str, body: Value| -> Value { serde_json::from_str(&seam_say(svc.seam, &format!("{verb} {owner} {body}"))).unwrap() };
    let get = |id: &str| ok(&svc.call("soup_get", json!({"cauldron": "kb", "ids": [id]}), &c))["records"][0].clone();

    // What a newer command line once sent on APPROVE: refused, nothing written.
    for body in [
        json!({"cauldron": "kb", "ids": [a[0]], "action": "reject", "reason": "spam"}),
        json!({"cauldron": "kb", "ids": [a[1]], "action": "unreject"}),
        json!({"cauldron": "kb", "dump": "DM-1", "action": "reject"}),
        json!({"cauldron": "kb", "ids": [a[0]], "from": "cauldron soup approve"}),
    ] {
        let v = line("APPROVE", body.clone());
        assert_eq!((v["ok"].clone(), v["code"].clone()), (json!(false), json!("usage")), "{body}: {v}");
        assert!(v["message"].as_str().unwrap().contains("goes on DECIDE"), "{v}");
    }
    for id in &a {
        assert_eq!(get(id)["unreviewed"], json!(true), "{id}");
    }
    assert!(s.calls().iter().all(|l| l["tool"] == "soup_decide" && l["outcome"] == "usage"), "audited as a decision, not an approval: {:?}", s.calls());
    // DECIDE refuses what it does not read.
    let v = line("DECIDE", json!({"cauldron": "kb", "ids": [a[0]], "action": "reject", "why": "a later field"}));
    assert_eq!(v["code"], json!("usage"), "{v}");
    assert!(v["message"].as_str().unwrap().contains("does not take `why`"), "{v}");
    let v = line("DECIDE", json!({"cauldron": "kb", "ids": [a[0]], "action": "approve", "from": "a script"}));
    assert_eq!(v["code"], json!("usage"), "{v}");
    // The earlier body still approves by id, on APPROVE.
    let v = line("APPROVE", json!({"cauldron": "kb", "ids": [a[2]]}));
    assert_eq!((v["outcome"].clone(), v["approved"].clone()), (json!("approved"), json!([a[2]])), "{v}");
    let v = line("APPROVE", json!({"cauldron": "kb", "ids": [a[2]], "unapprove": true}));
    assert_eq!(v["outcome"], json!("unapproved"), "{v}");

    // Approve some and reject others, in one statement.
    let v = line("DECIDE", json!({"cauldron": "kb", "action": "approve_and_reject", "approve": [a[0]], "reject": [a[1], a[2]], "reason": "dup", "all_or_nothing": true}));
    assert_eq!((v["ok"].clone(), v["outcome"].clone(), v["approved"].clone()), (json!(true), json!("decided"), json!([a[0]])), "{v}");
    assert_eq!(v["rejected"].as_array().unwrap().len(), 2, "{v}");
    assert_eq!((get(&a[0])["meta"]["approved"]["by"].clone(), get(&a[1])["meta"]["rejected"]["reason"].clone()), (json!("tty"), json!("dup")));
    let mixed: Vec<Value> = s.calls().into_iter().filter(|l| l["tool"] == "soup_approve_and_reject").collect();
    assert_eq!(mixed.len(), 1, "one audited act");
    assert_eq!((mixed[0]["counts"]["approved"].clone(), mixed[0]["counts"]["rejected"].clone()), (json!(1), json!(2)), "{}", mixed[0]);
    // All or nothing: one record is no longer as listed, so nothing lands.
    let v = line("DECIDE", json!({"cauldron": "kb", "action": "unreject", "ids": [a[1]]}));
    assert_eq!(v["outcome"], json!("unrejected"), "{v}");
    let v = line("DECIDE", json!({"cauldron": "kb", "action": "approve_and_reject", "approve": [a[1]], "reject": [a[2]], "all_or_nothing": true}));
    assert_eq!((v["ok"].clone(), v["code"].clone()), (json!(false), json!("changed")), "{v}");
    assert_eq!(v["detail"]["skipped"][0]["id"], json!(a[2]), "{v}");
    assert_eq!(get(&a[1])["unreviewed"], json!(true), "nothing of the group was written");
    // Without it, the eligible records land and the rest are skipped with why.
    let v = line("DECIDE", json!({"cauldron": "kb", "action": "approve_and_reject", "approve": [a[1]], "reject": [a[2]]}));
    assert_eq!((v["outcome"].clone(), v["approved"].clone(), v["skipped"].as_array().map(Vec::len)), (json!("decided"), json!([a[1]]), Some(1)), "{v}");
    // Usage of the combined act.
    for (body, says) in [
        (json!({"cauldron": "kb", "action": "approve_and_reject", "approve": [a[0]], "reject": [a[0]]}), "approved and rejected"),
        (json!({"cauldron": "kb", "action": "approve_and_reject", "approve": [a[0]]}), "both"),
        (json!({"cauldron": "kb", "action": "approve_and_reject", "approve": [a[0]], "reject": [a[1]], "ids": [a[2]]}), "does not take `ids`"),
        (json!({"cauldron": "kb", "action": "reject", "ids": [a[0]], "all_or_nothing": "yes"}), "true or false"),
    ] {
        let v = line("DECIDE", body.clone());
        assert_eq!(v["code"], json!("usage"), "{body}: {v}");
        assert!(v["message"].as_str().unwrap().contains(says), "{body}: {v}");
    }
    // A run's decision names the run in its audit line.
    let v = line("DECIDE", json!({"cauldron": "kb", "action": "unapprove", "run": "run-a"}));
    assert_eq!(v["ok"], json!(true), "{v}");
    let l = s.calls().into_iter().rev().find(|l| l["tool"] == "soup_unapprove").unwrap();
    assert_eq!(l["counts"]["run"], json!("run-a"), "{l}");
}

/// **Paging survives decisions.** The owner lists a page, decides every
/// record on it, and asks for the next page with its `next_after`: every
/// record is shown once and none is skipped (paging by a position in a set
/// that decisions shrink skipped about half). A cursor that is no record of
/// the queue is refused.
#[test]
fn rv5_paging_by_a_cursor_skips_nothing_when_records_are_decided_between_pages() {
    let s = Scratch::new("rv5", &["kb"]);
    s.import("kb", &example());
    let mut all = Vec::new();
    for i in 0..6 {
        let hs: Vec<String> = (0..20).map(|j| format!("bulk record {i}-{j} {}", "y".repeat(100))).collect();
        let hr: Vec<&str> = hs.iter().map(String::as_str).collect();
        all.extend(ripple_batch(&s, "kb", &format!("run-{i}"), &format!("DM-{}", i + 1), &hr));
    }
    s.grant("kb", &[Scope::Read, Scope::Write], &[]);
    let svc = Service::start(&s);
    let owner = std::fs::read_to_string(s.mcp_dir().join("approve.secret")).unwrap().trim().to_string();
    let t = s.target("kb");
    let mut seen: Vec<String> = Vec::new();
    let mut after: Option<String> = None;
    let mut pages = 0;
    loop {
        let o = soup_door::review::QueueOpts { max_chars: Some(3000), after: after.clone(), ..Default::default() };
        let p = soup_door::review::queue(&t, &o).unwrap();
        let ids: Vec<String> = p["groups"].as_array().unwrap().iter().flat_map(|g| g["records"].as_array().unwrap().iter().map(|r| r["id"].as_str().unwrap().to_string())).collect();
        assert!(!ids.is_empty(), "{p}");
        seen.extend(ids.iter().cloned());
        pages += 1;
        // Half the pages approved, half rejected, before the next is asked.
        let action = if pages % 2 == 0 { "approve" } else { "reject" };
        let v: Value = serde_json::from_str(&seam_say(svc.seam, &format!("DECIDE {owner} {}", json!({"cauldron": "kb", "action": action, "ids": ids})))).unwrap();
        assert_eq!(v["ok"], json!(true), "{v}");
        match p["next_after"].as_str() {
            Some(n) => after = Some(n.to_string()),
            None => break,
        }
    }
    assert!(pages > 3, "several pages: {pages}");
    let mut a = all.clone();
    a.sort();
    let mut sn = seen.clone();
    sn.sort();
    assert_eq!(sn, a, "every record shown once, none skipped");
    assert_eq!(soup_door::review::waiting(&t).unwrap(), 0, "nothing left unseen");
    let e = soup_door::review::queue(&t, &soup_door::review::QueueOpts { after: Some("C-999".into()), ..Default::default() }).unwrap_err();
    assert_eq!(e.code, soup_door::Code::Usage, "{}", e.message);
    assert!(e.message.contains("not a record of this queue"), "{}", e.message);
    // A cursor at a decided record still pages from where it was.
    let o = soup_door::review::QueueOpts { after: Some(all[0].clone()), ..Default::default() };
    assert_eq!(soup_door::review::queue(&t, &o).unwrap()["shown"], json!(0), "all decided");
}

/// **The door's review tools follow the owner's `owner_acts` choice.**
/// `soup_approve` / `soup_reject` are refused `owner_act_closed` by default
/// (nothing written), and still refused when the service's own environment
/// carries `CAULDRON_OWNER_ACTS` (that variable belongs to the CLI command
/// that carries it, never to a service); allowed when the owner's persisted
/// choice opens `approve` for the cauldron (a cauldron's own `terminal`
/// closes it again);
/// each decision is written `by: "agent"` with the session. A ripple's
/// identity is refused under every setting.
#[test]
fn rv6_the_review_tools_follow_owner_acts_and_never_serve_a_ripple() {
    use env::owner_acts::{Act, Choice, How, Setting};
    let s = Scratch::new("rv6", &["kb"]);
    s.import("kb", &example());
    let ids: Vec<String> = (0..4).map(|i| ripple_idea(&s, "kb", &format!("idea number {i}"))).collect();
    s.grant("kb", &[Scope::Read, Scope::Write], &[]);
    let mut c = ctx(&s.project("p"));
    c["session_id"] = json!("sess-9");
    let ripple = json!({"client": "ripples", "cwd": s.project("p").display().to_string()});

    // Closed: the default.
    let svc = Service::start(&s);
    let (code, msg, _) = refused(&svc.call("soup_approve", json!({"cauldron": "kb", "ids": [ids[0]]}), &c));
    assert_eq!(code, "owner_act_closed", "{msg}");
    assert!(msg.contains("cauldron owner-acts open --only approve"), "{msg}");
    assert_eq!(refused(&svc.call("soup_reject", json!({"cauldron": "kb", "ids": [ids[0]]}), &ripple)).0, "ripple_refused");
    drop(svc);

    // The service's own environment opens nothing.
    let svc = Service::start_with(&s, &[("CAULDRON_OWNER_ACTS", "approve")]);
    let (code, msg, _) = refused(&svc.call("soup_approve", json!({"cauldron": "kb", "ids": [ids[0]]}), &c));
    assert_eq!(code, "owner_act_closed", "the service's variable opened the door: {msg}");
    assert!(msg.contains("CAULDRON_OWNER_ACTS opens nothing here"), "{msg}");
    drop(svc);

    // The persisted choice opens it; a cauldron's own `terminal` closes it.
    let mut set = Setting::default();
    set.set_root(Choice::new(&[Act::Approve], How::Tty));
    env::owner_acts::store(&s.root(), &set).unwrap();
    let svc = Service::start(&s);
    let v = ok(&svc.call("soup_approve", json!({"cauldron": "kb", "ids": [ids[0]]}), &c));
    assert_eq!((v["by"].clone(), v["session"].clone(), v["approved"].clone()), (json!("agent"), json!("sess-9"), json!([ids[0]])), "{v}");
    assert_eq!(refused(&svc.call("soup_approve", json!({"cauldron": "kb", "ids": [ids[1]]}), &ripple)).0, "ripple_refused", "open, and still never a ripple");
    let v = ok(&svc.call("soup_reject", json!({"cauldron": "kb", "ids": [ids[1]], "reason": "off topic"}), &c));
    assert_eq!((v["by"].clone(), v["rejected"].clone()), (json!("agent"), json!([ids[1]])), "{v}");
    let id = s.target("kb").cauldron.id();
    set.set_cauldron("dev", "kb", id, Choice::new(&[], How::Tty));
    env::owner_acts::store(&s.root(), &set).unwrap();
    assert_eq!(refused(&svc.call("soup_approve", json!({"cauldron": "kb", "ids": [ids[2]]}), &c)).0, "owner_act_closed", "read on every call");
    let got = reads::get(&s.target("kb"), &[ids[0].clone(), ids[1].clone()], &ReadOpts::default()).unwrap();
    assert_eq!(got["records"][0]["meta"]["approved"]["by"], json!("agent"), "{got}");
    assert_eq!(got["records"][1]["meta"]["rejected"]["session"], json!("sess-9"), "{got}");
    let audit = s.calls();
    assert!(audit.iter().any(|l| l["tool"] == "soup_reject" && l["by"] == "agent" && l["session"] == "sess-9"), "{audit:?}");
}

/// The published write-back list (`schema/soup/write-back.json`).
fn write_back() -> Value {
    serde_json::from_str(include_str!("../../../../schema/soup/write-back.json")).unwrap()
}

/// `record` with the write-back list's `refused` keys taken out the way a
/// client would, from the list alone (a dotted `meta.<k>` from `meta`), and
/// its `id` replaced by `label` (to land under `remint`).
fn strip_refused(record: &Value, label: &str) -> Value {
    let mut r = record.clone();
    for k in write_back()["refused"].as_array().unwrap() {
        let key = k["key"].as_str().unwrap();
        match key.strip_prefix("meta.") {
            Some(inner) => {
                if let Some(m) = r.get_mut("meta").and_then(Value::as_object_mut) {
                    m.remove(inner);
                }
            }
            None => {
                r.as_object_mut().unwrap().remove(key);
            }
        }
    }
    r["id"] = json!(label);
    r
}

/// **Writing back what you read** (issue #23, items 2.4 and 2.6). Records
/// read with `soup_get` (a superseded record, a ripple's unreviewed record,
/// a rejected one, a cluster with its claims) land again through
/// `soup_put` with only the published list's `refused` keys taken out and
/// a new label: the keys a read adds are dropped and named in `ignored`,
/// and what stays refused is refused by name. A `soup_link` note's bound
/// is in the tool's own schema.
#[test]
fn wb1_a_record_read_with_soup_get_is_written_back_from_the_published_list() {
    let s = Scratch::new("wb1", &["kb"]);
    s.import("kb", &example());
    let rejected = ripple_batch(&s, "kb", "run-a", "DM-1", &["an idea the owner turned down"]);
    let pending = ripple_batch(&s, "kb", "run-b", "DM-2", &["an idea nobody reviewed yet"]);
    s.grant("kb", &[Scope::Read, Scope::Write], &[]);
    let svc = Service::start(&s);
    let c = ctx(&s.project("p"));
    let owner = std::fs::read_to_string(s.mcp_dir().join("approve.secret")).unwrap().trim().to_string();
    let r: Value = serde_json::from_str(&seam_say(svc.seam, &format!("DECIDE {owner} {}", json!({"cauldron": "kb", "dump": "DM-1", "action": "reject"})))).unwrap();
    assert_eq!(r["outcome"], json!("rejected"), "{r}");
    let boundary = "Inside: everything that gets a release to the people who use it, end to end.";
    let put = ok(&svc.call(
        "soup_put",
        json!({"cauldron": "kb", "remint": true, "records": [
            {"id": "old", "type": "idea", "state": "idea", "handle": "the old idea", "edges": [{"kind": "in_region", "to": "R-1"}]},
            {"id": "new", "type": "idea", "state": "idea", "handle": "the new idea",
             "edges": [{"kind": "in_region", "to": "R-1"}, {"kind": "supersedes", "to": "old", "note": "all of it"}]},
            {"id": "k", "type": "cluster", "handle": "the release", "boundary": boundary, "edges": [{"kind": "claims", "to": "R-1"}]}]}),
        &c,
    ));
    let (old, cl) = (put["id_map"]["old"].as_str().unwrap().to_string(), put["id_map"]["k"].as_str().unwrap().to_string());
    let read = ok(&svc.call("soup_get", json!({"cauldron": "kb", "ids": [old, rejected[0], pending[0], cl]}), &c));
    let recs = read["records"].as_array().unwrap().clone();
    assert_eq!(recs.len(), 4, "{read}");
    let by_id = |id: &str| recs.iter().find(|r| r["id"] == id).unwrap().clone();
    assert!(by_id(&old)["superseded_by"].is_array() && by_id(&rejected[0])["rejected"] == json!(true), "{read}");
    assert!(by_id(&pending[0])["unreviewed"] == json!(true) && by_id(&cl)["claims"].is_array(), "{read}");

    // As read, with only a new label: what stays refused is refused by name.
    let relabel = |r: &Value, l: &str| -> Value {
        let mut r = r.clone();
        r["id"] = json!(l);
        r
    };
    let (code, msg, detail) = refused(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "dry_run": true, "records": [relabel(&by_id(&pending[0]), "p")]}), &c));
    assert_eq!((code.as_str(), detail["refused"].as_str()), ("usage", Some("ripple_provenance")), "{msg}");
    let (code, msg, _) = refused(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "dry_run": true, "records": [relabel(&by_id(&rejected[0]), "r")]}), &c));
    assert!(code == "usage" && msg.contains("are the owner's"), "{msg}");

    // From the published list alone: lands, and says what it dropped.
    let back: Vec<Value> = recs.iter().map(|r| strip_refused(r, &format!("rt-{}", r["id"].as_str().unwrap()))).collect();
    let mut copies = json!(null);
    for dry in [true, false] {
        let landed = ok(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "dry_run": dry, "allow_duplicate": true, "records": back}), &c));
        let mut ignored: Vec<String> = landed["ignored"].as_array().unwrap().iter().map(|v| format!("{}:{}", v["record"].as_str().unwrap(), v["key"].as_str().unwrap())).collect();
        ignored.sort();
        let mut want = vec![
            format!("rt-{old}:superseded_by"),
            format!("rt-{}:rejected", rejected[0]),
            format!("rt-{}:unreviewed", pending[0]),
            format!("rt-{cl}:claims"),
        ];
        want.sort();
        assert_eq!(ignored, want, "dry_run {dry}: {landed}");
        assert!(landed["ignored_why"].as_str().unwrap().contains("write-back.json"));
        assert_eq!(landed["id_map"].as_object().unwrap().len(), 4, "dry_run {dry}: {landed}");
        copies = landed["id_map"].clone();
    }
    // Nothing a read adds was stored: the copies are a session's records,
    // neither rejected nor unreviewed nor superseded, and the copied
    // cluster's claims are its edges.
    let ids: Vec<Value> = copies.as_object().unwrap().values().cloned().collect();
    let after = ok(&svc.call("soup_get", json!({"cauldron": "kb", "ids": ids}), &c));
    for r in after["records"].as_array().unwrap() {
        assert!(r.get("rejected").is_none() && r.get("unreviewed").is_none() && r.get("superseded_by").is_none(), "{r}");
        assert!(r["meta"].get("ripple").is_none() && r["meta"].get("rejected").is_none(), "{r}");
        if r["type"] == "cluster" {
            assert_eq!(r["claims"].as_array().unwrap().len(), 1, "{r}");
        }
    }

    // 2.6: the link tool's schema bounds a note, as soup_claim's does.
    let link = soup_door::catalogue::find("soup_link").unwrap().def().input_schema;
    assert_eq!(link["properties"]["links"]["items"]["properties"]["note"]["maxLength"], json!(soup_door::notes::EDGE_NOTE_MAX));
    svc.stop();
}

/// **No session writes `meta.ripple`** (issue #23, item 2.10): `soup_put`
/// refuses it, as `ripple_provenance`, landed or dry, with or without
/// `remint` or `partial`, in every casing and nesting and as a top-level
/// `ripple`; nothing lands. The ripple path still stamps it.
#[test]
fn wb2_no_session_writes_ripple_provenance() {
    let s = Scratch::new("wb2", &["kb"]);
    s.import("kb", &example());
    s.grant("kb", &[Scope::Read, Scope::Write], &[]);
    let svc = Service::start(&s);
    let c = ctx(&s.project("p"));
    let fake = json!({"package": "dump", "version": "v1", "run": "made-up", "model": "opus"});
    let variants = [
        ("meta", json!({"ripple": fake})),
        ("meta", json!({"Ripple": fake})),
        ("meta", json!({"RIPPLE": null})),
        ("meta", json!({"ripple": {}})),
        ("meta", json!({"meta": {"ripple": fake}})),
        ("meta", json!({"meta.ripple": fake})),
        ("Meta", json!({"ripple": fake})),
        ("meta.ripple", fake.clone()),
        ("Meta.Ripple", fake.clone()),
        ("ripple", fake.clone()),
    ];
    let before = s.node_count("kb");
    for (k, m) in &variants {
        let mut rec = json!({"id": "C-2", "type": "idea", "state": "idea", "handle": "a session's own", "edges": [{"kind": "in_region", "to": "R-1"}]});
        rec[*k] = m.clone();
        for flags in [json!({}), json!({"dry_run": true}), json!({"remint": true}), json!({"remint": true, "partial": true}), json!({"remint": true, "dry_run": true})] {
            let mut args = json!({"cauldron": "kb", "records": [rec]});
            for (f, v) in flags.as_object().unwrap() {
                args[f] = v.clone();
            }
            let (code, msg, detail) = refused(&svc.call("soup_put", args, &c));
            assert_eq!((code.as_str(), detail["refused"].as_str()), ("usage", Some("ripple_provenance")), "{k} {m} {flags}: {msg}");
            assert!(msg.contains("refused: ripple_provenance") && msg.contains("Nothing was written"), "{msg}");
        }
    }
    assert_eq!(s.node_count("kb"), before, "nothing landed");
    // A key that only looks like it is no provenance.
    let fine = ok(&svc.call(
        "soup_put",
        json!({"cauldron": "kb", "remint": true, "dry_run": true, "records": [{"id": "x", "type": "idea", "state": "idea", "handle": "about ripples",
            "body": "meta.ripple in the text", "edges": [{"kind": "in_region", "to": "R-1"}]}]}),
        &c,
    ));
    assert_eq!(fine["outcome"], json!("validated"), "{fine}");
    svc.stop();
}

/// **Write-back of records an agent decided, and a citation on them.** With
/// `approve` opened to agents (the service's own environment), one ripple
/// record is approved and one rejected through `soup_approve` /
/// `soup_reject` (`by: "agent"`, with the session). `soup_cite` adds and
/// removes a citation on each and leaves the decision and `meta.ripple` as
/// they were. Read with `soup_get` and written back as read, each is
/// refused by name (the agent's decision is a review flag like a person's,
/// and `meta.ripple` is provenance); stripped from the published list
/// alone, both land, with only the rejected one's top-level `rejected`
/// named as ignored, and the copies carry no decision and no provenance.
#[test]
fn wb3_records_an_agent_decided_write_back_and_take_a_citation_like_any_other() {
    let s = Scratch::new("wb3", &["kb"]);
    s.import("kb", &example());
    let ids = ripple_batch(&s, "kb", "run-a", "DM-1", &["one idea an agent approved", "one idea an agent rejected"]);
    s.grant("kb", &[Scope::Read, Scope::Write], &[]);
    let mut set = env::owner_acts::Setting::default();
    set.set_root(env::owner_acts::Choice::new(&[env::owner_acts::Act::Approve], env::owner_acts::How::Tty));
    env::owner_acts::store(&s.root(), &set).unwrap();
    let svc = Service::start(&s);
    let mut c = ctx(&s.project("p"));
    c["session_id"] = json!("sess-3");
    ok(&svc.call("soup_approve", json!({"cauldron": "kb", "ids": [ids[0]]}), &c));
    ok(&svc.call("soup_reject", json!({"cauldron": "kb", "ids": [ids[1]], "reason": "not this"}), &c));
    let get = |id: &str| ok(&svc.call("soup_get", json!({"cauldron": "kb", "ids": [id]}), &c))["records"][0].clone();
    let trail = |r: &Value| json!([r["meta"]["approved"], r["meta"]["rejected"], r["meta"]["ripple"]]);
    let before: Vec<Value> = ids.iter().map(|id| trail(&get(id))).collect();
    assert_eq!(before[0][0]["by"], "agent", "{before:?}");
    assert_eq!(before[1][1]["session"], "sess-3", "{before:?}");
    for id in &ids {
        let u = json!({"kind": "url", "url": "https://example.com/why"});
        ok(&svc.call("soup_cite", json!({"cauldron": "kb", "id": id, "add": u}), &c));
        ok(&svc.call("soup_cite", json!({"cauldron": "kb", "id": id, "remove": u}), &c));
    }
    let recs: Vec<Value> = ids.iter().map(|id| get(id)).collect();
    assert_eq!(recs.iter().map(trail).collect::<Vec<_>>(), before, "a citation moved no decision and no provenance");

    for (r, l) in recs.iter().zip(["a", "b"]) {
        let mut as_read = r.clone();
        as_read["id"] = json!(l);
        let (code, msg, _) = refused(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "dry_run": true, "records": [as_read]}), &c));
        assert_eq!(code, "usage", "{msg}");
        assert!(msg.contains("meta.approved") || msg.contains("meta.rejected") || msg.contains("ripple"), "{msg}");
    }
    let back: Vec<Value> = recs.iter().zip(["a", "b"]).map(|(r, l)| strip_refused(r, l)).collect();
    let landed = ok(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "records": back}), &c));
    let ignored: Vec<String> = landed["ignored"].as_array().unwrap().iter().map(|v| format!("{}:{}", v["record"].as_str().unwrap(), v["key"].as_str().unwrap())).collect();
    assert_eq!(ignored, ["b:rejected"], "{landed}");
    for l in ["a", "b"] {
        let copy = get(landed["id_map"][l].as_str().unwrap());
        assert_eq!(trail(&copy), json!([null, null, null]), "{copy}");
        assert!(copy.get("rejected").is_none() && copy.get("unreviewed").is_none(), "{copy}");
    }
    svc.stop();
}
