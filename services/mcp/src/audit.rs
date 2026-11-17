//! **`calls.jsonl`**: one line per write-scoped call, and one per open and
//! close of a cauldron, at `<data root>/mcp/calls.jsonl` (mode 0600):
//!
//! ```json
//! {"at":"…","client":"claude-code","session":"…","cauldron":"kb","tool":"soup_put",
//!  "outcome":"landed","tx":"<hex>","counts":{"records":3,"accepted":3,"refused":0}}
//! ```
//!
//! It never holds record text, a dump's text or transcript, a citation, a
//! link note or the door secret: only who, which cauldron, which tool, how it
//! came out, the transaction and counts (and a landed dump's id and size). An open adds what the open recovered (transaction hashes and
//! id ranges, never text).
//!
//! Each line is appended by one write, so a reader of the file sees whole
//! lines only.
//!
//! It is rotated: once it holds [`ROTATE_AT`] bytes, the next line first
//! moves it to `calls.jsonl.1` (replacing an older one), so the audit never
//! holds more than about twice that.

use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{json, Value};

/// The file's name under the service's directory.
pub const FILE: &str = "calls.jsonl";

/// The size at which the file is moved aside to `calls.jsonl.1`.
pub const ROTATE_AT: u64 = 8 * 1024 * 1024;

/// The audit log of one environment.
pub struct Audit {
    path: PathBuf,
    lock: Mutex<()>,
    rotate_at: u64,
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
    /// Only on a dump that landed: its id (never its text).
    pub dump: Option<String>,
}

impl Audit {
    pub fn new(dir: &Path) -> Audit {
        Audit::rotating_at(dir, ROTATE_AT)
    }

    /// An audit log moved aside at `rotate_at` bytes.
    pub fn rotating_at(dir: &Path, rotate_at: u64) -> Audit {
        Audit { path: dir.join(FILE), lock: Mutex::new(()), rotate_at }
    }

    /// The file a full log is moved to.
    pub fn previous(&self) -> PathBuf {
        let mut p = self.path.as_os_str().to_owned();
        p.push(".1");
        PathBuf::from(p)
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
        if let Some(d) = l.dump {
            v["dump"] = json!(d);
        }
        // The whole line, newline included, goes to the file in ONE write
        // (`O_APPEND`): a reader never sees part of a line. (`writeln!` on a
        // `File` writes each piece the formatter makes as a write of its
        // own, which a reader could catch half done.)
        let line = format!("{v}\n");
        let _g = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        if std::fs::metadata(&self.path).is_ok_and(|m| m.len() >= self.rotate_at) {
            if let Err(e) = std::fs::rename(&self.path, self.previous()) {
                tracing::warn!(path = %self.path.display(), error = %e, "could not rotate the audit log");
            }
        }
        let wrote = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&self.path)
            .and_then(|mut f| f.write_all(line.as_bytes()));
        if let Err(e) = wrote {
            tracing::warn!(path = %self.path.display(), error = %e, "could not append to the audit log");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reader of `calls.jsonl` never sees part of a line: each line is
    /// appended by one write, so the file always ends at a line's end and
    /// every line in it parses.
    #[test]
    fn a_reader_never_sees_part_of_a_line() {
        let dir = std::env::temp_dir().join(format!("mcp-audit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = std::sync::Arc::new(Audit::new(&dir));
        let w = a.clone();
        let writer = std::thread::spawn(move || {
            for i in 0..3000 {
                w.write(Line {
                    client: Some("claude-code".into()),
                    session: Some(format!("sess-{i}")),
                    cauldron: Some("kb".into()),
                    tool: "soup_put".into(),
                    outcome: "landed".into(),
                    counts: json!({"records": i, "accepted": i, "refused": 0, "pad": "x".repeat(200)}),
                    ..Line::default()
                });
            }
        });
        let mut reads = 0;
        while !writer.is_finished() || reads == 0 {
            let text = std::fs::read_to_string(a.path()).unwrap_or_default();
            assert!(text.is_empty() || text.ends_with('\n'), "a read saw part of a line: …{}", &text[text.len().saturating_sub(80)..]);
            for l in text.lines() {
                serde_json::from_str::<Value>(l).unwrap();
            }
            reads += 1;
        }
        writer.join().unwrap();
        assert_eq!(std::fs::read_to_string(a.path()).unwrap().lines().count(), 3000);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Past its size the log is moved aside once, keeping one previous file,
    /// and every line is in one of the two.
    #[test]
    fn a_full_log_is_moved_aside_and_one_previous_kept() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("mcp-audit-rotate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = Audit::rotating_at(&dir, 2000);
        let line = |i: usize| Line { tool: "soup_put".into(), outcome: "landed".into(), counts: json!({"n": i}), ..Line::default() };
        for i in 0..100 {
            a.write(line(i));
        }
        let now = std::fs::read_to_string(a.path()).unwrap();
        let prev = std::fs::read_to_string(a.previous()).unwrap();
        assert!(now.len() < 2200 && prev.len() < 2200, "{} {}", now.len(), prev.len());
        let last: Vec<i64> = now.lines().chain(prev.lines()).map(|l| serde_json::from_str::<Value>(l).unwrap()["counts"]["n"].as_i64().unwrap()).collect();
        assert!(last.contains(&99) && !last.contains(&0), "older lines went with the older file");
        assert_eq!(std::fs::metadata(a.previous()).unwrap().permissions().mode() & 0o777, 0o600);
        assert!(!dir.join(format!("{FILE}.2")).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
