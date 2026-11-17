//! The door's limits: what is refused before any body is read, the cap on
//! connections, the in-flight body budget, and the timeouts. Each test drives
//! the real binary with raw sockets, so a refusal can be read back before a
//! byte of the body is sent.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::support::*;

const LIST: &str = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}"#;

/// The service's resident memory, in KiB.
fn rss_kib(pid: u32) -> u64 {
    let out = std::process::Command::new("/bin/ps").args(["-o", "rss=", "-p", &pid.to_string()]).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().parse().unwrap_or(0)
}

/// The service's established TCP connections on its door port.
fn door_connections(pid: u32, port: u16) -> usize {
    let out = std::process::Command::new("/usr/sbin/lsof")
        .args(["-nP", "-a", "-p", &pid.to_string(), "-iTCP", "-sTCP:ESTABLISHED", "-Fn"])
        .output()
        .unwrap();
    let local = format!("127.0.0.1:{port}->");
    String::from_utf8_lossy(&out.stdout).lines().filter(|l| l.starts_with('n') && l[1..].starts_with(&local)).count()
}

/// Sample the service's memory every 20 ms until `stop`; the peak, in KiB.
fn watch_rss(pid: u32, stop: Arc<AtomicBool>) -> std::thread::JoinHandle<u64> {
    let peak = Arc::new(AtomicU64::new(0));
    std::thread::spawn(move || {
        while !stop.load(Ordering::SeqCst) {
            peak.fetch_max(rss_kib(pid), Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(20));
        }
        peak.load(Ordering::SeqCst)
    })
}

/// A request head (no body) with these headers.
fn head(door: SocketAddr, auth: Option<&str>, extra: &[(&str, &str)], len: Option<usize>) -> String {
    let mut h = format!(
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAccept: application/json, text/event-stream\r\nContent-Type: application/json\r\n",
        door.port()
    );
    if let Some(a) = auth {
        h.push_str(&format!("Authorization: {a}\r\n"));
    }
    for (k, v) in extra {
        h.push_str(&format!("{k}: {v}\r\n"));
    }
    if let Some(n) = len {
        h.push_str(&format!("Content-Length: {n}\r\n"));
    }
    h.push_str("\r\n");
    h
}

/// Send `head` alone and read the answer: the status and body, and how
/// long it took. The body the head announces is never sent.
fn head_only(door: SocketAddr, head: &str) -> (u16, String, Duration) {
    let t = Instant::now();
    let mut s = TcpStream::connect(door).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    s.write_all(head.as_bytes()).unwrap();
    let mut raw = Vec::new();
    let _ = s.read_to_end(&mut raw);
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (h, b) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status = h.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
    (status, b.to_string(), t.elapsed())
}

#[test]
fn l1_a_request_is_refused_from_its_head_before_any_body_is_read() {
    let s = Scratch::new("l1", &["kb"]);
    let svc = Service::start(&s);
    let big = Some(1_000_000);
    // No secret, a wrong one: 401 at once, though the body never comes.
    for auth in [None, Some("Bearer 0000")] {
        let (status, body, took) = head_only(svc.door, &head(svc.door, auth, &[], big));
        assert_eq!(status, 401, "{auth:?}: {body}");
        assert!(took < Duration::from_secs(2), "the refusal waited for the body: {took:?}");
    }
    // The secret, and a Host or an Origin a browser would send: 403 at once.
    let (status, body, took) = head_only(svc.door, &head(svc.door, Some(&svc.bearer), &[("Origin", "http://evil.example")], big));
    assert_eq!(status, 403, "{body}");
    assert!(took < Duration::from_secs(2), "{took:?}");
    let h = head(svc.door, Some(&svc.bearer), &[], big).replace(&format!("Host: 127.0.0.1:{}", svc.door.port()), "Host: evil.example");
    let (status, body, took) = head_only(svc.door, &h);
    assert_eq!(status, 403, "{body}");
    assert!(took < Duration::from_secs(2), "{took:?}");
    // The secret and a declared length over the limit: 413 `too_large`,
    // naming the limit, at once.
    let over = cauldron_mcp::http::BODY_LIMIT + 1;
    let (status, body, took) = head_only(svc.door, &head(svc.door, Some(&svc.bearer), &[], Some(over)));
    assert_eq!(status, 413, "{body}");
    assert!(took < Duration::from_secs(2), "{took:?}");
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["error"]["data"]["code"], "too_large", "{v}");
    assert_eq!(v["error"]["data"]["limit_bytes"].as_u64(), Some(cauldron_mcp::http::BODY_LIMIT as u64));
    assert!(v["error"]["message"].as_str().unwrap().contains("16 MiB"), "{v}");
    // No declared length (a chunked body): 411, nothing read.
    let h = head(svc.door, Some(&svc.bearer), &[("Transfer-Encoding", "chunked")], None);
    let (status, body, _) = head_only(svc.door, &h);
    assert_eq!(status, 411, "{body}");
    // The door still answers.
    assert_eq!(svc.post(LIST, &[]).status, 200);
}

