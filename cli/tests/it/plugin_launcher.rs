//! The plugin's MCP launcher, `claude-code/cauldron/bin/cauldron-mcp`, run
//! with a STRIPPED environment (`env_clear`, then only what each case sets),
//! the way Claude Code started from the Dock runs it: no login-shell PATH.
//!
//! It must find `cauldron` in the fixed order ($CAULDRON_BIN, the install
//! marker, the default install location, PATH), run `cauldron mcp stdio`
//! with its own arguments, refuse a path that is not a regular file owned by
//! the user or root (a symlink, a group-writable file, a file in a directory
//! others can write, a file in the project directory from any source, a PATH
//! entry that is empty, relative or inside the project), and, with
//! nothing found, answer the MCP handshake itself with a tool-less server
//! whose instructions say how to install, printing nothing but JSON-RPC on
//! stdout.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

const MARKER_REL: &str = "Library/Application Support/cauldron/cauldron-bin";

fn launcher() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../claude-code/cauldron/bin/cauldron-mcp")
}

/// A scratch HOME, removed on drop.
struct Scratch(PathBuf);
impl Scratch {
    fn new(tag: &str) -> Scratch {
        let p = std::env::temp_dir().join(format!("cauldron-launcher-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        Scratch(p.canonicalize().unwrap())
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A fake `cauldron` at `path` that prints `<tag> <args>` and exits 0.
fn fake(path: &Path, tag: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, format!("#!/bin/sh\nprintf '%s' '{tag}'; printf ' %s' \"$@\"; printf '\\n'\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Run the launcher with ONLY `vars` in its environment, `input` on stdin.
fn run(vars: &[(&str, &str)], args: &[&str], input: &str) -> (i32, String, String) {
    run_in(None, vars, args, input)
}

/// [`run`] with the launcher's current directory at `cwd` (Claude Code starts
/// an MCP server in the project).
fn run_in(cwd: Option<&Path>, vars: &[(&str, &str)], args: &[&str], input: &str) -> (i32, String, String) {
    let mut c = Command::new(launcher());
    c.env_clear().args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    if let Some(d) = cwd {
        c.current_dir(d);
    }
    for (k, v) in vars {
        c.env(k, v);
    }
    let mut child = c.spawn().expect("the launcher runs");
    child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn launcher_is_an_executable_file_and_the_plugin_runs_it() {
    let meta = std::fs::metadata(launcher()).expect("the launcher ships in the plugin");
    assert!(meta.permissions().mode() & 0o111 != 0, "the launcher is not executable");
    let mcp: Value = serde_json::from_str(
        &std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../claude-code/cauldron/.mcp.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(mcp["mcpServers"]["cauldron"]["command"], "${CLAUDE_PLUGIN_ROOT}/bin/cauldron-mcp");
}

#[test]
fn the_default_install_location_is_found_with_no_path() {
    let s = Scratch::new("default");
    let home = s.0.join("home");
    fake(&home.join(".cauldron/bin/cauldron"), "default");
    let (rc, out, _) = run(&[("HOME", home.to_str().unwrap())], &["--env", "prod"], "");
    assert_eq!(rc, 0);
    assert_eq!(out, "default mcp stdio --env prod\n", "runs `cauldron mcp stdio` with the launcher's own arguments");
}

#[test]
fn the_order_is_cauldron_bin_then_the_marker_then_the_default_then_path() {
    let s = Scratch::new("order");
    let home = s.0.join("home");
    let h = home.to_str().unwrap();
    fake(&home.join(".cauldron/bin/cauldron"), "default");
    fake(&s.0.join("pathdir/cauldron"), "path");
    fake(&s.0.join("marked/cauldron"), "marker");
    fake(&s.0.join("explicit/cauldron"), "explicit");
    let pathdir = s.0.join("pathdir");
    let path = format!("{}:/usr/bin:/bin", pathdir.display());
    let explicit = s.0.join("explicit/cauldron");
    let explicit = explicit.to_str().unwrap();

    // PATH alone (nothing else exists for it): found on PATH.
    let (_, out, _) = run(&[("HOME", s.0.join("nohome").to_str().unwrap()), ("PATH", &path)], &[], "");
    assert_eq!(out, "path mcp stdio\n");
    // The default location beats PATH.
    let (_, out, _) = run(&[("HOME", h), ("PATH", &path)], &[], "");
    assert_eq!(out, "default mcp stdio\n");
    // CAULDRON_HOME moves the default location.
    let alt = s.0.join("altroot");
    fake(&alt.join("bin/cauldron"), "altroot");
    let (_, out, _) = run(&[("HOME", h), ("CAULDRON_HOME", alt.to_str().unwrap())], &[], "");
    assert_eq!(out, "altroot mcp stdio\n");
    // The install marker beats the default location.
    let marker = home.join(MARKER_REL);
    std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
    std::fs::write(&marker, format!("{}\n", s.0.join("marked/cauldron").display())).unwrap();
    let (_, out, _) = run(&[("HOME", h), ("PATH", &path)], &[], "");
    assert_eq!(out, "marker mcp stdio\n");
    // CAULDRON_BIN beats everything.
    let (_, out, _) = run(&[("HOME", h), ("PATH", &path), ("CAULDRON_BIN", explicit)], &["-x"], "");
    assert_eq!(out, "explicit mcp stdio -x\n");
}

#[test]
fn a_path_that_is_not_a_regular_file_owned_by_the_user_is_never_run() {
    let s = Scratch::new("unsafe");
    let home = s.0.join("home");
    let h = home.to_str().unwrap();
    fake(&home.join(".cauldron/bin/cauldron"), "default");
    // A symlink to a fake: skipped, the default is run.
    fake(&s.0.join("real/cauldron"), "linked");
    let link = s.0.join("link-cauldron");
    std::os::unix::fs::symlink(s.0.join("real/cauldron"), &link).unwrap();
    let (_, out, err) = run(&[("HOME", h), ("CAULDRON_BIN", link.to_str().unwrap())], &[], "");
    assert_eq!(out, "default mcp stdio\n");
    assert!(err.contains("is a symlink"), "{err}");
    // A file owned by root, in root's directories (/bin/echo): run, by its path.
    let (_, out, _) = run(&[("HOME", h), ("CAULDRON_BIN", "/bin/echo")], &[], "");
    assert_eq!(out, "mcp stdio\n");
    // A group-writable file: skipped.
    let gw = s.0.join("gw/cauldron");
    fake(&gw, "groupwritable");
    std::fs::set_permissions(&gw, std::fs::Permissions::from_mode(0o775)).unwrap();
    let (_, out, err) = run(&[("HOME", h), ("CAULDRON_BIN", gw.to_str().unwrap())], &[], "");
    assert_eq!(out, "default mcp stdio\n");
    assert!(err.contains("writable by group or others"), "{err}");
    // A relative path: skipped.
    let (_, out, err) = run(&[("HOME", h), ("CAULDRON_BIN", "cauldron")], &[], "");
    assert_eq!(out, "default mcp stdio\n");
    assert!(err.contains("is not an absolute path"), "{err}");
    // A symlinked default location is skipped too, leaving nothing: the
    // tool-less server answers.
    let home2 = s.0.join("home2");
    std::fs::create_dir_all(home2.join(".cauldron/bin")).unwrap();
    std::os::unix::fs::symlink(s.0.join("real/cauldron"), home2.join(".cauldron/bin/cauldron")).unwrap();
    let (rc, out, _) = run(&[("HOME", home2.to_str().unwrap())], &[], "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}\n");
    assert_eq!(rc, 0);
    let v: Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(v["result"]["tools"], serde_json::json!([]));
}

#[test]
fn with_nothing_installed_it_answers_the_handshake_with_install_instructions() {
    let s = Scratch::new("none");
    let home = s.0.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let input = concat!(
        "{\"jsonrpc\":\"2.0\",\"id\":0,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-06-18\",\"capabilities\":{},\"clientInfo\":{\"name\":\"t\",\"version\":\"1\"}}}\n",
        "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
        "{\"method\":\"tools/list\",\"params\":{\"cursor\":{\"id\":99}},\"jsonrpc\":\"2.0\",\"id\":\"two\"}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"ping\"}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":4,\"method\":\"tools/call\",\"params\":{\"name\":\"soup_get\",\"arguments\":{\"id\":5}}}\n",
    );
    let (rc, out, err) = run(&[("HOME", home.to_str().unwrap()), ("PATH", "/usr/bin:/bin")], &[], input);
    assert_eq!(rc, 0, "{err}");
    let lines: Vec<Value> = out
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("stdout carries a non-JSON line {l:?}: {e}")))
        .collect();
    assert_eq!(lines.len(), 4, "one answer per request, none for the notification: {out}");
    assert_eq!(lines[0]["id"], 0);
    assert_eq!(lines[0]["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(lines[0]["result"]["capabilities"], serde_json::json!({"tools": {}}));
    let instr = lines[0]["result"]["instructions"].as_str().unwrap();
    for want in ["./install --prebuilt", "shasum -a 256 -c SHA256SUMS", "scripts/install", "CAULDRON_BIN", "restart Claude Code"] {
        assert!(instr.contains(want), "the instructions lack {want:?}: {instr}");
    }
    assert!(instr.contains(&format!("{}/.cauldron/bin/cauldron", home.display())), "the instructions name where it looked: {instr}");
    assert_eq!(lines[1]["id"], "two", "the top-level id, not one nested in params");
    assert_eq!(lines[1]["result"]["tools"], serde_json::json!([]));
    assert_eq!(lines[2]["id"], 3);
    assert_eq!(lines[2]["result"], serde_json::json!({}));
    assert_eq!(lines[3]["id"], 4);
    assert_eq!(lines[3]["error"]["code"], -32601);
    assert!(err.contains("no usable cauldron found"), "the note goes to stderr: {err}");
}

#[test]
fn with_no_home_and_no_path_it_still_answers() {
    let (rc, out, _) = run(&[], &[], "{\"jsonrpc\":\"2.0\",\"id\":9,\"method\":\"initialize\",\"params\":{}}\n");
    assert_eq!(rc, 0);
    let v: Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(v["id"], 9);
    assert!(v["result"]["instructions"].as_str().unwrap().contains("Cauldron is not installed"));
}

/// The answer a launcher gives when it found nothing: the tool-less server.
fn answered_tool_less(out: &str) -> bool {
    serde_json::from_str::<Value>(out.trim()).is_ok_and(|v| v["result"]["tools"] == serde_json::json!([]))
}

const LIST: &str = "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}\n";

/// A cloned repository that carries its own `cauldron` (and a `dirname`, in
/// case the launcher looked one up on PATH): with `.` or an empty entry in
/// PATH, started in that repository, the launcher runs neither.
#[test]
fn a_cauldron_in_the_project_is_never_run_through_an_empty_or_relative_path_entry() {
    let s = Scratch::new("dotpath");
    let home = s.0.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let proj = s.0.join("proj");
    fake(&proj.join("cauldron"), "PROJECT");
    fake(&proj.join("dirname"), "PROJECT-DIRNAME");
    fake(&proj.join("bin/cauldron"), "PROJECT-BIN");
    let h = home.to_str().unwrap();
    let bin = proj.join("bin");
    let inside = format!("{}:/usr/bin:/bin", bin.display());
    for path in [".:/usr/bin:/bin", "/usr/bin:/bin:", ":/usr/bin:/bin", "/usr/bin::/bin", "bin:/usr/bin:/bin", inside.as_str()] {
        let (rc, out, err) = run_in(Some(&proj), &[("HOME", h), ("PATH", path)], &[], LIST);
        assert_eq!(rc, 0, "{path}: {err}");
        assert!(!out.contains("PROJECT"), "PATH={path} ran the project's file: {out}");
        assert!(answered_tool_less(&out), "PATH={path}: {out} / {err}");
    }
    let (_, _, err) = run_in(Some(&proj), &[("HOME", h), ("PATH", "/usr/bin::/bin")], &[], LIST);
    assert!(err.contains("ignored an empty PATH entry"), "{err}");
    let (_, _, err) = run_in(Some(&proj), &[("HOME", h), ("PATH", &inside)], &[], LIST);
    assert!(err.contains("inside the project directory"), "{err}");
    // The same PATH entry, started anywhere else, is a PATH entry like any other.
    let (_, out, _) = run_in(Some(&home), &[("HOME", s.0.join("nohome").to_str().unwrap()), ("PATH", &inside)], &[], "");
    assert_eq!(out, "PROJECT-BIN mcp stdio\n");
}

/// CAULDRON_BIN naming a file in the project directory is refused, both the
/// current directory and CLAUDE_PROJECT_DIR; the next source is used.
#[test]
fn cauldron_bin_inside_the_project_is_refused() {
    let s = Scratch::new("projbin");
    let home = s.0.join("home");
    fake(&home.join(".cauldron/bin/cauldron"), "default");
    let proj = s.0.join("proj");
    let planted = proj.join("tools/cauldron");
    fake(&planted, "PROJECT");
    let h = home.to_str().unwrap();
    let p = planted.to_str().unwrap();
    let (_, out, err) = run_in(Some(&proj), &[("HOME", h), ("CAULDRON_BIN", p)], &[], "");
    assert_eq!(out, "default mcp stdio\n");
    assert!(err.contains("inside the project directory"), "{err}");
    let elsewhere = s.0.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let pd = proj.to_str().unwrap();
    let (_, out, _) = run_in(Some(&elsewhere), &[("HOME", h), ("CAULDRON_BIN", p), ("CLAUDE_PROJECT_DIR", pd)], &[], "");
    assert_eq!(out, "default mcp stdio\n");
    // Not a project: started in HOME itself, CAULDRON_BIN under HOME is used.
    fake(&home.join("bin/cauldron"), "homebin");
    let hb = home.join("bin/cauldron");
    let (_, out, _) = run_in(Some(&home), &[("HOME", h), ("CAULDRON_BIN", hb.to_str().unwrap())], &[], "");
    assert_eq!(out, "homebin mcp stdio\n");
}

/// A file of yours is still refused when a directory on its path, as given
/// or behind a symlink, is writable by others; the same checks apply to
/// CAULDRON_BIN, the marker, the default location and PATH alike.
#[test]
fn every_source_refuses_a_file_in_a_directory_others_can_write() {
    let s = Scratch::new("wwdir");
    let home = s.0.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let h = home.to_str().unwrap();
    let ww = s.0.join("ww");
    fake(&ww.join("sub/cauldron"), "WORLD");
    std::fs::set_permissions(&ww, std::fs::Permissions::from_mode(0o777)).unwrap();
    let in_ww = ww.join("sub/cauldron");
    // CAULDRON_BIN.
    let (_, out, err) = run(&[("HOME", h), ("CAULDRON_BIN", in_ww.to_str().unwrap())], &[], LIST);
    assert!(answered_tool_less(&out), "{out}");
    assert!(err.contains("writable by group or others"), "{err}");
    // Through a symlinked parent directory that itself sits somewhere safe.
    let link = s.0.join("linkdir");
    std::os::unix::fs::symlink(ww.join("sub"), &link).unwrap();
    let (_, out, err) = run(&[("HOME", h), ("CAULDRON_BIN", link.join("cauldron").to_str().unwrap())], &[], LIST);
    assert!(answered_tool_less(&out), "{out}");
    assert!(err.contains("writable by group or others"), "{err}");
    // The marker.
    let marker = home.join(MARKER_REL);
    std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
    std::fs::write(&marker, format!("{}\n", in_ww.display())).unwrap();
    let (_, out, _) = run(&[("HOME", h)], &[], LIST);
    assert!(answered_tool_less(&out), "{out}");
    std::fs::remove_file(&marker).unwrap();
    // The default location under a world-writable CAULDRON_HOME.
    let root = ww.join("root");
    fake(&root.join("bin/cauldron"), "WORLDROOT");
    let (_, out, _) = run(&[("HOME", h), ("CAULDRON_HOME", root.to_str().unwrap())], &[], LIST);
    assert!(answered_tool_less(&out), "{out}");
    // A PATH entry inside it.
    let pe = format!("{}:/usr/bin:/bin", ww.join("sub").display());
    let (_, out, _) = run(&[("HOME", h), ("PATH", &pe)], &[], LIST);
    assert!(answered_tool_less(&out), "{out}");
    // Once the directory is safe again, the symlinked path is run by its resolved path.
    std::fs::set_permissions(&ww, std::fs::Permissions::from_mode(0o755)).unwrap();
    let (_, out, _) = run(&[("HOME", h), ("CAULDRON_BIN", link.join("cauldron").to_str().unwrap())], &[], "");
    assert_eq!(out, "WORLD mcp stdio\n");
}

/// CAULDRON_HOME and the install marker follow the project rule too: a
/// `bin/cauldron` the project carries is never run when CAULDRON_HOME names
/// the project (directly, through a symlink outside it, or with the project
/// named by CLAUDE_PROJECT_DIR), nor when the marker names a file in it; the
/// next source is used, and the refusal is named on stderr.
#[test]
fn cauldron_home_and_the_marker_inside_the_project_are_refused() {
    let s = Scratch::new("projhome");
    let home = s.0.join("home");
    let h = home.to_str().unwrap();
    let proj = s.0.join("proj");
    fake(&proj.join("bin/cauldron"), "PROJECT-HOME");
    fake(&proj.join("tools/cauldron"), "PROJECT-MARKED");
    let pathdir = s.0.join("pathdir");
    fake(&pathdir.join("cauldron"), "path");
    let path = format!("{}:/usr/bin:/bin", pathdir.display());
    let pr = proj.to_str().unwrap();

    // CAULDRON_HOME is the project, started in it: PATH is used instead.
    let (rc, out, err) = run_in(Some(&proj), &[("HOME", h), ("CAULDRON_HOME", pr), ("PATH", &path)], &[], "");
    assert_eq!((rc, out.as_str()), (0, "path mcp stdio\n"), "{err}");
    assert!(err.contains("skipped the default install location") && err.contains("inside the project directory"), "{err}");
    // Started in a subdirectory of the project, CAULDRON_HOME the project: the file is not inside the
    // current directory, so only CLAUDE_PROJECT_DIR says it is the project.
    let sub = proj.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    let (_, out, _) = run_in(Some(&sub), &[("HOME", h), ("CAULDRON_HOME", pr), ("PATH", &path), ("CLAUDE_PROJECT_DIR", pr)], &[], "");
    assert_eq!(out, "path mcp stdio\n");
    // Started elsewhere with the project named by CLAUDE_PROJECT_DIR.
    let elsewhere = s.0.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let (_, out, _) = run_in(Some(&elsewhere), &[("HOME", h), ("CAULDRON_HOME", pr), ("PATH", &path), ("CLAUDE_PROJECT_DIR", pr)], &[], "");
    assert_eq!(out, "path mcp stdio\n");
    // CAULDRON_HOME through a symlink that sits outside the project and points into it.
    let link = s.0.join("rootlink");
    std::os::unix::fs::symlink(&proj, &link).unwrap();
    let (_, out, err) = run_in(Some(&proj), &[("HOME", h), ("CAULDRON_HOME", link.to_str().unwrap())], &[], LIST);
    assert!(answered_tool_less(&out), "{out}");
    assert!(err.contains("inside the project directory"), "{err}");
    // The same CAULDRON_HOME, started outside the project with no project named: it is an install like any other.
    let (_, out, _) = run_in(Some(&elsewhere), &[("HOME", h), ("CAULDRON_HOME", pr)], &[], "");
    assert_eq!(out, "PROJECT-HOME mcp stdio\n");

    // The install marker names a file in the project: the default location is used instead.
    fake(&home.join(".cauldron/bin/cauldron"), "default");
    let marker = home.join(MARKER_REL);
    std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
    std::fs::write(&marker, format!("{}\n", proj.join("tools/cauldron").display())).unwrap();
    let (_, out, err) = run_in(Some(&proj), &[("HOME", h)], &[], "");
    assert_eq!(out, "default mcp stdio\n", "{err}");
    assert!(err.contains("skipped the install marker") && err.contains("inside the project directory"), "{err}");
    let (_, out, _) = run_in(Some(&elsewhere), &[("HOME", h), ("CLAUDE_PROJECT_DIR", pr)], &[], "");
    assert_eq!(out, "default mcp stdio\n");
    // Marker and CAULDRON_HOME both in the project, nothing else: nothing from it runs.
    let (_, out, _) = run_in(Some(&proj), &[("HOME", h), ("CAULDRON_HOME", pr)], &[], LIST);
    assert!(answered_tool_less(&out), "{out}");
    // Outside the project the marker is honoured.
    let (_, out, _) = run_in(Some(&elsewhere), &[("HOME", h)], &[], "");
    assert_eq!(out, "PROJECT-MARKED mcp stdio\n");
}

/// A session started in HOME (CLAUDE_PROJECT_DIR is HOME itself) is not a
/// project: the default install under HOME is used.
#[test]
fn a_session_in_home_still_finds_the_default_install() {
    let s = Scratch::new("homeproj");
    let home = s.0.join("home");
    fake(&home.join(".cauldron/bin/cauldron"), "default");
    let h = home.to_str().unwrap();
    let (_, out, err) = run_in(Some(&home), &[("HOME", h), ("CLAUDE_PROJECT_DIR", h)], &[], "");
    assert_eq!(out, "default mcp stdio\n", "{err}");
}
