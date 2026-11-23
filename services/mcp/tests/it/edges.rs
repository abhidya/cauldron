//! Correcting a record after the fact (issue #24): `soup_unlink` removes
//! edges between records the soup holds (flat or a list, idempotent,
//! refusing the edges a record must keep, honouring region locks, audited,
//! `unlink` in the change feed), and `soup_cite` adds or removes one entry
//! of `meta.cites` and nothing else.

use serde_json::{json, Value};
use soup_door::grants::Scope;

use crate::support::*;

fn setup(tag: &str, scopes: &[Scope]) -> (Scratch, Service, Value) {
    let s = Scratch::new(tag, &["kb"]);
    s.import("kb", &example());
    s.grant("kb", scopes, &[&s.project("p")]);
    let svc = Service::start(&s);
    let c = ctx(&s.project("p"));
    (s, svc, c)
}

fn rw() -> Vec<Scope> {
    vec![Scope::Read, Scope::Write]
}

fn edges_of(svc: &Service, c: &Value, id: &str) -> Vec<(String, String)> {
    let got = ok(&svc.call("soup_get", json!({"ids": [id]}), c));
    got["records"][0]["edges"].as_array().unwrap().iter().map(|e| (e["kind"].as_str().unwrap().to_string(), e["to"].as_str().unwrap().to_string())).collect()
}

fn has(svc: &Service, c: &Value, id: &str, kind: &str, to: &str) -> bool {
    edges_of(svc, c, id).contains(&(kind.to_string(), to.to_string()))
}

fn cursor(svc: &Service, c: &Value) -> String {
    ok(&svc.call("soup_status", json!({}), c))["cursor"].as_str().unwrap().to_string()
}

/// One wrong edge, removed flat: `soup_get` stops showing it, the change
/// feed has it as `unlink`, the call is audited; the same call again says
/// `not-linked` and writes nothing.
#[test]
fn ul1_one_edge_is_removed_and_a_repeat_changes_nothing() {
    let (s, svc, c) = setup("ul1", &rw());
    assert!(has(&svc, &c, "DEC-1", "derived_from", "IS-1"));
    let since = cursor(&svc, &c);
    let d = ok(&svc.call("soup_unlink", json!({"from": "DEC-1", "kind": "derived_from", "to": "IS-1"}), &c));
    assert_eq!((d["outcome"].as_str(), d["written"].as_bool()), (Some("unlinked"), Some(true)), "{d}");
    assert_eq!(d["links"], json!([{"from": "DEC-1", "kind": "derived_from", "to": "IS-1", "status": "unlinked"}]), "{d}");
    assert!(!has(&svc, &c, "DEC-1", "derived_from", "IS-1"), "the old record keeps no wrong edge");
    assert!(has(&svc, &c, "DEC-1", "in_region", "R-1"), "nothing else moved");
    let ch = ok(&svc.call("soup_changes", json!({"since": since}), &c));
    let changes = ch["changes"].as_array().unwrap();
    assert_eq!(changes.len(), 1, "{ch}");
    assert_eq!(changes[0]["op"], "unlink", "{ch}");
    let after = ch["cursor"].as_str().unwrap().to_string();
    let again = ok(&svc.call("soup_unlink", json!({"from": "DEC-1", "kind": "derived_from", "to": "IS-1"}), &c));
    assert_eq!((again["outcome"].as_str(), again["written"].as_bool()), (Some("not-linked"), Some(false)), "{again}");
    assert_eq!(again["links"][0]["status"], "not-linked");
    let ch = ok(&svc.call("soup_changes", json!({"since": after}), &c));
    assert!(ch["changes"].as_array().unwrap().is_empty(), "a repeat writes nothing: {ch}");
    let lines: Vec<Value> = s.calls().into_iter().filter(|l| l["tool"] == "soup_unlink").collect();
    let outcomes: Vec<&str> = lines.iter().map(|l| l["outcome"].as_str().unwrap()).collect();
    assert_eq!(outcomes, ["unlinked", "not-linked"]);
    assert!(lines[0]["tx"].is_string(), "{}", lines[0]);
}

