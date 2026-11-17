//! **`calls.jsonl`**: one line per write-scoped call, and one per open and
//! close of a cauldron, at `<data root>/mcp/calls.jsonl` (mode 0600):
//!
//! ```json
//! {"at":"…","client":"claude-code","session":"…","cauldron":"kb","tool":"soup_put",
//!  "outcome":"landed","tx":"<hex>","counts":{"records":3,"accepted":3,"refused":0}}
//! ```
//!
//! It never holds record text, a citation, a link note or the door secret:
//! only who, which cauldron, which tool, how it came out, the transaction
//! and counts. An open adds what the open recovered (transaction hashes and
//! id ranges, never text).

use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{json, Value};

/// The file's name under the service's directory.
pub const FILE: &str = "calls.jsonl";

/// The audit log of one environment.
pub struct Audit {
    path: PathBuf,
    lock: Mutex<()>,
}

/// One line's fields (`at` is added on write).
#[derive(Debug, Clone, Default)]
pub struct Line {
    pub client: Option<String>,
    pub session: Option<String>,
    pub cauldron: Option<String>,
    pub tool: String,
    pub outcome: String,
    pub tx: Option<String>,
    pub counts: Value,
    /// Only on an `open`: what the open recovered.
    pub recovered: Option<Value>,
    /// Only on a `close`: why.
    pub reason: Option<String>,
}

impl Audit {
    pub fn new(dir: &Path) -> Audit {
        Audit { path: dir.join(FILE), lock: Mutex::new(()) }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one line. A failure is logged once as a warning (with the
    /// path, never the line) and does not fail the call: the write already
    /// happened, and the spine's own transaction row is its durable record.
    pub fn write(&self, l: Line) {
        let mut v = json!({
            "at": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            "client": l.client,
            "session": l.session,
            "cauldron": l.cauldron,
            "tool": l.tool,
            "outcome": l.outcome,
            "tx": l.tx,
            "counts": if l.counts.is_null() { json!({}) } else { l.counts },
        });
        if let Some(r) = l.recovered {
            v["recovered_at_open"] = r;
        }
        if let Some(r) = l.reason {
            v["reason"] = json!(r);
        }
        let _g = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let wrote = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&self.path)
            .and_then(|mut f| writeln!(f, "{v}"));
        if let Err(e) = wrote {
            tracing::warn!(path = %self.path.display(), error = %e, "could not append to the audit log");
        }
    }
}
