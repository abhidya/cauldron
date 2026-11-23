//! **The whole-reply bound.** A read's `max_chars` per field (`crate::cut`)
//! bounds each text, not the answer; a reply that travels to a model has to
//! be bounded as a whole. [`budget`] takes any answer and a character budget
//! and returns an answer whose compact JSON text is at most that many
//! characters, still valid JSON, and saying what it left out.
//!
//! What it gives up, in order, until the reply fits:
//!
//! 1. items from the TAIL of the reply's top-level lists, the longest list
//!    first (every list a read returns is ordered most relevant first),
//!    down to one item per list;
//! 2. the tail of the longest text field anywhere in the reply, repeatedly;
//! 3. items from the tail of the longest list nested anywhere;
//! 4. everything but the account of what was dropped (only when the budget
//!    cannot hold even an empty reply's worth of the rest).
//!
//! The reply always carries
//! `truncated: {fields: [...], items_dropped: n}`, plus `dropped_from:
//! {<list>: n}` when items were dropped (a caller that pages builds its
//! `next` from it) and `fields_omitted: n` when the list of cut fields
//! itself had to be shortened. A `truncated` list that a per-field cut
//! already put in the reply is carried over into `fields`; a field cut here
//! is listed as `{path, chars}`, `path` a JSON pointer and `chars` the
//! field's whole length.

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};

/// The budget when a caller names none.
pub const DEFAULT_MAX_CHARS: usize = 12_000;
/// The largest budget a caller may ask for.
pub const MAX_CHARS_CEILING: usize = 50_000;
/// The smallest budget [`budget`] works to; a smaller one is raised to it,
/// so that the account of what was dropped always fits.
pub const MAX_CHARS_FLOOR: usize = 256;

/// The budget for a caller's `max_chars`: the default when none, held
/// between [`MAX_CHARS_FLOOR`] and [`MAX_CHARS_CEILING`].
pub fn clamp(asked: Option<usize>) -> usize {
    asked.unwrap_or(DEFAULT_MAX_CHARS).clamp(MAX_CHARS_FLOOR, MAX_CHARS_CEILING)
}

/// The size [`budget`] bounds: the characters of `v`'s compact JSON text.
pub fn size(v: &Value) -> usize {
    serde_json::to_string(v).map(|s| s.chars().count()).unwrap_or(usize::MAX)
}

#[derive(Default)]
struct Account {
    fields: Vec<Value>,
    fields_omitted: usize,
    items_dropped: usize,
    dropped_from: BTreeMap<String, usize>,
    reply_dropped: bool,
}

impl Account {
    fn value(&self) -> Value {
        let mut t = json!({"fields": self.fields, "items_dropped": self.items_dropped});
        if !self.dropped_from.is_empty() {
            t["dropped_from"] = json!(self.dropped_from);
        }
        if self.fields_omitted > 0 {
            t["fields_omitted"] = json!(self.fields_omitted);
        }
        if self.reply_dropped {
            t["reply_dropped"] = json!(true);
        }
        t
    }

    fn size(&self) -> usize {
        size(&self.value())
    }

    /// Shorten the list of cut fields until the account is at most `max`.
    fn shrink_to(&mut self, max: usize) {
        while self.size() > max && !self.fields.is_empty() {
            self.fields.pop();
            self.fields_omitted += 1;
        }
    }
}

/// The size of `doc` (an object without `truncated`) once `t` is added.
fn total(doc_size: usize, doc_empty: bool, t: &Account) -> usize {
    // `{...}` + `,"truncated":` + T, or `{` + `"truncated":` + T + `}`.
    let key = "\"truncated\":".len();
    if doc_empty {
        2 + key + t.size()
    } else {
        doc_size + 1 + key + t.size()
    }
}