/// The list form removes several in one write; an absent one among them is
/// `not-linked`; both forms at once is `usage`.
#[test]
fn ul2_the_list_form_removes_several_in_one_write() {
    let (_s, svc, c) = setup("ul2", &rw());
    ok(&svc.call("soup_link", json!({"links": [{"from": "T-1", "kind": "ref", "to": "M-1"}, {"from": "T-1", "kind": "ref", "to": "DEC-1"}]}), &c));
    let since = cursor(&svc, &c);
    let d = ok(&svc.call("soup_unlink", json!({"links": [
        {"from": "T-1", "kind": "ref", "to": "M-1"}, {"from": "T-1", "kind": "ref", "to": "DEC-1"}, {"from": "T-1", "kind": "ref", "to": "CON-1"}]}), &c));
    let st: Vec<&str> = d["links"].as_array().unwrap().iter().map(|l| l["status"].as_str().unwrap()).collect();
    assert_eq!(st, ["unlinked", "unlinked", "not-linked"], "{d}");
    assert!(!has(&svc, &c, "T-1", "ref", "M-1") && !has(&svc, &c, "T-1", "ref", "DEC-1"));
    let ch = ok(&svc.call("soup_changes", json!({"since": since}), &c));
    assert_eq!(ch["changes"].as_array().unwrap().len(), 1, "one write: {ch}");
    let (code, msg, _) = refused(&svc.call("soup_unlink", json!({"from": "T-1", "kind": "ref", "to": "M-1", "links": [{"from": "T-1", "kind": "ref", "to": "M-1"}]}), &c));
    assert_eq!(code, "usage", "{msg}");
    assert!(msg.contains("soup_unlink") && msg.contains("not both"), "{msg}");
    let (code, _, _) = refused(&svc.call("soup_unlink", json!({"from": "T-1", "kind": "ref", "to": "M-1", "note": "why"}), &c));
    assert_eq!(code, "usage", "a removed edge takes no note");
}

/// What a record must keep is refused by name, and one refusal in a batch
/// writes nothing: the last `in_region` (removable once another is added),
/// a milestone's last `serves`, a `supersedes`, a `claims`, an unknown
/// record or kind.
#[test]
fn ul3_the_edges_a_record_must_keep_are_refused_and_nothing_is_written() {
    let (s, svc, c) = setup("ul3", &rw());
    let reason = |args: Value| -> (String, Value) {
        let (code, _, detail) = refused(&svc.call("soup_unlink", args, &c));
        (code, detail)
    };
    // T-1's only in_region.
    let (code, detail) = reason(json!({"from": "T-1", "kind": "in_region", "to": "R-2"}));
    assert_eq!(code, "links_refused", "{detail}");
    assert_eq!(detail["links"][0]["refused"]["reason"], "required-edge", "{detail}");
    assert!(detail["links"][0]["refused"]["detail"].as_str().unwrap().contains("in_region"), "{detail}");
    assert!(has(&svc, &c, "T-1", "in_region", "R-2"));
    // With a second in_region it may go: the record is moved, not orphaned.
    ok(&svc.call("soup_link", json!({"from": "T-1", "kind": "in_region", "to": "R-1"}), &c));
    let d = ok(&svc.call("soup_unlink", json!({"from": "T-1", "kind": "in_region", "to": "R-2"}), &c));
    assert_eq!(d["outcome"], "unlinked", "{d}");
    assert_eq!(edges_of(&svc, &c, "T-1").iter().filter(|(k, _)| k == "in_region").count(), 1);
    // DEC-1's one in_region, given as a list: refused the same way.
    let (_, detail) = reason(json!({"links": [{"from": "DEC-1", "kind": "in_region", "to": "R-1"}]}));
    assert_eq!(detail["links"][0]["refused"]["reason"], "required-edge", "{detail}");
    // A milestone's last `serves`.
    let (_, detail) = reason(json!({"from": "M-1", "kind": "serves", "to": "DIR-1"}));
    assert_eq!(detail["links"][0]["refused"]["reason"], "required-edge", "{detail}");
    assert!(detail["links"][0]["refused"]["detail"].as_str().unwrap().contains("serves"), "{detail}");
    // A supersedes edge is history.
    let put = ok(&svc.call("soup_put", json!({"remint": true, "records": [{"id": "d2", "type": "decision", "state": "decided",
        "handle": "Validate every record, again", "edges": [{"kind": "in_region", "to": "R-1"}, {"kind": "supersedes", "to": "DEC-1", "note": "restated"}]}]}), &c));
    let d2 = put["id_map"]["d2"].as_str().unwrap().to_string();
    let (_, detail) = reason(json!({"from": d2, "kind": "supersedes", "to": "DEC-1"}));
    assert_eq!(detail["links"][0]["refused"]["reason"], "supersedes-is-history", "{detail}");
    assert!(has(&svc, &c, &d2, "supersedes", "DEC-1"));
    // A claim is soup_claim's.
    let (_, detail) = reason(json!({"from": "CL-1", "kind": "claims", "to": "R-1"}));
    assert_eq!(detail["links"][0]["refused"]["reason"], "claims-is-soup-claim", "{detail}");
    // Unknown record, unknown kind.
    let (_, detail) = reason(json!({"from": "T-99", "kind": "ref", "to": "M-1"}));
    assert_eq!(detail["links"][0]["refused"]["reason"], "unknown-node", "{detail}");
    let (_, detail) = reason(json!({"from": "T-1", "kind": "evidence", "to": "M-1"}));
    assert_eq!(detail["links"][0]["refused"]["reason"], "unknown-kind", "{detail}");
    // One refusal in a batch: the removable one stays too.
    let since = cursor(&svc, &c);
    let (_, detail) = reason(json!({"links": [{"from": "IS-1", "kind": "ref", "to": "DEC-1"}, {"from": "M-1", "kind": "serves", "to": "DIR-1"}]}));
    assert_eq!(detail["links"][0]["status"], "accepted", "{detail}");
    assert_eq!(detail["links"][1]["status"], "refused", "{detail}");
    assert!(has(&svc, &c, "IS-1", "ref", "DEC-1"), "all or nothing");
    let ch = ok(&svc.call("soup_changes", json!({"since": since}), &c));
    assert!(ch["changes"].as_array().unwrap().is_empty(), "{ch}");
    let refusals = s.calls().into_iter().filter(|l| l["tool"] == "soup_unlink" && l["outcome"] == "links_refused").count();
    assert!(refusals >= 5, "refusals are audited: {refusals}");
}

