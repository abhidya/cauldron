//! `soup_changes`: what landed since a cursor, and the cursor's refusals.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use soup_door::grants::Scope;

use crate::support::*;

fn task(label: &str, handle: &str) -> Value {
    json!({"id": label, "type": "task", "state": "ready", "handle": handle, "edges": [{"kind": "in_region", "to": "R-1"}]})
}

/// Every change from `since` on, following `cursor` while `more`, `limit`
/// at a time; also answers how many calls it took.
fn follow(svc: &Service, c: &Value, since: &str, extra: Value) -> (Vec<Value>, usize) {
    let mut out = Vec::new();
    let mut since = since.to_string();
    for calls in 1..=100 {
        let mut args = json!({"cauldron": "kb", "since": since});
        for (k, v) in extra.as_object().unwrap() {
            args[k] = v.clone();
        }
        let d = ok(&svc.call("soup_changes", args, c));
        out.extend(d["changes"].as_array().unwrap().iter().cloned());
        if d["more"] != json!(true) {
            assert!(d.get("next").is_none(), "{d}");
            assert_eq!(d["cursor"], d["head"], "{d}");
            return (out, calls);
        }
        assert!(d["next"].as_str().is_some_and(|n| n.contains("since")), "{d}");
        since = d["cursor"].as_str().unwrap().to_string();
    }
    panic!("never reached the head");
}

#[test]
fn c1_a_cursor_round_trips_through_puts_and_links() {
    let s = Scratch::new("c1", &["kb"]);
    s.import("kb", &example());
    s.grant("kb", &Scope::ALL, &[]);
    let svc = Service::start(&s);
    let c = json!({"client": "test", "cwd": s.project("p").display().to_string(), "session_id": "sess-c1"});

    // No `since`: the cursor only, never the soup.
    let first = ok(&svc.call("soup_changes", json!({"cauldron": "kb"}), &c));
    assert_eq!(first["changes"], json!([]), "{first}");
    assert_eq!(first["more"], json!(false));
    let cursor = first["cursor"].as_str().unwrap().to_string();
    assert_eq!(first["head"].as_str(), Some(cursor.as_str()));
    let st = ok(&svc.call("soup_status", json!({"cauldron": "kb"}), &c));
    assert_eq!(st["cursor"].as_str(), Some(cursor.as_str()), "status and changes agree on the cursor");

    // Since the head: nothing.
    let none = ok(&svc.call("soup_changes", json!({"cauldron": "kb", "since": cursor}), &c));
    assert_eq!((none["changes"].clone(), none["cursor"].clone()), (json!([]), json!(cursor)), "{none}");

    // A put, then a link: two changes, in order, with what each did.
    let put = ok(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "records": [task("a", "Changes task a"), task("b", "Changes task b")]}), &c));
    let (a, b) = (put["id_map"]["a"].as_str().unwrap().to_string(), put["id_map"]["b"].as_str().unwrap().to_string());
    let link = ok(&svc.call("soup_link", json!({"cauldron": "kb", "links": [{"from": a, "kind": "ref", "to": b}]}), &c));
    let d = ok(&svc.call("soup_changes", json!({"cauldron": "kb", "since": cursor}), &c));
    let ch = d["changes"].as_array().unwrap();
    assert_eq!(ch.len(), 2, "{d}");
    assert_eq!(ch[0]["op"], "create");
    assert_eq!(ch[0]["count"], 2);
    assert_eq!(ch[0]["tx"], put["landed"]["tx"]);
    assert_eq!(ch[0]["by"], "mcp:test:sess-c1");
    let ids: BTreeSet<&str> = ch[0]["ids"].as_array().unwrap().iter().map(|i| i["id"].as_str().unwrap()).collect();
    assert_eq!(ids, BTreeSet::from([a.as_str(), b.as_str()]));
    assert!(ch[0]["ids"].as_array().unwrap().iter().any(|i| i["handle"] == "Changes task a" && i["type"] == "task"), "{d}");
    assert_eq!(ch[1]["op"], "link", "{d}");
    assert_eq!(ch[1]["ids"], json!([{"id": a, "type": "task", "handle": "Changes task a"}]));
    assert_eq!(ch[1]["edges"], 1);
    assert!(ch[0]["seq"].as_u64().unwrap() < ch[1]["seq"].as_u64().unwrap());
    // The cursor after the last change is the head, and the one the link answered.
    assert_eq!(d["cursor"], link["cursor"], "{d}");
    assert_eq!(d["cursor"], d["head"]);
    assert_eq!(d["more"], json!(false));
    // From the put's own cursor: only the link.
    let d = ok(&svc.call("soup_changes", json!({"cauldron": "kb", "since": put["cursor"]}), &c));
    assert_eq!(d["changes"].as_array().unwrap().len(), 1);
    assert_eq!(d["changes"][0]["op"], "link");
    // A read-only grant may read the stream.
    s.grant("kb", &[Scope::Read], &[]);
    ok(&svc.call("soup_changes", json!({"cauldron": "kb", "since": cursor}), &c));
}