/// **Bound `value` to `max_chars` characters of compact JSON** (raised to
/// [`MAX_CHARS_FLOOR`] when smaller). A value that is not an object is
/// answered as `{"value": <it>}`. See the module docs for what is given up,
/// in what order, and how the reply says so.
pub fn budget(value: Value, max_chars: usize) -> Value {
    let max = max_chars.max(MAX_CHARS_FLOOR);
    let mut doc: Map<String, Value> = match value {
        Value::Object(m) => m,
        other => {
            let mut m = Map::new();
            m.insert("value".into(), other);
            m
        }
    };
    let mut t = Account::default();
    match doc.remove("truncated") {
        Some(Value::Array(cut)) => t.fields = cut,
        Some(Value::Object(prior)) => {
            // Already bounded once: keep its account.
            if let Some(Value::Array(f)) = prior.get("fields") {
                t.fields = f.clone();
            }
            t.items_dropped = prior.get("items_dropped").and_then(Value::as_u64).unwrap_or(0) as usize;
            t.fields_omitted = prior.get("fields_omitted").and_then(Value::as_u64).unwrap_or(0) as usize;
            if let Some(Value::Object(d)) = prior.get("dropped_from") {
                for (k, v) in d {
                    t.dropped_from.insert(k.clone(), v.as_u64().unwrap_or(0) as usize);
                }
            }
        }
        Some(other) => t.fields.push(other),
        None => {}
    }
    // The account may take at most a quarter of the budget.
    t.shrink_to(max / 4);

    let mut doc = Value::Object(doc);
    let mut doc_size = size(&doc);

    // 1. The tails of the top-level lists, the longest list first. Sizes are
    //    kept per item, so this is linear in the reply.
    if total(doc_size, is_empty(&doc), &t) > max {
        let map = doc.as_object_mut().expect("an object");
        let mut lists: BTreeMap<String, (Vec<usize>, usize)> = BTreeMap::new();
        for (k, v) in map.iter() {
            if let Value::Array(a) = v {
                let sizes: Vec<usize> = a.iter().map(size).collect();
                let sum = sizes.iter().sum();
                lists.insert(k.clone(), (sizes, sum));
            }
        }
        while total(doc_size, false, &t) > max {
            let Some(k) = lists.iter().filter(|(_, (s, _))| s.len() > 1).max_by_key(|(_, (_, sum))| *sum).map(|(k, _)| k.clone()) else {
                break;
            };
            let (sizes, sum) = lists.get_mut(&k).expect("listed");
            let item = sizes.pop().expect("non-empty");
            *sum -= item;
            if let Some(Value::Array(a)) = map.get_mut(&k) {
                a.pop();
            }
            doc_size -= item + usize::from(!sizes.is_empty());
            t.items_dropped += 1;
            *t.dropped_from.entry(k).or_default() += 1;
        }
        doc_size = size(&doc);
    }

    // 2. The longest text field, cut from its tail, again and again.
    let mut cuts: BTreeMap<String, usize> = BTreeMap::new();
    for _ in 0..10_000 {
        let over = total(doc_size, is_empty(&doc), &t).saturating_sub(max);
        if over == 0 {
            break;
        }
        let Some((path, chars)) = longest_text(&doc) else { break };
        if chars == 0 {
            break;
        }
        let keep = chars.saturating_sub(over.max(1));
        if let Some(Value::String(s)) = doc.pointer_mut(&path) {
            *s = s.chars().take(keep).collect();
        }
        if !cuts.contains_key(&path) {
            cuts.insert(path.clone(), chars);
            t.fields.push(json!({"path": path, "chars": chars}));
            t.shrink_to(max / 4);
        }
        doc_size = size(&doc);
        if keep == 0 {
            // An emptied field cannot give more; go on with the next.
            continue;
        }
    }

    // 3. The tail of the longest list nested anywhere.
    for _ in 0..100_000 {
        if total(doc_size, is_empty(&doc), &t) <= max {
            break;
        }
        let Some(path) = longest_list(&doc) else { break };
        if let Some(Value::Array(a)) = doc.pointer_mut(&path) {
            a.pop();
        }
        t.items_dropped += 1;
        *t.dropped_from.entry(path).or_default() += 1;
        doc_size = size(&doc);
    }

    // 4. Nothing of the reply fits beside its account.
    if total(doc_size, is_empty(&doc), &t) > max {
        doc = Value::Object(Map::new());
        t.reply_dropped = true;
        t.dropped_from.clear();
        t.shrink_to(max.saturating_sub(16));
    }

    let mut out = doc;
    out["truncated"] = t.value();
    out
}

fn is_empty(doc: &Value) -> bool {
    doc.as_object().is_some_and(Map::is_empty)
}

