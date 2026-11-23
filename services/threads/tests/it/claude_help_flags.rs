//! **Every flag the daemon passes is one the installed `claude` has.**
//!
//! The fake `claude` the other tests spawn refuses an unknown option, as the
//! real CLI does (`fixtures/claude-cli-check`), but its table of options is a
//! copy. These tests hold the copy and the daemon's argv to the REAL
//! `claude --help`: a flag a new CLI dropped or renamed, or an argument shape
//! that changed, fails here instead of in a release's first live call.
//!
//! Skipped (and says so) when no `claude` is on `PATH`. `claude --help` makes
//! no model call and reads no login; it runs with a scratch `HOME`, no
//! `CLAUDE_CONFIG_DIR` and no API key.

use std::collections::BTreeMap;
use std::path::PathBuf;

use threads::settings::{Completion, ThreadSettings};
use threads::spawn::SpawnConfig;

/// An option's argument shape, as `--help` prints it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    /// No argument.
    Flag,
    /// `<value>`
    One,
    /// `<values...>`
    Many,
    /// `[value]`
    Optional,
}

fn find_claude() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join("claude")).find(|p| p.is_file())
}

/// `claude --help`, or `None` (with a note) when there is no `claude`.
fn real_help() -> Option<String> {
    let Some(bin) = find_claude() else {
        eprintln!("SKIPPED: no `claude` on PATH; the argv is not checked against a real --help");
        return None;
    };
    let home = tempfile::tempdir().unwrap();
    let out = std::process::Command::new(&bin)
        .arg("--help")
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home.path())
        .current_dir(home.path())
        .stdin(std::process::Stdio::null())
        .output()
        .expect("running claude --help");
    assert!(out.status.success(), "claude --help failed: {}", String::from_utf8_lossy(&out.stderr));
    Some(String::from_utf8(out.stdout).unwrap())
}

