//! **The environment's real soups, as the Cauldron tab's data.**
//!
//! Before this module the tab read one thing: a seed corpus
//! (`<env>/tui/soup/seed/clusters.json`, the older workspace format, schema
//! version 5), which only `cauldron-build` carries. The soup that `cauldron
//! soup`, the MCP door and the ripples read and write lives elsewhere
//! (`<env>/cauldrons/<name>/soup`, the published `workspace@6` grammar and the
//! `cauldron@1` cluster pool), and the tab never opened it. So a cauldron made
//! with `cauldron create`, filled from Claude Code or by a ripple, showed an
//! empty tab.
//!
//! [`read`] closes that gap, READ-ONLY: for an environment with no seed, it
//! opens every cauldron of the environment that has a soup, through
//! `soup-door`'s offline reads (the ones `cauldron soup regions|nodes|get`
//! print), and maps what it finds onto the shapes the tab already draws:
//!
//! - each cauldron is one root cluster, named after it, claiming every region
//!   of its soup (so no region is an orphan);
//! - each `cauldron@1` cluster is a child of that root, its handle the name,
//!   its boundary the description, its `claims` the regions it claims;
//! - each region and node is a corpus region and node (`narrows` and
//!   `in_region` as the corpus has them).
//!
//! # Ids
//!
//! A cluster's id in the tab is a minted `X-<n>`; a soup's cluster is
//! `CL-<n>`. A bridged cluster is `X-<LIVE_BASE + i * PER_CAULDRON + n>`,
//! where `i` is its cauldron's place by name; the cauldron's root is `n = 0`.
//! [`crate::cauldron::store::mint_id`] ignores ids at or above [`LIVE_BASE`],
//! so a cluster made in the face never takes one.
//!
//! A soup's region and node ids are unique in that soup only. With one
//! cauldron they are shown as written (`R-3`, `DEC-12`). With several, each
//! carries its cauldron as a suffix (`R-3.kb`), so two soups' `R-1` stay two
//! regions.
//!
//! # What it never does
//!
//! It writes nothing. It never stops the face opening: a soup that cannot be
//! read, a record the tab's grammar refuses, or a list longer than
//! [`NODE_LIMIT`] is skipped and said in a note (the face's stale notice).
//! Rejected and superseded records are left out, as the soup's own reads
//! leave them out. Questions, tasks and human actions in the tab's own sense
//! (serials, options, steps) are not derived from soup records here: a soup
//! `question` or `task` is shown as a node under its region.

use std::collections::HashSet;
use std::path::Path;

use serde_json::Value;
use soup_door::reads::ReadOpts;

use crate::cauldron::corpus::{CorpusSpec, NodeSpec, RegionSpec, CURRENT_SCHEMA};
use crate::cauldron::model::ClusterSpec;

/// The first cluster id a bridged cluster may take.
pub const LIVE_BASE: u32 = 1_000_000_000;
/// The ids one cauldron's clusters take: its root, then `CL-1` and up.
pub const PER_CAULDRON: u32 = 1_000_000;
/// How many cauldrons of one environment are bridged.
pub const CAULDRON_LIMIT: usize = 1_000;
/// How many nodes of one soup are read.
pub const NODE_LIMIT: usize = 2_000;
/// How long a node's body is shown before it is cut.
const BODY_CHARS: usize = 4_000;
/// How many nodes one `get` asks for.
const GET_CHUNK: usize = 40;

/// What [`read`] found.
#[derive(Debug, Default)]
pub struct Live {
    pub clusters: Vec<ClusterSpec>,
    pub corpus: CorpusSpec,
    /// What was skipped, and why: shown in the face's stale notice.
    pub notes: Vec<String>,
}

impl Live {
    pub fn is_empty(&self) -> bool {
        self.clusters.is_empty()
    }
}