#[test]
fn l2_a_flood_of_unauthenticated_bodies_leaves_memory_bounded() {
    let s = Scratch::new("l2", &["kb"]);
    let svc = Service::start(&s);
    let pid = svc.child.id();
    let before = rss_kib(pid);
    let stop = Arc::new(AtomicBool::new(false));
    let watcher = watch_rss(pid, stop.clone());
    // The flood: 200 connections, no secret, each announcing
    // 16,000,000 bytes and sending 15,000,000.
    let door = svc.door;
    let chunk = vec![b'a'; 1 << 20];
    let flood: Vec<_> = (0..200)
        .map(|_| {
            let chunk = chunk.clone();
            std::thread::spawn(move || {
                let Ok(mut s) = TcpStream::connect_timeout(&door, Duration::from_secs(5)) else { return };
                let _ = s.set_write_timeout(Some(Duration::from_secs(5)));
                let h = head(door, None, &[], Some(16_000_000));
                if s.write_all(h.as_bytes()).is_err() {
                    return;
                }
                for _ in 0..15 {
                    if s.write_all(&chunk).is_err() {
                        return;
                    }
                }
            })
        })
        .collect();
    // A real call during the flood is answered.
    let t = Instant::now();
    let r = svc.post(LIST, &[]);
    assert_eq!(r.status, 200, "{}", r.body);
    let during = t.elapsed();
    for f in flood {
        f.join().unwrap();
    }
    std::thread::sleep(Duration::from_millis(300));
    stop.store(true, Ordering::SeqCst);
    let peak = watcher.join().unwrap();
    let after = rss_kib(pid);
    eprintln!("l2: rss before {before} KiB, peak {peak} KiB, after {after} KiB; a call during the flood took {during:?}");
    // Before the change this reached 3 GB. Bounded: 64 connections x 64 KiB
    // read buffers, with room for the allocator.
    assert!(peak < before + 64 * 1024, "the flood grew the service from {before} KiB to {peak} KiB");
    assert_eq!(svc.post(LIST, &[]).status, 200);
}

