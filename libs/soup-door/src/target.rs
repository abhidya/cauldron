//! **Which soup a call reads or writes**: an existing environment, an
//! existing cauldron in it, and that cauldron's soup.

use env::Environment;

use crate::marker::{refuse_untrusted_marker_for_write, Resolved};
use crate::{refuse, Code, Error, Result};

/// A resolved, existing target: the environment, the cauldron, and, when the
/// project marker supplied either name, `marker:<path>` (callers put it in
/// their answer as `resolved_from`).
#[derive(Debug, Clone)]
pub struct Target {
    pub env: Environment,
    pub cauldron: env::Cauldron,
    pub resolved_from: Option<String>,
}

impl Target {
    /// The cauldron's soup directory.
    pub fn soup(&self) -> std::path::PathBuf {
        self.cauldron.soup()
    }
}

/// Turn what [`crate::marker::resolve`] found into an existing target. The
/// environment and the cauldron must exist and the cauldron must have a
/// soup. When the marker supplied either name and the caller `writes`, the
/// marker must be this user's own and untracked by git
/// ([`refuse_untrusted_marker_for_write`]). `verb` is only for the messages.
pub fn locate(verb: &str, r: &Resolved, writes: bool) -> Result<Target> {
    let e = Environment::resolve()
        .and_then(|e| e.for_name(&r.env))
        .map_err(|err| Error::from_anyhow(Code::NoEnvironment, err))?;
    if !e.data_root.is_dir() {
        refuse!(Code::NoEnvironment, "REFUSED: there is no environment `{}` ({} does not exist)", e.name, e.data_root.display());
    }
    let mut resolved_from = None;
    if let Some(m) = &r.marker {
        if writes {
            refuse_untrusted_marker_for_write(verb, r, &e.data_root)?;
        }
        resolved_from = Some(format!("marker:{}", m.display()));
    }
    let name = &r.cauldron;
    let Some(c) = e.find_cauldron(name).map_err(|err| Error::from_anyhow(Code::NoCauldron, err))? else {
        refuse!(
            Code::NoCauldron,
            "REFUSED: there is no cauldron `{name}` in environment `{}`. `cauldron list --env {}` shows its cauldrons.",
            e.name,
            e.name
        );
    };
    let soup = c.soup();
    if !soup.join(spine::spine_db::SPINE_FILE).is_file() {
        refuse!(Code::NoSoup, "REFUSED: cauldron `{name}` has no soup yet ({} holds no spine.db)", soup.display());
    }
    Ok(Target { env: e, cauldron: c, resolved_from })
}

/// A target named outright, with no project marker read: the environment
/// `env` and the cauldron `cauldron`, both of which must exist.
pub fn named(env: &str, cauldron: &str) -> Result<Target> {
    let r = Resolved {
        env: env.to_string(),
        env_from: "named".to_string(),
        cauldron: cauldron.to_string(),
        cauldron_from: "named".to_string(),
        marker: None,
    };
    locate("", &r, false)
}
