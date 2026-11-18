//! **A README never states a release that is not this one.** Any "This is
//! **<x.y.z>**" (or "this is <x.y.z>") in the top README or the plugin's
//! README must name the workspace version, so a statement left behind by an
//! earlier release fails here instead of going stale.

use std::path::Path;

/// Every `x.y.z` that follows "this is" (any case, optional `**`) in `text`.
fn stated(text: &str) -> Vec<String> {
    let lower = text.to_lowercase();
    let mut out = vec![];
    let mut at = 0;
    while let Some(i) = lower[at..].find("this is ") {
        let rest = lower[at + i + 8..].trim_start_matches('*').trim_start_matches('v');
        let v: String = rest.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
        if v.matches('.').count() == 2 && v.split('.').all(|p| !p.is_empty()) {
            out.push(v);
        }
        at += i + 8;
    }
    out
}

#[test]
fn every_readme_version_statement_names_the_workspace_version() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let version = env!("CARGO_PKG_VERSION");
    for f in ["README.md", "claude-code/cauldron/README.md"] {
        let text = std::fs::read_to_string(root.join(f)).unwrap();
        for v in stated(&text) {
            assert_eq!(v, version, "{f} says \"this is {v}\", but the workspace version is {version}");
        }
    }
}

/// Opposition: the scan finds a stale statement.
#[test]
fn a_stale_statement_is_found() {
    assert_eq!(stated("Intro. This is **0.1.8**. More; this is v2.3.4 and this is a test."), vec!["0.1.8", "2.3.4"]);
}
