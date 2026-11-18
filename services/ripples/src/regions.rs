//! **Which regions a run declares before its model call**, and the lock
//! it holds on them.
//!
//! A run reads the cauldron's regions, then holds the ones its dump's text
//! clearly points at (`RLOCK` on the `mcp` service: a row each in the soup's
//! region-lock table) from before its model call until its records land or
//! it gives up. A curator, or another run, writing into a held region is
//! refused at once, by name, with a retry-after; writes into every other
//! region proceed.
//!
//! # The declared set: [`declare`]
//!
//! No model is asked. Each region is scored against the dump's text:
//!
//! - the words compared are those of four letters or more, less a list of
//!   stop words ([`STOP`]), with a plural, `-ing` or `-ed` ending taken off
//!   and then cut to their first six letters; a region's words are those of
//!   its handle and of the inside of its boundary (the text before
//!   `Outside:`);
//! - each word the dump shares with a region scores by how rare it is among
//!   the regions: `ln((N + 1) / n) / ln(N + 1)`, where `N` is how many
//!   regions there are and `n` how many of them use the word. A word only
//!   one region uses scores 1; a word every region uses scores little (with
//!   4 regions, 0.14);
//! - a region is declared when its score is at least [`THRESHOLD`] (2: two
//!   words that region alone uses, or the like in commoner ones) and at least
//!   half the best region's score;
//! - at most [`MAX_DECLARED`] (4), and never more than half the regions
//!   (rounded down): with 4 regions at most 2, and a soup of one region is
//!   never declared ahead of the call.
//!
//! When nothing clears the threshold the run declares nothing and holds
//! nothing during its call.
//!
//! # What is held, and when
//!
//! - From before the model call until the records land or the run gives
//!   up: `write` on each declared region, `link` on each of their
//!   ancestors, renewed while the call is open.
//! - At the put, in the one act on the `mcp` service's writer thread that
//!   writes the records: every region the answer places a record in (and
//!   `link` on its ancestors), declared or not. Another holder's row there
//!   refuses the put `region_locked`; nothing lands in a region the run did
//!   not hold at that moment.
//!
//! # The hold budget
//!
//! Ripples may hold one region of a cauldron for at most
//! [`hold_budget_secs`] (900 s by default) in any hour, counted from this
//! service's journal. A run whose declared regions include one already held
//! that long is deferred (`hold_budget`) before it takes any lock or makes
//! any call, so no dump's text can keep a busy region held hour after hour.
//!
//! The model is still shown every region. A record it places in a region
//! is checked at the put against that region's head revision (its boundary
//! and identity, never its members, which the model is never shown), so the
//! declaration decides who waits during the call, never whether a record
//! lands on a boundary the run did not see.

use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// The most regions a run declares.
pub const MAX_DECLARED: usize = 4;

/// The score a region needs to be declared ([`declare`]).
pub const THRESHOLD: f64 = 2.0;

/// A lock's time to live: `CAULDRON_RIPPLES_LOCK_TTL_S` (1 to 900), 120 s by
/// default. A run renews it while its model call is open, every third of it.
pub fn ttl_secs() -> u64 {
    std::env::var("CAULDRON_RIPPLES_LOCK_TTL_S").ok().and_then(|v| v.parse().ok()).filter(|n| (1..=900).contains(n)).unwrap_or(120)
}

/// How long ripples may hold one region of a cauldron in any hour:
/// `CAULDRON_RIPPLES_REGION_HOLD_S` (1 to 3600), 900 s by default.
pub fn hold_budget_secs() -> u64 {
    std::env::var("CAULDRON_RIPPLES_REGION_HOLD_S").ok().and_then(|v| v.parse().ok()).filter(|n| (1..=3600).contains(n)).unwrap_or(900)
}

