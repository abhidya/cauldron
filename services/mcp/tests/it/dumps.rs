//! The dump tools: `soup_dump`, `soup_dump_session` (its own route),
//! `soup_dumps`, `soup_dump_get`, the `dump` scope, the store beside the
//! soup, `soup_changes` and `soup_status` carrying dumps, the audit lines.

use serde_json::{json, Value};
use soup_door::grants::Scope;

use crate::support::*;

fn lock_free(s: &Scratch, c: &str) -> bool {
    let p = dumps_dir(s, c).join("writer.lock");
    if !p.exists() {
        return true;
    }
    let f = std::fs::OpenOptions::new().write(true).open(p).unwrap();
    f.try_lock().is_ok()
}

fn blobs(s: &Scratch, c: &str) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dumps_dir(s, c).join("blobs"))
        .map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

/// A text dump lands as DM-1 `received` in the store beside the soup, and
/// reads back; the soup is untouched; the same text twice is two dumps and
/// one blob; the size cap; the audit lines; the side-effect log line.
#[test]
fn dm1_a_text_dump_lands_received_beside_the_soup_and_reads_back() {
    let s = Scratch::new("dm1", &["kb"]);
    s.import("kb", &example());
    let held = s.node_count("kb");
    s.grant("kb", &Scope::ALL, &[]);
    let svc = Service::start(&s);
    let c = ctx(&s.project("p"));
    assert!(!dumps_dir(&s, "kb").exists(), "no store before the first dump");
    let st = ok(&svc.call("soup_status", json!({"cauldron": "kb"}), &c));
    assert_eq!(st["dumps"], json!({"store": "absent", "total": 0, "by_state": {}, "held": false}), "{st}");
    let first = ok(&svc.call("soup_changes", json!({"cauldron": "kb"}), &c))["cursor"].as_str().unwrap().to_string();
    assert!(first.ends_with(".0"), "{first}");

    let text = "A thought worth keeping.\nIt has two lines, \"quotes\" and é.\n";
    let r = ok(&svc.call("soup_dump", json!({"cauldron": "kb", "text": text, "title": "A thought", "source": "a test",
        "cite": {"kind": "url", "url": "https://example.org/a"}, "refs": ["T-1"]}), &c));
    assert_eq!((r["dump_id"].clone(), r["state"].clone(), r["kind"].clone()), (json!("DM-1"), json!("received"), json!("text")), "{r}");
    assert_eq!(r["bytes"], json!(text.len()));
    assert_eq!(r["sha256"], json!(sha256_hex(text.as_bytes())));
    assert!(r["next"].as_str().unwrap().contains("later release"), "{r}");
    assert!(!r.to_string().contains("worth keeping"), "a receipt never holds the text");
    assert!(r["cursor"].as_str().unwrap().ends_with(".1"), "{r}");
    assert_eq!(s.node_count("kb"), held, "nothing lands in the soup");
    assert!(spine::offline::rest_check(&s.soup("kb")).unwrap().is_empty(), "the soup is at rest and untouched");
    assert!(lock_free_soup(&s, "kb"), "a dump does not open the soup's writer");
    assert!(!lock_free(&s, "kb"), "the service holds the dumps store");
    assert_eq!(std::fs::read(dumps_dir(&s, "kb").join("blobs").join(format!("{}.txt", r["sha256"].as_str().unwrap()))).unwrap(), text.as_bytes());

    // The same text again: a second dump, the same blob.
    let r2 = ok(&svc.call("soup_dump", json!({"cauldron": "kb", "text": text}), &c));
    assert_eq!((r2["dump_id"].clone(), r2["blob_reused"].clone()), (json!("DM-2"), json!(true)));
    assert_eq!(blobs(&s, "kb").len(), 1, "{:?}", blobs(&s, "kb"));

    // The cap: exactly 1 MiB lands, one byte more is refused.
    let big = "x".repeat(soup_door::dumps::TEXT_LIMIT);
    assert_eq!(ok(&svc.call("soup_dump", json!({"cauldron": "kb", "text": big}), &c))["dump_id"], "DM-3");
    let (code, msg, _) = refused(&svc.call("soup_dump", json!({"cauldron": "kb", "text": format!("{big}y")}), &c));
    assert_eq!(code, "too_large", "{msg}");
    let (code, _, _) = refused(&svc.call("soup_dump", json!({"cauldron": "kb", "text": "  "}), &c));
    assert_eq!(code, "usage");
    let (code, _, _) = refused(&svc.call("soup_dump", json!({"cauldron": "kb", "text": "x", "colour": "red"}), &c));
    assert_eq!(code, "usage");

    // Status counts them; the list never holds text; the content reads back.
    let st = ok(&svc.call("soup_status", json!({"cauldron": "kb"}), &c));
    assert_eq!(st["dumps"], json!({"store": "present", "total": 3, "by_state": {"received": 3}, "held": true}), "{st}");
    let l = ok(&svc.call("soup_dumps", json!({"cauldron": "kb"}), &c));
    let ids: Vec<&str> = l["dumps"].as_array().unwrap().iter().map(|d| d["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["DM-3", "DM-2", "DM-1"], "{l}");
    assert_eq!(l["dumps"][2]["title"], "A thought");
    assert_eq!(l["dumps"][2]["source"], json!({"kind": "text", "client": "test", "from": "a test"}));
    assert!(!l.to_string().contains("worth keeping"));
    let g = ok(&svc.call("soup_dump_get", json!({"cauldron": "kb", "id": "DM-1"}), &c));
    assert_eq!((g["text"].clone(), g["more"].clone()), (json!(text), json!(false)), "{g}");
    // A big one, in windows, each within its bound.
    let (mut got, mut off) = (String::new(), 0u64);
    for _ in 0..1000 {
        let w = ok(&svc.call("soup_dump_get", json!({"cauldron": "kb", "id": "DM-3", "offset": off, "max_chars": 50000}), &c));
        assert!(soup_door::budget::size(&w) <= 50000, "{}", soup_door::budget::size(&w));
        got.push_str(w["text"].as_str().unwrap());
        match w["next_offset"].as_u64() {
            Some(n) => off = n,
            None => break,
        }
    }
    assert_eq!(got.len(), big.len());
    let (code, _, _) = refused(&svc.call("soup_dump_get", json!({"cauldron": "kb", "id": "DM-99"}), &c));
    assert_eq!(code, "not_found");

    // soup_changes carries the dumps, ids and sizes only, and the cursor moves on.
    let ch = ok(&svc.call("soup_changes", json!({"cauldron": "kb", "since": first}), &c));
    let d: Vec<&str> = ch["dumps"].as_array().unwrap().iter().map(|d| d["id"].as_str().unwrap()).collect();
    assert_eq!(d, ["DM-1", "DM-2", "DM-3"], "{ch}");
    assert_eq!(ch["dumps"][0]["bytes"], json!(text.len()));
    assert_eq!(ch["changes"], json!([]));
    assert_eq!(ch["cursor"], st["cursor"]);
    assert!(!ch.to_string().contains("worth keeping"));
    let again = ok(&svc.call("soup_changes", json!({"cauldron": "kb", "since": ch["cursor"]}), &c));
    assert_eq!((again["dumps"].clone(), again["changes"].clone()), (json!([]), json!([])));
    // A cursor from before dumps existed (three parts) reads every dump.
    let three = first.rsplit_once('.').unwrap().0.to_string();
    assert_eq!(ok(&svc.call("soup_changes", json!({"cauldron": "kb", "since": three}), &c))["dumps"].as_array().unwrap().len(), 3);
    // Bounded: limit 1 pages through them without missing one.
    let (mut seen, mut since) = (Vec::new(), first.clone());
    for _ in 0..10 {
        let p = ok(&svc.call("soup_changes", json!({"cauldron": "kb", "since": since, "limit": 1}), &c));
        seen.extend(p["dumps"].as_array().unwrap().iter().map(|d| d["id"].as_str().unwrap().to_string()));
        since = p["cursor"].as_str().unwrap().to_string();
        if p["more"] != json!(true) {
            break;
        }
    }
    assert_eq!(seen, ["DM-1", "DM-2", "DM-3"]);

    // The audit: one line per dump call, never the text.
    let calls = s.calls();
    let dumps: Vec<&Value> = calls.iter().filter(|l| l["tool"] == "soup_dump").collect();
    let outcomes: Vec<&str> = dumps.iter().map(|l| l["outcome"].as_str().unwrap()).collect();
    assert_eq!(outcomes, ["received", "received", "received", "too_large", "usage", "usage"], "{calls:?}");
    assert_eq!(dumps[0]["dump"], "DM-1");
    assert_eq!(dumps[0]["counts"]["bytes"], json!(text.len()));
    assert!(dumps[0]["tx"].as_str().is_some_and(|t| t.len() == 64));
    assert!(calls.iter().any(|l| l["tool"] == "open-dumps"));
    let raw = std::fs::read_to_string(s.mcp_dir().join("calls.jsonl")).unwrap();
    assert!(!raw.contains("worth keeping") && !raw.contains("example.org") && !raw.contains("xxxxxxxx"), "text in the audit");
    // The side effect ran after the write: a log line, nothing else.
    assert!(svc.log().contains("dump received") && svc.log().contains("DM-3"), "{}", svc.log());
    assert!(!svc.log().contains("worth keeping"));
}

fn lock_free_soup(s: &Scratch, c: &str) -> bool {
    let f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(s.soup(c).join("writer.lock")).unwrap();
    f.try_lock().is_ok()
}

/// The `dump` scope gates every dump write; reading a dump's content needs
/// `read` and `dump`; listing needs `read`. A revoke closes the store and
/// frees its lock, and the next dump is refused.
#[test]
fn dm2_the_dump_scope_gates_dumps_and_revoke_closes_the_store() {
    let s = Scratch::new("dm2", &["kb"]);
    let svc = Service::start(&s);
    let c = ctx(&s.project("p"));
    // Closed by default.
    let (code, _, _) = refused(&svc.call("soup_dump", json!({"cauldron": "kb", "text": "x"}), &c));
    assert_eq!(code, "not_granted");
    // read and write, no dump: refused, naming the scope; nothing is made.
    s.grant("kb", &[Scope::Read, Scope::Write], &[]);
    let (code, msg, _) = refused(&svc.call("soup_dump", json!({"cauldron": "kb", "text": "x"}), &c));
    assert_eq!(code, "not_granted");
    assert!(msg.contains("no `dump` grant") && msg.contains("--scopes read,write,dump"), "{msg}");
    let (code, _, _) = refused(&svc.dump_session_result(json!({"cauldron": "kb", "session_id": "sess-a"}), &c, &transcript(3, "/w")));
    assert_eq!(code, "not_granted");
    assert!(!dumps_dir(&s, "kb").exists(), "a refused dump makes no store and stores no byte");
    // dump only: dumps land; listing (read) and content (read + dump) refused.
    s.grant("kb", &[Scope::Dump], &[]);
    assert_eq!(ok(&svc.call("soup_dump", json!({"cauldron": "kb", "text": "kept"}), &c))["dump_id"], "DM-1");
    assert_eq!(refused(&svc.call("soup_dumps", json!({"cauldron": "kb"}), &c)).0, "not_granted");
    assert_eq!(refused(&svc.call("soup_dump_get", json!({"cauldron": "kb", "id": "DM-1"}), &c)).0, "not_granted");
    // read only: the list (no text), not the content.
    s.grant("kb", &[Scope::Read], &[]);
    assert_eq!(ok(&svc.call("soup_dumps", json!({"cauldron": "kb"}), &c))["total"], 1);
    let (code, msg, _) = refused(&svc.call("soup_dump_get", json!({"cauldron": "kb", "id": "DM-1"}), &c));
    assert_eq!(code, "not_granted");
    assert!(msg.contains("no `dump` grant"), "{msg}");
    // Losing `dump` closed the store and freed its lock, with no call.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !lock_free(&s, "kb") {
        assert!(std::time::Instant::now() < deadline, "the dumps lock was not freed");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    // Everything: the content reads back.
    s.grant("kb", &Scope::ALL, &[]);
    assert_eq!(ok(&svc.call("soup_dump_get", json!({"cauldron": "kb", "id": "DM-1"}), &c))["text"], "kept");
    assert_eq!(ok(&svc.call("soup_dump", json!({"cauldron": "kb", "text": "more"}), &c))["dump_id"], "DM-2");
    assert!(!lock_free(&s, "kb"));
    s.revoke("kb");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !lock_free(&s, "kb") {
        assert!(std::time::Instant::now() < deadline, "revoke did not free the dumps lock");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert_eq!(refused(&svc.call("soup_dump", json!({"cauldron": "kb", "text": "late"}), &c)).0, "not_granted");
    assert!(s.calls().iter().filter(|l| l["tool"] == "close-dumps").count() >= 2);
    // A writer other than the service (as `cauldron soup dump` direct) can
    // now take the store; while the service held it, it could not.
    let st = soup_door::dumps::Store::open(&s.target("kb").cauldron, "cli 1").unwrap();
    assert_eq!(st.recovered["interrupted"], "none");
    drop(st);
    assert_eq!(svc.stop().code(), Some(0));
}

/// `soup_dump_session` through its route: the transcript is stored
/// verbatim, its blob's hash is the transcript's; refusals read nothing;
/// the plain `/mcp` route never takes it.
#[test]
fn dm3_a_session_transcript_is_stored_verbatim() {
    let s = Scratch::new("dm3", &["kb"]);
    s.grant("kb", &Scope::ALL, &[]);
    let svc = Service::start(&s);
    let c = json!({"client": "claude-code", "cwd": s.project("p").display().to_string(), "session_id": "sess-b"});
    let t = transcript(500, "/work/proj");
    let r = ok(&svc.dump_session_result(json!({"cauldron": "kb", "session_id": "sess-a", "title": "Session A"}), &c, &t));
    assert_eq!((r["dump_id"].clone(), r["state"].clone(), r["kind"].clone()), (json!("DM-1"), json!("received"), json!("session")), "{r}");
    assert_eq!(r["sha256"], json!(sha256_hex(&t)), "the blob's hash is the transcript's");
    assert_eq!((r["bytes"].clone(), r["lines"].clone(), r["cwd"].clone()), (json!(t.len()), json!(500), json!("/work/proj")));
    assert_eq!(r["session_id"], "sess-a");
    let blob = dumps_dir(&s, "kb").join("blobs").join(format!("{}.jsonl", sha256_hex(&t)));
    assert_eq!(std::fs::read(&blob).unwrap(), t, "stored verbatim");
    // Read back whole, in windows.
    let (mut got, mut off) = (Vec::new(), 0u64);
    loop {
        let w = ok(&svc.call("soup_dump_get", json!({"cauldron": "kb", "id": "DM-1", "offset": off, "max_chars": 20000}), &c));
        assert_eq!(w["dump"]["source"], json!({"kind": "session", "id": "sess-a", "cwd": "/work/proj", "client": "claude-code"}));
        got.extend_from_slice(w["text"].as_str().unwrap().as_bytes());
        match w["next_offset"].as_u64() {
            Some(n) => off = n,
            None => break,
        }
    }
    assert_eq!(got, t);
    // The same transcript again: a second dump, one blob.
    assert_eq!(ok(&svc.dump_session_result(json!({"cauldron": "kb", "session_id": "sess-a"}), &c, &t))["blob_reused"], true);
    assert_eq!(blobs(&s, "kb").len(), 1);
    // A session id that is not an id; no session id at all.
    for bad in [json!({"cauldron": "kb", "session_id": "../x"}), json!({"cauldron": "kb"})] {
        let (code, _, _) = refused(&svc.dump_session_result(bad, &c, b"{}\n"));
        assert_eq!(code, "usage");
    }
    // Through the plain door: refused, nothing read.
    let (code, msg, _) = refused(&svc.call("soup_dump_session", json!({"cauldron": "kb", "session_id": "sess-a"}), &c));
    assert_eq!(code, "usage");
    assert!(msg.contains("bridge"), "{msg}");
    // Over the cap, from the length alone.
    let h = vec![
        ("Host".to_string(), format!("127.0.0.1:{}", svc.door.port())),
        ("Authorization".to_string(), svc.bearer.clone()),
    ];
    let mut req = std::net::TcpStream::connect(svc.door).unwrap();
    use std::io::{Read, Write};
    let mut head = "POST /mcp/dump-session HTTP/1.1\r\n".to_string();
    for (k, v) in &h {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str(&format!("Content-Length: {}\r\nConnection: close\r\n\r\n", 65 * 1024 * 1024 + 70_000));
    req.write_all(head.as_bytes()).unwrap();
    let mut out = String::new();
    req.read_to_string(&mut out).unwrap();
    assert!(out.starts_with("HTTP/1.1 413") && out.contains("too_large"), "{out}");
    // Without the secret: 401, nothing read.
    let r = http_bytes(svc.door, "POST", "/mcp/dump-session", &h[..1], b"{}\n");
    assert_eq!(r.status, 401);
    // The audit: the two landed, the refusals, never a transcript line.
    let raw = std::fs::read_to_string(s.mcp_dir().join("calls.jsonl")).unwrap();
    assert!(!raw.contains("some words"), "transcript text in the audit");
    let lines: Vec<Value> = s.calls().into_iter().filter(|l| l["tool"] == "soup_dump_session").collect();
    assert_eq!(lines.iter().map(|l| l["outcome"].as_str().unwrap()).collect::<Vec<_>>(), ["received", "received", "usage", "usage", "usage"], "{lines:?}");
    assert_eq!(lines[0]["counts"]["lines"], 500);
    assert_eq!(lines[0]["session"], "sess-b", "the audit names the calling session");
    let st = ok(&svc.call("soup_status", json!({"cauldron": "kb"}), &c));
    assert_eq!(st["dumps"]["total"], 2);
}

/// A dump killed between its blob and its commit leaves no node (the open
/// rolls the row back) and a blob no node names, which the next open of
/// the store sweeps; the same text sent again lands with a blob of its own.
#[cfg(feature = "fault-injection")]
#[test]
fn dm4_killed_between_the_blob_and_the_commit_no_node_and_a_retry_lands() {
    let s = Scratch::new("dm4", &["kb"]);
    s.grant("kb", &Scope::ALL, &[]);
    let c = ctx(&s.project("p"));
    let text = "kept across a crash";
    let svc = Service::start_with(&s, &[("SPINE_FAULT", "row-appended")]);
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "soup_dump",
        "arguments": {"cauldron": "kb", "text": text, "_ctx": c}}});
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| svc.post(&body.to_string(), &[])));
    let mut svc = svc;
    let st = svc.child.wait().unwrap();
    assert!(!st.success(), "the fault seam did not stop the service: {st}");
    drop(svc);
    assert_eq!(blobs(&s, "kb"), [format!("{}.txt", sha256_hex(text.as_bytes()))], "the blob was written first");
    assert!(lock_free(&s, "kb"));
    // No node: the store's row was never decided.
    let svc = Service::start(&s);
    let l = ok(&svc.call("soup_dumps", json!({"cauldron": "kb"}), &c));
    assert_eq!(l["total"], 0, "{l}");
    let r = ok(&svc.call("soup_dump", json!({"cauldron": "kb", "text": text}), &c));
    assert_eq!((r["dump_id"].clone(), r["blob_reused"].clone()), (json!("DM-1"), json!(false)), "{r}");
    // The open found the half-written row rolled back, or (when the kill
    // came before the row reached the disk) nothing to do; never landed.
    // And it swept the blob no node named.
    let open = s.calls().into_iter().rfind(|l| l["tool"] == "open-dumps").unwrap();
    let how = open["recovered_at_open"]["interrupted"].as_str().unwrap().to_string();
    assert!(how == "rolled-back" || how == "none", "{open}");
    assert_eq!(open["recovered_at_open"]["swept"], json!({"incoming": 0, "blobs": 1}), "{open}");
    assert_eq!(blobs(&s, "kb"), [format!("{}.txt", sha256_hex(text.as_bytes()))]);
}