/// A held region refuses a removal touching it, as it refuses a link; a
/// read-only grant refuses both tools.
#[test]
fn ul4_region_locks_and_the_write_scope_hold() {
    let s = Scratch::new("ul4", &["kb"]);
    s.import("kb", &example());
    s.grant("kb", &[Scope::Read, Scope::Write, Scope::Dump], &[&s.project("p")]);
    let svc = Service::start(&s);
    let c = ctx(&s.project("p"));
    ok(&svc.call("soup_dump", json!({"text": "a note about the schema"}), &c));
    let key = std::fs::read_to_string(s.mcp_dir().join("ripples.secret")).unwrap().trim().to_string();
    let lock: Value = serde_json::from_str(&seam_say(
        svc.seam,
        &format!("RLOCK {key} {}", json!({"cauldron": "kb", "run": "run-a", "dump": "DM-1", "regions": ["R-1"], "ttl_secs": 120})),
    ))
    .unwrap();
    assert_eq!(lock["outcome"], "locked", "{lock}");
    let (code, msg, _) = refused(&svc.call("soup_unlink", json!({"from": "IS-1", "kind": "ref", "to": "DEC-1"}), &c));
    assert_eq!(code, "region_locked", "{msg}");
    assert!(has(&svc, &c, "IS-1", "ref", "DEC-1"));
    let d = ok(&svc.call("soup_unlink", json!({"from": "Q-1", "kind": "blocks", "to": "T-1"}), &c));
    assert_eq!(d["outcome"], "unlinked", "a removal in R-2 alone is not held: {d}");
    drop(svc);

    let (_s2, svc2, c2) = setup("ul4b", &[Scope::Read]);
    for (tool, args) in [
        ("soup_unlink", json!({"from": "DEC-1", "kind": "derived_from", "to": "IS-1"})),
        ("soup_cite", json!({"id": "T-1", "add": {"kind": "url", "url": "https://example.com/a"}})),
    ] {
        let (code, _, _) = refused(&svc2.call(tool, args, &c2));
        assert_eq!(code, "not_granted", "{tool}");
    }
}