/// The words never compared: common English, and the words every region
/// file uses about itself.
pub const STOP: &[&str] = &[
    "about", "above", "after", "again", "also", "anything", "because", "been", "before", "being", "below", "between", "both", "could", "does", "each", "every",
    "from", "goes", "have", "here", "inside", "into", "just", "more", "most", "much", "must", "only", "other", "outside", "over", "same", "should", "some",
    "such", "than", "that", "their", "them", "then", "there", "these", "they", "this", "those", "through", "under", "very", "what", "when", "where", "which",
    "while", "will", "with", "would", "your", "region", "regions", "project", "projects", "thing", "things", "something", "everything", "nothing", "like",
    "want", "need", "needs", "make", "makes", "made", "done", "doing", "used", "uses", "using", "well", "good", "also", "still", "even", "many", "where",
    "whose", "whom", "yours", "ours", "mine", "its", "it's", "isn't", "don't", "doesn't", "can't", "won't",
];

/// The compared form of one word, or `None` for a word not compared.
fn stem(w: &str) -> Option<String> {
    let w = w.to_lowercase();
    let w = w.strip_suffix("'s").unwrap_or(&w).to_string();
    if w.chars().count() < 4 || STOP.contains(&w.as_str()) || w.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let mut base = w.clone();
    for suffix in ["ings", "ing", "ies", "ied", "ed", "es", "s"] {
        if let Some(b) = w.strip_suffix(suffix) {
            if b.chars().count() >= 4 {
                base = if suffix == "ies" || suffix == "ied" { format!("{b}y") } else { b.to_string() };
                break;
            }
        }
    }
    Some(base.chars().take(6).collect())
}

/// The compared forms of every word of `text` worth comparing.
pub fn words(text: &str) -> BTreeSet<String> {
    text.split(|c: char| !(c.is_alphanumeric() || c == '\'')).filter_map(stem).collect()
}

/// What of a region is compared: its handle and the inside of its
/// boundary.
fn region_words(r: &Value) -> BTreeSet<String> {
    let handle = r["handle"].as_str().unwrap_or("");
    let boundary = r["boundary"].as_str().unwrap_or("");
    let inside = match boundary.to_lowercase().find("outside:") {
        Some(i) => &boundary[..i],
        None => boundary,
    };
    words(&format!("{handle} {inside}"))
}

/// **Every region's score** for `text` (module docs): `(id, score, shared
/// words)`, in the order of `regions`.
pub fn scores(text: &str, regions: &[Value]) -> Vec<(String, f64, Vec<String>)> {
    let mine = words(text);
    let each: Vec<(String, BTreeSet<String>)> = regions.iter().filter_map(|r| Some((r["id"].as_str()?.to_string(), region_words(r)))).collect();
    let n = each.len() as f64;
    let mut df: BTreeMap<&str, usize> = BTreeMap::new();
    for (_, ws) in &each {
        for w in ws {
            *df.entry(w.as_str()).or_insert(0) += 1;
        }
    }
    let weight = |w: &str| -> f64 {
        let d = *df.get(w).unwrap_or(&1) as f64;
        ((n + 1.0) / d).ln() / (n + 1.0).ln()
    };
    each.iter()
        .map(|(id, ws)| {
            let shared: Vec<String> = ws.intersection(&mine).cloned().collect();
            let score = shared.iter().map(|w| weight(w)).sum::<f64>();
            (id.clone(), score, shared)
        })
        .collect()
}

/// The most regions a soup of `n` regions may have declared at once.
pub fn most_declared(n: usize) -> usize {
    MAX_DECLARED.min(n / 2)
}

