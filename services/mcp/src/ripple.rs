//! **The ripple seam**: the verbs only the `ripples` service may call, on
//! this service's cauldron seam (the `CAULDRON_BIND` line protocol).
//!
//! The `ripples` service never opens a cauldron: this service is the one
//! writer of every granted cauldron, and a ripple reads and writes through
//! it.
//!
//! # Who is calling: the ripples key
//!
//! At every start this service mints a second secret beside the door's,
//! `<data root>/mcp/ripples.secret` (32 random bytes as hex, mode 0600, in
//! the 0700 directory; `crate::secret`). Every ripple verb carries it as its
//! first word, and a line without it, or with any other value, is refused as
//! `unauthorized` before anything is read or written. Only a process that can
//! read this user's private files holds it: the same trust the door secret
//! gives. The stdio bridge never reads it, and the door's tools (`POST
//! /mcp`) never accept it, so an MCP client cannot reach these verbs through
//! the door; a process of this user that reads the file directly can, as it
//! could read the cauldron's files themselves.
//!
//! # Provenance
//!
//! A record a ripple lands is written under the initiator
//! `{"class":"ripple","kind":"type_two","run_id","step_id","scope":[]}`,
//! minted HERE: the class, the kind and the empty scope are this module's
//! constants, and only the run and step come from the request (they are what
//! a run is known by). Nothing in a record can change it: a record carrying
//! `provenance` is refused, and `meta.ripple` (which a reader sees) is
//! overwritten from the same request fields. With an empty scope, the
//! spine's flag rule refuses the ripple every flag, `meta.approved`
//! included.
//!
//! # The verbs (one line in: `<VERB> <key> <json>`; one JSON line out)
//!
//! - `RDUMP {cauldron, dump}`: the dump's summary and, for a text dump, its
//!   text. A session dump's text is read through `RTEXT` only.
//! - `RTEXT {cauldron, dump, offset?, turn_offset?, max_chars?}`: one page of
//!   a session dump's text view (user and assistant text only).
//! - `RREAD {cauldron, query?, limit?}`: the regions, each with its head
//!   revision (`rev`, `spine::regions::region_head_rev`: its boundary and
//!   identity, never its members), `revs` (every region's), and the
//!   existing nodes a search for `query` finds (at most `limit`, default
//!   20).
//! - `RRUN {cauldron, run}`: whether run `run` already landed, and what.
//! - `RPUT {cauldron, dump, run, step, package, version, model, records}`:
//!   the batch held to the stage's limits (`soup_door::ripple`), stamped,
//!   judged without landing, then landed as ONE write (one spine
//!   transaction, all or nothing) on the cauldron's writer thread, which is
//!   the only writer, so nothing else writes the soup between the two;
//!   then the dump is marked `processed` in its own write, and the run's
//!   region locks are let go. In that one act on the writer thread, before
//!   anything is written, the regions the records go into (and their
//!   ancestors, `link`) are taken for the run: one another holder holds
//!   refuses the put `region_locked`, nothing written, whether or not the
//!   run declared it. So no record lands in a region the run did not hold
//!   at the moment it landed. With `revs` (the head revisions the run read,
//!   from `RLOCK` or `RREAD`), every region a record is placed in, and every
//!   region the run holds `write`, must still be at the head revision the
//!   run read, or the put is refused `region_changed` (which regions, read
//!   and now), nothing written: the run re-plans. A member added to, moved
//!   in or out of, or superseded in a region never moves its head revision:
//!   the model is never shown a member. An empty batch
//!   (the model found nothing worth recording) marks the dump and lands
//!   nothing. A run that
//!   already landed is never landed again: the dump is marked and the
//!   records it landed are answered.
//! - `RLOCK {cauldron, run, dump?, regions, ttl_secs?}`: hold `regions`
//!   for `run` (a row each in the soup's region-lock table, `write`, and
//!   `link` on each of their ancestors) for `ttl_secs` (default 120, at
//!   most 900), replacing what the run held before; answered with the
//!   lock and, read on the writer thread at the same moment, what `RREAD`
//!   answers (the regions and their revisions), which is what the run's
//!   prompt is built from. Refused `region_locked` (who holds what, since
//!   when, until when) when another holder holds one of them, changing
//!   nothing. Expired rows and rows of runs that landed are swept first.
//! - `RRENEW {cauldron, run, ttl_secs?}`: move the run's rows still held to
//!   `ttl_secs` from now; an expired row is never revived (`renewed: 0`).
//! - `RUNLOCK {cauldron, run}`: let go of every row of the run.
//! - `RCHECK {cauldron, run, step, package, version, model, records,
//!   dump?}`: a dry run of `RPUT`, for the ripples evaluation: the same
//!   limits, stamp and judgement (without landing, on the writer thread),
//!   answered `would_land` with how many records would land, or refused
//!   as `RPUT` would refuse them. Nothing is written and no dump is marked;
//!   the dump is optional (a fixture outside the soup has none).
//! - `RDUMPS {cauldron, after?, limit?}`: the dumps still `received` whose
//!   serial is above `after` (default 0), oldest first, at most `limit`
//!   (default 50, at most 500), as summaries (never text): what the
//!   service's automatic processing picks from.
//! - `RNOTE {cauldron, dump, run, auto}`: note on a `received` dump what
//!   automatic processing is doing with it (`meta.auto`: deferred and why,
//!   or retrying); its state does not change. Answers `noted` (`false` when
//!   the dump is no longer `received`).
//! - `RFAIL {cauldron, dump, run, failure}`: park a `received` dump
//!   `failed`, with `meta.failure` (the code, the message, the attempts);
//!   automatic processing gave up on it. It can still be run by hand.
//!
//! Every answer is `{"ok": true, ...}` or `{"ok": false, "code", "message",
//! "detail"?}`. Every call after the key is checked is a line in
//! `calls.jsonl` (client `ripples`), never with text.
//!
//! A cauldron must be granted `read`, `write` and `dump` (`Door::ripple_target`).

