//! **The writer**: one thread per open cauldron, the only holder of that
//! cauldron's `spine::Cauldron` handle and of its soup's writer lock.
//!
//! # Opening, on the first write-scoped call (never at boot)
//!
//! The same three proofs `cauldron soup import` makes, in the same order:
//!
//! 1. no live `soup` seam in the environment (a soup daemon may hold the
//!    store);
//! 2. `<soup>/writer.lock` taken exclusive and non-blocking; this door then
//!    writes `mcp <pid>` into it (informational: the lock is the truth);
//! 3. `spine::Cauldron::open`.
//!
//! Unlike the import, the open does NOT refuse a soup that is not at rest.
//! Holding the lock proves no other writer exists, so the open-time recovery
//! the spine runs is exactly `cauldron soup recover`: an interrupted write
//! is rolled forward (it had decided: all of it lands) or rolled back (it
//! had not: none of it lands). What it did is kept as `recovered_at_open`,
//! answered by `soup_status` and written to `calls.jsonl`.
//!
//! # Calls
//!
//! `soup_put` and `soup_link` calls are queued to the thread over a
//! channel, so writes are strictly serialized; concurrent callers wait in
//! the queue and are never refused for being concurrent. Reads never touch
//! this thread.
//!
//! # The grant, before every write
//!
//! Immediately before it applies a queued write, the thread re-reads the
//! grants ([`Access::why_not_writable`]); when the cauldron's grant has been
//! revoked or has lost `write`, that call is refused as `not_granted` and
//! nothing of it is written. So a call queued before a revoke never lands
//! after the revoke is seen. A write the thread had already started applying
//! when the revoke came finishes (all of it lands, or none of it); it is the
//! only one.
//!
//! # Closing
//!
//! The lock is held until the cauldron's grant loses `write` (or is
//! revoked), or the service stops. There is no idle close. Closing marks the
//! writer closed and drops the channel: every call still queued is refused
//! as `not_granted` without being applied, the write in progress (if any)
//! finishes, then the handle and the lock are dropped and the thread is
//! joined, so when [`Writers::close`] returns the lock is free.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::File;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use serde_json::{json, Value};
use shapewire::envelope::Refusal;
use soup_door::import::{self, ImportOptions, ImportOutcome};
use soup_door::reads::DEFAULT_POOL;
use soup_door::Target;
use spine::envelope::{Envelope, Initiator, Instrument, Mode, Provenance, ENVELOPE_FORMAT_VERSION};
use spine::pool::{NodeId, Pool};
use spine::schema_def::PoolName;

use crate::access::Access;
use crate::audit::{Audit, Line};
use crate::refusal;

/// What `soup_put` asks.
#[derive(Debug, Clone)]
pub struct Put {
    pub records: Vec<Value>,
    pub cite: Option<Value>,
    pub remint: bool,
    pub dry_run: bool,
    pub partial: bool,
    pub allow_duplicate: bool,
}

/// One `soup_link` link.
#[derive(Debug, Clone)]
pub struct Link {
    pub from: String,
    pub kind: String,
    pub to: String,
    pub note: Option<String>,
}

/// Who is writing: the client, the spine initiator's `thread`, and the
/// session to cite.
#[derive(Debug, Clone)]
pub struct Who {
    pub client: String,
    pub thread: String,
    pub session: Option<String>,
}

enum Job {
    Put(Put, Who, mpsc::Sender<Result<Value, Refusal>>),
    Link(Vec<Link>, Who, mpsc::Sender<Result<Value, Refusal>>),
}

/// One open cauldron.
pub struct Writer {
    pub cauldron: String,
    pub opened_at: String,
    /// What the open recovered (see the module docs).
    pub recovered: Value,
    jobs: Mutex<Option<mpsc::Sender<Job>>>,
    thread: Mutex<Option<JoinHandle<()>>>,
    /// Set by [`Writers::close`]: the thread applies nothing more.
    closed: Arc<AtomicBool>,
}

