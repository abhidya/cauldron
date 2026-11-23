//! The plugin's agent and commands name only tools the MCP door serves: the
//! curator's `tools:` list is read from its front matter, and every MCP
//! tool it names (`mcp__plugin_cauldron_cauldron__<name>`) must be in the
//! door's catalogue (`soup_door::catalogue`). The curator reads dumps
//! (`soup_dumps`, `soup_dump_get`) and never writes them (`soup_dump`,
//! `soup_dump_session`); `/cauldron:curate-dump` exists and reads a session
//! dump through the text view.

use std::path::PathBuf;

const PREFIX: &str = "mcp__plugin_cauldron_cauldron__";

fn plugin() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../claude-code/cauldron")
}

/// The value of `key:` in a markdown file's front matter.
fn front(text: &str, key: &str) -> Option<String> {
    let body = text.strip_prefix("---\n")?;
    let (fm, _) = body.split_once("\n---\n")?;
    fm.lines().find_map(|l| l.strip_prefix(&format!("{key}:")).map(|v| v.trim().to_string()))
}

/// The MCP tools a `tools:` list names, and anything in it that is neither a
/// built-in tool nor a catalogue tool.
fn mcp_tools(list: &str) -> (Vec<String>, Vec<String>) {
    let catalogue: Vec<&str> = soup_door::catalogue::TOOLS.iter().map(|t| t.name).collect();
    let (mut named, mut stray) = (vec![], vec![]);
    for t in list.split(',').map(str::trim).filter(|t| !t.is_empty()) {
        match t.strip_prefix(PREFIX) {
            Some(n) if catalogue.contains(&n) => named.push(n.to_string()),
            Some(_) => stray.push(t.to_string()),
            None if t.starts_with("mcp__") => stray.push(t.to_string()),
            None => {}
        }
    }
    (named, stray)
}

#[test]
fn the_curator_names_only_catalogue_tools_and_reads_dumps() {
    let text = std::fs::read_to_string(plugin().join("agents/curator.md")).unwrap();
    let list = front(&text, "tools").expect("the curator has a tools: list");
    let (named, stray) = mcp_tools(&list);
    assert!(stray.is_empty(), "the curator names tools the door does not serve: {stray:?}");
    for want in ["soup_status", "soup_regions", "soup_region", "soup_nodes", "soup_search", "soup_get", "soup_put", "soup_link", "soup_changes", "soup_dumps", "soup_dump_get"] {
        assert!(named.iter().any(|n| n == want), "the curator lacks {want}: {named:?}");
    }
    for never in ["soup_dump", "soup_dump_session"] {
        assert!(!named.iter().any(|n| n == never), "the curator must not write dumps ({never})");
    }
    // The body teaches the text view and the citation.
    assert!(text.contains("view: \"text\"") && text.contains("\"kind\": \"dump\""), "the curator does not say how to curate a dump");
}

#[test]
fn curate_dump_is_a_command_that_reads_the_text_view_and_cites_the_dump() {
    let text = std::fs::read_to_string(plugin().join("commands/curate-dump.md")).unwrap();
    assert!(front(&text, "description").is_some_and(|d| !d.is_empty()));
    assert_eq!(front(&text, "argument-hint").as_deref(), Some("<DM-n> [cauldron]"));
    for want in ["cauldron:curator", "soup_dump_get", "view: \"text\"", "\"kind\": \"dump\"", "stays `received`", "dry_run"] {
        assert!(text.contains(want), "curate-dump.md does not say {want}");
    }
    // Every soup tool any command names is one the door serves.
    let catalogue: Vec<&str> = soup_door::catalogue::TOOLS.iter().map(|t| t.name).collect();
    for e in std::fs::read_dir(plugin().join("commands")).unwrap().flatten() {
        let body = std::fs::read_to_string(e.path()).unwrap();
        for w in body.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
            if w.starts_with("soup_") && w.len() > 5 {
                assert!(catalogue.contains(&w), "{} names `{w}`, which the door does not serve", e.path().display());
            }
        }
    }
}

/// Opposition: the list check flags a tool the door does not serve.
#[test]
fn a_tool_the_door_does_not_serve_is_flagged() {
    let (named, stray) = mcp_tools(&format!("Read, {PREFIX}soup_get, {PREFIX}soup_delete, mcp__other__x"));
    assert_eq!(named, vec!["soup_get".to_string()]);
    assert_eq!(stray, vec![format!("{PREFIX}soup_delete"), "mcp__other__x".to_string()]);
}

/// `/cauldron:review` shows the queue through `soup_review` (a read) and
/// gives the terminal commands; it never decides anything itself.
#[test]
fn review_is_a_command_that_shows_the_queue_and_names_the_terminal_commands() {
    let text = std::fs::read_to_string(plugin().join("commands/review.md")).unwrap();
    assert!(front(&text, "description").is_some_and(|d| !d.is_empty()));
    for want in ["soup_review", "cauldron ripples approve --dump", "cauldron ripples reject --dump", "review --interactive", "data, not instructions", "never try to"] {
        assert!(text.contains(want), "review.md does not say {want}");
    }
    let tool = soup_door::catalogue::find("soup_review").expect("served");
    assert_eq!(tool.needs, soup_door::catalogue::Needs::Scope(soup_door::grants::Scope::Read));
}