use serde_json::{json, Map, Value};
use shapewire::envelope::Refusal;
use soup_door::reads::{self, ReadOpts};
use soup_door::Target;
use spine::envelope::{Initiator, RippleKind};

use crate::audit::Line;
use crate::door::Door;
use crate::refusal;
use crate::secret::Secret;
use crate::writer::Who;

/// The verbs, in the order the seam lists them.
pub const VERBS: [&str; 12] = ["RDUMP", "RTEXT", "RREAD", "RRUN", "RPUT", "RCHECK", "RDUMPS", "RNOTE", "RFAIL", "RLOCK", "RRENEW", "RUNLOCK"];

/// The client name every ripple call is audited under.
pub const CLIENT: &str = "ripples";

/// The longest a run or step id may be.
const ID_MAX: usize = 128;

/// The initiator a ripple's write is landed under. The class, the kind and
/// the scope are fixed here.
pub fn initiator(run: &str, step: &str) -> Initiator {
    Initiator::Ripple { kind: RippleKind::TypeTwo, run_id: run.to_string(), step_id: step.to_string(), scope: vec![] }
}

fn fail(r: &Refusal) -> Value {
    let mut v = json!({"ok": false, "code": r.code, "message": r.message});
    if let Some(d) = &r.detail {
        v["detail"] = d.clone();
    }
    v
}

fn usage(msg: impl Into<String>) -> Refusal {
    Refusal::new("usage", format!("{}. Nothing was read or written.", msg.into()))
}

fn who() -> Who {
    Who { client: CLIENT.into(), thread: CLIENT.into(), session: None }
}

/// **Answer one ripple verb**: `rest` is `<key> <json>`. Always one JSON
/// line, never an error.
pub fn answer(door: &Door, key: &Secret, verb: &str, rest: &str) -> String {
    let (given, body) = rest.split_once(' ').unwrap_or((rest, ""));
    if given.is_empty() || !key.is(given) {
        tracing::warn!(verb, "a ripple verb without the ripples key was refused");
        return fail(&Refusal::new(
            "unauthorized",
            "the ripple verbs answer only the ripples service: the line must carry the key this service minted into \
             its directory (ripples.secret) at its last start. Nothing was read or written.",
        ))
        .to_string();
    }
    let args: Map<String, Value> = match serde_json::from_str::<Value>(body.trim()) {
        Ok(Value::Object(m)) => m,
        _ => return fail(&usage(format!("{verb} takes one JSON object after the key"))).to_string(),
    };
    let cauldron = args.get("cauldron").and_then(Value::as_str).unwrap_or("").to_string();
    let out = run(door, verb, &args);
    let mut line = Line {
        client: Some(CLIENT.into()),
        cauldron: (!cauldron.is_empty()).then_some(cauldron),
        tool: format!("ripple_{}", verb[1..].to_lowercase()),
        dump: args.get("dump").and_then(Value::as_str).map(str::to_string),
        ..Line::default()
    };
    let mut counts = Map::new();
    match &out {
        Ok(v) => {
            line.outcome = v.get("outcome").and_then(Value::as_str).unwrap_or("ok").to_string();
            line.tx = v.get("tx").and_then(Value::as_str).map(str::to_string);
            if let Some(r) = v.get("records").and_then(Value::as_array) {
                counts.insert("records".into(), json!(r.len()));
            }
            if let Some(r) = v.get("turns").and_then(Value::as_array) {
                counts.insert("turns".into(), json!(r.len()));
            }
        }
        Err(r) => line.outcome = r.code.to_string(),
    }
    line.counts = Value::Object(counts);
    door.audit(line);
    match out {
        Ok(mut v) => {
            v["ok"] = json!(true);
            v.to_string()
        }
        Err(r) => fail(&r).to_string(),
    }
}