/// `soup_cite`: add one, see it, add it again (`already-cited`), remove it
/// by match and another by index, refuse a bad citation, an index past the
/// end and a match naming two; no other meta key moves; a cluster's
/// citations too; the feed shows each write.
#[test]
fn ci1_one_citation_is_added_or_removed_and_nothing_else() {
    let (s, svc, c) = setup("ci1", &rw());
    let meta = |id: &str| ok(&svc.call("soup_get", json!({"ids": [id]}), &c))["records"][0]["meta"].clone();
    let before = meta("DEC-1");
    assert_eq!(before["kind"], "pattern", "{before}");
    let url = json!({"kind": "url", "url": "https://example.com/decision"});
    let commit = json!({"kind": "commit", "repo": "example/repo", "sha": "0123abc"});
    let since = cursor(&svc, &c);
    let d = ok(&svc.call("soup_cite", json!({"id": "DEC-1", "add": url}), &c));
    assert_eq!((d["outcome"].as_str(), d["written"].as_bool()), (Some("cited"), Some(true)), "{d}");
    ok(&svc.call("soup_cite", json!({"id": "DEC-1", "add": commit}), &c));
    let m = meta("DEC-1");
    assert_eq!(m["cites"], json!([url, commit]), "{m}");
    assert_eq!(m["kind"], "pattern", "no other meta key moved: {m}");
    let again = ok(&svc.call("soup_cite", json!({"id": "DEC-1", "add": url}), &c));
    assert_eq!((again["outcome"].as_str(), again["written"].as_bool()), (Some("already-cited"), Some(false)), "{again}");
    let ch = ok(&svc.call("soup_changes", json!({"since": since}), &c));
    assert_eq!(ch["changes"].as_array().unwrap().len(), 2, "{ch}");
    // Each entry keeps its id after a later write to the same record: the
    // first citation's entry is read from its own write, not from the
    // record's latest one.
    for e in ch["changes"].as_array().unwrap() {
        assert_eq!((e["op"].as_str(), e["count"].as_u64(), e["ids"][0]["id"].as_str()), (Some("update"), Some(1), Some("DEC-1")), "{ch}");
    }
    // Refusals.
    let (code, msg, _) = refused(&svc.call("soup_cite", json!({"id": "DEC-1", "add": {"kind": "commit", "repo": "x"}}), &c));
    assert_eq!(code, "usage");
    assert!(msg.contains("bad-cite") && msg.contains("sha"), "{msg}");
    let (code, _, _) = refused(&svc.call("soup_cite", json!({"id": "DEC-1", "add": {"kind": "approved", "at": "now"}}), &c));
    assert_eq!(code, "usage", "a review flag is no citation");
    let (code, msg, _) = refused(&svc.call("soup_cite", json!({"id": "DEC-1", "remove": 5}), &c));
    assert_eq!(code, "usage", "{msg}");
    ok(&svc.call("soup_cite", json!({"id": "DEC-1", "add": {"kind": "url", "url": "https://example.com/other"}}), &c));
    let (code, msg, _) = refused(&svc.call("soup_cite", json!({"id": "DEC-1", "remove": {"kind": "url"}}), &c));
    assert_eq!(code, "usage", "{msg}");
    assert!(msg.contains("index"), "{msg}");
    let (code, _, _) = refused(&svc.call("soup_cite", json!({"id": "DEC-1", "add": url, "remove": 0}), &c));
    assert_eq!(code, "usage");
    let (code, _, _) = refused(&svc.call("soup_cite", json!({"id": "T-99", "add": url}), &c));
    assert_eq!(code, "not_found");
    // Removal by match, then by index.
    let r = ok(&svc.call("soup_cite", json!({"id": "DEC-1", "remove": {"kind": "commit", "sha": "0123abc"}}), &c));
    assert_eq!(r["outcome"], "uncited", "{r}");
    assert_eq!(r["removed"], commit);
    let r = ok(&svc.call("soup_cite", json!({"id": "DEC-1", "remove": 0}), &c));
    assert_eq!(r["removed"], url, "{r}");
    let r = ok(&svc.call("soup_cite", json!({"id": "DEC-1", "remove": {"kind": "commit"}}), &c));
    assert_eq!((r["outcome"].as_str(), r["written"].as_bool()), (Some("not-cited"), Some(false)), "{r}");
    let m = meta("DEC-1");
    assert_eq!(m["cites"], json!([{"kind": "url", "url": "https://example.com/other"}]), "{m}");
    assert_eq!(m["kind"], "pattern");
    // A record with no meta yet, and a cluster.
    ok(&svc.call("soup_cite", json!({"id": "T-1", "add": url}), &c));
    assert_eq!(meta("T-1")["cites"], json!([url]));
    let put = ok(&svc.call("soup_put", json!({"remint": true, "records": [{"id": "k", "type": "cluster", "handle": "Shipping",
        "boundary": "Inside: everything that gets a release to the people who use it, end to end."}]}), &c));
    let k = put["id_map"]["k"].as_str().unwrap().to_string();
    let d = ok(&svc.call("soup_cite", json!({"id": k, "add": url}), &c));
    assert_eq!(d["outcome"], "cited", "{d}");
    let got = ok(&svc.call("soup_get", json!({"ids": [k]}), &c));
    assert!(got["records"][0]["meta"]["cites"].as_array().unwrap().contains(&url), "{got}");
    assert_eq!(s.calls().into_iter().filter(|l| l["tool"] == "soup_cite").count(), 15, "every call is audited");
}

