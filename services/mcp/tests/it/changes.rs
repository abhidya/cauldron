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

/// `wait_secs`: a waiting call answers as soon as a write lands through the
/// service (well inside its wait), at its timeout with nothing and the same
/// cursor, and within about a second of a write by another process. Bounds
/// and a wait without `since` are usage.
#[test]
fn c9_a_change_wait_ends_on_a_write_or_at_its_timeout() {
    use std::time::{Duration, Instant};
    let s = Scratch::new("c9", &["kb"]);
    s.import("kb", &example());
    s.grant("kb", &[Scope::Read, Scope::Write], &[]);
    let svc = Service::start(&s);
    let c = ctx(&s.project("p"));
    let cur = ok(&svc.call("soup_status", json!({"cauldron": "kb"}), &c))["cursor"].as_str().unwrap().to_string();
    for (args, why) in [
        (json!({"cauldron": "kb", "since": cur, "wait_secs": 61}), "0 to 60"),
        (json!({"cauldron": "kb", "wait_secs": 5}), "needs `since`"),
    ] {
        let (code, msg, _) = refused(&svc.call("soup_changes", args, &c));
        assert_eq!(code, "usage");
        assert!(msg.contains(why), "{msg}");
    }
    // The timeout: nothing lands, the cursor is unchanged.
    let t0 = Instant::now();
    let d = ok(&svc.call("soup_changes", json!({"cauldron": "kb", "since": cur, "wait_secs": 2}), &c));
    let took = t0.elapsed();
    assert!(took >= Duration::from_millis(1900) && took < Duration::from_secs(5), "{took:?}");
    assert_eq!((d["changes"].clone(), d["cursor"].clone(), d["waited"]["ended"].clone()), (json!([]), json!(cur), json!("timeout")), "{d}");
    // A write through the service ends the wait at once.
    std::thread::scope(|sc| {
        let waiter = sc.spawn(|| {
            let t0 = Instant::now();
            (ok(&svc.call("soup_changes", json!({"cauldron": "kb", "since": cur, "wait_secs": 30}), &c)), t0.elapsed())
        });
        std::thread::sleep(Duration::from_millis(500));
        let t1 = Instant::now();
        ok(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "records": [task("t", "Wakes the waiter")]}), &c));
        let (d, took) = waiter.join().unwrap();
        assert_eq!(d["waited"]["ended"], "landed", "{d}");
        assert_eq!(d["changes"][0]["op"], "create", "{d}");
        assert!(took < Duration::from_secs(10) && t1.elapsed() < Duration::from_secs(5), "{took:?}");
    });
    // A write by another process (the service is not this cauldron's writer
    // here: a second cauldron, imported directly) is seen within the look.
    let s2 = Scratch::new("c9b", &["kb"]);
    s2.import("kb", &example());
    s2.grant("kb", &[Scope::Read], &[]);
    let svc2 = Service::start(&s2);
    let c2 = ctx(&s2.project("p"));
    let cur2 = ok(&svc2.call("soup_status", json!({"cauldron": "kb"}), &c2))["cursor"].as_str().unwrap().to_string();
    std::thread::scope(|sc| {
        let waiter = sc.spawn(|| ok(&svc2.call("soup_changes", json!({"cauldron": "kb", "since": cur2, "wait_secs": 30}), &c2)));
        std::thread::sleep(Duration::from_millis(500));
        let t1 = Instant::now();
        s2.import("kb", &[task("o", "Written by another process")]);
        let d = waiter.join().unwrap();
        assert_eq!(d["waited"]["ended"], "landed", "{d}");
        assert!(t1.elapsed() < Duration::from_secs(4), "seen within about a second: {:?}", t1.elapsed());
    });
}

/// A revoke while a call waits past the head ends it with `not_granted`.
#[test]
fn c10_a_revoke_ends_a_change_wait() {
    use std::time::{Duration, Instant};
    let s = Scratch::new("c10", &["kb"]);
    s.import("kb", &example());
    s.grant("kb", &[Scope::Read], &[]);
    let svc = Service::start(&s);
    let c = ctx(&s.project("p"));
    let cur = ok(&svc.call("soup_status", json!({"cauldron": "kb"}), &c))["cursor"].as_str().unwrap().to_string();
    std::thread::scope(|sc| {
        let waiter = sc.spawn(|| svc.call("soup_changes", json!({"cauldron": "kb", "since": cur, "wait_secs": 30}), &c));
        std::thread::sleep(Duration::from_millis(500));
        let t1 = Instant::now();
        s.revoke("kb");
        let r = waiter.join().unwrap();
        let (code, msg, _) = refused(&r);
        assert_eq!(code, "not_granted", "{msg}");
        assert!(t1.elapsed() < Duration::from_secs(4), "{:?}", t1.elapsed());
    });
}

/// A `soup_changes` wait sent on a raw connection that the test then closes
/// (a hung-up caller): the request, as the bridge sends it.
fn open_wait(svc: &Service, since: &str, session: &str, secs: u64, cwd: &std::path::Path) -> std::net::TcpStream {
    use std::io::Write;
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "soup_changes", "arguments": {
        "cauldron": "kb", "since": since, "wait_secs": secs,
        "_ctx": {"client": "test", "cwd": cwd.display().to_string(), "session_id": session}}}})
    .to_string();
    let mut s = std::net::TcpStream::connect(svc.door).unwrap();
    write!(
        s,
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAuthorization: {}\r\nAccept: application/json, text/event-stream\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        svc.door.port(),
        svc.bearer,
        body.len()
    )
    .unwrap();
    s.flush().unwrap();
    s
}