fn s<'a>(a: &'a Map<String, Value>, k: &str) -> Result<&'a str, Refusal> {
    a.get(k).and_then(Value::as_str).filter(|v| !v.trim().is_empty()).ok_or_else(|| usage(format!("`{k}` must be a non-empty string")))
}

fn n(a: &Map<String, Value>, k: &str) -> Result<Option<u64>, Refusal> {
    match a.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_u64().map(Some).ok_or_else(|| usage(format!("`{k}` must be a whole number"))),
    }
}

fn plain_id(a: &Map<String, Value>, k: &str) -> Result<String, Refusal> {
    let v = s(a, k)?;
    if v.len() > ID_MAX || !v.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-')) {
        return Err(usage(format!("`{k}` is a plain id ([A-Za-z0-9._:-], at most {ID_MAX})")));
    }
    Ok(v.to_string())
}

fn run(door: &Door, verb: &str, a: &Map<String, Value>) -> Result<Value, Refusal> {
    let t = door.ripple_target(s(a, "cauldron")?)?;
    match verb {
        "RDUMP" => rdump(&t, a),
        "RTEXT" => rtext(&t, a),
        "RREAD" => rread(&t, a),
        "RRUN" => {
            let run = plain_id(a, "run")?;
            let (txs, records) = landed(&t, &run)?;
            Ok(json!({"run": run, "landed": !txs.is_empty(), "records": records, "txs": txs}))
        }
        "RPUT" => rput(door, &t, a),
        "RCHECK" => rcheck(door, &t, a),
        "RDUMPS" => rdumps(&t, a),
        "RNOTE" | "RFAIL" => rnote(door, &t, verb, a),
        "RLOCK" => rlock(door, &t, a),
        "RRENEW" => rrenew(door, &t, a),
        "RUNLOCK" => runlock(door, &t, a),
        other => Err(usage(format!("`{other}` is not a ripple verb"))),
    }
}

fn dump_id(a: &Map<String, Value>) -> Result<String, Refusal> {
    let id = s(a, "dump")?;
    if !soup_door::dumps::dump_id_ok(id) {
        return Err(usage(format!("`{id}` is not a dump id (DM-<n>)")));
    }
    Ok(id.to_string())
}

/// Is the dump being removed or removed (`removing` or `removed`)? Either
/// way its content is gone or going, and a ripple never reads or processes it.
fn removed(summary: &Value) -> bool {
    matches!(summary["state"].as_str(), Some(spine::grammar_dump::REMOVED) | Some(spine::grammar_dump::REMOVING))
}

fn rdump(t: &Target, a: &Map<String, Value>) -> Result<Value, Refusal> {
    let id = dump_id(a)?;
    let summary = soup_door::dumps::summary(&t.cauldron, &id).map_err(refusal)?;
    let created_at = t.cauldron.meta().ok().and_then(|m| m.created_at).unwrap_or_default();
    let mut v = json!({"env": t.env.name, "cauldron": t.cauldron.name, "cauldron_created_at": created_at, "dump": summary});
    if summary["kind"] == "text" && !removed(&summary) {
        v["text"] = json!(soup_door::dumps::text_of(&t.cauldron, &id).map_err(refusal)?);
    }
    Ok(v)
}

fn rtext(t: &Target, a: &Map<String, Value>) -> Result<Value, Refusal> {
    let id = dump_id(a)?;
    let max = n(a, "max_chars")?.unwrap_or(20_000) as usize;
    soup_door::dumps::check_page_max(Some(max)).map_err(refusal)?;
    let max = max.min(soup_door::budget::MAX_CHARS_CEILING);
    soup_door::dumps::get_text(&t.cauldron, &id, n(a, "offset")?.unwrap_or(0), n(a, "turn_offset")?.unwrap_or(0), max).map_err(refusal)
}

/// The regions a ripple may place records in (every region not retired),
/// each `{id, handle, boundary, rev}`, and every head revision.
fn catalogue(t: &Target) -> Result<(Vec<Value>, Value), Refusal> {
    let regions = reads::regions(t, &ReadOpts::default()).map_err(refusal)?;
    let revs = soup_door::region_lock::head_revisions(&t.cauldron).map_err(refusal)?;
    let regions: Vec<Value> = regions["regions"]
        .as_array()
        .map(|rs| {
            rs.iter()
                .filter(|r| r["flags"].get("retired").is_none_or(|x| x != &json!(true)))
                .map(|r| json!({"id": r["id"], "handle": r["handle"], "boundary": r["boundary"], "rev": revs.get(r["id"].as_str().unwrap_or("")).cloned()}))
                .collect()
        })
        .unwrap_or_default();
    Ok((regions, json!(revs)))
}

