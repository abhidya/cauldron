//! `cauldron-mcp --build-stamp`: the commit, how it was obtained, whether the
//! scoped tree was dirty, and the scope, read off the binary cargo just
//! built, with no environment at all (what an `artifact` declaration of the
//! service reads back).

use std::process::Command;

fn git(args: &[&str]) -> String {
    let o = Command::new("git").arg("-C").arg(env!("CARGO_MANIFEST_DIR")).args(args).output().unwrap();
    String::from_utf8(o.stdout).unwrap()
}

#[test]
fn the_stamp_names_this_trees_commit_and_scope_with_no_environment() {
    let out = Command::new(crate::support::BIN).arg("--build-stamp").env_clear().output().unwrap();
    assert!(out.status.success(), "the stamp needs no environment: {}", String::from_utf8_lossy(&out.stderr));
    let s: std::collections::BTreeMap<String, String> = String::from_utf8(out.stdout)
        .unwrap()
        .trim()
        .split(' ')
        .map(|kv| kv.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())).expect("k=v"))
        .collect();
    assert_eq!(s.keys().map(String::as_str).collect::<Vec<_>>(), ["commit", "dirt_scope", "dirty", "provenance"]);
    assert_eq!(s["provenance"], "git", "nobody pinned this build");
    assert_eq!(s["commit"], git(&["rev-parse", "HEAD"]).trim());
    let scope: Vec<&str> = s["dirt_scope"].split(',').collect();
    for p in ["Cargo.lock", "Cargo.toml", "libs/soup-door", "libs/spine", "services/mcp"] {
        assert!(scope.contains(&p), "{p} is in the scope: {scope:?}");
    }
    assert!(["0", "1"].contains(&s["dirty"].as_str()), "git can read this tree: {}", s["dirty"]);
}
