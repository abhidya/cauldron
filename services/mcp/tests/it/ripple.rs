//! The ripple seam: the verbs only the `ripples` service may call. The key
//! (`ripples.secret`) is the only way in; the provenance a ripple's records
//! carry is the door's, whatever the batch says; the stage's limits hold;
//! a run lands once.

use serde_json::{json, Value};
use soup_door::grants::Scope;

use crate::support::*;

/// The ripples key the running service minted.
fn key(s: &Scratch) -> String {
    std::fs::read_to_string(s.mcp_dir().join("ripples.secret")).unwrap().trim().to_string()
}

/// One ripple verb with the right key; the answer's JSON.
fn say(svc: &Service, s: &Scratch, verb: &str, args: Value) -> Value {
    serde_json::from_str(&seam_say(svc.seam, &format!("{verb} {} {args}", key(s)))).unwrap()
}

fn code(v: &Value) -> &str {
    assert_eq!(v["ok"], json!(false), "not refused: {v}");
    v["code"].as_str().unwrap()
}

/// A put of `records` for run `run` on DM-1.
fn put(run: &str, records: Value) -> Value {
    json!({"cauldron": "kb", "dump": "DM-1", "run": run, "step": "put", "package": "dump", "version": "v1", "model": "sonnet", "records": records})
}

fn idea(label: &str, handle: &str) -> Value {
    json!({"id": label, "type": "idea", "state": "idea", "handle": handle, "edges": [{"kind": "in_region", "to": "R-1"}]})
}

/// The raw initiator JSON of every landed ripple transaction.
fn ripple_rows(s: &Scratch) -> Vec<Value> {
    let conn = spine::offline::read_only_connection(&s.soup("kb").join("spine.db")).unwrap();
    let mut st = conn.prepare("SELECT initiator FROM transactions WHERE decided = 1 AND json_extract(initiator, '$.class') = 'ripple'").unwrap();
    let rows = st.query_map([], |r| r.get::<_, String>(0)).unwrap();
    rows.map(|r| serde_json::from_str(&r.unwrap()).unwrap()).collect()
}