fn rread(t: &Target, a: &Map<String, Value>) -> Result<Value, Refusal> {
    let (regions, revs) = catalogue(t)?;
    let mut related = vec![];
    if let Some(q) = a.get("query").and_then(Value::as_str).filter(|q| !reads::search_terms(q).is_empty()) {
        let o = ReadOpts { limit: Some(n(a, "limit")?.unwrap_or(20).min(50) as usize), ..ReadOpts::default() };
        let found = reads::search(t, q, &o).map_err(refusal)?;
        related = found["results"]
            .as_array()
            .map(|rs| rs.iter().map(|r| json!({"id": r["id"], "type": r["type"], "state": r["state"], "handle": r["handle"], "snippet": r["snippet"]})).collect())
            .unwrap_or_default();
    }
    Ok(json!({"env": t.env.name, "cauldron": t.cauldron.name, "regions": regions, "revs": revs, "related": related}))
}

/// What run `run` landed: its transactions and the ids they created.
fn landed(t: &Target, run: &str) -> Result<(Vec<String>, Vec<String>), Refusal> {
    let txs = soup_door::ripple::run_landed(&t.cauldron, run).map_err(refusal)?;
    let ids = soup_door::ripple::created_by(&t.cauldron, &txs).map_err(refusal)?;
    Ok((txs, ids))
}

/// Mark the dump processed by `run` (its own write in the dumps store).
fn mark(door: &Door, t: &Target, dump: &str, run: &str, step: &str, records: &[String]) -> Result<bool, Refusal> {
    let processed = json!({
        "run": run,
        "records": records,
        "at": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    });
    door.dumps.with_store(t, &who(), |store| store.mark_processed(dump, processed, initiator(run, &format!("{step}:dump"))).map_err(refusal))
}

/// The records as the door lands them: `meta.cites` gains the dump,
/// `meta.ripple` is the request's, whatever the batch said.
fn stamp(records: &[Value], dump: &str, ripple: &Value) -> Vec<Value> {
    records
        .iter()
        .map(|r| {
            let mut r = r.clone();
            if let Some(o) = r.as_object_mut() {
                let meta = o.entry("meta").or_insert_with(|| json!({}));
                if let Some(m) = meta.as_object_mut() {
                    m.insert("ripple".into(), ripple.clone());
                    let cites = m.entry("cites").or_insert_with(|| json!([]));
                    if let Some(list) = cites.as_array_mut() {
                        let cite = json!({"kind": "dump", "id": dump});
                        if !list.iter().any(|c| c.get("kind") == cite.get("kind") && c.get("id") == cite.get("id")) {
                            list.push(cite);
                        }
                    }
                }
            }
            r
        })
        .collect()
}