/// Every open cauldron of this service's environment.
pub struct Writers {
    audit: Arc<Audit>,
    access: Arc<Access>,
    open: Mutex<HashMap<String, Arc<Writer>>>,
}

fn closed() -> Refusal {
    Refusal::new(
        "not_granted",
        "the cauldron was closed to this door (its grant was revoked or lost `write`, or the service is stopping) \
         before this call ran. Nothing was written.",
    )
}

impl Writers {
    pub fn new(audit: Arc<Audit>, access: Arc<Access>) -> Writers {
        Writers { audit, access, open: Mutex::new(HashMap::new()) }
    }

    /// The open writer of `cauldron`, if any.
    pub fn get(&self, cauldron: &str) -> Option<Arc<Writer>> {
        self.open.lock().unwrap_or_else(|p| p.into_inner()).get(cauldron).cloned()
    }

    /// The names of every open cauldron.
    pub fn names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.open.lock().unwrap_or_else(|p| p.into_inner()).keys().cloned().collect();
        v.sort();
        v
    }

    /// Every open cauldron, as `cauldron mcp status` shows it:
    /// `[{cauldron, opened_at, recovered_at_open}]`, by name.
    pub fn snapshot(&self) -> Value {
        let open = self.open.lock().unwrap_or_else(|p| p.into_inner());
        let mut v: Vec<&Arc<Writer>> = open.values().collect();
        v.sort_by(|a, b| a.cauldron.cmp(&b.cauldron));
        Value::Array(
            v.into_iter()
                .map(|w| json!({"cauldron": w.cauldron, "opened_at": w.opened_at, "recovered_at_open": w.recovered}))
                .collect(),
        )
    }

    /// Run `put` on the target's writer, opening it first if needed.
    pub fn put(&self, t: &Target, who: Who, put: Put) -> Result<Value, Refusal> {
        let w = self.get_or_open(t, &who)?;
        let (tx, rx) = mpsc::channel();
        w.send(Job::Put(put, who, tx))?;
        rx.recv().unwrap_or_else(|_| Err(closed()))
    }

    /// Run `links` on the target's writer, opening it first if needed.
    pub fn link(&self, t: &Target, who: Who, links: Vec<Link>) -> Result<Value, Refusal> {
        let w = self.get_or_open(t, &who)?;
        let (tx, rx) = mpsc::channel();
        w.send(Job::Link(links, who, tx))?;
        rx.recv().unwrap_or_else(|_| Err(closed()))
    }

    fn get_or_open(&self, t: &Target, who: &Who) -> Result<Arc<Writer>, Refusal> {
        let mut open = self.open.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(w) = open.get(&t.cauldron.name) {
            return Ok(w.clone());
        }
        // A call authorized just before a revoke does not open the cauldron
        // just after it.
        if let Some(why) = self.access.why_not_writable(&t.cauldron) {
            return Err(revoked(&t.cauldron.name, why));
        }
        let (ready_tx, ready_rx) = mpsc::channel::<Result<Value, Refusal>>();
        let (jobs_tx, jobs_rx) = mpsc::channel::<Job>();
        let target = t.clone();
        let closed = Arc::new(AtomicBool::new(false));
        let gate = Gate { access: self.access.clone(), closed: closed.clone() };
        let thread = std::thread::Builder::new()
            .name(format!("writer-{}", t.cauldron.name))
            .spawn(move || run(target, gate, ready_tx, jobs_rx))
            .map_err(|e| Refusal::new("store", format!("cannot start a writer thread: {e}")))?;
        let recovered = match ready_rx.recv() {
            Ok(Ok(r)) => r,
            Ok(Err(refused)) => {
                let _ = thread.join();
                return Err(refused);
            }
            Err(_) => {
                let _ = thread.join();
                return Err(Refusal::new("store", "the writer thread stopped while opening the cauldron"));
            }
        };
        let w = Arc::new(Writer {
            cauldron: t.cauldron.name.clone(),
            opened_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            recovered: recovered.clone(),
            jobs: Mutex::new(Some(jobs_tx)),
            thread: Mutex::new(Some(thread)),
            closed,
        });
        open.insert(t.cauldron.name.clone(), w.clone());
        drop(open);
        tracing::info!(cauldron = %t.cauldron.name, interrupted = %recovered["interrupted"], "opened a cauldron for writing");
        self.audit.write(Line {
            client: Some(who.client.clone()),
            session: who.session.clone(),
            cauldron: Some(t.cauldron.name.clone()),
            tool: "open".into(),
            outcome: "opened".into(),
            recovered: Some(recovered),
            ..Line::default()
        });
        Ok(w)
    }

    /// Close `cauldron`: refuse what is queued (`not_granted`), let the
    /// write in progress finish, drop the handle and the lock, join the
    /// thread. A no-op when it is not open.
    pub fn close(&self, cauldron: &str, reason: &str) {
        let w = self.open.lock().unwrap_or_else(|p| p.into_inner()).remove(cauldron);
        let Some(w) = w else { return };
        w.closed.store(true, Ordering::SeqCst);
        w.jobs.lock().unwrap_or_else(|p| p.into_inner()).take();
        if let Some(t) = w.thread.lock().unwrap_or_else(|p| p.into_inner()).take() {
            let _ = t.join();
        }
        tracing::info!(%cauldron, %reason, "closed a cauldron; its writer lock is free");
        self.audit.write(Line {
            cauldron: Some(cauldron.to_string()),
            tool: "close".into(),
            outcome: "closed".into(),
            reason: Some(reason.to_string()),
            ..Line::default()
        });
    }

    /// Close every open cauldron.
    pub fn close_all(&self, reason: &str) {
        for name in self.names() {
            self.close(&name, reason);
        }
    }
}