#[test]
fn rp1_only_the_ripples_key_opens_the_ripple_verbs() {
    let s = Scratch::new("rp1", &["kb"]);
    s.import("kb", &example());
    s.grant("kb", &[Scope::Read, Scope::Write, Scope::Dump], &[]);
    let svc = Service::start(&s);
    let c = ctx(&s.project("p"));
    ok(&svc.call("soup_dump", json!({"cauldron": "kb", "text": "we should cache the build"}), &c));
    let k = key(&s);
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(std::fs::metadata(s.mcp_dir().join("ripples.secret")).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(k.len(), 64);
    let door_secret = std::fs::read_to_string(s.mcp_dir().join("door.secret")).unwrap().trim().to_string();
    assert_ne!(k, door_secret, "a key of its own");
    let args = json!({"cauldron": "kb", "dump": "DM-1"});
    let before = s.node_count("kb");
    for line in [
        format!("RDUMP {args}"),
        format!("RDUMP  {args}"),
        format!("RDUMP {} {args}", "0".repeat(64)),
        format!("RDUMP {door_secret} {args}"),
        format!("RDUMP {}x {args}", k),
        format!("RPUT {door_secret} {}", put("run-a", json!([idea("n1", "x")]))),
        "RPUT".to_string(),
    ] {
        let v: Value = serde_json::from_str(&seam_say(svc.seam, &line)).unwrap();
        assert_eq!(code(&v), "unauthorized", "{line}");
        assert!(!v.to_string().contains("cache the build"));
    }
    assert_eq!(s.node_count("kb"), before, "nothing landed");
    // The door's tools never show or take the ripple verbs.
    let r = svc.post(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#, &[]);
    assert!(!r.body.contains("RPUT") && !r.body.contains("RDUMP") && !r.body.contains("\"name\":\"ripple"), "{}", r.body);
    // With the key: the dump.
    let d = say(&svc, &s, "RDUMP", args.clone());
    assert_eq!((d["ok"].clone(), d["text"].clone(), d["dump"]["state"].clone()), (json!(true), json!("we should cache the build"), json!("received")));
    // Every call after the key is audited, never with text.
    let calls = s.calls();
    let rl: Vec<&Value> = calls.iter().filter(|l| l["client"] == "ripples").collect();
    assert_eq!(rl.len(), 1, "{calls:?}");
    assert_eq!((rl[0]["tool"].as_str(), rl[0]["outcome"].as_str()), (Some("ripple_dump"), Some("ok")));
    assert!(!std::fs::read_to_string(s.mcp_dir().join("calls.jsonl")).unwrap().contains("cache the build"));
    // A restart mints a new key; the old one is refused.
    drop(svc);
    let svc = Service::start(&s);
    assert_ne!(key(&s), k);
    let v: Value = serde_json::from_str(&seam_say(svc.seam, &format!("RDUMP {k} {args}"))).unwrap();
    assert_eq!(code(&v), "unauthorized");
    // A cauldron without `dump` (or `write`) is closed to a ripple.
    s.grant("kb", &[Scope::Read, Scope::Write], &[]);
    assert_eq!(code(&say(&svc, &s, "RDUMP", args.clone())), "not_granted");
    s.grant("kb", &[Scope::Read, Scope::Dump], &[]);
    assert_eq!(code(&say(&svc, &s, "RREAD", json!({"cauldron": "kb"}))), "not_granted");
}

#[test]
fn rp2_a_put_lands_new_cited_unreviewed_records_under_the_door_s_provenance_once() {
    let s = Scratch::new("rp2", &["kb"]);
    s.import("kb", &example());
    s.grant("kb", &[Scope::Read, Scope::Write, Scope::Dump], &[]);
    let svc = Service::start(&s);
    let c = ctx(&s.project("p"));
    ok(&svc.call("soup_dump", json!({"cauldron": "kb", "text": "the build cache is flaky; should we pin it?"}), &c));
    ok(&svc.call("soup_dump", json!({"cauldron": "kb", "text": "second"}), &c));
    let before = s.node_count("kb");

    let r = say(&svc, &s, "RREAD", json!({"cauldron": "kb", "query": "cache"}));
    assert_eq!(r["regions"].as_array().unwrap().len(), 2, "{r}");
    assert!(r["regions"][0]["boundary"].is_string());

    // The stage's limits: each refused whole, nothing landed.
    let existing = s.import("kb", &[json!({"id": "x", "type": "task", "state": "ready", "handle": "an existing task", "edges": [{"kind": "in_region", "to": "R-2"}]})]);
    let task = existing["id_map"]["x"].as_str().unwrap().to_string();
    let before = before + 1;
    let mut many: Vec<Value> = (0..26).map(|i| idea(&format!("n{i}"), "many")).collect();
    let cases: Vec<(&str, Value)> = vec![
        ("a directive", json!([{"id": "n1", "type": "directive", "state": "active", "handle": "do it", "operator": "me", "edges": [{"kind": "in_region", "to": "R-1"}]}])),
        ("a region", json!([{"id": "n1", "type": "region", "handle": "a new region", "boundary": "Inside: anything at all, for forty characters. Outside: nothing."}])),
        ("no region", json!([{"id": "n1", "type": "idea", "state": "idea", "handle": "x"}])),
        ("an unknown region", json!([{"id": "n1", "type": "idea", "state": "idea", "handle": "x", "edges": [{"kind": "in_region", "to": "R-9"}]}])),
        ("a supersede", json!([{"id": "n1", "type": "task", "state": "ready", "handle": "x", "edges": [{"kind": "in_region", "to": "R-1"}, {"kind": "supersedes", "to": task}]}])),
        ("a link to nothing", json!([{"id": "n1", "type": "idea", "state": "idea", "handle": "x", "edges": [{"kind": "in_region", "to": "R-1"}, {"kind": "ref", "to": "C-99"}]}])),
        ("provenance", json!([{"id": "n1", "type": "idea", "state": "idea", "handle": "x", "provenance": {"created_by": "owner"}, "edges": [{"kind": "in_region", "to": "R-1"}]}])),
        ("approved", json!([{"id": "n1", "type": "idea", "state": "idea", "handle": "x", "meta": {"approved": true}, "edges": [{"kind": "in_region", "to": "R-1"}]}])),
        ("too many", Value::Array(std::mem::take(&mut many))),
    ];
    for (what, records) in cases {
        let v = say(&svc, &s, "RPUT", put("run-limits", records));
        assert_eq!(code(&v), "batch_refused", "{what}: {v}");
    }
    // A label used twice, and a label that shadows a held id: refused by
    // the names the record import uses, so nothing lands and no report
    // keyed by label can miss a record or point a reference at itself.
    let named = [
        ("duplicate-id-in-batch", json!([idea("n1", "one"), idea("n1", "two")])),
        ("label-shadows-existing", json!([{"id": task, "type": "idea", "state": "idea", "handle": "x", "edges": [{"kind": "in_region", "to": "R-1"}, {"kind": "ref", "to": task}]}])),
        ("label-shadows-existing", json!([{"id": "R-1", "type": "idea", "state": "idea", "handle": "x", "edges": [{"kind": "in_region", "to": "R-2"}]}])),
    ];
    for (want, records) in named {
        let v = say(&svc, &s, "RPUT", put("run-labels", records));
        assert_eq!(code(&v), "batch_refused", "{want}: {v}");
        let codes: Vec<&str> = v["detail"]["problems"].as_array().unwrap().iter().filter_map(|p| p["code"].as_str()).collect();
        assert!(codes.contains(&want), "{want}: {v}");
    }
    let v = say(&svc, &s, "RPUT", put("run-labels", json!([{"id": "n1", "type": "idea", "state": "idea", "handle": "x", "edges": [{"kind": "in_region", "to": "R-1"}, {"kind": "derived_from", "to": "n1"}]}])));
    assert!(v["detail"]["problems"].to_string().contains("itself"), "a self-edge: {v}");
    // The grammar's own refusal (a state the type does not have).
    let v = say(&svc, &s, "RPUT", put("run-grammar", json!([{"id": "n1", "type": "idea", "state": "nope", "handle": "x", "edges": [{"kind": "in_region", "to": "R-1"}]}])));
    assert_eq!(code(&v), "records_refused", "{v}");
    assert_eq!(s.node_count("kb"), before, "nothing landed");
    assert!(ripple_rows(&s).is_empty());

    // A batch that tries to say who wrote it: the door says instead.
    let batch = json!([
        {"id": "n1", "type": "issue", "state": "open", "handle": "the build cache is flaky",
         "meta": {"ripple": {"package": "owner", "version": "x", "run": "someone-else", "model": "none"}, "cites": [{"kind": "url", "url": "https://example.com/build"}]},
         "edges": [{"kind": "in_region", "to": "R-1"}, {"kind": "ref", "to": task}]},
        {"id": "n2", "type": "question", "state": "open", "handle": "should we pin the cache?",
         "edges": [{"kind": "in_region", "to": "R-1"}, {"kind": "derived_from", "to": "n1"}]},
    ]);
    let mut req = put("run-a", batch);
    // Request fields a runner might try: never read.
    req["kind"] = json!("type_one");
    req["scope"] = json!(["meta.approved"]);
    req["class"] = json!("agent");
    let v = say(&svc, &s, "RPUT", req.clone());
    assert_eq!((v["ok"].clone(), v["outcome"].clone()), (json!(true), json!("landed")), "{v}");
    let ids: Vec<String> = v["records"].as_array().unwrap().iter().map(|x| x.as_str().unwrap().to_string()).collect();
    assert_eq!(ids.len(), 2);
    assert_eq!(s.node_count("kb"), before + 2);
    let rows = ripple_rows(&s);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0], json!({"class": "ripple", "kind": "type_two", "run_id": "run-a", "step_id": "put", "scope": []}));
    let got = ok(&svc.call("soup_get", json!({"cauldron": "kb", "ids": ids}), &c));
    for rec in got["records"].as_array().unwrap() {
        assert_eq!(rec["unreviewed"], json!(true), "{rec}");
        assert_eq!(rec["meta"]["ripple"], json!({"package": "dump", "version": "v1", "run": "run-a", "model": "sonnet"}));
        assert!(rec["meta"]["cites"].as_array().unwrap().contains(&json!({"kind": "dump", "id": "DM-1"})), "{rec}");
    }
    // The dump is processed, and says by which run and into what.
    let d = say(&svc, &s, "RDUMP", json!({"cauldron": "kb", "dump": "DM-1"}));
    assert_eq!(d["dump"]["state"], "processed");
    assert_eq!(d["dump"]["processed"]["run"], "run-a");
    assert_eq!(d["dump"]["processed"]["records"], json!(ids));
    let listed = ok(&svc.call("soup_dumps", json!({"cauldron": "kb", "state": "processed"}), &c));
    assert_eq!(listed["total"], 1);

    // The same run again lands nothing more.
    let r = say(&svc, &s, "RRUN", json!({"cauldron": "kb", "run": "run-a"}));
    assert_eq!((r["landed"].clone(), r["records"].clone()), (json!(true), json!(ids)));
    let again = say(&svc, &s, "RPUT", req);
    assert_eq!((again["outcome"].clone(), again["records"].clone(), again["dump_marked"].clone()), (json!("already_landed"), json!(ids), json!(false)));
    assert_eq!(s.node_count("kb"), before + 2);
    // Another run on a processed dump is refused.
    assert_eq!(code(&say(&svc, &s, "RPUT", put("run-b", json!([idea("n1", "y")])))), "dump_processed");
    assert_eq!(s.node_count("kb"), before + 2);
    // The approval is the owner's side's: the records approve, and read as reviewed.
    let owner = std::fs::read_to_string(s.mcp_dir().join("approve.secret")).unwrap().trim().to_string();
    let a: Value = serde_json::from_str(&seam_say(svc.seam, &format!("APPROVE {owner} {}", json!({"cauldron": "kb", "ids": [ids[0]]})))).unwrap();
    assert_eq!(a["approved"], json!([ids[0]]));
    // The ripples key never approves.
    assert_eq!(code(&say(&svc, &s, "APPROVE", json!({"cauldron": "kb", "ids": [ids[1]]}))), "unauthorized");

    // A removed dump is never processed.
    let svc_stopped = svc.stop();
    assert!(svc_stopped.success());
    let by = soup_door::dumps::By { thread: "cli".into(), dispatch: "test".into(), instrument: "test".into(), version: "0".into() };
    soup_door::dumps::remove(&s.target("kb").cauldron, "DM-2", true, &by).unwrap();
    let svc = Service::start(&s);
    let v = say(&svc, &s, "RPUT", json!({"cauldron": "kb", "dump": "DM-2", "run": "run-c", "step": "put", "package": "dump", "version": "v1", "model": "sonnet", "records": [idea("n1", "z")]}));
    assert_eq!(code(&v), "dump_removed");
    let d = say(&svc, &s, "RDUMP", json!({"cauldron": "kb", "dump": "DM-2"}));
    assert!(d.get("text").is_none(), "{d}");
}