/// **The write tools under owner acts.** `soup_link`, `soup_unlink` and
/// `soup_cite` are ordinary `write` tools: they work on a record a ripple
/// wrote as on any record, and none of them sets or clears what only the
/// owner's decision or the door writes. On a ripple's records, one
/// rejected and one approved at the owner's verb, the decision (`approved`,
/// `rejected`, its `by`, `at` and `reason`), `meta.ripple` and the record's
/// place in the review queue are the same after a link, an unlink and a
/// citation added and removed as before them. The `dump` citation that
/// ties a ripple's record to its dump (what review groups and decides by)
/// is neither removed (by index or by match) nor added (a second dump
/// citation would misstate where the record came from), each refused
/// `dump-cite-is-provenance` with nothing written; on a record no ripple wrote, a `dump` citation is
/// added and removed as any other.
#[test]
fn ci2_the_write_tools_never_touch_a_review_trail() {
    let s = Scratch::new("ci2", &["kb"]);
    s.import("kb", &example());
    let a = crate::review::ripple_batch(&s, "kb", "run-a", "DM-1", &["alpha idea", "beta idea"]);
    let bare = crate::review::ripple_idea(&s, "kb", "gamma idea");
    s.grant("kb", &rw(), &[&s.project("p")]);
    let svc = Service::start(&s);
    let c = ctx(&s.project("p"));
    let owner = std::fs::read_to_string(s.mcp_dir().join("approve.secret")).unwrap().trim().to_string();
    let say = |body: Value| -> Value { serde_json::from_str(&seam_say(svc.seam, &format!("DECIDE {owner} {body}"))).unwrap() };
    assert_eq!(say(json!({"cauldron": "kb", "ids": [a[0]], "action": "reject", "reason": "off topic"}))["ok"], json!(true));
    assert_eq!(say(json!({"cauldron": "kb", "ids": [a[1]], "action": "approve"}))["ok"], json!(true));
    let trail = |id: &str| {
        let r = ok(&svc.call("soup_get", json!({"ids": [id]}), &c))["records"][0].clone();
        let m = &r["meta"];
        json!({"approved": m["approved"], "rejected": m["rejected"], "ripple": m["ripple"], "unreviewed": r["unreviewed"], "flag": r["rejected"]})
    };
    let queue = || ok(&svc.call("soup_review", json!({}), &c))["counts"].clone();
    let before: Vec<Value> = [&a[0], &a[1], &bare].iter().map(|id| trail(id)).collect();
    assert_eq!(before[0]["rejected"]["by"], "tty", "{before:?}");
    assert_eq!(before[1]["approved"]["by"], "tty", "{before:?}");
    let q0 = queue();

    // Ordinary writes on the ripple's records.
    for id in [&a[0], &a[1], &bare] {
        ok(&svc.call("soup_link", json!({"from": id, "kind": "ref", "to": "DEC-1"}), &c));
        ok(&svc.call("soup_unlink", json!({"from": id, "kind": "ref", "to": "DEC-1"}), &c));
        let url = json!({"kind": "url", "url": "https://example.com/seen"});
        ok(&svc.call("soup_cite", json!({"id": id, "add": url}), &c));
        ok(&svc.call("soup_cite", json!({"id": id, "remove": url}), &c));
    }
    let after: Vec<Value> = [&a[0], &a[1], &bare].iter().map(|id| trail(id)).collect();
    assert_eq!(after, before, "no write tool moved a decision, its `by` or a ripple's provenance");
    assert_eq!(queue(), q0, "the review queue is as it was");

    // The dump citation of a ripple's record is its review trail.
    let since = cursor(&svc, &c);
    for args in [
        json!({"id": a[1], "remove": 0}),
        json!({"id": a[1], "remove": {"kind": "dump"}}),
        json!({"id": a[1], "remove": {"kind": "dump", "id": "DM-1"}}),
        json!({"id": a[1], "add": {"kind": "dump", "id": "DM-9"}}),
        json!({"id": bare, "remove": {"kind": "dump"}}),
    ] {
        let (code, msg, detail) = refused(&svc.call("soup_cite", args.clone(), &c));
        assert_eq!(code, "usage", "{args}: {msg}");
        assert!(msg.contains("dump-cite-is-provenance") && msg.contains("Nothing was written"), "{args}: {msg}");
        assert_eq!(detail["reason"], "dump-cite-is-provenance", "{args}: {detail}");
    }
    let ch = ok(&svc.call("soup_changes", json!({"since": since}), &c));
    assert!(ch["changes"].as_array().unwrap().is_empty(), "nothing was written: {ch}");
    let cites = |id: &str| ok(&svc.call("soup_get", json!({"ids": [id]}), &c))["records"][0]["meta"]["cites"].clone();
    assert_eq!(cites(&a[1]), json!([{"kind": "dump", "id": "DM-1"}]));
    // On a record no ripple wrote, a dump citation is an ordinary one.
    let d = json!({"kind": "dump", "id": "DM-1"});
    ok(&svc.call("soup_cite", json!({"id": "DEC-1", "add": d}), &c));
    let r = ok(&svc.call("soup_cite", json!({"id": "DEC-1", "remove": d}), &c));
    assert_eq!(r["outcome"], "uncited", "{r}");
    svc.stop();
}

