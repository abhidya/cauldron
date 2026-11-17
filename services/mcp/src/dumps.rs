//! **Dumps**: raw text put into a granted cauldron for later processing,
//! kept in the cauldron's dumps store beside its soup
//! (`soup_door::dumps`), never in the soup.
//!
//! # The store, held
//!
//! The first dump-scoped write to a cauldron opens its dumps store (making
//! it, the first time anything is dumped there): the store's writer lock is
//! taken exclusive and `mcp <pid>` written into it, then its spine is opened,
//! which recovers a dump that a killed service left half done (rolled back
//! before its decision, landed after it). The store is held, like the soup's
//! writer, until the grant loses `dump` or is revoked, or the service stops;
//! there is no idle close. While it is held, `cauldron soup dump` sends its
//! text here.
//!
//! # One dump, one tranche
//!
//! A dump's bytes go to the blob store first, as a temporary file. Then,
//! holding the store, the grant is read again (a revoke that came while the
//! bytes arrived refuses the dump), the bytes are synced and named by their
//! SHA-256, and ONE transaction creates `DM-<n>` in state `received`. A
//! dump refused or failed at any of these steps leaves no byte behind: the
//! temporary file is removed, and so is a named blob no dump refers to. A
//! transcript still arriving when its grant is revoked stops being read. Its commit ends the
//! write and the store is let go for the next dump. Only after that is
//! [`SideEffects::after_commit`] called: whatever a dump sets off later runs
//! after its write, never inside it, and the write never waits on it. In
//! this release the only side effect is a log line ([`LogOnly`]); processing
//! comes in a later release, and picks up the dumps still `received`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use shapewire::envelope::Refusal;
use soup_door::dumps::{self, By, Landed, NewDump, Store};
use soup_door::Target;

use crate::access::Access;
use crate::audit::{Audit, Line};
use crate::refusal;
use crate::writer::Who;

/// What landed, as a side effect sees it.
#[derive(Debug, Clone)]
pub enum Landing<'a> {
    /// A dump was received.
    Dump { env: &'a str, cauldron: &'a str, id: &'a str, kind: &'a str, bytes: u64, cursor: &'a str },
}

/// **What a landed write sets off**, called after its transaction has
/// committed and the store has been let go. The hook later processing
/// attaches to; nothing it does can undo or delay the write.
pub trait SideEffects: Send + Sync {
    fn after_commit(&self, landed: &Landing<'_>);
}

/// This release's side effects: a log line, and nothing else.
pub struct LogOnly;

impl SideEffects for LogOnly {
    fn after_commit(&self, landed: &Landing<'_>) {
        match landed {
            Landing::Dump { env, cauldron, id, kind, bytes, cursor } => tracing::info!(
                %env, %cauldron, dump = %id, %kind, bytes, %cursor,
                "dump received; it waits as `received` until its owner processes it (`cauldron ripples run`)"
            ),
        }
    }
}

/// One cauldron's dumps store, held open.
pub struct Held {
    pub opened_at: String,
    pub recovered: Value,
    store: Mutex<Option<Store>>,
}

/// Every dumps store this service holds.
pub struct Dumps {
    audit: Arc<Audit>,
    access: Arc<Access>,
    effects: Arc<dyn SideEffects>,
    open: Mutex<HashMap<String, Arc<Held>>>,
    /// Held while a store is being opened, so opens run one at a time
    /// without holding `open` (see `writer::Writers`).
    opening: Mutex<()>,
}

fn closed() -> Refusal {
    Refusal::new(
        "not_granted",
        "the cauldron's dumps store was closed to this door (its grant was revoked or lost `dump`, or the service is \
         stopping) before this dump was written. Nothing was written.",
    )
}

fn revoked(cauldron: &str, why: &str) -> Refusal {
    Refusal::new(
        "not_granted",
        format!(
            "cauldron `{cauldron}` is closed to this door: {why} before this dump was written. Nothing was written. \
             The owner opens it again from a terminal with `cauldron mcp grant`."
        ),
    )
}

impl Dumps {
    pub fn new(audit: Arc<Audit>, access: Arc<Access>, effects: Arc<dyn SideEffects>) -> Dumps {
        Dumps { audit, access, effects, open: Mutex::new(HashMap::new()), opening: Mutex::new(()) }
    }

    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<Held>>> {
        self.open.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The names of every cauldron whose dumps store this service holds.
    pub fn names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.map().keys().cloned().collect();
        v.sort();
        v
    }

    /// Every held dumps store, as `cauldron mcp status` shows it:
    /// `[{cauldron, opened_at, recovered_at_open}]`, by name.
    pub fn snapshot(&self) -> Value {
        let open = self.map();
        let mut v: Vec<(&String, &Arc<Held>)> = open.iter().collect();
        v.sort_by(|a, b| a.0.cmp(b.0));
        Value::Array(
            v.into_iter()
                .map(|(name, h)| serde_json::json!({"cauldron": name, "opened_at": h.opened_at, "recovered_at_open": h.recovered}))
                .collect(),
        )
    }