fn escape(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

/// The JSON pointer and char length of the longest string in `v`.
fn longest_text(v: &Value) -> Option<(String, usize)> {
    fn walk(v: &Value, path: &mut String, best: &mut Option<(String, usize)>) {
        match v {
            Value::String(s) => {
                let n = s.chars().count();
                if best.as_ref().is_none_or(|b| n > b.1) {
                    *best = Some((path.clone(), n));
                }
            }
            Value::Array(a) => {
                for (i, x) in a.iter().enumerate() {
                    let len = path.len();
                    path.push_str(&format!("/{i}"));
                    walk(x, path, best);
                    path.truncate(len);
                }
            }
            Value::Object(m) => {
                for (k, x) in m {
                    let len = path.len();
                    path.push('/');
                    path.push_str(&escape(k));
                    walk(x, path, best);
                    path.truncate(len);
                }
            }
            _ => {}
        }
    }
    let mut best = None;
    walk(v, &mut String::new(), &mut best);
    best
}

/// The JSON pointer of the non-empty list in `v` whose text is longest.
fn longest_list(v: &Value) -> Option<String> {
    fn walk(v: &Value, path: &mut String, best: &mut Option<(String, usize)>) {
        match v {
            Value::Array(a) => {
                if !a.is_empty() {
                    let n = size(v);
                    if best.as_ref().is_none_or(|b| n > b.1) {
                        *best = Some((path.clone(), n));
                    }
                }
                for (i, x) in a.iter().enumerate() {
                    let len = path.len();
                    path.push_str(&format!("/{i}"));
                    walk(x, path, best);
                    path.truncate(len);
                }
            }
            Value::Object(m) => {
                for (k, x) in m {
                    let len = path.len();
                    path.push('/');
                    path.push_str(&escape(k));
                    walk(x, path, best);
                    path.truncate(len);
                }
            }
            _ => {}
        }
    }
    let mut best = None;
    walk(v, &mut String::new(), &mut best);
    best.map(|b| b.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deterministic stream of numbers, so the shapes below vary without
    /// a random-number dependency.
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self, n: u64) -> u64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (self.0 >> 33) % n.max(1)
        }
    }

    fn text(r: &mut Lcg, up_to: u64) -> String {
        let n = r.next(up_to) as usize;
        // Letters, quotes and backslashes (escaped in JSON) and multi-byte chars.
        let alphabet = ['a', 'b', 'é', '"', '\\', ' ', '\n', '字', 'z', '🙂'];
        (0..n).map(|_| alphabet[r.next(alphabet.len() as u64) as usize]).collect()
    }

    fn reply(r: &mut Lcg) -> Value {
        let items = r.next(300) as usize;
        let nodes: Vec<Value> = (0..items)
            .map(|i| {
                json!({"id": format!("IS-{i}"), "type": "issue", "handle": text(r, 80),
                       "regions": (0..r.next(4)).map(|j| format!("R-{j}")).collect::<Vec<_>>(),
                       "body": text(r, 2000)})
            })
            .collect();
        let mut v = json!({"env": "dev", "cauldron": "kb", "pool": "ws", "total": items, "nodes": nodes,
                           "query": text(r, 40), "terms": ["a", "b"]});
        if r.next(2) == 0 {
            v["truncated"] = json!([{"id": "IS-0", "field": "handle", "chars": 99}]);
        }
        if r.next(3) == 0 {
            v["huge"] = json!(text(r, 120_000));
        }
        v
    }

    #[test]
    fn a_bounded_reply_never_exceeds_its_budget_and_stays_valid_json() {
        let mut r = Lcg(7);
        for round in 0..60 {
            let v = reply(&mut r);
            for max in [0, 1, 100, 256, 257, 500, 1_000, 4_096, 12_000, 50_000] {
                let out = budget(v.clone(), max);
                let text = serde_json::to_string(&out).unwrap();
                let limit = max.max(MAX_CHARS_FLOOR);
                assert!(text.chars().count() <= limit, "round {round}, max {max}: {} chars", text.chars().count());
                let back: Value = serde_json::from_str(&text).expect("valid JSON");
                assert!(back["truncated"]["items_dropped"].is_u64(), "no account: {back}");
                assert!(back["truncated"]["fields"].is_array(), "no account: {back}");
            }
        }
    }

    #[test]
    fn a_reply_that_fits_is_unchanged_but_for_its_account() {
        let v = json!({"env": "dev", "nodes": [{"id": "IS-1"}], "truncated": [{"id": "IS-1", "field": "body", "chars": 400}]});
        let out = budget(v, 12_000);
        assert_eq!(
            out,
            json!({"env": "dev", "nodes": [{"id": "IS-1"}],
                   "truncated": {"fields": [{"id": "IS-1", "field": "body", "chars": 400}], "items_dropped": 0}})
        );
    }

    #[test]
    fn items_are_dropped_from_the_tail_and_counted() {
        let nodes: Vec<Value> = (0..100).map(|i| json!({"id": format!("N-{i}"), "handle": "x".repeat(50)})).collect();
        let out = budget(json!({"env": "dev", "nodes": nodes}), 2_000);
        let kept = out["nodes"].as_array().unwrap();
        let dropped = out["truncated"]["items_dropped"].as_u64().unwrap() as usize;
        assert!(dropped > 0 && !kept.is_empty(), "{out}");
        assert_eq!(kept.len() + dropped, 100);
        assert_eq!(out["truncated"]["dropped_from"]["nodes"], json!(dropped));
        // The head is kept, in order.
        for (i, n) in kept.iter().enumerate() {
            assert_eq!(n["id"], json!(format!("N-{i}")));
        }
        assert!(size(&out) <= 2_000);
    }

    #[test]
    fn a_long_field_is_cut_on_a_char_boundary_and_named() {
        let out = budget(json!({"records": [{"id": "IS-1", "body": "字".repeat(5_000)}]}), 1_000);
        let body = out["records"][0]["body"].as_str().unwrap();
        assert!(!body.is_empty() && body.chars().all(|c| c == '字'));
        assert_eq!(out["truncated"]["fields"], json!([{"path": "/records/0/body", "chars": 5000}]));
        assert_eq!(out["truncated"]["items_dropped"], 0, "a single record is cut, not dropped: {out}");
        assert!(size(&out) <= 1_000);
    }

    #[test]
    fn the_budget_is_held_between_its_floor_and_its_ceiling() {
        assert_eq!(clamp(None), DEFAULT_MAX_CHARS);
        assert_eq!(clamp(Some(10)), MAX_CHARS_FLOOR);
        assert_eq!(clamp(Some(1_000_000)), MAX_CHARS_CEILING);
        assert_eq!(clamp(Some(3_000)), 3_000);
    }
}