impl Writer {
    fn send(&self, job: Job) -> Result<(), Refusal> {
        let jobs = self.jobs.lock().unwrap_or_else(|p| p.into_inner());
        match jobs.as_ref() {
            Some(tx) => tx.send(job).map_err(|_| closed()),
            None => Err(closed()),
        }
    }
}

/// `not_granted`, for a call the grant no longer allows.
fn revoked(cauldron: &str, why: &str) -> Refusal {
    Refusal::new(
        "not_granted",
        format!(
            "cauldron `{cauldron}` is closed to this door: {why} before this call was applied. Nothing from this call was \
             written. The owner opens it again from a terminal with `cauldron mcp grant`."
        ),
    )
}

// ------------------------------------------------------------ the thread

/// What the thread checks before every write.
struct Gate {
    access: Arc<Access>,
    closed: Arc<AtomicBool>,
}

impl Gate {
    fn refusal(&self, cauldron: &env::Cauldron) -> Option<Refusal> {
        if self.closed.load(Ordering::SeqCst) {
            return Some(closed());
        }
        self.access.why_not_writable(cauldron).map(|why| revoked(&cauldron.name, why))
    }
}

fn run(t: Target, gate: Gate, ready: mpsc::Sender<Result<Value, Refusal>>, jobs: mpsc::Receiver<Job>) {
    let (cauldron, lock, report) = match open(&t) {
        Ok(x) => x,
        Err(r) => {
            let _ = ready.send(Err(r));
            return;
        }
    };
    if ready.send(Ok(report)).is_err() {
        return;
    }
    for job in jobs {
        // The grant, re-read immediately before this write is applied.
        let refused = gate.refusal(&t.cauldron);
        match job {
            Job::Put(p, who, reply) => {
                let _ = reply.send(match refused {
                    Some(r) => Err(r),
                    None => put(&cauldron, &t, &p, &who),
                });
            }
            Job::Link(l, who, reply) => {
                let _ = reply.send(match refused {
                    Some(r) => Err(r),
                    None => link(&cauldron, &t, &l, &who),
                });
            }
        }
    }
    drop(cauldron);
    drop(lock);
}

