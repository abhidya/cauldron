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