/// Killed after the records landed and before the dump was marked: the
/// restarted door finds the run landed, marks the dump, lands nothing more.
#[cfg(feature = "fault-injection")]
#[test]
fn rp3_killed_between_the_landing_and_the_mark_the_run_still_lands_once() {
    let s = Scratch::new("rp3", &["kb"]);
    s.import("kb", &example());
    s.grant("kb", &[Scope::Read, Scope::Write, Scope::Dump], &[]);
    let before = s.node_count("kb");
    let mut svc = Service::start_with(&s, &[("CAULDRON_MCP_FAULT", "ripple-landed")]);
    let c = ctx(&s.project("p"));
    ok(&svc.call("soup_dump", json!({"cauldron": "kb", "text": "a note"}), &c));
    let req = put("run-k", json!([idea("n1", "from the note")]));
    let line = format!("RPUT {} {req}", key(&s));
    let mut st = std::net::TcpStream::connect(svc.seam).unwrap();
    use std::io::{BufRead, Write};
    writeln!(st, "{line}").unwrap();
    let mut out = String::new();
    let _ = std::io::BufReader::new(st).read_line(&mut out);
    assert!(out.trim().is_empty(), "the service answered: {out}");
    let status = svc.child.wait().unwrap();
    assert!(!status.success());
    assert_eq!(s.node_count("kb"), before + 1, "the records landed");
    let svc = Service::start(&s);
    let d = say(&svc, &s, "RDUMP", json!({"cauldron": "kb", "dump": "DM-1"}));
    assert_eq!(d["dump"]["state"], "received", "not marked yet");
    let v = say(&svc, &s, "RPUT", put("run-k", json!([])));
    assert_eq!((v["outcome"].clone(), v["dump_marked"].clone()), (json!("already_landed"), json!(true)), "{v}");
    assert_eq!(s.node_count("kb"), before + 1, "nothing landed twice");
    let d = say(&svc, &s, "RDUMP", json!({"cauldron": "kb", "dump": "DM-1"}));
    assert_eq!(d["dump"]["state"], "processed");
}