/// The three proofs, then the open, and what it recovered.
fn open(t: &Target) -> Result<(spine::Cauldron, File, Value), Refusal> {
    let c = &t.cauldron;
    soup_door::lock::refuse_live_soup(&t.env).map_err(refusal)?;
    let path = c.soup().join(soup_door::lock::WRITER_LOCK);
    let mut fh = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|e| Refusal::new("store", format!("opening {}: {e}", path.display())))?;
    match fh.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => {
            return Err(Refusal::new(
                "locked",
                format!(
                    "another writer (a `cauldron soup import` or `cauldron soup recover`) is writing to cauldron `{}`'s \
                     soup right now, so this door did not open it. Nothing was written. Try again when it has finished.",
                    c.name
                ),
            ))
        }
        Err(std::fs::TryLockError::Error(e)) => return Err(Refusal::new("store", format!("locking {}: {e}", path.display()))),
    }
    let _ = fh.set_len(0).and_then(|_| writeln!(fh, "mcp {}", std::process::id())).and_then(|_| fh.flush());
    let soup = c.soup();
    let cauldron = spine::Cauldron::open(&soup, spine::config::SpineConfig::default())
        .map_err(|e| Refusal::new("store", format!("opening cauldron `{}`'s soup: {e}", c.name)))?;
    let rec = cauldron.recovered_at_open();
    let hexes = |v: &[spine::envelope::TxHash]| v.iter().map(|h| h.to_hex()).collect::<Vec<_>>();
    let landed_tx: Vec<String> = hexes(&rec.rolled_forward).into_iter().chain(hexes(&rec.cleared_only)).collect();
    let rolled_back = rec.aborted.len() + rec.aborted_elsewhere.len();
    let landed = soup_door::recover::landed_nodes(&soup, &landed_tx).map_err(refusal)?;
    let interrupted = match (landed_tx.is_empty(), rolled_back) {
        (true, 0) => "none",
        (false, 0) => "landed",
        (true, _) => "rolled-back",
        (false, _) => "landed-and-rolled-back",
    };
    let mut report = json!({
        "at": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        "interrupted": interrupted,
        "rolled_forward": hexes(&rec.rolled_forward),
        "cleared": hexes(&rec.cleared_only),
        "aborted": hexes(&rec.aborted).into_iter().chain(hexes(&rec.aborted_elsewhere)).collect::<Vec<_>>(),
        "live_tranches": cauldron.tranches_at_open().tranches.len(),
    });
    if !landed_tx.is_empty() {
        report["landed_records"] = json!(landed.len());
        report["landed_ids"] = soup_door::recover::landed_summary(&landed);
    }
    Ok((cauldron, fh, report))
}

fn pool() -> PoolName {
    PoolName::new(DEFAULT_POOL)
}

fn cursor_after(c: &spine::Cauldron, t: &Target) -> Value {
    match c.committed_commit_counter() {
        Ok(n) => json!(crate::cursor::cursor(&t.cauldron, n)),
        Err(_) => Value::Null,
    }
}

// --------------------------------------------------------------- soup_put

/// Does `type` declare a `meta` field in the pool's pinned grammar?
fn has_meta(c: &spine::Cauldron, node_type: Option<&str>) -> bool {
    let (Some(g), Some(t)) = (c.grammar(&pool()), node_type) else { return false };
    g.node_types.get(t).is_some_and(|d| d.fields.contains_key("meta"))
}

