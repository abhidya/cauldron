//! **Which `llama-server`, and is it new enough**: the parts of the runtime
//! check that need no running service, shared by the `inference` service
//! (which checks the binary before every load) and `cauldron inference
//! status` (which says what is missing before the service runs).
//!
//! The binary is the one `llama_server` names in the settings, else the
//! first `llama-server` on `PATH`. Its `--version` must state a build of at
//! least [`MIN_BUILD`].

use std::path::{Path, PathBuf};

/// The oldest build accepted. Every flag passed is checked in `--help`
/// besides; this floor turns away builds far older than the one this
/// service was written against ([`TESTED_BUILD`]).
pub const MIN_BUILD: u64 = 6000;

/// The build this release was measured with.
pub const TESTED_BUILD: u64 = 12110;

/// What to do when there is no `llama-server`, in one sentence.
pub const HOW_TO_GET: &str = "Install llama.cpp, which provides it: on macOS, `brew install llama.cpp` (Homebrew)";

/// The binary the settings mean: `configured`, else the first
/// `llama-server` on `PATH`.
pub fn locate(configured: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(p) = configured {
        return if p.is_file() { Ok(p.to_path_buf()) } else { Err(format!("`llama_server` in the settings names {}, which is not a file", p.display())) };
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    for d in std::env::split_paths(&path) {
        let c = d.join("llama-server");
        if c.is_file() {
            return Ok(c);
        }
    }
    Err(format!(
        "`llama-server` is not on PATH. {HOW_TO_GET}, build {MIN_BUILD} or later, or set `llama_server = \"/absolute/path\"` in the inference settings (data/<env>/inference/config.toml); then try again"
    ))
}

/// The build number in `--version`'s output.
pub fn parse_build(text: &str) -> Option<(u64, String)> {
    let line = text.lines().find(|l| l.trim_start().starts_with("version:"))?.trim().to_string();
    let digits = |s: &str| -> Option<u64> { s.chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse().ok() };
    if let Some(i) = line.find("build ") {
        return digits(&line[i + 6..]).map(|b| (b, line.clone()));
    }
    // The older form: `version: 4589 (<short commit>)`.
    let rest = line.trim_start_matches("version:").trim();
    let b = digits(rest)?;
    rest[b.to_string().len()..].trim_start().starts_with('(').then_some((b, line.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_read_as_llama_server_prints_them() {
        assert_eq!(parse_build("version: 0.9.0 (build 12110, more)\nbuilt with clang").unwrap().0, 12110);
        assert_eq!(parse_build("load_backend: x\nversion: 4589 (z)\n").unwrap().0, 4589);
        assert!(parse_build("llama-server 1.0").is_none());
        assert!(parse_build("version: 0.9.0").is_none());
        assert!(locate(Some(Path::new("/nonexistent/llama-server"))).unwrap_err().contains("not a file"));
    }
}