/// **The regions a run declares** for `text` among `regions` (module
/// docs): their ids, best first.
pub fn declare(text: &str, regions: &[Value]) -> Vec<String> {
    let all = scores(text, regions);
    let best = all.iter().map(|x| x.1).fold(0.0, f64::max);
    let mut picked: Vec<(f64, String)> = all.into_iter().filter(|(_, s, _)| *s >= THRESHOLD && *s >= best / 2.0).map(|(id, s, _)| (s, id)).collect();
    picked.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal).then(a.1.cmp(&b.1)));
    picked.into_iter().take(most_declared(regions.len())).map(|(_, id)| id).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn regions() -> Vec<Value> {
        vec![
            json!({"id": "R-1", "handle": "Publishing the soup schema", "boundary": "Inside: the published, machine-readable schema. Outside: the importer's code goes to R-2."}),
            json!({"id": "R-2", "handle": "Importing records into a soup", "boundary": "Inside: landing externally written records: ids, collisions. Outside: the schema itself goes to R-1."}),
            json!({"id": "R-3", "handle": "Build caching", "boundary": "Inside: the build cache and its keys."}),
            json!({"id": "R-4", "handle": "Release notes", "boundary": "Inside: the changelog and what each release says."}),
        ]
    }

    /// The plugin's starter regions file, as shipped.
    fn example() -> Vec<Value> {
        serde_json::from_str(include_str!("../../../claude-code/cauldron/regions.example.json")).unwrap()
    }

    #[test]
    fn words_drop_endings_and_stop_words() {
        assert_eq!(words("tests testing tested"), ["test".to_string()].into_iter().collect());
        assert_eq!(words("the project's regions, anything"), BTreeSet::new());
        assert_eq!(words("importers importing"), ["import".to_string()].into_iter().collect());
    }

    #[test]
    fn a_region_is_declared_by_a_score_not_by_any_shared_word() {
        assert_eq!(declare("the published schema needs a drift test", &regions()), vec!["R-1"]);
        assert_eq!(declare("importing records collides on ids", &regions()), vec!["R-2"]);
        assert_eq!(declare("the build cache keys go stale", &regions()), vec!["R-3"]);
        // One shared word is not enough.
        assert!(declare("the schema", &regions()).is_empty());
        assert!(declare("a build failed", &regions()).is_empty());
        // A word in R-1's `Outside:` clause does not count for R-1.
        assert!(!declare("the importer's code", &regions()).contains(&"R-1".to_string()));
        // Nothing shared: nothing declared.
        assert!(declare("lunch on friday", &regions()).is_empty());
    }

    #[test]
    fn never_more_than_half_the_regions_and_never_more_than_four() {
        assert_eq!(most_declared(1), 0);
        assert_eq!(most_declared(4), 2);
        assert_eq!(most_declared(9), 4);
        let many: Vec<Value> = (1..=9).map(|i| json!({"id": format!("R-{i}"), "handle": format!("schema drift topic{i}"), "boundary": "Inside: schema drift."})).collect();
        assert!(declare("schema drift topic3", &many).len() <= MAX_DECLARED);
        // A soup of one region is never declared ahead of the call.
        assert!(declare("the published schema needs a drift test", &regions()[..1]).is_empty());
    }

    /// **The starter regions file** (the plugin's `regions.example.json`, 4
    /// regions): ordinary notes declare one region or none, never most of
    /// the soup.
    #[test]
    fn the_starter_regions_file_declares_narrowly() {
        let ex = example();
        assert_eq!(ex.len(), 4);
        let cases: &[(&str, &[&str])] = &[
            ("project", &[]),
            ("this project", &[]),
            ("the build broke after the release; two tests failed in CI", &[]),
            ("we decided the architecture: the build tools move to a new developer workflow and the code is split by architecture", &["r-code"]),
            ("the test suites need new fixtures, and the checks before a release should catch these failures", &["r-tests"]),
            ("users asked for a feature: the behaviour they see when a requirement changes", &["r-product"]),
            ("the release steps and the review rules: plans reviewed before each release, in order", &["r-process"]),
            ("I imported a batch of records from the old tracker yesterday", &[]),
        ];
        for (text, want) in cases {
            let got = declare(text, &ex);
            assert_eq!(got, want.iter().map(|s| s.to_string()).collect::<Vec<_>>(), "{text}: {:?}", scores(text, &ex));
        }
    }

    /// **Hostile text**: a dump made of every word of every region still
    /// declares at most half of them.
    #[test]
    fn hostile_text_declares_at_most_half_the_regions() {
        let ex = example();
        let everything: String = ex.iter().map(|r| format!("{} {}", r["handle"].as_str().unwrap(), r["boundary"].as_str().unwrap())).collect::<Vec<_>>().join(" ");
        let got = declare(&everything.repeat(20), &ex);
        assert!(got.len() <= 2, "{got:?}");
        let got = declare(&everything, &regions());
        assert!(got.len() <= 2, "{got:?}");
    }
}