/// Start a `soup_dump_session` by hand: the head and the call's line, then
/// the first `first` bytes of `transcript`. The caller sends the rest.
fn begin_session(svc: &Service, c: &Value, transcript: &[u8], first: usize) -> std::net::TcpStream {
    use std::io::Write;
    let call = json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": {"name": "soup_dump_session",
        "arguments": {"cauldron": "kb", "session_id": "sess-slow", "_ctx": c}}});
    let line = format!("{call}\n");
    let mut t = std::net::TcpStream::connect(svc.door).unwrap();
    t.set_read_timeout(Some(std::time::Duration::from_secs(120))).unwrap();
    let head = format!(
        "POST /mcp/dump-session HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAuthorization: {}\r\nContent-Type: application/octet-stream\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n",
        svc.door.port(),
        svc.bearer,
        line.len() + transcript.len()
    );
    t.write_all(head.as_bytes()).unwrap();
    t.write_all(line.as_bytes()).unwrap();
    t.write_all(&transcript[..first]).unwrap();
    t.flush().unwrap();
    t
}

/// Wait until the blob store holds a temporary file with some bytes in it.
fn wait_for_incoming(s: &Scratch) -> String {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let dir = dumps_dir(s, "kb").join("blobs");
        if let Some(n) = blobs(s, "kb").into_iter().find(|n| n.starts_with(".incoming-")) {
            if std::fs::metadata(dir.join(&n)).map(|m| m.len() > 0).unwrap_or(false) {
                return n;
            }
        }
        assert!(std::time::Instant::now() < deadline, "no transcript bytes arrived: {:?}", blobs(s, "kb"));
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// A grant revoked while a session's transcript streams in: the dump is
/// refused `not_granted`, the reading stops, and the answer's "Nothing was
/// written" is true: no blob, no temporary file, no node.
#[test]
fn dm5_a_revoke_while_a_transcript_streams_leaves_no_byte() {
    use std::io::{Read, Write};
    let s = Scratch::new("dm5", &["kb"]);
    s.grant("kb", &Scope::ALL, &[]);
    let svc = Service::start(&s);
    let c = ctx(&s.project("p"));
    let t = transcript(80_000, "/work/slow");
    assert!(t.len() > 8 * 1024 * 1024, "{}", t.len());
    let mut conn = begin_session(&svc, &c, &t, 1024 * 1024);
    wait_for_incoming(&s);
    s.revoke("kb");
    // The rest, in pieces; the service may stop reading part way.
    for chunk in t[1024 * 1024..].chunks(256 * 1024) {
        if conn.write_all(chunk).is_err() {
            break;
        }
    }
    let mut raw = Vec::new();
    let _ = conn.read_to_end(&mut raw);
    let text = String::from_utf8_lossy(&raw).into_owned();
    let body = text.split_once("\r\n\r\n").map(|x| x.1).unwrap_or("");
    let v: Value = serde_json::from_str(body).unwrap_or_else(|e| panic!("{e}: {text}"));
    let (code, msg, _) = refused(&v["result"]);
    assert_eq!(code, "not_granted", "{msg}");
    assert!(msg.contains("Nothing was written"), "{msg}");
    assert_eq!(blobs(&s, "kb"), Vec::<String>::new(), "a refused dump left bytes behind");
    assert_eq!(soup_door::dumps::list(&s.target("kb").cauldron, &Default::default()).unwrap()["total"], 0);
    let line = s.calls().into_iter().rfind(|l| l["tool"] == "soup_dump_session").unwrap();
    assert_eq!(line["outcome"], "not_granted", "{line}");
    assert_eq!(svc.stop().code(), Some(0));
}

/// The service killed while a transcript streams in leaves a temporary
/// file; the next open of the store (a later dump) sweeps it, and says so.
#[test]
fn dm6_a_kill_while_a_transcript_streams_is_swept_at_the_next_open() {
    let s = Scratch::new("dm6", &["kb"]);
    s.grant("kb", &Scope::ALL, &[]);
    let c = ctx(&s.project("p"));
    let t = transcript(20_000, "/work/killed");
    let svc = Service::start(&s);
    let conn = begin_session(&svc, &c, &t, 512 * 1024);
    let left = wait_for_incoming(&s);
    let mut svc = svc;
    svc.child.kill().unwrap();
    svc.child.wait().unwrap();
    drop(conn);
    drop(svc);
    assert_eq!(blobs(&s, "kb"), [left.clone()], "the kill left the partial transcript");
    // A store opened by another writer while a transcript is still being
    // written by a live one keeps that one's file.
    let live = soup_door::dumps::blob_writer(&s.target("kb").cauldron, soup_door::dumps::Kind::Session).unwrap();
    let st = soup_door::dumps::Store::open(&s.target("kb").cauldron, "cli 1").unwrap();
    assert_eq!(st.recovered["swept"], json!({"incoming": 1, "blobs": 0}), "{}", st.recovered);
    assert_eq!(blobs(&s, "kb").len(), 1, "the live writer's file is kept: {:?}", blobs(&s, "kb"));
    assert!(!blobs(&s, "kb").contains(&left));
    drop(st);
    drop(live);
    assert_eq!(blobs(&s, "kb"), Vec::<String>::new());
    // The service, restarted, takes the next dump with nothing left over.
    let svc = Service::start(&s);
    assert_eq!(ok(&svc.dump_session_result(json!({"cauldron": "kb", "session_id": "sess-k"}), &c, &t))["dump_id"], "DM-1");
    assert_eq!(blobs(&s, "kb"), [format!("{}.jsonl", sha256_hex(&t))]);
    assert_eq!(svc.stop().code(), Some(0));
}