/// The corpus's id grammar ([`crate::cauldron::corpus`]'s `is_id`).
fn is_id(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn text(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or_default().to_string()
}

fn strings(v: &Value, key: &str) -> Vec<String> {
    v.get(key).and_then(Value::as_array).into_iter().flatten().filter_map(|x| x.as_str().map(str::to_string)).collect()
}

/// The soups of environment `name`, whose face keeps its own files in
/// `dir` (`<data_root>/tui/soup`). `None` when `dir` is not that
/// environment's (a test's scratch directory, a fixture) or it has no
/// cauldron with a soup.
pub fn read(name: &str, dir: &Path) -> Option<Live> {
    let env = env::Environment::resolve().ok()?.for_name(name).ok()?;
    if crate::cauldron::store::soup_dir(&env).ok()?.as_path() != dir {
        return None;
    }
    let mut names: Vec<String> = env.cauldrons().ok()?.into_iter().filter(|c| !c.is_legacy()).map(|c| c.name).collect();
    names.sort();
    let mut live = Live::default();
    let targets: Vec<soup_door::Target> = names.iter().filter_map(|n| soup_door::target::named(name, n).ok()).collect();
    if targets.is_empty() {
        return None;
    }
    if targets.len() > CAULDRON_LIMIT {
        live.notes.push(format!("environment `{name}` has {} cauldrons with a soup; the first {CAULDRON_LIMIT} by name are shown", targets.len()));
    }
    let several = targets.len() > 1;
    live.corpus.schema_version = CURRENT_SCHEMA;
    for (i, t) in targets.iter().take(CAULDRON_LIMIT).enumerate() {
        one(t, i as u32, several, &mut live);
    }
    (!live.is_empty()).then_some(live)
}

/// One cauldron's soup, added to `live`.
fn one(t: &soup_door::Target, index: u32, several: bool, live: &mut Live) {
    let cauldron = t.cauldron.name.clone();
    let q = |id: &str| if several { format!("{id}.{cauldron}") } else { id.to_string() };
    let base = LIVE_BASE + index * PER_CAULDRON;

    // Regions.
    let regions = match soup_door::reads::regions(t, &ReadOpts::default()) {
        Ok(v) => v,
        Err(e) => {
            live.notes.push(format!("cauldron `{cauldron}`'s soup could not be read: {e}"));
            return;
        }
    };
    let mut held: HashSet<String> = HashSet::new();
    let mut specs = Vec::new();
    for r in regions.get("regions").and_then(Value::as_array).into_iter().flatten() {
        let id = q(&text(r, "id"));
        if !is_id(&id) {
            live.notes.push(format!("cauldron `{cauldron}`: region `{id}` is not shown (its id is outside the tab's id grammar)"));
            continue;
        }
        held.insert(id.clone());
        specs.push((id, text(r, "handle"), text(r, "boundary"), strings(r, "narrows")));
    }
    for (id, handle, boundary, narrows) in specs {
        let narrows = narrows.iter().map(|n| q(n)).filter(|n| held.contains(n) && *n != id).collect();
        live.corpus.regions.push(RegionSpec { id, handle, boundary, schema_version: None, narrows });
    }

    // Nodes, newest first, then their bodies.
    let opts = ReadOpts { limit: Some(NODE_LIMIT), ..ReadOpts::default() };
    let mut nodes: Vec<NodeSpec> = Vec::new();
    match soup_door::reads::nodes(t, &opts) {
        Ok(v) => {
            if let Some(more) = v.get("more").and_then(Value::as_u64).filter(|m| *m > 0) {
                live.notes.push(format!("cauldron `{cauldron}`: its {NODE_LIMIT} newest nodes are shown, {more} older ones are not"));
            }
            let mut unplaced = 0usize;
            for n in v.get("nodes").and_then(Value::as_array).into_iter().flatten() {
                let id = q(&text(n, "id"));
                let in_region: Vec<String> = strings(n, "regions").iter().map(|r| q(r)).filter(|r| held.contains(r)).collect();
                if !is_id(&id) {
                    live.notes.push(format!("cauldron `{cauldron}`: node `{id}` is not shown (its id is outside the tab's id grammar)"));
                    continue;
                }
                if in_region.is_empty() {
                    // The tab reaches a node through a region, and its corpus
                    // refuses one with none.
                    unplaced += 1;
                    continue;
                }
                nodes.push(NodeSpec { id, kind: text(n, "type"), handle: text(n, "handle"), state: text(n, "state"), body: String::new(), in_region, cite: None });
            }
            if unplaced > 0 {
                live.notes.push(format!("cauldron `{cauldron}`: {unplaced} node(s) in no region are not shown (`cauldron soup nodes` lists them)"));
            }
        }
        Err(e) => live.notes.push(format!("cauldron `{cauldron}`: its nodes could not be read: {e}")),
    }
    bodies(t, several, &cauldron, &mut nodes);
    live.corpus.nodes.extend(nodes);

    // Clusters: the cauldron's root, then each `cauldron@1` cluster under it.
    let mut root = ClusterSpec::bare(&format!("X-{base}"), &cauldron);
    root.description = format!("The soup of cauldron `{cauldron}`, read from its own store. Read-only here: write to it with `cauldron soup`, from Claude Code, or by a ripple.");
    root.regions = {
        let mut all: Vec<String> = held.iter().cloned().collect();
        all.sort();
        all
    };
    match soup_door::clusters::listed(&t.cauldron) {
        Ok(listed) => {
            // `listed` is newest first; the tab reads oldest first.
            for c in listed.into_iter().rev() {
                let serial = soup_door::reads::serial(&c.id);
                if serial == 0 || serial >= u64::from(PER_CAULDRON) {
                    live.notes.push(format!("cauldron `{cauldron}`: cluster `{}` is not shown (its serial is outside 1..{PER_CAULDRON})", c.id));
                    continue;
                }
                let mut spec = ClusterSpec::bare(&format!("X-{}", base + serial as u32), &c.handle);
                spec.description = c.boundary;
                spec.regions = c.regions.iter().map(|r| q(r)).filter(|r| held.contains(r)).collect();
                root.children.push(spec);
            }
        }
        Err(e) => live.notes.push(format!("cauldron `{cauldron}`: its clusters could not be read: {e}")),
    }
    live.clusters.push(root);
}

/// Fill each node's body from the soup's own `get`, a chunk at a time. A
/// chunk that cannot be read leaves its nodes with a handle and no body.
fn bodies(t: &soup_door::Target, several: bool, cauldron: &str, nodes: &mut [NodeSpec]) {
    let opts = ReadOpts { max_chars: Some(BODY_CHARS), ..ReadOpts::default() };
    let raw = |shown: &str| if several { shown.strip_suffix(&format!(".{cauldron}")).unwrap_or(shown).to_string() } else { shown.to_string() };
    for chunk in nodes.chunks_mut(GET_CHUNK) {
        let ids: Vec<String> = chunk.iter().map(|n| raw(&n.id)).collect();
        let Ok(v) = soup_door::reads::get(t, &ids, &opts) else { continue };
        for rec in v.get("records").and_then(Value::as_array).into_iter().flatten() {
            let id = text(rec, "id");
            if let Some(n) = chunk.iter_mut().find(|n| raw(&n.id) == id) {
                n.body = text(rec, "body");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_grammar_matches_the_corpus() {
        assert!(is_id("R-3"));
        assert!(is_id("R-3.kb"));
        assert!(is_id("DEC-12.my_soup"));
        assert!(!is_id(""));
        assert!(!is_id("R 3"));
        assert!(!is_id("R-3@kb"));
    }

    #[test]
    fn a_directory_that_is_no_environments_is_not_bridged() {
        let scratch = std::env::temp_dir().join("livesoup-not-an-env");
        assert!(read("no-such-environment-for-livesoup", &scratch).is_none());
    }

    #[test]
    fn bridged_ids_never_collide_with_minted_ones() {
        // A face mints `X-<max + 1>` over the ids below LIVE_BASE only.
        let keys = ["X-1", "X-7", "X-1000000000", "X-1000000042"];
        assert_eq!(crate::cauldron::store::mint_id(keys), "X-8");
    }
}
