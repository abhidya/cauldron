//! **The MCP door's read tools answer the same items as `cauldron soup`.**
//!
//! One fixture (the published README's worked example, imported with
//! `cauldron soup import`), each read verb run by the built `cauldron`, and
//! the matching tool of the MCP door called in process on the same soup
//! (the door is `services/mcp`; its HTTP side has its own suite there). The
//! door bounds a whole reply by `max_chars`; every call here asks for the
//! ceiling, so nothing is left out and the lists compare whole.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{json, Value};
use soup_door::grants::Scope;

const CAULDRON: &str = env!("CARGO_BIN_EXE_cauldron");
const README: &str = include_str!("../../../schema/soup/workspace-v6/README.md");

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run(s: &Scratch, args: &[&str]) -> Value {
    let o = Command::new(CAULDRON)
        .args(args)
        .env("CAULDRON_HOME", s.0.join("root"))
        .env("HOME", s.0.join("home"))
        .env_remove("CAULDRON_ENV")
        .env_remove("ENV")
        .env_remove("CAULDRON_CAULDRON")
        .stdin(Stdio::null())
        .output()
        .expect("spawn cauldron");
    assert_eq!(o.status.code(), Some(0), "{args:?}: {}", String::from_utf8_lossy(&o.stderr));
    serde_json::from_slice(&o.stdout).unwrap_or(Value::Null)
}

fn example() -> Vec<Value> {
    let at = README.find("## A worked example").expect("README has its worked example");
    let start = README[at..].find("```json").unwrap() + at + "```json".len();
    let end = README[start..].find("```").unwrap() + start;
    serde_json::from_str(&README[start..end]).expect("the worked example is JSON")
}

fn verb(s: &Scratch, args: &[&str]) -> Value {
    let mut a = vec!["soup"];
    a.extend_from_slice(args);
    a.extend_from_slice(&["--env", "dev", "--cauldron", "kb"]);
    run(s, &a)
}

#[test]
fn m1_each_read_tool_answers_the_cli_verbs_items() {
    let p = std::env::temp_dir().join(format!(
        "cauldron-mcp-reads-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    for d in ["root/data/dev", "home", "work"] {
        std::fs::create_dir_all(p.join(d)).unwrap();
    }
    let s = Scratch(p.canonicalize().unwrap());
    if let Some(h) = std::env::var_os("HOME") {
        assert!(!s.0.starts_with(Path::new(&h).join(".cauldron")));
    }
    run(&s, &["create", "kb", "--env", "dev"]);
    let input = s.0.join("example.json");
    std::fs::write(&input, serde_json::to_string(&example()).unwrap()).unwrap();
    run(&s, &["soup", "import", input.to_str().unwrap(), "--env", "dev", "--cauldron", "kb"]);

    // The door, in process, on the same environment.
    let data = s.0.join("root").join("data").join("dev");
    let e = env::Environment {
        root: s.0.join("root"),
        root_source: env::paths::Source::CauldronHome,
        name: "dev".into(),
        name_source: env::paths::Source::CauldronEnv,
        data_root: data.clone(),
    };
    soup_door::grants::grant(&e, "kb", &[Scope::Read], &[], "tty").unwrap();
    let dir = cauldron_mcp::dir(&e);
    let door = cauldron_mcp::door::Door::new(e, cauldron_mcp::secret::mint(&dir).unwrap(), "test".into(), &dir);
    let call = |tool: &str, mut args: Value| -> Value {
        args["cauldron"] = json!("kb");
        args["max_chars"] = json!(50_000);
        args["_ctx"] = json!({"client": "test", "cwd": s.0.join("work").display().to_string()});
        let t = cauldron_mcp::catalogue::find(tool).expect("a listed tool");
        door.call(t, args).unwrap_or_else(|r| panic!("{tool} refused: {} {}", r.code, r.message))
    };

    let (cli, mcp) = (verb(&s, &["regions"]), call("soup_regions", json!({})));
    assert_eq!(mcp["regions"], cli["regions"]);
    assert_eq!(mcp["regions"].as_array().unwrap().len(), 2);
    for (args, tool_args) in [
        (vec!["nodes"], json!({})),
        (vec!["nodes", "--type", "issue"], json!({"type": "issue"})),
        (vec!["nodes", "--region", "R-2", "--limit", "2"], json!({"region": "R-2", "limit": 2})),
        (vec!["nodes", "--state", "open"], json!({"state": "open"})),
    ] {
        let (cli, mcp) = (verb(&s, &args), call("soup_nodes", tool_args));
        assert_eq!(mcp["nodes"], cli["nodes"], "{args:?}");
        assert_eq!((mcp["total"].clone(), mcp["more"].clone()), (cli["total"].clone(), cli["more"].clone()), "{args:?}");
    }
    for (args, tool_args) in [
        (vec!["search", "schema", "record"], json!({"query": "schema record"})),
        (vec!["search", "import", "--type", "task"], json!({"query": "import", "type": "task"})),
        (vec!["search", "schema", "--region", "R-1", "--limit", "1"], json!({"query": "schema", "region": "R-1", "limit": 1})),
    ] {
        let (cli, mcp) = (verb(&s, &args), call("soup_search", tool_args));
        assert_eq!(mcp["results"], cli["results"], "{args:?}");
        assert!(!cli["results"].as_array().unwrap().is_empty(), "{args:?}");
    }
    for (args, tool_args) in [(vec!["region", "R-1"], json!({"id": "R-1"})), (vec!["region", "R-2", "--depth", "2", "--limit", "1"], json!({"id": "R-2", "depth": 2, "limit": 1}))] {
        let (cli, mcp) = (verb(&s, &args), call("soup_region", tool_args));
        for k in ["region", "parents", "children", "relates", "members", "members_total"] {
            assert_eq!(mcp[k], cli[k], "{args:?} {k}");
        }
    }
    let (cli, mcp) = (verb(&s, &["get", "T-1", "IS-1", "DEC-1"]), call("soup_get", json!({"ids": ["T-1", "IS-1", "DEC-1"]})));
    assert_eq!(mcp["records"], cli["records"]);
    assert_eq!(mcp["records"].as_array().unwrap().len(), 3);
}