/// Add `cite` to `meta.cites` of every record whose type has `meta`,
/// unless an equal citation is already there. `same` decides "equal".
fn cite_all(c: &spine::Cauldron, records: &[Value], cite: &Value, same: impl Fn(&Value) -> bool) -> Vec<Value> {
    records
        .iter()
        .map(|r| {
            let mut r = r.clone();
            if !has_meta(c, r.get("type").and_then(Value::as_str)) {
                return r;
            }
            let Some(obj) = r.as_object_mut() else { return r };
            let meta = obj.entry("meta").or_insert_with(|| json!({}));
            // A `meta` that is not an object is left as written: the
            // grammar refuses it and says why.
            let Some(meta) = meta.as_object_mut() else { return r };
            let cites = meta.entry("cites").or_insert_with(|| json!([]));
            let Some(list) = cites.as_array_mut() else { return r };
            if !list.iter().any(&same) {
                list.push(cite.clone());
            }
            r
        })
        .collect()
}

fn put(c: &spine::Cauldron, t: &Target, p: &Put, who: &Who) -> Result<Value, Refusal> {
    let pool = pool();
    let mut records = p.records.clone();
    if let Some(cite) = &p.cite {
        records = cite_all(c, &records, cite, |x| x == cite);
    }
    // The batch's identity: the records as asked, with the call's own
    // citation, BEFORE the session is cited, so the same batch sent again
    // from another session is still recognised as the same batch.
    let digest = import::batch_digest(&pool, &records);
    if let Some(s) = &who.session {
        let cite = json!({"kind": "session", "id": s});
        records = cite_all(c, &records, &cite, |x| x.get("kind") == Some(&json!("session")) && x.get("id") == Some(&json!(s)));
    }
    let opts = ImportOptions {
        pool: None,
        dry_run: p.dry_run,
        remint: p.remint,
        allow_duplicate: p.allow_duplicate,
        agent: who.thread.clone(),
        dispatch: "mcp soup_put".into(),
    };
    if !p.allow_duplicate {
        let earlier = import::landed_before(&t.soup(), &t.cauldron.name, &digest).map_err(refusal)?;
        if !earlier.is_empty() {
            let ImportOutcome::AlreadyLanded { mut doc, why } = import::already_landed(t, &pool, &digest, &opts, earlier) else {
                unreachable!("already_landed answers AlreadyLanded")
            };
            let why = mcp_words(&why);
            doc["why"] = json!(why);
            return Err(Refusal::with_detail("batch_already_landed", why, doc));
        }
    }
    let judge = |records: &[Value]| -> Result<(Value, bool), Refusal> {
        let mut o = opts.clone();
        o.dry_run = true;
        let (mut doc, ok) = import::submit(c, t, records, &o, &digest).map_err(refusal)?;
        doc["judged_on"] = json!("the soup as it stands, validated without landing; nothing was written");
        Ok((doc, ok))
    };
    let land = |records: &[Value]| -> Result<(Value, bool), Refusal> {
        let mut o = opts.clone();
        o.dry_run = false;
        import::submit(c, t, records, &o, &digest).map_err(refusal)
    };
    if p.dry_run {
        return judge(&records).map(|(doc, _)| doc);
    }
    if !p.partial {
        let (mut doc, ok) = land(&records)?;
        if !ok {
            let n = doc["refused"].as_u64().unwrap_or(0);
            return Err(Refusal::with_detail(
                "records_refused",
                format!(
                    "{n} of {} record(s) were refused by the soup's grammar, so nothing was written (all or nothing). \
                     `detail.records` names each refused record and why; fix those, or call again with `partial: true` \
                     to land the rest.",
                    records.len()
                ),
                doc,
            ));
        }
        doc["cursor"] = cursor_after(c, t);
        return Ok(doc);
    }
    // PARTIAL: judge the whole batch once; drop what was refused (a record
    // whose edge points at a refused record of the batch is refused too, as
    // `target-refused`); land the rest, judged again, all or nothing.
    let (first, ok) = judge(&records)?;
    if ok {
        let (mut doc, ok) = land(&records)?;
        if !ok {
            return Err(Refusal::with_detail("records_refused", "the batch passed its judgment and was refused when it landed; nothing was written", doc));
        }
        doc["partial"] = json!(true);
        doc["dropped"] = json!([]);
        doc["cursor"] = cursor_after(c, t);
        return Ok(doc);
    }
    let dropped: Vec<Value> = first["records"]
        .as_array()
        .map(|rs| rs.iter().filter(|r| r["disposition"] != "accepted").cloned().collect())
        .unwrap_or_default();
    let refused_ids: BTreeSet<String> = dropped.iter().filter_map(|r| r["id"].as_str().map(str::to_string)).collect();
    let kept: Vec<Value> = records
        .iter()
        .filter(|r| !r.get("id").and_then(Value::as_str).is_some_and(|id| refused_ids.contains(id)))
        .cloned()
        .collect();
    if kept.is_empty() || kept.len() == records.len() {
        return Err(Refusal::with_detail(
            "records_refused",
            format!("every one of the {} record(s) was refused, so nothing was written.", records.len()),
            first,
        ));
    }
    let (mut doc, ok) = land(&kept)?;
    if !ok {
        return Err(Refusal::with_detail(
            "records_refused",
            format!(
                "after the {} refused record(s) were dropped, the rest was refused when judged again, so nothing was written.",
                dropped.len()
            ),
            json!({"first": first, "second": doc}),
        ));
    }
    doc["partial"] = json!(true);
    doc["dropped"] = json!(dropped);
    doc["cursor"] = cursor_after(c, t);
    Ok(doc)
}

