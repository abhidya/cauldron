//! Records a ripple wrote: every read shows them marked `unreviewed: true`,
//! `review` filters on that, only the owner's command line approves them
//! (the `APPROVE` seam verb with the approval key; no MCP tool, no ripple),
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

    // No MCP session can approve: there is no such tool.
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "soup_approve", "arguments": {"cauldron": "kb", "ids": [id], "_ctx": c}}});
    let no_tool: Value = serde_json::from_str(&svc.post(&body.to_string(), &[]).body).unwrap();
    assert_eq!(no_tool["error"]["message"], json!("unknown tool: soup_approve"), "{no_tool}");
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
    assert_eq!(got["records"][0]["meta"]["approved"]["by"], json!("owner"));
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
    assert_eq!(rec["meta"]["rejected"]["by"], json!("owner"));
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
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "soup_reject", "arguments": {"cauldron": "kb", "dump": "DM-2", "_ctx": c}}});
    let no_tool: Value = serde_json::from_str(&svc.post(&body.to_string(), &[]).body).unwrap();
    assert_eq!(no_tool["error"]["message"], json!("unknown tool: soup_reject"), "{no_tool}");
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
    // Nothing a session can call decides a record.
    for name in ["soup_approve", "soup_reject", "soup_unreject", "soup_unapprove"] {
        assert!(soup_door::catalogue::find(name).is_none(), "{name}");
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
    assert_eq!((get(&a[0])["meta"]["approved"]["by"].clone(), get(&a[1])["meta"]["rejected"]["reason"].clone()), (json!("owner"), json!("dup")));
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
