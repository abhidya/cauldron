//! **The runtime: `llama-server`**, never bundled. It is the binary named by
//! `llama_server` in the settings, else the first `llama-server` on `PATH`
//! (Homebrew's `/opt/homebrew/bin/llama-server` on this kind of machine).
//!
//! Before a model is loaded the binary is checked, by running it with
//! `--version` and `--help` only:
//!
//! - its build number (`version: 0.9.0 (build N, …)`, or the older
//!   `version: N (…)`) is at least [`MIN_BUILD`];
//! - every flag this service passes ([`REQUIRED_FLAGS`]) is in its `--help`.
//!   `--json-schema` is among them: it is how a build says it compiles a JSON
//!   schema into a grammar, which is what constrains the answer of
//!   `/v1/chat/completions` given a `response_format` of type
//!   `json_schema`. The flags in [`OPTIONAL_FLAGS`] are passed only when the
//!   build lists them.
//!
//! A missing binary, one that does not answer, or one too old is refused
//! with what to do; nothing is loaded.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::Serialize;

pub use inference_registry::runtime::{locate, parse_build, MIN_BUILD};

/// Flags passed on every load; a build that lacks one is refused.
pub const REQUIRED_FLAGS: &[&str] =
    &["--model", "--alias", "--host", "--port", "--api-key-file", "--ctx-size", "--parallel", "--no-webui", "--no-slots", "--offline", "--jinja", "--json-schema"];

/// Flags passed only when the build lists them: `--no-agent` (no built-in
/// tools and no CORS proxy, explicitly), `--reasoning off` (no thinking
/// section before the constrained answer, where the template allows it:
/// gpt-oss writes one anyway, outside the answer, counted in its output
/// tokens), `--cache-ram 0` (no host-memory
/// prompt cache beyond the estimate).
pub const OPTIONAL_FLAGS: &[&str] = &["--no-agent", "--reasoning", "--cache-ram"];

/// How long `--version` or `--help` may take.
const ASK_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Serialize)]
pub struct Runtime {
    pub path: PathBuf,
    /// The first line `--version` printed.
    pub version: String,
    pub build: u64,
    /// Every flag `--help` lists.
    #[serde(skip)]
    pub flags: BTreeSet<String>,
    /// The optional flags this build has.
    pub optional: Vec<String>,
}

impl Runtime {
    pub fn has(&self, flag: &str) -> bool {
        self.flags.contains(flag)
    }
}

/// Run `bin arg` and answer what it printed (stdout and stderr together),
/// within [`ASK_TIMEOUT`].
fn ask(bin: &Path, arg: &str) -> Result<String, String> {
    let mut child = Command::new(bin)
        .arg(arg)
        .env_clear()
        .envs(crate::supervisor::child_env())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{} {arg} could not be run: {e}", bin.display()))?;
    let (mut out, mut err) = (child.stdout.take(), child.stderr.take());
    let t_out = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(o) = out.as_mut() {
            let _ = std::io::Read::read_to_string(o, &mut s);
        }
        s
    });
    let t_err = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(e) = err.as_mut() {
            let _ = std::io::Read::read_to_string(e, &mut s);
        }
        s
    });
    let deadline = Instant::now() + ASK_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{} {arg} did not answer within {} s", bin.display(), ASK_TIMEOUT.as_secs()));
            }
        }
    }
    Ok(format!("{}\n{}", t_out.join().unwrap_or_default(), t_err.join().unwrap_or_default()))
}

/// Every flag `--help` lists (each word of a line that starts with `-`).
pub fn parse_flags(help: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for l in help.lines() {
        let l = l.trim_start();
        if !l.starts_with('-') {
            continue;
        }
        for w in l.split_whitespace() {
            let w = w.trim_end_matches(',');
            if w.starts_with('-') && w.len() > 1 {
                out.insert(w.to_string());
            } else {
                break;
            }
        }
    }
    out
}

/// **Check the runtime** the settings mean; `Err` is the refusal, with what
/// to do.
pub fn check(configured: Option<&Path>) -> Result<Runtime, String> {
    let path = locate(configured)?;
    let v = ask(&path, "--version")?;
    let Some((build, version)) = parse_build(&v) else {
        return Err(format!("{} --version printed no build number this service can read ({:?}); it needs a llama.cpp build {MIN_BUILD} or later", path.display(), v.trim().chars().take(200).collect::<String>()));
    };
    if build < MIN_BUILD {
        return Err(format!("{} is llama.cpp build {build} ({version}); this service needs build {MIN_BUILD} or later. Update it (on macOS, `brew upgrade llama.cpp`), then try again", path.display()));
    }
    let help = ask(&path, "--help")?;
    let flags = parse_flags(&help);
    let missing: Vec<&str> = REQUIRED_FLAGS.iter().copied().filter(|f| !flags.contains(*f)).collect();
    if !missing.is_empty() {
        return Err(format!(
            "{} (build {build}) does not list {} in --help; this service passes them on every load (the JSON-schema constrained answer, the loopback-only key, no web UI, no slot monitor, offline). Update llama.cpp, then try again",
            path.display(),
            missing.join(", ")
        ));
    }
    let optional = OPTIONAL_FLAGS.iter().filter(|f| flags.contains(**f)).map(|f| f.to_string()).collect();
    Ok(Runtime { path, version, build, flags, optional })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_and_flags_are_read_as_llama_server_prints_them() {
        assert_eq!(parse_build("version: 0.9.0 (build 12110, more)\nbuilt with clang").unwrap().0, 12110);
        assert_eq!(parse_build("load_backend: x\nversion: 4589 (z)\n").unwrap().0, 4589);
        assert!(parse_build("llama-server 1.0").is_none());
        assert!(parse_build("version: 0.9.0").is_none());
        let f = parse_flags("-m,    --model FNAME   model path\n--ui,  --webui, --no-ui, --no-webui     whether\n   (env: LLAMA_ARG_UI)\n-to,   --timeout N  x\n");
        for x in ["-m", "--model", "--ui", "--webui", "--no-ui", "--no-webui", "-to", "--timeout"] {
            assert!(f.contains(x), "{x}");
        }
        assert!(!f.contains("FNAME") && !f.contains("N"));
    }
}
