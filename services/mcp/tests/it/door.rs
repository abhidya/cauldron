//! The service skeleton: the door's guards, the catalogue, the seam.

use serde_json::{json, Value};

use crate::support::*;

const LIST: &str = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}"#;

/// Every tool of the design's catalogue, in order.
const TOOLS: [&str; 12] = [
    "soup_status",
    "soup_regions",
    "soup_region",
    "soup_nodes",
    "soup_search",
    "soup_get",
    "soup_put",
    "soup_link",
    "soup_changes",
    "soup_claim",
    "soup_dump",
    "soup_dump_session",
];

#[test]
fn d1_the_door_admits_only_the_secret_on_loopback_and_no_browser() {
    let s = Scratch::new("d1", &["kb"]);
    let svc = Service::start(&s);

    // The secret: absent, wrong, and without its scheme are all 401, and the
    // refusal never echoes a secret.
    for auth in ["", "Bearer 0000", &svc.bearer.replace("Bearer ", "")] {
        let r = svc.post(LIST, &[("Authorization", auth)]);
        assert_eq!(r.status, 401, "{auth:?}: {}", r.body);
        assert!(r.head.to_ascii_lowercase().contains("www-authenticate"), "{}", r.head);
    }
    let secret = svc.bearer.trim_start_matches("Bearer ").to_string();
    assert_eq!(secret.len(), 64);
    // An Origin, any Origin, is 403 even with the secret.
    for origin in ["http://evil.example", "null", &format!("http://127.0.0.1:{}", svc.door.port())] {
        let r = svc.post(LIST, &[("Origin", origin)]);
        assert_eq!(r.status, 403, "{origin}: {}", r.body);
    }
    // A Host that is not this loopback door is 403 (the DNS-rebinding shape).
    for host in ["evil.example", "evil.example:80", &format!("127.0.0.2:{}", svc.door.port()), "127.0.0.1:1", ""] {
        let r = svc.post(LIST, &[("Host", host)]);
        assert_eq!(r.status, 403, "{host:?}: {}", r.body);
    }
    let r = svc.post(LIST, &[("Host", &format!("localhost:{}", svc.door.port()))]);
    assert_eq!(r.status, 200, "{}", r.body);

    // The catalogue: every tool, `_ctx` never shown.
    let r = svc.post(LIST, &[]);
    assert_eq!(r.status, 200, "{}", r.body);
    let v: Value = serde_json::from_str(&r.body).unwrap();
    let tools = v["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, TOOLS);
    assert!(!r.body.contains("_ctx"), "tools/list shows _ctx");
    assert!(names.iter().all(|n| !n.contains("grant") && !n.contains("revoke")));

    // initialize, ping, a notification, GET.
    let r = svc.post(r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{}}}"#, &[]);
    let v: Value = serde_json::from_str(&r.body).unwrap();
    assert_eq!(v["result"]["serverInfo"]["name"], "cauldron-mcp", "{v}");
    assert!(v["result"]["instructions"].as_str().unwrap().contains("closed to it until its owner grants it"));
    assert!(r.head.to_ascii_lowercase().contains("mcp-session-id"));
    let r = svc.post(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#, &[]);
    assert_eq!(r.status, 202);
    let h = vec![("Host".to_string(), format!("127.0.0.1:{}", svc.door.port())), ("Authorization".to_string(), svc.bearer.clone())];
    assert_eq!(http(svc.door, "GET", &h, "").status, 405);

    // Default closed: no grant, nothing read.
    let work = s.project("p");
    let res = svc.call("soup_status", json!({"cauldron": "kb"}), &ctx(&work));
    let (code, msg, _) = refused(&res);
    assert_eq!(code, "not_granted", "{msg}");
    assert!(msg.contains("cauldron mcp grant kb --env dev"), "{msg}");
    let (code, _, _) = refused(&svc.call("soup_status", json!({}), &ctx(&work)));
    assert_eq!(code, "no_cauldron_for_project");
    // No `_ctx`, a malformed one: `bad_ctx`.
    let body = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "soup_status", "arguments": {"cauldron": "kb"}}});
    let r: Value = serde_json::from_str(&svc.post(&body.to_string(), &[]).body).unwrap();
    assert_eq!(refused(&r["result"]).0, "bad_ctx");
    assert_eq!(refused(&svc.call("soup_status", json!({"cauldron": "kb"}), &json!({"cwd": "/"}))).0, "bad_ctx");
    // Listed but not built here, and never supported.
    assert_eq!(refused(&svc.call("soup_dump", json!({"text": "x"}), &ctx(&work))).0, "not_available_yet");
    assert_eq!(refused(&svc.call("soup_claim", json!({"cluster": "x", "region": "R-1"}), &ctx(&work))).0, "unsupported");
    // An unknown tool is the protocol's -32602.
    let body = json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "soup_grant", "arguments": {}}});
    let r: Value = serde_json::from_str(&svc.post(&body.to_string(), &[]).body).unwrap();
    assert_eq!(r["error"]["code"], json!(-32602), "{r}");

    // Loopback only: every listener the process holds is on 127.0.0.1.
    let out = std::process::Command::new("/usr/sbin/lsof")
        .args(["-nP", "-a", "-p", &svc.child.id().to_string(), "-iTCP", "-sTCP:LISTEN", "-Fn"])
        .output()
        .unwrap();
    let names: Vec<String> = String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| l.strip_prefix('n').map(str::to_string)).collect();
    assert_eq!(names.len(), 2, "the door and the seam, nothing else: {names:?}");
    assert!(names.iter().all(|n| n.starts_with("127.0.0.1:")), "a listener off loopback: {names:?}");
    assert!(names.contains(&format!("127.0.0.1:{}", svc.door.port())), "{names:?}");

    // The secret file is private, and the secret is nowhere in the log.
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(s.mcp_dir().join("door.secret")).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    let log = svc.log();
    assert!(log.contains("cauldron-mcp listening"), "{log}");
    assert!(!log.contains(&secret), "the secret is in the log");
}

#[test]
fn d2_the_seam_is_live_by_cauldron_own_probe_and_a_restart_mints_a_new_secret() {
    let s = Scratch::new("d2", &["kb"]);
    let svc = Service::start(&s);
    let first = svc.bearer.clone();
    let table = env::seams::Table::load("dev", &s.data(), &s.data().join("seams.json")).unwrap();
    let row = table.seams.get("mcp").expect("the service wrote its row");
    assert!(row.registered);
    // The supervisor's own instruments, not this test's: the HELLO probe and
    // the warnings compliance check (what `cauldron status` reports).
    env::egress::grant_addr(row.addr.parse().unwrap()).unwrap();
    let verdict = env::seams::probe(row);
    assert_eq!(verdict.state, env::seams::SeamState::Live, "{}", verdict.detail);
    let c = env::warnings::check(row, &verdict, Some(env::warnings::SHAPE));
    assert!(!c.is_violation(), "{:?}", c.missing());
    assert_eq!(seam_say(row.addr.parse().unwrap(), "PING"), "PONG");

    let st = svc.stop();
    assert!(st.success(), "SIGTERM is a clean stop: {st}");
    let again = Service::start(&s);
    assert_ne!(again.bearer, first, "a start mints a new secret");
    let r = http(
        again.door,
        "POST",
        &[
            ("Host".into(), format!("127.0.0.1:{}", again.door.port())),
            ("Authorization".into(), first),
            ("Accept".into(), "application/json, text/event-stream".into()),
            ("Content-Type".into(), "application/json".into()),
        ],
        LIST,
    );
    assert_eq!(r.status, 401, "the old secret still opens the door");
}