/// **A repeated link writes nothing** (issue #23, item 2.12). An edge the
/// soup already holds is answered `already-linked`, and when every link of
/// the call is, nothing is written (`written: false`, no change entry), as
/// `soup_claim` answers `already-claimed`; its note stays as it was. In a
/// list, the new edges land in one write and the held ones are named.
#[test]
fn lk1_a_repeated_link_is_already_linked_and_writes_nothing() {
    let (s, svc, c) = setup("lk1", &rw());
    let first = ok(&svc.call("soup_link", json!({"from": "DEC-1", "kind": "ref", "to": "T-1", "note": "first"}), &c));
    assert_eq!((first["outcome"].as_str(), first["written"].as_bool(), first["links"][0]["status"].as_str()), (Some("linked"), Some(true), Some("linked")), "{first}");
    let since = cursor(&svc, &c);
    let again = ok(&svc.call("soup_link", json!({"from": "DEC-1", "kind": "ref", "to": "T-1", "note": "second"}), &c));
    assert_eq!((again["outcome"].as_str(), again["written"].as_bool(), again["links"][0]["status"].as_str()), (Some("already-linked"), Some(false), Some("already-linked")), "{again}");
    let ch = ok(&svc.call("soup_changes", json!({"since": since}), &c));
    assert!(ch["changes"].as_array().unwrap().is_empty(), "a repeat writes no change entry: {ch}");
    let got = ok(&svc.call("soup_get", json!({"ids": ["DEC-1"]}), &c));
    let notes: Vec<&Value> = got["records"][0]["edges"].as_array().unwrap().iter().filter(|e| e["kind"] == "ref" && e["to"] == "T-1").collect();
    assert_eq!(notes.len(), 1, "{got}");
    assert_eq!(notes[0]["note"], "first", "the note stays as it was: {got}");
    // A list with one held and one new edge: one write, both named.
    let mixed = ok(&svc.call("soup_link", json!({"links": [{"from": "DEC-1", "kind": "ref", "to": "T-1"}, {"from": "T-1", "kind": "ref", "to": "DEC-1"}]}), &c));
    let st: Vec<&str> = mixed["links"].as_array().unwrap().iter().map(|l| l["status"].as_str().unwrap()).collect();
    assert_eq!((mixed["outcome"].as_str(), st), (Some("linked"), vec!["already-linked", "linked"]), "{mixed}");
    let ch = ok(&svc.call("soup_changes", json!({"since": since}), &c));
    assert_eq!(ch["changes"].as_array().unwrap().len(), 1, "{ch}");
    let _ = s;
    svc.stop();
}