fn rput(door: &Door, t: &Target, a: &Map<String, Value>) -> Result<Value, Refusal> {
    let dump = dump_id(a)?;
    let run = plain_id(a, "run")?;
    let step = plain_id(a, "step")?;
    let ripple = json!({
        "package": plain_id(a, "package")?,
        "version": plain_id(a, "version")?,
        "run": run,
        "model": plain_id(a, "model")?,
    });
    let records: Vec<Value> = match a.get("records") {
        Some(Value::Array(r)) => r.clone(),
        _ => return Err(usage("`records` must be a list")),
    };
    let summary = soup_door::dumps::summary(&t.cauldron, &dump).map_err(refusal)?;
    if removed(&summary) {
        return Err(Refusal::new("dump_removed", format!("dump `{dump}` was removed (or its removal is unfinished); it is never processed. Nothing was written.")));
    }
    let base = json!({"env": t.env.name, "cauldron": t.cauldron.name, "run": run, "dump": dump});
    // A run lands once: one that already landed is answered, never landed again.
    let (txs, ids) = landed(t, &run)?;
    if !txs.is_empty() {
        let _ = let_go(door, t, &run);
        let marked = mark(door, t, &dump, &run, &step, &ids)?;
        let mut v = base;
        v["outcome"] = json!("already_landed");
        v["records"] = json!(ids);
        v["txs"] = json!(txs);
        v["dump_marked"] = json!(marked);
        return Ok(v);
    }
    if summary["state"] == "processed" {
        return Err(Refusal::with_detail(
            "dump_processed",
            format!("dump `{dump}` was already processed (by another run). Nothing was written."),
            summary,
        ));
    }
    // An answer with nothing worth recording: the dump is processed, and
    // nothing lands.
    if records.is_empty() {
        let marked = mark(door, t, &dump, &run, &step, &[])?;
        let _ = let_go(door, t, &run);
        let mut v = base;
        v["outcome"] = json!("nothing_to_record");
        v["records"] = json!([]);
        v["dump_marked"] = json!(marked);
        return Ok(v);
    }
    let problems = soup_door::ripple::check_batch(&t.cauldron, &records).map_err(refusal)?;
    if !problems.is_empty() {
        return Err(Refusal::with_detail(
            "batch_refused",
            format!("{} problem(s) with the batch against what a ripple may write; nothing was written", problems.len()),
            json!({"problems": problems}),
        ));
    }
    let records = stamp(&records, &dump, &ripple);
    let read_revs: Option<Map<String, Value>> = match a.get("revs") {
        None | Some(Value::Null) => None,
        Some(Value::Object(m)) => Some(m.clone()),
        Some(_) => return Err(usage("`revs` is an object of region id to revision")),
    };
    let placed: Vec<String> = {
        let mut v: Vec<String> = records
            .iter()
            .flat_map(|r| r["edges"].as_array().cloned().unwrap_or_default())
            .filter(|e| e["kind"] == "in_region")
            .filter_map(|e| e["to"].as_str().map(str::to_string))
            .collect();
        v.sort();
        v.dedup();
        v
    };
    let (run2, step2) = (run.clone(), step.clone());
    let landed_doc = door.writers.with(
        t,
        who(),
        Box::new(move |c, t| {
            // Again on the writer thread: two puts of one run never both land.
            if !soup_door::ripple::run_landed(&t.cauldron, &run2).map_err(refusal)?.is_empty() {
                return Err(Refusal::new("run_landed", "this run landed while this put waited; nothing more was written"));
            }
            // Lock, then verify, then write, in this one act (this is the
            // one writer, so nothing else writes between the three): the
            // regions the records go into are taken for the run first, and
            // one another holder holds refuses the put, declared or not.
            let now_ms = env::region_locks::now_ms();
            let table = soup_door::region_lock::load(&t.cauldron).map_err(refusal)?;
            let above = soup_door::region_lock::ancestors(&t.cauldron, &placed).map_err(refusal)?;
            let mut wanted: Vec<env::region_locks::Want> = placed.iter().map(|r| (reads::DEFAULT_POOL.to_string(), r.clone(), env::region_locks::WRITE.to_string())).collect();
            wanted.extend(above.iter().map(|r| (reads::DEFAULT_POOL.to_string(), r.clone(), env::region_locks::LINK.to_string())));
            let others = soup_door::region_lock::conflicts(&t.cauldron, &table, now_ms, Some(&run2), &wanted).map_err(refusal)?;
            if !others.is_empty() {
                let mut detail = env::region_locks::refusal_detail(&others, now_ms);
                detail["at"] = json!("put");
                return Err(Refusal::with_detail("region_locked", env::region_locks::refusal_text(&others, now_ms), detail));
            }
            // The regions the run's plan depended on, at the head revision
            // the run read: every region a record goes into and every
            // region the run holds `write`. Their boundary and identity
            // only: the model is never shown a member, so a member's
            // change is not staleness.
            if let Some(read) = &read_revs {
                let now = soup_door::region_lock::head_revisions(&t.cauldron).map_err(refusal)?;
                let mut guarded: Vec<String> = placed.clone();
                for r in table.live(now_ms) {
                    if r.holder.run == run2 && r.mode == env::region_locks::WRITE && !guarded.contains(&r.region) {
                        guarded.push(r.region.clone());
                    }
                }
                guarded.sort();
                let changed: Vec<Value> = guarded
                    .iter()
                    .filter(|r| read.get(*r).and_then(Value::as_str) != now.get(*r).map(String::as_str))
                    .map(|r| json!({"region": r, "read": read.get(r), "now": now.get(r)}))
                    .collect();
                if !changed.is_empty() {
                    return Err(Refusal::with_detail(
                        "region_changed",
                        format!(
                            "{} of the region(s) this run planned against changed after it read them (a boundary or handle rewritten, the region retired, superseded, re-parented or given a child region); nothing was written. Read the regions again and plan again. A member's change never counts here.",
                            changed.len()
                        ),
                        json!({"changed": changed}),
                    ));
                }
            }
            // Judged without landing first, then landed: one write.
            for commit in [false, true] {
                match soup_door::ripple::land(c, &t.cauldron, &records, initiator(&run2, &step2), commit).map_err(refusal)? {
                    soup_door::ripple::Landed::Refused { witnesses } => {
                        return Err(Refusal::with_detail(
                            "records_refused",
                            format!(
                                "the soup's grammar refused the batch when {}; nothing was written. `detail.witnesses` says why.",
                                if commit { "it landed" } else { "judged without landing" }
                            ),
                            json!({"witnesses": witnesses}),
                        ))
                    }
                    soup_door::ripple::Landed::Ok { ids, tx } if commit => {
                        // Landed: the run's region locks go in the same act
                        // (a crash before this leaves rows the store already
                        // knows are done: they are swept).
                        fault_at("ripple-landed-held");
                        let released = release(&t.cauldron, &run2);
                        return Ok(json!({"id_map": ids, "tx": tx, "released": released}));
                    }
                    soup_door::ripple::Landed::Ok { .. } => {}
                }
            }
            unreachable!("the second pass lands or refuses")
        }),
    );
    let doc = match landed_doc {
        Err(r) if r.code == "run_landed" => return rput(door, t, a),
        other => other?,
    };
    fault_point();
    let mut ids: Vec<String> = doc["id_map"].as_object().map(|m| m.values().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default();
    ids.sort_by(|a, b| reads::prefix(a).cmp(reads::prefix(b)).then(reads::serial(a).cmp(&reads::serial(b))));
    let marked = mark(door, t, &dump, &run, &step, &ids)?;
    let mut v = base;
    v["outcome"] = json!("landed");
    v["locks_released"] = doc["released"].clone();
    v["records"] = json!(ids);
    v["id_map"] = doc["id_map"].clone();
    v["tx"] = doc["tx"].clone();
    v["dump_marked"] = json!(marked);
    Ok(v)
}

/// The longest a region lock is held before it must be renewed, and the
/// default.
const TTL_MAX: u64 = 900;
const TTL_DEFAULT: u64 = 120;

fn ttl_ms(a: &Map<String, Value>) -> Result<i64, Refusal> {
    let secs = n(a, "ttl_secs")?.unwrap_or(TTL_DEFAULT);
    if secs == 0 || secs > TTL_MAX {
        return Err(usage(format!("`ttl_secs` is 1 to {TTL_MAX}")));
    }
    Ok(secs as i64 * 1000)
}

/// Let go of `run`'s rows (on the writer thread); how many there were.
/// A table that cannot be written is logged: its rows expire.
fn release(c: &env::Cauldron, run: &str) -> usize {
    let Ok(mut table) = env::region_locks::load(c) else { return 0 };
    let n = table.release(run);
    if n > 0 {
        if let Err(e) = env::region_locks::save(c, &table) {
            tracing::warn!(cauldron = %c.name, error = %format!("{e:#}"), "the region locks of a run could not be let go; they expire");
        }
    }
    n
}

/// [`release`] as its own job on the writer thread.
fn let_go(door: &Door, t: &Target, run: &str) -> Result<Value, Refusal> {
    let run = run.to_string();
    door.writers.with(t, who(), Box::new(move |_c, t| Ok(json!(release(&t.cauldron, &run)))))
}

/// The table with expired rows and rows of landed runs swept (on the
/// writer thread).
fn swept(c: &env::Cauldron, now: i64) -> Result<(env::region_locks::Table, bool), Refusal> {
    let mut table = soup_door::region_lock::load(c).map_err(refusal)?;
    let landed = |run: &str| soup_door::ripple::run_landed(c, run).map(|t| !t.is_empty()).unwrap_or(false);
    let n = table.sweep(now, &landed);
    // A row read as cut back (too far ahead) is written back cut.
    let dirty = n > 0 || !table.clamped.is_empty();
    Ok((table, dirty))
}

/// **`RLOCK`** (module docs).
fn rlock(door: &Door, t: &Target, a: &Map<String, Value>) -> Result<Value, Refusal> {
    let run = plain_id(a, "run")?;
    let dump = match a.get("dump") {
        None | Some(Value::Null) => None,
        Some(_) => Some(dump_id(a)?),
    };
    let ttl = ttl_ms(a)?;
    let regions: Vec<String> = match a.get("regions") {
        Some(Value::Array(v)) if !v.is_empty() && v.len() <= 50 && v.iter().all(|x| x.as_str().is_some_and(|s| !s.is_empty() && s.len() <= 64)) => {
            let mut v: Vec<String> = v.iter().filter_map(Value::as_str).map(str::to_string).collect();
            v.sort();
            v.dedup();
            v
        }
        _ => return Err(usage("`regions` is a list of 1 to 50 region ids")),
    };
    door.writers.with(
        t,
        who(),
        Box::new(move |_c, t| {
            let now = env::region_locks::now_ms();
            let (regions_doc, revs) = catalogue(t)?;
            let known: Vec<&str> = regions_doc.iter().filter_map(|r| r["id"].as_str()).collect();
            if let Some(r) = regions.iter().find(|r| !known.contains(&r.as_str())) {
                return Err(usage(format!("`{r}` is not a region of cauldron `{}` (or it is retired)", t.cauldron.name)));
            }
            let above = soup_door::region_lock::ancestors(&t.cauldron, &regions).map_err(refusal)?;
            let (mut table, dirty) = swept(&t.cauldron, now)?;
            let holder = env::region_locks::Holder { kind: "ripple".into(), run: run.clone(), dump: dump.clone() };
            let purpose = match &dump {
                Some(d) => format!("placing the records drawn from dump {d}"),
                None => "placing a ripple's records".to_string(),
            };
            match table.acquire(&holder, &purpose, reads::DEFAULT_POOL, &regions, &above, now, ttl) {
                Ok(()) => {}
                Err(rows) => {
                    if dirty {
                        let _ = env::region_locks::save(&t.cauldron, &table);
                    }
                    let mut rows = rows;
                    let landed = |run: &str| soup_door::ripple::run_landed(&t.cauldron, run).map(|t| !t.is_empty()).unwrap_or(false);
                    rows.retain(|r| !landed(&r.holder.run));
                    return Err(Refusal::with_detail("region_locked", env::region_locks::refusal_text(&rows, now), env::region_locks::refusal_detail(&rows, now)));
                }
            }
            env::region_locks::save(&t.cauldron, &table).map_err(|e| Refusal::new("store", format!("writing the region locks: {e:#}; nothing is held")))?;
            let mine: Vec<&env::region_locks::Row> = table.rows.iter().filter(|r| r.holder.run == run).collect();
            Ok(json!({
                "env": t.env.name, "cauldron": t.cauldron.name, "run": run, "outcome": "locked",
                "lock": {"regions": regions, "above": above, "since": mine.first().map(|r| r.since.clone()), "until": mine.first().map(|r| r.until.clone()), "ttl_secs": ttl / 1000},
                "regions": regions_doc, "revs": revs,
            }))
        }),
    )
}

/// **`RRENEW`** (module docs).
fn rrenew(door: &Door, t: &Target, a: &Map<String, Value>) -> Result<Value, Refusal> {
    let run = plain_id(a, "run")?;
    let ttl = ttl_ms(a)?;
    door.writers.with(
        t,
        who(),
        Box::new(move |_c, t| {
            let now = env::region_locks::now_ms();
            let mut table = soup_door::region_lock::load(&t.cauldron).map_err(refusal)?;
            let n = table.renew(&run, now, ttl);
            if n > 0 || !table.clamped.is_empty() {
                env::region_locks::save(&t.cauldron, &table).map_err(|e| Refusal::new("store", format!("writing the region locks: {e:#}")))?;
            }
            let until = table.rows.iter().filter(|r| r.holder.run == run && r.live(now)).map(|r| r.until.clone()).max();
            Ok(json!({"env": t.env.name, "cauldron": t.cauldron.name, "run": run, "outcome": "renewed", "renewed": n, "until": until}))
        }),
    )
}

/// **`RUNLOCK`** (module docs).
fn runlock(door: &Door, t: &Target, a: &Map<String, Value>) -> Result<Value, Refusal> {
    let run = plain_id(a, "run")?;
    let n = let_go(door, t, &run)?;
    Ok(json!({"env": t.env.name, "cauldron": t.cauldron.name, "run": run, "outcome": "released", "released": n}))
}

/// **`RDUMPS`**: the `received` dumps above `after`, oldest first.
fn rdumps(t: &Target, a: &Map<String, Value>) -> Result<Value, Refusal> {
    let after = n(a, "after")?.unwrap_or(0);
    let limit = n(a, "limit")?.unwrap_or(50).clamp(1, 500) as usize;
    let o = soup_door::dumps::ListOpts { state: Some(spine::grammar_dump::RECEIVED.into()), kind: None, limit: Some(usize::MAX) };
    let doc = soup_door::dumps::list(&t.cauldron, &o).map_err(refusal)?;
    let mut v: Vec<Value> = doc["dumps"].as_array().cloned().unwrap_or_default().into_iter().filter(|d| env::ripples_auto::serial(d["id"].as_str().unwrap_or("")) > after).collect();
    v.sort_by_key(|d| env::ripples_auto::serial(d["id"].as_str().unwrap_or("")));
    let more = v.len() > limit;
    v.truncate(limit);
    Ok(json!({"env": t.env.name, "cauldron": t.cauldron.name, "dumps": v, "more": more}))
}

/// **`RNOTE`** and **`RFAIL`**: what automatic processing did with a dump
/// that waits, in the dumps store.
fn rnote(door: &Door, t: &Target, verb: &str, a: &Map<String, Value>) -> Result<Value, Refusal> {
    let dump = dump_id(a)?;
    let run = plain_id(a, "run")?;
    let (key, outcome) = if verb == "RFAIL" { ("failure", "parked") } else { ("auto", "noted") };
    let value = match a.get(key) {
        Some(Value::Object(m)) => {
            let text = Value::Object(m.clone()).to_string();
            if text.len() > 4096 {
                return Err(usage(format!("`{key}` is at most 4096 bytes of JSON")));
            }
            let mut m = m.clone();
            m.insert("run".into(), json!(run));
            m.insert("by".into(), json!("automatic"));
            m.insert("at".into(), json!(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)));
            Value::Object(m)
        }
        _ => return Err(usage(format!("`{key}` must be an object"))),
    };
    let summary = soup_door::dumps::summary(&t.cauldron, &dump).map_err(refusal)?;
    if removed(&summary) {
        return Err(Refusal::new("dump_removed", format!("dump `{dump}` was removed (or its removal is unfinished). Nothing was written.")));
    }
    let step = format!("auto:{}", key);
    let done = door.dumps.with_store(t, &who(), |store| {
        if verb == "RFAIL" {
            store.mark_failed(&dump, value, initiator(&run, &step)).map_err(refusal)
        } else {
            store.note_auto(&dump, value, initiator(&run, &step)).map_err(refusal)
        }
    })?;
    Ok(json!({"env": t.env.name, "cauldron": t.cauldron.name, "dump": dump, "run": run, "outcome": outcome, outcome: done}))
}