#[test]
fn c2_unknown_foreign_recreated_and_stale_cursors_are_refused() {
    let s = Scratch::new("c2", &["kb", "other"]);
    s.grant("kb", &Scope::ALL, &[]);
    s.grant("other", &Scope::ALL, &[]);
    let svc = Service::start(&s);
    let c = ctx(&s.project("p"));
    let kb = ok(&svc.call("soup_changes", json!({"cauldron": "kb"}), &c))["cursor"].as_str().unwrap().to_string();
    let other = ok(&svc.call("soup_changes", json!({"cauldron": "other"}), &c))["cursor"].as_str().unwrap().to_string();
    assert_ne!(kb, other);
    for bad in ["", "nonsense", "v2.000000000000.0", "v1.abc", &format!("{kb}x")] {
        let (code, msg, _) = refused(&svc.call("soup_changes", json!({"cauldron": "kb", "since": bad}), &c));
        assert_eq!(code, "cursor_unknown", "{bad}: {msg}");
    }
    // Another cauldron's cursor.
    let (code, msg, _) = refused(&svc.call("soup_changes", json!({"cauldron": "kb", "since": other}), &c));
    assert_eq!(code, "cursor_unknown", "{msg}");
    assert!(msg.contains("another cauldron"), "{msg}");
    // Ahead of the soup.
    let (inst, n) = kb.rsplit_once('.').unwrap();
    let ahead = format!("{inst}.{}", n.parse::<u64>().unwrap() + 5);
    let (code, msg, _) = refused(&svc.call("soup_changes", json!({"cauldron": "kb", "since": ahead}), &c));
    assert_eq!(code, "cursor_stale", "{msg}");
    assert!(msg.contains(&kb), "the refusal names the current cursor: {msg}");
    // Removed and made again under the same name: the old cursor is unknown,
    // even though the new soup's counter is the same number.
    std::fs::remove_dir_all(s.data().join("cauldrons").join("kb")).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    s.create("kb");
    // The old grant does not open the new cauldron; granted again, it does.
    assert_eq!(refused(&svc.call("soup_changes", json!({"cauldron": "kb"}), &c)).0, "cauldron_gone");
    s.grant("kb", &Scope::ALL, &[]);
    let fresh = ok(&svc.call("soup_changes", json!({"cauldron": "kb"}), &c))["cursor"].as_str().unwrap().to_string();
    assert_ne!(fresh, kb);
    assert_eq!(fresh.rsplit_once('.').unwrap().1, n, "same counter, other instance");
    let (code, _, _) = refused(&svc.call("soup_changes", json!({"cauldron": "kb", "since": kb}), &c));
    assert_eq!(code, "cursor_unknown");
    ok(&svc.call("soup_changes", json!({"cauldron": "kb", "since": fresh}), &c));
}

#[test]
fn c3_a_bounded_list_says_where_to_read_on_and_misses_nothing() {
    let s = Scratch::new("c3", &["kb"]);
    s.import("kb", &example());
    s.grant("kb", &Scope::ALL, &[]);
    let svc = Service::start(&s);
    let c = ctx(&s.project("p"));
    let start = ok(&svc.call("soup_changes", json!({"cauldron": "kb"}), &c))["cursor"].as_str().unwrap().to_string();
    let mut txs = Vec::new();
    for i in 0..7 {
        let d = ok(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "records": [task("t", &format!("Bounded change {i} with a handle long enough to weigh something in a reply"))]}), &c));
        txs.push(d["landed"]["tx"].as_str().unwrap().to_string());
    }
    // By `limit`.
    let d = ok(&svc.call("soup_changes", json!({"cauldron": "kb", "since": start, "limit": 2}), &c));
    assert_eq!(d["changes"].as_array().unwrap().len(), 2);
    assert_eq!(d["more"], json!(true));
    let p: Vec<&str> = start.split('.').collect();
    assert_eq!(d["cursor"], json!(format!("{}.{}.{}.{}", p[0], p[1], d["changes"][1]["seq"], p[3])), "{d}");
    assert_ne!(d["cursor"], d["head"]);
    let (all, calls) = follow(&svc, &c, &start, json!({"limit": 2}));
    assert_eq!(calls, 4);
    let seen: Vec<String> = all.iter().map(|x| x["tx"].as_str().unwrap().to_string()).collect();
    assert_eq!(seen, txs, "every change once, in order");
    // By `max_chars`: the whole reply stays bounded, the cursor moves back to
    // the last change kept, and following it still misses nothing.
    let d = ok(&svc.call("soup_changes", json!({"cauldron": "kb", "since": start, "max_chars": 700}), &c));
    assert!(serde_json::to_string(&d).unwrap().len() <= 700, "{d}");
    assert!(d["truncated"]["dropped_from"]["changes"].as_u64().unwrap_or(0) > 0, "{d}");
    assert_eq!(d["more"], json!(true));
    let kept = d["changes"].as_array().unwrap();
    assert_eq!(d["cursor"], json!(format!("{}.{}.{}.{}", p[0], p[1], kept.last().unwrap()["seq"], p[3])), "{d}");
    let (all, _) = follow(&svc, &c, &start, json!({"max_chars": 700}));
    let seen: Vec<String> = all.iter().map(|x| x["tx"].as_str().unwrap().to_string()).collect();
    assert_eq!(seen, txs, "every change once, in order, under the bound");
}
