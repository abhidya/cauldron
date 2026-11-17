//! The read tools: the same items as the soup-door reads the CLI prints,
//! the grant checked on every call, the whole-reply budget, and how a
//! cauldron is chosen.

use serde_json::{json, Value};
use soup_door::grants::Scope;
use soup_door::reads::{self, ReadOpts};

use crate::support::*;

#[test]
fn r1_each_read_answers_the_same_items_as_the_library_the_cli_prints() {
    let s = Scratch::new("r1", &["kb"]);
    s.import("kb", &example());
    let t = s.target("kb");
    s.grant("kb", &[Scope::Read], &[]);
    let svc = Service::start(&s);
    let c = ctx(&s.project("p"));
    let big = 50_000;

    let lib = reads::regions(&t, &ReadOpts::default()).unwrap();
    let got = ok(&svc.call("soup_regions", json!({"cauldron": "kb", "max_chars": big}), &c));
    assert_eq!(got["regions"], lib["regions"]);
    assert_eq!((got["env"].as_str(), got["cauldron"].as_str(), got["resolved_from"].as_str()), (Some("dev"), Some("kb"), Some("arg")));
    assert_eq!(got["truncated"]["items_dropped"], 0);

    let lib = reads::nodes(&t, &ReadOpts { node_type: Some("issue".into()), ..ReadOpts::default() }).unwrap();
    let got = ok(&svc.call("soup_nodes", json!({"cauldron": "kb", "type": "issue", "max_chars": big}), &c));
    assert_eq!(got["nodes"], lib["nodes"]);
    assert!(!got["nodes"].as_array().unwrap().is_empty());
    let lib = reads::nodes(&t, &ReadOpts { region: Some("R-2".into()), limit: Some(2), ..ReadOpts::default() }).unwrap();
    let got = ok(&svc.call("soup_nodes", json!({"cauldron": "kb", "region": "R-2", "limit": 2, "max_chars": big}), &c));
    assert_eq!((got["nodes"].clone(), got["more"].clone()), (lib["nodes"].clone(), lib["more"].clone()));

    let lib = reads::search(&t, "schema record", &ReadOpts::default()).unwrap();
    let got = ok(&svc.call("soup_search", json!({"cauldron": "kb", "query": "schema record", "max_chars": big}), &c));
    assert_eq!(got["results"], lib["results"]);
    assert!(!got["results"].as_array().unwrap().is_empty());

    let lib = reads::region(&t, "R-1", &ReadOpts::default()).unwrap();
    let got = ok(&svc.call("soup_region", json!({"cauldron": "kb", "id": "R-1", "max_chars": big}), &c));
    for k in ["region", "parents", "children", "relates", "members", "members_total"] {
        assert_eq!(got[k], lib[k], "{k}");
    }
    assert_eq!(got["clusters"], json!([]));

    let lib = reads::get(&t, &["T-1".into(), "IS-1".into(), "NOPE-9".into()], &ReadOpts::default()).unwrap();
    let got = ok(&svc.call("soup_get", json!({"cauldron": "kb", "ids": ["T-1", "IS-1", "NOPE-9"], "max_chars": big}), &c));
    assert_eq!(got["records"], lib["records"]);
    assert_eq!(got["missing"], json!(["NOPE-9"]));

    // soup_status: the counts, the cursor, the writer closed.
    let st = ok(&svc.call("soup_status", json!({"cauldron": "kb"}), &c));
    assert_eq!(st["schema"], "workspace@6");
    assert_eq!(st["regions"], 2);
    assert_eq!(st["total"], 11, "{st}");
    assert_eq!(st["scopes"], json!(["read"]));
    assert_eq!(st["writer"]["open"], false);
    assert_eq!(st["writer"]["at_rest"], true);
    assert!(st["cursor"].as_str().unwrap().starts_with("v1."), "{st}");

    // Usage: a bad argument, an unknown argument, an empty query.
    assert_eq!(refused(&svc.call("soup_nodes", json!({"cauldron": "kb", "limit": "x"}), &c)).0, "usage");
    assert_eq!(refused(&svc.call("soup_nodes", json!({"cauldron": "kb", "colour": "red"}), &c)).0, "usage");
    assert_eq!(refused(&svc.call("soup_search", json!({"cauldron": "kb", "query": "  "}), &c)).0, "usage");
    assert_eq!(refused(&svc.call("soup_region", json!({"cauldron": "kb", "id": "T-1"}), &c)).0, "usage");
    // A read-only grant does not write.
    let (code, msg, _) = refused(&svc.call("soup_put", json!({"cauldron": "kb", "records": [{"id": "x"}]}), &c));
    assert_eq!(code, "not_granted");
    assert!(msg.contains("no `write` grant"), "{msg}");
}