/// Damage no write makes any more, written straight into the spine before
/// the service holds the soup (as a 0.1.17 candidate's `soup_link` let a
/// session do): `DEC-1 supersedes DEC-1`, and the cycle `IS-1 supersedes
/// IS-2`, `IS-2 supersedes IS-1` around a second issue.
fn damage(s: &Scratch) -> String {
    use spine::envelope::{Envelope, Initiator, Instrument, Mode, Provenance, ENVELOPE_FORMAT_VERSION};
    let cauldron = spine::Cauldron::open(&s.soup("kb"), spine::config::SpineConfig::default()).unwrap();
    let submit = |statement: &str, b: Vec<(&str, Value)>, nonce: &str| -> Value {
        let env = Envelope {
            statement: statement.into(),
            bindings: b.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
            format_version: ENVELOPE_FORMAT_VERSION,
            provenance: Provenance {
                initiator: Initiator::Agent { thread: "test".into(), dispatch: "damage".into() },
                instrument: Instrument { name: "test".into(), version: "0".into() },
            },
            mode: Mode::Commit,
            idempotency_nonce: Some(nonce.into()),
        };
        match cauldron.submit(env).unwrap() {
            spine::Outcome::Committed { returned, .. } => json!(returned.0),
            other => panic!("{other:?}"),
        }
    };
    let got = submit(
        "MATCH (r: ws/region { id: $r }) CREATE (i: ws/issue { state: \"open\", handle: $h }) CREATE (i) -[in_region]-> (r) RETURN i.id",
        vec![("r", json!("R-1")), ("h", json!("a second issue, for the cycle"))],
        "damage-issue",
    );
    let is2 = got[0]["i.id"].as_str().unwrap().to_string();
    submit("MATCH (d: ws/decision { id: $d }) CREATE (d) -[supersedes]-> (d) NOTE $n", vec![("d", json!("DEC-1")), ("n", json!("itself"))], "damage-self");
    submit(
        "MATCH (a: ws/issue { id: $a }) MATCH (b: ws/issue { id: $b }) CREATE (a) -[supersedes]-> (b) NOTE $n CREATE (b) -[supersedes]-> (a) NOTE $n",
        vec![("a", json!("IS-1")), ("b", json!(is2.clone())), ("n", json!("round"))],
        "damage-cycle",
    );
    is2
}