/// The import's sentence, in this door's words rather than the CLI's flags.
fn mcp_words(why: &str) -> String {
    why.replace(
        "If a second copy is really wanted, run the same command with --allow-duplicate.",
        "If a second copy is really wanted, call soup_put again with `allow_duplicate: true`.",
    )
    .replace("--remint", "`remint`")
}

// -------------------------------------------------------------- soup_link

fn link(c: &spine::Cauldron, t: &Target, links: &[Link], who: &Who) -> Result<Value, Refusal> {
    let pool = pool();
    let grammar = c.grammar(&pool).ok_or_else(|| Refusal::new("store", "the soup has no `ws` pool"))?.clone();
    let handle = Pool::open_read_only(&Pool::path_in(&t.soup(), &pool)).map_err(|e| Refusal::new("store", e.to_string()))?;
    let snap = handle.snapshot().map_err(|e| Refusal::new("store", e.to_string()))?;
    let type_of = |id: &str| -> Result<Option<String>, Refusal> {
        Ok(snap.node(&NodeId(id.to_string())).map_err(|e| Refusal::new("store", e.to_string()))?.map(|n| n.node_type))
    };
    let mut answers = Vec::new();
    let mut types: BTreeMap<String, String> = BTreeMap::new();
    let mut any_refused = false;
    for l in links {
        let mut why: Option<Value> = None;
        for end in [&l.from, &l.to] {
            if why.is_some() || types.contains_key(end) {
                continue;
            }
            match type_of(end)? {
                Some(ty) => {
                    types.insert(end.clone(), ty);
                }
                None => why = Some(json!({"reason": "unknown-node", "detail": format!("`{end}` is not a node in cauldron `{}`", t.cauldron.name)})),
            }
        }
        if why.is_none() && !grammar.edge_kinds.contains_key(&l.kind) {
            why = Some(json!({"reason": "unknown-kind", "detail": format!("`{}` is not an edge kind of this soup's grammar", l.kind)}));
        }
        if why.is_none() {
            let env = envelope(std::slice::from_ref(l), &types, who, Mode::Validate);
            match c.submit(env) {
                Ok(_) => {}
                Err(spine::error::SpineError::Refused { witnesses }) => {
                    why = Some(json!({"reason": "door-refused", "witnesses": witnesses}));
                }
                Err(e) => return Err(Refusal::new("store", format!("judging a link: {e}"))),
            }
        }
        let mut a = json!({"from": l.from, "kind": l.kind, "to": l.to});
        match why {
            Some(w) => {
                any_refused = true;
                a["status"] = json!("refused");
                a["refused"] = w;
            }
            None => a["status"] = json!("accepted"),
        }
        answers.push(a);
    }
    let mut doc = json!({"env": t.env.name, "cauldron": t.cauldron.name, "pool": pool.as_str(), "links": answers});
    if any_refused {
        doc["outcome"] = json!("refused");
        let n = doc["links"].as_array().map(|a| a.iter().filter(|x| x["status"] == "refused").count()).unwrap_or(0);
        return Err(Refusal::with_detail(
            "links_refused",
            format!("{n} of {} link(s) were refused, so none was written (all or nothing); `detail.links` says why.", links.len()),
            doc,
        ));
    }
    match c.submit(envelope(links, &types, who, Mode::Commit)) {
        Ok(spine::Outcome::Committed { tx, seq, deduplicated, .. }) => {
            for a in doc["links"].as_array_mut().into_iter().flatten() {
                a["status"] = json!("linked");
            }
            doc["outcome"] = json!("linked");
            doc["landed"] = json!({"tx": tx.to_hex(), "seq": seq, "deduplicated": deduplicated});
            doc["cursor"] = cursor_after(c, t);
            Ok(doc)
        }
        Ok(spine::Outcome::Validated { .. }) => Err(Refusal::new("store", "the link statement was validated and not committed")),
        Err(spine::error::SpineError::Refused { witnesses }) => {
            doc["outcome"] = json!("refused");
            doc["witnesses"] = json!(witnesses);
            Err(Refusal::with_detail("links_refused", "every link passed alone and the whole set was refused; nothing was written", doc))
        }
        Err(e) => Err(Refusal::new("store", format!("writing the links: {e}"))),
    }
}