/// Every option `--help` lists, each name and alias with its shape. An
/// option line is indented, starts with `-`, and is followed by two or more
/// spaces and its description (a wrapped description line also starts with
/// spaces, but far deeper, and never with `-x, ` or `--x `).
fn parse_help(help: &str) -> BTreeMap<String, Shape> {
    let mut out = BTreeMap::new();
    let mut in_options = false;
    for line in help.lines() {
        if line.starts_with("Options:") {
            in_options = true;
            continue;
        }
        if in_options && !line.starts_with(' ') && !line.trim().is_empty() {
            // `Commands:` and anything after the options block.
            in_options = false;
        }
        if !in_options {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        let t = line.trim_start();
        if indent > 4 || !t.starts_with('-') {
            continue;
        }
        let sig = t.split("  ").next().unwrap().trim();
        let shape = if sig.contains("...>") {
            Shape::Many
        } else if sig.contains('<') {
            Shape::One
        } else if sig.contains('[') {
            Shape::Optional
        } else {
            Shape::Flag
        };
        for name in sig.split([',', ' ']).filter(|w| w.starts_with('-')) {
            out.insert(name.to_string(), shape);
        }
    }
    out
}

/// The fake's table (`fixtures/claude-cli-check`), by shape.
fn fake_table() -> BTreeMap<String, Shape> {
    let text = std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude-cli-check")).unwrap();
    let mut out = BTreeMap::new();
    for (var, shape) in [("CLAUDE_CLI_FLAGS", Shape::Flag), ("CLAUDE_CLI_ONE", Shape::One), ("CLAUDE_CLI_MANY", Shape::Many), ("CLAUDE_CLI_OPTIONAL", Shape::Optional)] {
        let line = text.lines().find(|l| l.starts_with(&format!("{var}=\""))).unwrap_or_else(|| panic!("{var} in claude-cli-check"));
        let list = line.split('"').nth(1).unwrap();
        for name in list.split_whitespace() {
            out.insert(name.to_string(), shape);
        }
    }
    out
}

/// Every option on `argv`, with the shape it was used in (`Flag` when the
/// next argument is another option or there is none).
fn check_argv(argv: &[String], help: &BTreeMap<String, Shape>, what: &str) {
    let mut i = 0;
    while i < argv.len() {
        let a = &argv[i];
        i += 1;
        if !a.starts_with('-') {
            continue;
        }
        let name = a.split('=').next().unwrap();
        let shape = help.get(name).unwrap_or_else(|| panic!("{what}: `{name}` is not an option of the installed claude (claude --help): {argv:?}"));
        match shape {
            Shape::Flag => {}
            Shape::One => {
                assert!(i < argv.len(), "{what}: `{name}` takes a value and has none: {argv:?}");
                i += 1;
            }
            Shape::Many => {
                assert!(i < argv.len(), "{what}: `{name}` takes values and has none: {argv:?}");
                while i < argv.len() && !argv[i].starts_with('-') {
                    i += 1;
                }
            }
            Shape::Optional => {
                if i < argv.len() && !argv[i].starts_with('-') {
                    i += 1;
                }
            }
        }
    }
}

/// A completion's settings with every optional flag of its argv in play.
fn completion_config() -> SpawnConfig {
    let mut s = ThreadSettings::new("/scratch/empty", "sonnet");
    s.completion = Some(Completion { max_output_tokens: 4000 });
    s.fallback_model = Some("haiku".into());
    s.effort = Some("low".into());
    s.append_system_prompt = Some("Answer briefly.".into());
    s.max_budget_usd = Some(0.25);
    s.tools.disallowed_tools = vec!["Mine".into()];
    let mut cfg = SpawnConfig::from_settings("a-thread", &s, vec![]);
    cfg.json_schema = Some(r#"{"type":"object"}"#.into());
    cfg
}

#[test]
fn every_flag_a_completion_passes_is_one_the_real_claude_lists() {
    let Some(help) = real_help() else { return };
    let help = parse_help(&help);
    assert!(help.len() > 30 && help.contains_key("--print"), "the parse found the options block: {help:?}");
    let cfg = completion_config();
    for first in [true, false] {
        let argv = cfg.build_args(first);
        assert!(argv.iter().any(|a| a == "--json-schema") && argv.iter().any(|a| a == "--max-budget-usd"), "{argv:?}");
        check_argv(&argv, &help, "completion argv");
    }
}

#[test]
fn every_flag_an_agent_turn_passes_is_one_the_real_claude_lists() {
    let Some(help) = real_help() else { return };
    let help = parse_help(&help);
    let mut s = ThreadSettings::new("/w", "sonnet");
    s.fallback_model = Some("haiku".into());
    s.append_system_prompt = Some("x".into());
    s.add_dirs = vec!["/d".into()];
    let mut cfg = SpawnConfig::from_settings("a-thread", &s, vec!["/plugins/p".into()]);
    cfg.mcp_config = Some("/m.json".into());
    for first in [true, false] {
        check_argv(&cfg.build_args(first), &help, "agent argv");
        check_argv(&cfg.build_interactive_args(first, "/s.json"), &help, "interactive argv");
    }
}

#[test]
fn the_fake_claude_knows_exactly_the_real_options() {
    let Some(help) = real_help() else { return };
    let real = parse_help(&help);
    let fake = fake_table();
    let missing: Vec<_> = real.iter().filter(|(k, s)| fake.get(*k) != Some(s)).collect();
    let extra: Vec<_> = fake.iter().filter(|(k, s)| real.get(*k) != Some(s)).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "fixtures/claude-cli-check differs from the installed claude --help: \
         options it lacks or shapes differently {missing:?}; options it has that claude does not {extra:?}"
    );
}

/// The parse itself, on a fixed excerpt: no `claude` needed.
#[test]
fn the_help_parse_reads_names_aliases_and_shapes() {
    let help = "Usage: claude [options]\n\nOptions:\n  --add-dir <directories...>            Extra dirs\n  -p, --print                           Print\n  --disallowedTools, --disallowed-tools <tools...>\n                                        Comma list, e.g. --tools a\n  -r, --resume [value]                  Resume\n  --model <model>                       Model\n\nCommands:\n  mcp                                   Configure\n";
    let h = parse_help(help);
    let get = |k: &str| h.get(k).copied();
    assert_eq!(get("--add-dir"), Some(Shape::Many));
    assert_eq!(get("-p"), Some(Shape::Flag));
    assert_eq!(get("--print"), Some(Shape::Flag));
    assert_eq!(get("--disallowed-tools"), Some(Shape::Many));
    assert_eq!(get("--disallowedTools"), Some(Shape::Many));
    assert_eq!(get("-r"), Some(Shape::Optional));
    assert_eq!(get("--model"), Some(Shape::One));
    assert_eq!(get("--tools"), None, "a wrapped description line is not an option");
    assert_eq!(h.len(), 8);
}