/// Open `n` holding waits (`session(i)` each) and keep them all held: a
/// wait answered at once (the slots were not free yet, the calls racing
/// each other in) is sent again, until a whole look finds every one still
/// waiting.
fn hold_waits(svc: &Service, since: &str, n: usize, session: &dyn Fn(usize) -> String, secs: u64, cwd: &std::path::Path) -> Vec<std::net::TcpStream> {
    let mut held: Vec<std::net::TcpStream> = (0..n).map(|i| open_wait(svc, since, &session(i), secs, cwd)).collect();
    let t0 = std::time::Instant::now();
    loop {
        std::thread::sleep(std::time::Duration::from_millis(300));
        let mut answered = 0;
        for (i, c) in held.iter_mut().enumerate() {
            c.set_nonblocking(true).unwrap();
            let mut b = [0u8; 1];
            let early = c.peek(&mut b).is_ok();
            c.set_nonblocking(false).unwrap();
            if early {
                answered += 1;
                *c = open_wait(svc, since, &session(i), secs, cwd);
            }
        }
        if answered == 0 {
            return held;
        }
        assert!(t0.elapsed() < std::time::Duration::from_secs(20), "the waits were never all held");
    }
}

fn session_ctx(cwd: &std::path::Path, session: &str) -> Value {
    json!({"client": "test", "cwd": cwd.display().to_string(), "session_id": session})
}

/// Sixteen waits whose callers hang up free their slots at once, not at
/// their deadlines: a wait asked right after them is given a slot and waits.
/// While all sixteen hold, one more is answered at once with `not-waited`
/// and a `retry_after_secs` of at least a second and at most the time until
/// the soonest slot frees.
#[test]
fn c11_a_wait_whose_caller_hangs_up_gives_its_slot_back_at_once() {
    use std::time::{Duration, Instant};
    let s = Scratch::new("c11", &["kb"]);
    s.import("kb", &example());
    s.grant("kb", &[Scope::Read], &[]);
    let svc = Service::start(&s);
    let p = s.project("p");
    let cur = ok(&svc.call("soup_status", json!({"cauldron": "kb"}), &ctx(&p)))["cursor"].as_str().unwrap().to_string();
    let held = hold_waits(&svc, &cur, 16, &|i| format!("s{i}"), 40, &p);
    // Every slot taken: the next caller is told when to call again.
    let d = ok(&svc.call("soup_changes", json!({"cauldron": "kb", "since": cur, "wait_secs": 5}), &session_ctx(&p, "late")));
    assert_eq!(d["waited"]["ended"], "not-waited", "{d}");
    let retry = d["waited"]["retry_after_secs"].as_u64().unwrap_or(0);
    assert!((1..=40).contains(&retry), "{d}");
    assert!(d["waited"]["why"].as_str().unwrap().contains("retry_after_secs"), "{d}");
    // The sixteen hang up.
    drop(held);
    let t1 = Instant::now();
    let d = loop {
        let d = ok(&svc.call("soup_changes", json!({"cauldron": "kb", "since": cur, "wait_secs": 1}), &session_ctx(&p, "late")));
        if d["waited"]["ended"] == "timeout" {
            break d;
        }
        assert!(t1.elapsed() < Duration::from_secs(5), "the hung-up waits kept their slots: {d}");
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(d["cursor"], json!(cur), "{d}");
    assert!(t1.elapsed() < Duration::from_secs(5), "{:?}", t1.elapsed());
}

/// One session holds at most four of the sixteen slots: its fifth wait is
/// answered at once (`not-waited`, saying so, with `retry_after_secs`),
/// while another session's wait is still given a slot.
#[test]
fn c12_one_session_cannot_hold_every_wait_slot() {
    let s = Scratch::new("c12", &["kb"]);
    s.import("kb", &example());
    s.grant("kb", &[Scope::Read], &[]);
    let svc = Service::start(&s);
    let p = s.project("p");
    let cur = ok(&svc.call("soup_status", json!({"cauldron": "kb"}), &ctx(&p)))["cursor"].as_str().unwrap().to_string();
    let held = hold_waits(&svc, &cur, 4, &|_| "greedy".to_string(), 30, &p);
    let d = ok(&svc.call("soup_changes", json!({"cauldron": "kb", "since": cur, "wait_secs": 3}), &session_ctx(&p, "greedy")));
    assert_eq!(d["waited"]["ended"], "not-waited", "{d}");
    assert!(d["waited"]["why"].as_str().unwrap().contains("this session already has 4"), "{d}");
    assert!(d["waited"]["retry_after_secs"].as_u64().is_some_and(|r| r >= 1), "{d}");
    let d = ok(&svc.call("soup_changes", json!({"cauldron": "kb", "since": cur, "wait_secs": 1}), &session_ctx(&p, "other")));
    assert_eq!(d["waited"]["ended"], "timeout", "another session still waits: {d}");
    drop(held);
}