#[test]
fn l3_authenticated_bodies_in_flight_are_held_to_the_budget() {
    let s = Scratch::new("l3", &["kb"]);
    let svc = Service::start(&s);
    let pid = svc.child.id();
    let before = rss_kib(pid);
    let stop = Arc::new(AtomicBool::new(false));
    let watcher = watch_rss(pid, stop.clone());
    // 32 callers WITH the secret, each sending a full 16 MiB body (not
    // JSON, so each is refused once read). Unbounded, that is 512 MiB held.
    let door = svc.door;
    let bearer = svc.bearer.clone();
    let n = cauldron_mcp::http::BODY_LIMIT;
    let statuses: Vec<_> = (0..32)
        .map(|_| {
            let bearer = bearer.clone();
            std::thread::spawn(move || -> u16 {
                let mut s = TcpStream::connect(door).unwrap();
                s.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
                s.write_all(head(door, Some(&bearer), &[], Some(n)).as_bytes()).unwrap();
                let chunk = vec![b'x'; 1 << 20];
                for _ in 0..(n >> 20) {
                    if s.write_all(&chunk).is_err() {
                        break;
                    }
                }
                let mut raw = Vec::new();
                let _ = s.read_to_end(&mut raw);
                let t = String::from_utf8_lossy(&raw).into_owned();
                t.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0)
            })
        })
        .collect();
    let statuses: Vec<u16> = statuses.into_iter().map(|t| t.join().unwrap()).collect();
    std::thread::sleep(Duration::from_millis(200));
    stop.store(true, Ordering::SeqCst);
    let peak = watcher.join().unwrap();
    eprintln!("l3: rss before {before} KiB, peak {peak} KiB; statuses {statuses:?}");
    // Every body was read and answered (parse error), or told the door was busy.
    assert!(statuses.iter().all(|s| *s == 400 || *s == 503), "{statuses:?}");
    assert!(statuses.iter().filter(|s| **s == 400).count() >= 4, "{statuses:?}");
    // The budget is 64 MiB of bodies, each read into one buffer of its
    // length; 32 MiB more for the read buffers and the allocator.
    let budget_kib = (cauldron_mcp::http::INFLIGHT_BODY_BYTES / 1024) as u64;
    assert!(peak < before + budget_kib + 32 * 1024, "authenticated bodies grew the service from {before} KiB to {peak} KiB");
}

#[test]
fn l4_connections_are_capped_and_a_silent_one_is_closed_after_the_header_timeout() {
    let s = Scratch::new("l4", &["kb"]);
    let svc = Service::start(&s);
    let pid = svc.child.id();
    let cap = cauldron_mcp::http::MAX_CONNECTIONS;
    // 150 connections that send nothing.
    let idle: Vec<TcpStream> = (0..150).filter_map(|_| TcpStream::connect_timeout(&svc.door, Duration::from_secs(5)).ok()).collect();
    std::thread::sleep(Duration::from_millis(500));
    let held = door_connections(pid, svc.door.port());
    assert!(held <= cap, "the service holds {held} door connections; the cap is {cap}");
    // Once the header timeout passes the silent ones are closed, and a
    // real call is answered.
    let t = Instant::now();
    let r = svc.post(LIST, &[]);
    assert_eq!(r.status, 200, "{}", r.body);
    let waited = t.elapsed();
    assert!(waited < cauldron_mcp::http::HEADER_TIMEOUT * 3 + Duration::from_secs(5), "{waited:?}");
    eprintln!("l4: held {held} of {} idle connections; a real call waited {waited:?}", idle.len());
    // A connection that sends half a head is closed by the service.
    let mut slow = TcpStream::connect(svc.door).unwrap();
    slow.write_all(b"POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\n").unwrap();
    slow.set_read_timeout(Some(cauldron_mcp::http::HEADER_TIMEOUT * 4)).unwrap();
    let t = Instant::now();
    let mut buf = Vec::new();
    let _ = slow.read_to_end(&mut buf);
    assert!(t.elapsed() < cauldron_mcp::http::HEADER_TIMEOUT * 3, "a half head held its connection {:?}", t.elapsed());
    drop(idle);
}

#[test]
fn l5_a_body_that_stalls_is_answered_408_after_the_body_timeout() {
    let s = Scratch::new("l5", &["kb"]);
    let svc = Service::start(&s);
    let mut c = TcpStream::connect(svc.door).unwrap();
    c.set_read_timeout(Some(cauldron_mcp::http::BODY_TIMEOUT * 3)).unwrap();
    c.write_all(head(svc.door, Some(&svc.bearer), &[], Some(100)).as_bytes()).unwrap();
    c.write_all(b"{\"jsonrpc\"").unwrap();
    let t = Instant::now();
    let mut raw = Vec::new();
    let _ = c.read_to_end(&mut raw);
    let text = String::from_utf8_lossy(&raw);
    assert!(text.starts_with("HTTP/1.1 408"), "{text}");
    assert!(t.elapsed() >= cauldron_mcp::http::BODY_TIMEOUT - Duration::from_millis(500), "{:?}", t.elapsed());
}