    /// Is `cauldron`'s dumps store held here?
    pub fn holds(&self, cauldron: &str) -> bool {
        self.map().contains_key(cauldron)
    }

    fn get_or_open(&self, t: &Target, who: &Who) -> Result<Arc<Held>, Refusal> {
        if let Some(h) = self.map().get(&t.cauldron.name) {
            return Ok(h.clone());
        }
        let _opening = self.opening.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(h) = self.map().get(&t.cauldron.name) {
            return Ok(h.clone());
        }
        if let Some(why) = self.access.why_not_dumpable(&t.cauldron) {
            return Err(revoked(&t.cauldron.name, why));
        }
        let store = Store::open(&t.cauldron, &format!("{} {}", soup_door::lock::MCP_SERVICE, std::process::id())).map_err(refusal)?;
        let h = Arc::new(Held {
            opened_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            recovered: store.recovered.clone(),
            store: Mutex::new(Some(store)),
        });
        self.map().insert(t.cauldron.name.clone(), h.clone());
        tracing::info!(cauldron = %t.cauldron.name, "opened a cauldron's dumps store");
        self.audit.write(Line {
            client: Some(who.client.clone()),
            session: who.session.clone(),
            cauldron: Some(t.cauldron.name.clone()),
            tool: "open-dumps".into(),
            outcome: "opened".into(),
            recovered: Some(h.recovered.clone()),
            ..Line::default()
        });
        Ok(h)
    }

    /// Open (or find) the store, for a dump about to be written: the blob
    /// store is then ready to take its bytes.
    pub fn prepare(&self, t: &Target, who: &Who) -> Result<(), Refusal> {
        self.get_or_open(t, who).map(|_| ())
    }

    /// Is a dump into `t` still allowed (its grant read again)?
    pub fn still_allowed(&self, t: &Target) -> Result<(), Refusal> {
        match self.access.why_not_dumpable(&t.cauldron) {
            Some(why) => Err(revoked(&t.cauldron.name, why)),
            None => Ok(()),
        }
    }

    /// **Land a dump** whose bytes are in `blob` (not yet named): hold the
    /// store, read the grant again, name the blob, build the dump with
    /// `make` (given the blob and its path), write ONE transaction, let the
    /// store go, then call the side effects. Answers the receipt. A refusal
    /// or an error at any step leaves no byte: the temporary file, or the
    /// named blob no dump refers to, is removed (`Store::commit`).
    pub fn commit(
        &self,
        t: &Target,
        who: &Who,
        blob: dumps::BlobWriter,
        make: impl FnOnce(&dumps::Blob, &std::path::Path) -> Result<NewDump, Refusal>,
        dispatch: &str,
    ) -> Result<Value, Refusal> {
        let h = self.get_or_open(t, who)?;
        let (dump, landed): (NewDump, Landed) = {
            let store = h.store.lock().unwrap_or_else(|p| p.into_inner());
            let Some(store) = store.as_ref() else { return Err(closed()) };
            let by = By {
                thread: who.thread.clone(),
                dispatch: dispatch.to_string(),
                instrument: "cauldron-mcp".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            };
            store.commit(blob, || self.still_allowed(t), |b| make(b, &store.blob_path(b)), &by, refusal)?
        };
        let cursor = crate::cursor::now(&t.cauldron).unwrap_or_default();
        let mut receipt = dumps::receipt(&t.cauldron.name, &t.env.name, &dump, &landed);
        receipt["cursor"] = json!(cursor);
        self.effects.after_commit(&Landing::Dump {
            env: &t.env.name,
            cauldron: &t.cauldron.name,
            id: &landed.id,
            kind: dump.kind().as_str(),
            bytes: dump.blob.bytes,
            cursor: &cursor,
        });
        Ok(receipt)
    }

    /// Run `f` on `t`'s dumps store, holding it (opened first if needed, as
    /// for a dump).
    pub fn with_store<R>(&self, t: &Target, who: &Who, f: impl FnOnce(&Store) -> Result<R, Refusal>) -> Result<R, Refusal> {
        let h = self.get_or_open(t, who)?;
        let store = h.store.lock().unwrap_or_else(|p| p.into_inner());
        let Some(store) = store.as_ref() else { return Err(closed()) };
        f(store)
    }

    /// Close `cauldron`'s dumps store: the dump being written (if any)
    /// finishes, then the store and its lock are let go. A no-op when it is
    /// not held.
    pub fn close(&self, cauldron: &str, reason: &str) {
        let Some(h) = self.map().remove(cauldron) else { return };
        h.store.lock().unwrap_or_else(|p| p.into_inner()).take();
        tracing::info!(%cauldron, %reason, "closed a cauldron's dumps store; its lock is free");
        self.audit.write(Line {
            cauldron: Some(cauldron.to_string()),
            tool: "close-dumps".into(),
            outcome: "closed".into(),
            reason: Some(reason.to_string()),
            ..Line::default()
        });
    }

    /// Close every held store.
    pub fn close_all(&self, reason: &str) {
        for n in self.names() {
            self.close(&n, reason);
        }
    }
}
