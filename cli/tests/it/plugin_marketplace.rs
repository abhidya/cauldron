//! The Claude Code plugin is installable from the repository itself:
//! `/plugin marketplace add <owner>/<repo>` (or a checkout's root) reads
//! `.claude-plugin/marketplace.json` at the ROOT, and each plugin's relative
//! `source` is resolved against that root. This checks the shipped file does
//! resolve: every relative source is a directory holding
//! `.claude-plugin/plugin.json` whose `name` is the entry's name. There is one
//! marketplace, at the root; a second one under `claude-code/` would be a copy
//! that drifts.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// Every problem with the marketplace at `<root>/.claude-plugin/marketplace.json`;
/// empty means each plugin's source resolves to a plugin of that name.
fn marketplace_problems(root: &Path) -> Vec<String> {
    let file = root.join(".claude-plugin/marketplace.json");
    let text = match std::fs::read_to_string(&file) {
        Ok(t) => t,
        Err(e) => return vec![format!("{}: {e}", file.display())],
    };
    let v: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => return vec![format!("{}: not JSON: {e}", file.display())],
    };
    let Some(plugins) = v.get("plugins").and_then(|p| p.as_array()).filter(|p| !p.is_empty()) else {
        return vec![format!("{}: no `plugins` list", file.display())];
    };
    let mut problems = Vec::new();
    for p in plugins {
        let name = p.get("name").and_then(|n| n.as_str()).unwrap_or("");
        let Some(src) = p.get("source").and_then(|s| s.as_str()) else {
            problems.push(format!("plugin `{name}`: `source` is not a relative path"));
            continue;
        };
        let Some(rel) = src.strip_prefix("./") else {
            problems.push(format!("plugin `{name}`: source `{src}` does not start with ./"));
            continue;
        };
        if rel.split('/').any(|c| c == "..") {
            problems.push(format!("plugin `{name}`: source `{src}` leaves the repository"));
            continue;
        }
        let dir = root.join(rel);
        let manifest = dir.join(".claude-plugin/plugin.json");
        let Ok(m) = std::fs::read_to_string(&manifest) else {
            problems.push(format!("plugin `{name}`: source `{src}` has no .claude-plugin/plugin.json"));
            continue;
        };
        let got = serde_json::from_str::<serde_json::Value>(&m)
            .ok()
            .and_then(|m| m.get("name").and_then(|n| n.as_str()).map(str::to_string));
        if got.as_deref() != Some(name) {
            problems.push(format!("plugin `{name}`: {} names {got:?}", manifest.display()));
        }
    }
    problems
}

#[test]
fn the_root_marketplace_resolves_to_the_plugin() {
    let root = repo_root();
    let problems = marketplace_problems(&root);
    assert!(problems.is_empty(), "the root marketplace does not resolve:\n  {}", problems.join("\n  "));
    // The plugin that ships its MCP server is the one it names.
    assert!(root.join("claude-code/cauldron/.mcp.json").is_file(), "the plugin has no .mcp.json");
    // One marketplace, at the root.
    assert!(
        !root.join("claude-code/.claude-plugin/marketplace.json").exists(),
        "a second marketplace under claude-code/ duplicates the root one"
    );
}

/// Opposition: the check fails a source that does not resolve, one whose
/// manifest names another plugin, and a missing marketplace, so a check that
/// passes everything fails here.
#[test]
fn a_marketplace_whose_source_does_not_resolve_is_named() {
    let root = std::env::temp_dir().join(format!("cauldron-marketplace-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join(".claude-plugin")).unwrap();
    std::fs::create_dir_all(root.join("plug/.claude-plugin")).unwrap();
    std::fs::write(root.join("plug/.claude-plugin/plugin.json"), r#"{"name":"plug"}"#).unwrap();
    assert_eq!(marketplace_problems(&root).len(), 1, "a missing marketplace.json passed");

    let write = |src: &str, name: &str| {
        std::fs::write(
            root.join(".claude-plugin/marketplace.json"),
            format!(r#"{{"name":"m","owner":{{"name":"o"}},"plugins":[{{"name":"{name}","source":"{src}"}}]}}"#),
        )
        .unwrap();
    };
    write("./plug", "plug");
    assert!(marketplace_problems(&root).is_empty(), "the good fixture failed: {:?}", marketplace_problems(&root));
    write("./nowhere", "plug");
    assert!(marketplace_problems(&root)[0].contains("no .claude-plugin/plugin.json"));
    write("./plug", "other");
    assert!(marketplace_problems(&root)[0].contains("names Some(\"plug\")"));
    write("plug", "plug");
    assert!(marketplace_problems(&root)[0].contains("does not start with ./"));
    let _ = std::fs::remove_dir_all(&root);
}