/// **`RCHECK`**: `RPUT` judged and never landed (module docs).
fn rcheck(door: &Door, t: &Target, a: &Map<String, Value>) -> Result<Value, Refusal> {
    let run = plain_id(a, "run")?;
    let step = plain_id(a, "step")?;
    let ripple = json!({
        "package": plain_id(a, "package")?,
        "version": plain_id(a, "version")?,
        "run": run,
        "model": plain_id(a, "model")?,
    });
    let records: Vec<Value> = match a.get("records") {
        Some(Value::Array(r)) => r.clone(),
        _ => return Err(usage("`records` must be a list")),
    };
    let base = json!({"env": t.env.name, "cauldron": t.cauldron.name, "run": run, "dry_run": true});
    if records.is_empty() {
        let mut v = base;
        v["outcome"] = json!("would_record_nothing");
        return Ok(v);
    }
    let problems = soup_door::ripple::check_batch(&t.cauldron, &records).map_err(refusal)?;
    if !problems.is_empty() {
        return Err(Refusal::with_detail(
            "batch_refused",
            format!("{} problem(s) with the batch against what a ripple may write; nothing was written (a dry run)", problems.len()),
            json!({"problems": problems}),
        ));
    }
    let records = match a.get("dump") {
        Some(_) => stamp(&records, &dump_id(a)?, &ripple),
        None => records
            .into_iter()
            .map(|mut r| {
                if let Some(o) = r.as_object_mut() {
                    let meta = o.entry("meta").or_insert_with(|| json!({}));
                    if let Some(m) = meta.as_object_mut() {
                        m.insert("ripple".into(), ripple.clone());
                    }
                }
                r
            })
            .collect(),
    };
    let n = records.len();
    door.writers.with(
        t,
        who(),
        Box::new(move |c, t| match soup_door::ripple::land(c, &t.cauldron, &records, initiator(&run, &step), false).map_err(refusal)? {
            soup_door::ripple::Landed::Refused { witnesses } => Err(Refusal::with_detail(
                "records_refused",
                "the soup's grammar refused the batch when judged without landing; nothing was written (a dry run). `detail.witnesses` says why.",
                json!({"witnesses": witnesses}),
            )),
            soup_door::ripple::Landed::Ok { .. } => Ok(Value::Null),
        }),
    )?;
    let mut v = base;
    v["outcome"] = json!("would_land");
    v["would_land"] = json!(n);
    Ok(v)
}

/// Test-only: with `--features fault-injection` and
/// `CAULDRON_MCP_FAULT=ripple-landed`, the process dies right after a
/// ripple's records landed and before its dump is marked processed.
fn fault_point() {
    fault_at("ripple-landed");
}

/// Test-only: with `--features fault-injection`, `CAULDRON_MCP_FAULT=<point>`
/// kills the process at `point` (`ripple-landed-held`: the records landed
/// and the run's region locks are still in the table).
fn fault_at(_point: &str) {
    #[cfg(feature = "fault-injection")]
    if std::env::var("CAULDRON_MCP_FAULT").as_deref() == Ok(_point) {
        std::process::abort();
    }
}