#[test]
fn r2_max_chars_bounds_the_whole_reply_on_a_500_node_soup() {
    let s = Scratch::new("r2", &["kb"]);
    s.import("kb", &load_batch(499, "budget"));
    assert_eq!(s.node_count("kb"), 500);
    s.grant("kb", &[Scope::Read], &[]);
    let svc = Service::start(&s);
    let c = ctx(&s.project("p"));
    let size = |v: &Value| serde_json::to_string(v).unwrap().chars().count();

    for (tool, args) in [
        ("soup_nodes", json!({"cauldron": "kb", "limit": 500})),
        ("soup_search", json!({"cauldron": "kb", "query": "budget task", "limit": 500})),
        ("soup_region", json!({"cauldron": "kb", "id": "R-1", "limit": 500})),
        ("soup_regions", json!({"cauldron": "kb"})),
    ] {
        for max in [300usize, 2_000, 9_000] {
            let mut a = args.clone();
            a["max_chars"] = json!(max);
            let got = ok(&svc.call(tool, a, &c));
            assert!(size(&got) <= max, "{tool} at {max}: {} chars", size(&got));
            assert!(got.get("truncated").is_some(), "{tool}: no account");
        }
    }
    // Unbounded by the caller, the default bound still holds, and the reply
    // says what it left out and how to get it.
    let got = ok(&svc.call("soup_nodes", json!({"cauldron": "kb", "limit": 500}), &c));
    assert!(size(&got) <= 12_000, "{}", size(&got));
    let dropped = got["truncated"]["items_dropped"].as_u64().unwrap();
    assert!(dropped > 0);
    assert_eq!(got["nodes"].as_array().unwrap().len() as u64 + dropped, 500);
    assert!(got["next"].as_str().unwrap().contains("max_chars"), "{got}");
    // The ceiling holds whatever is asked.
    let got = ok(&svc.call("soup_nodes", json!({"cauldron": "kb", "limit": 500, "max_chars": 10_000_000}), &c));
    assert!(size(&got) <= 50_000);
}

#[test]
fn r3_the_grant_is_checked_on_every_call_and_chooses_the_cauldron() {
    let s = Scratch::new("r3", &["kb", "other"]);
    s.import("kb", &example());
    let proj = s.project("proj");
    let deep = proj.join("src").join("deep");
    std::fs::create_dir_all(&deep).unwrap();
    s.grant("kb", &[Scope::Read, Scope::Write], &[&proj]);
    let svc = Service::start(&s);

    // From the project (and below it, and through a symbolic link to it).
    let st = ok(&svc.call("soup_status", json!({}), &ctx(&deep)));
    assert_eq!(st["cauldron"], "kb");
    assert_eq!(st["resolved_from"], format!("project:{}", proj.display()));
    let link = s.0.join("link-to-proj");
    std::os::unix::fs::symlink(&proj, &link).unwrap();
    assert_eq!(ok(&svc.call("soup_status", json!({}), &ctx(&link)))["cauldron"], "kb");
    // `$CAULDRON_CAULDRON` of the session beats the project, and the
    // argument beats both. `other` is not granted: nothing is read.
    let mut c = ctx(&proj);
    c["cauldron_var"] = json!("other");
    assert_eq!(refused(&svc.call("soup_status", json!({}), &c)).0, "not_granted");
    let st = ok(&svc.call("soup_status", json!({"cauldron": "kb"}), &c));
    assert_eq!(st["resolved_from"], "arg");
    // Outside every project, nothing.
    assert_eq!(refused(&svc.call("soup_nodes", json!({}), &ctx(&s.project("elsewhere")))).0, "no_cauldron_for_project");

    // REVOKE BETWEEN TWO CALLS: the first reads, the second is refused.
    let first = svc.call("soup_nodes", json!({}), &ctx(&proj));
    assert_eq!(ok(&first)["cauldron"], "kb");
    s.revoke("kb");
    let (code, msg, _) = refused(&svc.call("soup_nodes", json!({}), &ctx(&proj)));
    assert_eq!(code, "no_cauldron_for_project", "{msg}");
    let (code, _, _) = refused(&svc.call("soup_nodes", json!({"cauldron": "kb"}), &ctx(&proj)));
    assert_eq!(code, "not_granted");
    // Granted again, read again.
    s.grant("kb", &[Scope::Read], &[]);
    assert_eq!(ok(&svc.call("soup_nodes", json!({"cauldron": "kb"}), &ctx(&proj)))["cauldron"], "kb");
    // An unreadable store is never "everything open" and never "unchanged".
    std::fs::write(s.mcp_dir().join("grants.json"), "{not json").unwrap();
    assert_eq!(refused(&svc.call("soup_nodes", json!({"cauldron": "kb"}), &ctx(&proj))).0, "grant_store");
}

#[test]
fn r4_a_cauldron_made_again_under_a_granted_name_is_not_opened_by_the_old_grant() {
    let s = Scratch::new("r4", &["ro"]);
    s.grant("ro", &[Scope::Read, Scope::Write], &[]);
    let svc = Service::start(&s);
    let c = ctx(&s.project("p"));
    let first = ok(&svc.call("soup_status", json!({"cauldron": "ro"}), &c));
    // Opened for writing, so the writer must let go of the old one too.
    ok(&svc.call("soup_put", json!({"cauldron": "ro", "remint": true, "records": example()}), &c));
    // Removed out of band: gone.
    let dir = s.data().join("cauldrons").join("ro");
    std::fs::rename(&dir, s.0.join("ro-removed")).unwrap();
    assert_eq!(refused(&svc.call("soup_status", json!({"cauldron": "ro"}), &c)).0, "cauldron_gone");
    // Made again under the same name: still gone, for reads and writes, and
    // nothing is written into the new one.
    std::thread::sleep(std::time::Duration::from_millis(10));
    s.create("ro");
    for (tool, args) in [("soup_status", json!({"cauldron": "ro"})), ("soup_put", json!({"cauldron": "ro", "remint": true, "records": example()}))] {
        let (code, msg, _) = refused(&svc.call(tool, args, &c));
        assert_eq!(code, "cauldron_gone", "{tool}: {msg}");
        assert!(msg.contains("cauldron mcp grant ro --env dev"), "{msg}");
    }
    assert_eq!(s.node_count("ro"), 0);
    // Granted again, the new one opens, under a new cursor.
    s.grant("ro", &[Scope::Read], &[]);
    let second = ok(&svc.call("soup_status", json!({"cauldron": "ro"}), &c));
    assert_ne!(first["cursor"], second["cursor"]);
}