/// One statement: a MATCH (pinned to exactly one node) per distinct
/// endpoint, then a CREATE per link. Values ride as parameters, never in
/// the text; the type and kind names spliced in are the grammar's own.
fn envelope(links: &[Link], types: &BTreeMap<String, String>, who: &Who, mode: Mode) -> Envelope {
    let mut binding: BTreeMap<&str, String> = BTreeMap::new();
    let mut lines = Vec::new();
    let mut expects = Vec::new();
    let mut creates = Vec::new();
    let mut params = BTreeMap::new();
    for l in links {
        for end in [&l.from, &l.to] {
            if !binding.contains_key(end.as_str()) {
                let b = format!("n{}", binding.len());
                lines.push(format!("MATCH ({b}: {}/{} {{ id: $id_{b} }})", DEFAULT_POOL, types[end]));
                expects.push(format!("EXPECT COUNT({b}) = 1"));
                params.insert(format!("id_{b}"), json!(end));
                binding.insert(end, b);
            }
        }
    }
    for (i, l) in links.iter().enumerate() {
        let mut line = format!("CREATE ({}) -[{}]-> ({})", binding[l.from.as_str()], l.kind, binding[l.to.as_str()]);
        if let Some(n) = &l.note {
            line.push_str(&format!(" NOTE $note_{i}"));
            params.insert(format!("note_{i}"), json!(n));
        }
        creates.push(line);
    }
    let statement = lines.into_iter().chain(expects).chain(creates).collect::<Vec<_>>().join("\n");
    Envelope {
        statement,
        bindings: params,
        format_version: ENVELOPE_FORMAT_VERSION,
        provenance: Provenance {
            initiator: Initiator::Agent { thread: who.thread.clone(), dispatch: "mcp soup_link".into() },
            instrument: Instrument { name: "cauldron-mcp".into(), version: env!("CARGO_PKG_VERSION").into() },
        },
        mode,
        idempotency_nonce: Some(format!(
            "mcp-link:{}:{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
        )),
    }
}