/// **`supersedes` is written only with its new record, and never in a
/// loop.** `soup_link` refuses every `supersedes` (`supersedes-at-creation`)
/// and a `narrows` self-edge or cycle (`edge-loop`); `soup_put` refuses a
/// record superseding itself and a batch whose `supersedes` (or `narrows`)
/// edges make a cycle, and lands a new record superseding an old one of its
/// type. The way out of damage already made: `soup_unlink` removes a
/// self-edge and an edge on a cycle, while an ordinary `supersedes` is still
/// history, and its refusal names the exception.
#[test]
fn sp1_supersedes_only_with_its_new_record_and_never_in_a_loop() {
    let s = Scratch::new("sp1", &["kb"]);
    s.import("kb", &example());
    let is2 = damage(&s);
    s.grant("kb", &rw(), &[&s.project("p")]);
    let svc = Service::start(&s);
    let c = ctx(&s.project("p"));
    let link_refusal = |args: Value| -> Value {
        let (code, _, detail) = refused(&svc.call("soup_link", args, &c));
        assert_eq!(code, "links_refused", "{detail}");
        detail["links"][0]["refused"].clone()
    };
    let since = cursor(&svc, &c);
    // soup_link: no supersedes at all, self or not.
    let r = link_refusal(json!({"from": "IS-1", "kind": "supersedes", "to": "IS-1"}));
    assert_eq!(r["reason"], "supersedes-at-creation", "{r}");
    assert!(r["detail"].as_str().unwrap().contains("soup_link never adds one"), "{r}");
    let put = ok(&svc.call("soup_put", json!({"remint": true, "records": [{"id": "d2", "type": "decision", "state": "decided",
        "handle": "Validate every record, twice", "edges": [{"kind": "in_region", "to": "R-1"}]}]}), &c));
    let d2 = put["id_map"]["d2"].as_str().unwrap().to_string();
    assert_eq!(link_refusal(json!({"from": d2, "kind": "supersedes", "to": "DEC-1"}))["reason"], "supersedes-at-creation");
    assert!(!has(&svc, &c, &d2, "supersedes", "DEC-1"));
    // narrows: a self-edge, and an edge that would close a cycle.
    let regions = ok(&svc.call("soup_regions", json!({}), &c));
    let narrows: Vec<(String, String)> = regions["regions"].as_array().unwrap().iter().flat_map(|r| {
        let id = r["id"].as_str().unwrap().to_string();
        r["narrows"].as_array().into_iter().flatten().map(move |p| (id.clone(), p.as_str().unwrap_or_default().to_string())).collect::<Vec<_>>()
    }).collect();
    assert_eq!(link_refusal(json!({"from": "R-1", "kind": "narrows", "to": "R-1"}))["reason"], "edge-loop");
    if let Some((child, parent)) = narrows.first() {
        let r = link_refusal(json!({"from": parent, "kind": "narrows", "to": child}));
        assert_eq!(r["reason"], "edge-loop", "{r}");
        assert!(r["detail"].as_str().unwrap().contains("would close a cycle"), "{r}");
    }
    let r = link_refusal(json!({"links": [{"from": "R-1", "kind": "narrows", "to": "R-2"}, {"from": "R-2", "kind": "narrows", "to": "R-1"}]}));
    assert_eq!(r["reason"], "edge-loop", "two new edges of one call close a cycle: {r}");
    // soup_put: itself, and a cycle in the batch (supersedes and narrows).
    let (code, msg, detail) = refused(&svc.call("soup_put", json!({"remint": true, "records": [{"id": "d3", "type": "decision", "state": "decided",
        "handle": "Superseding itself", "edges": [{"kind": "in_region", "to": "R-1"}, {"kind": "supersedes", "to": "d3", "note": "x"}]}]}), &c));
    assert_eq!((code.as_str(), detail["reason"].as_str()), ("usage", Some("edge-loop")), "{msg}");
    assert!(msg.contains("never supersedes itself"), "{msg}");
    let (code, msg, detail) = refused(&svc.call("soup_put", json!({"remint": true, "records": [
        {"id": "a", "type": "idea", "state": "idea", "handle": "first of a loop", "edges": [{"kind": "in_region", "to": "R-1"}, {"kind": "supersedes", "to": "b", "note": "x"}]},
        {"id": "b", "type": "idea", "state": "idea", "handle": "second of a loop", "edges": [{"kind": "in_region", "to": "R-1"}, {"kind": "supersedes", "to": "a", "note": "y"}]}]}), &c));
    assert_eq!((code.as_str(), detail["reason"].as_str()), ("usage", Some("edge-loop")), "{msg}");
    assert!(msg.contains("make a cycle"), "{msg}");
    let (code, msg, _) = refused(&svc.call("soup_put", json!({"remint": true, "records": [
        {"id": "ra", "type": "region", "handle": "a region", "boundary": "Inside: a. Outside: b.", "edges": [{"kind": "narrows", "to": "rb"}]},
        {"id": "rb", "type": "region", "handle": "b region", "boundary": "Inside: b. Outside: a.", "edges": [{"kind": "narrows", "to": "ra"}]}]}), &c));
    assert!(code == "usage" && msg.contains("`narrows` edges make a cycle"), "{code}: {msg}");
    let ch = ok(&svc.call("soup_changes", json!({"since": since}), &c));
    assert_eq!(ch["changes"].as_array().unwrap().len(), 1, "only d2 landed: {ch}");
    // A new record superseding an old one of its type lands, as the grammar intends.
    let put = ok(&svc.call("soup_put", json!({"remint": true, "records": [{"id": "d4", "type": "decision", "state": "decided",
        "handle": "Validate every record, always", "edges": [{"kind": "in_region", "to": "R-1"}, {"kind": "supersedes", "to": d2, "note": "always, not twice"}]}]}), &c));
    let d4 = put["id_map"]["d4"].as_str().unwrap().to_string();
    assert!(has(&svc, &c, &d4, "supersedes", &d2));
    // The way out: a self-edge and an edge on a cycle may be removed ...
    let got = ok(&svc.call("soup_get", json!({"ids": ["DEC-1", "IS-1"]}), &c));
    assert_eq!(got["records"][0]["superseded_by"], json!(["DEC-1"]), "the damage is there: {got}");
    let d = ok(&svc.call("soup_unlink", json!({"from": "DEC-1", "kind": "supersedes", "to": "DEC-1"}), &c));
    assert_eq!(d["outcome"], "unlinked", "{d}");
    let d = ok(&svc.call("soup_unlink", json!({"from": is2, "kind": "supersedes", "to": "IS-1"}), &c));
    assert_eq!(d["outcome"], "unlinked", "{d}");
    let got = ok(&svc.call("soup_get", json!({"ids": ["DEC-1", "IS-1"]}), &c));
    assert!(got["records"][0].get("superseded_by").map_or(true, |v| v.as_array().is_some_and(|a| a.is_empty())), "{got}");
    assert!(got["records"][1].get("superseded_by").map_or(true, |v| v.as_array().is_some_and(|a| a.is_empty())), "IS-1 reads as current again: {got}");
    // ... and the edge left of the broken cycle is history again.
    let (_, _, detail) = refused(&svc.call("soup_unlink", json!({"from": "IS-1", "kind": "supersedes", "to": is2}), &c));
    let r = &detail["links"][0]["refused"];
    assert_eq!(r["reason"], "supersedes-is-history", "{detail}");
    assert!(r["detail"].as_str().unwrap().contains("from a record to itself, or one on a cycle"), "{r}");
    let (_, _, detail) = refused(&svc.call("soup_unlink", json!({"from": d4, "kind": "supersedes", "to": d2}), &c));
    assert_eq!(detail["links"][0]["refused"]["reason"], "supersedes-is-history", "{detail}");
    svc.stop();
}
