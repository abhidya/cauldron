//! The PUBLISHED schema of the `workspace` grammar v6 — machine-readable
//! files under `schema/soup/workspace-v6/`, GENERATED from
//! [`crate::grammar_v6::workspace_v6`] and never edited by hand.
//!
//! What it writes (see [`render`]):
//!
//! - `grammar.json` — the [`PoolGrammar`] itself, serialized by serde
//!   (pretty-printed; it parses back to `workspace_v6("ws")` exactly).
//! - `node.<type>.schema.json` — one JSON Schema (draft 2020-12) per node
//!   type: the id pattern, the closed state vocabulary (or no `state` at
//!   all), every declared field with its JSON kind and character floor and
//!   cap, the edge kinds that may LEAVE the type with the id forms each may
//!   target, and the required-at-mint edges.
//! - `edges.json` — the closed edge set: sources, targets, declared edge
//!   fields, and the required-at-mint rules of every type.
//!
//! A RECORD, as these schemas describe it, is one node at mint:
//! `{id, type, state?, <declared fields>, edges: [{kind, to, note?,
//! <declared edge fields>}]}`. `rev`, the schema stamp, provenance and the
//! seal flag are machine-owned and are not part of a record.
//!
//! Two shapes below come from the spine's machinery rather than from the
//! grammar data, and are named here so a reader can find their homes:
//! the SERIAL part of an id (`[1-9][0-9]*`, the rule `ladle::compile`
//! enforces on a carried id as `carried_id_not_serial`) and the edge's
//! free-text `note` (every edge record has one, `pool::EdgeRecord::note`,
//! uncapped and optional). A pool name is
//! `schema_def`'s well-formed name (`[a-z][a-z0-9-]*`).
//!
//! # The drift test
//!
//! `tests::published_schema_matches_the_grammar` fails when the committed
//! files differ from [`render`]'s output, or when the directory holds a
//! `.json` file [`render`] does not produce. To regenerate:
//!
//! ```text
//! SPINE_SCHEMA_BLESS=1 cargo test -p spine --lib schema_publish
//! ```

use crate::grammar_v6::{self, KIND, PIN};
use crate::schema_def::{FieldDef, FieldKind, NodeTypeDef, PoolGrammar, TypeRef};
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;

/// The directory, relative to the repository root, the files live in.
pub const PUBLISH_DIR: &str = "schema/soup/workspace-v6";
/// The open object field whose `cites` key the schema describes.
const META: &str = "meta";
/// The pool name the published `grammar.json` is rendered for (the soup's
/// one pool, `SOUP_POOL` in the CLI's engine).
pub const PUBLISH_POOL: &str = "ws";
/// The serial part of every id: a positive integer with no leading zero.
pub const SERIAL: &str = "[1-9][0-9]*";
/// A well-formed pool name (`schema_def::well_formed_name`).
pub const POOL_NAME: &str = "[a-z][a-z0-9-]*";
/// Any type-prefixed id, for a target in a pool of ANOTHER kind whose
/// prefixes this grammar does not declare.
pub const ANY_ID: &str = "[A-Z][A-Z0-9]*-[1-9][0-9]*";

const DRAFT: &str = "https://json-schema.org/draft/2020-12/schema";

fn id_urn(file: &str) -> String {
    format!(
        "urn:cauldron:schema:soup:{KIND}-v{PIN}:{}",
        file.trim_end_matches(".json").trim_end_matches(".schema")
    )
}

/// Every file, as (file name, contents), in a fixed order.
pub fn render() -> Vec<(String, String)> {
    let g = grammar_v6::workspace_v6(PUBLISH_POOL);
    let mut out = vec![(
        "grammar.json".to_string(),
        pretty(&serde_json::to_value(&g).expect("grammar serializes")),
    )];
    out.push(("edges.json".to_string(), pretty(&edges_doc(&g))));
    for name in g.node_types.keys() {
        let file = format!("node.{name}.schema.json");
        let doc = node_schema(&g, name, &file);
        out.push((file, pretty(&doc)));
    }
    out
}

fn pretty(v: &Value) -> String {
    let mut s = serde_json::to_string_pretty(v).expect("json renders");
    s.push('\n');
    s
}

/// The id prefix of a local type, plus its import-only legacy prefix.
fn prefixes(g: &PoolGrammar, node_type: &str) -> (String, Option<&'static str>) {
    let p = g.node_types[node_type]
        .id_prefix
        .clone()
        .expect("every v6 type declares an id prefix");
    (p, grammar_v6::legacy_import_prefix(g.kind.as_str(), node_type))
}

fn alt(ps: &BTreeSet<String>) -> String {
    ps.iter().cloned().collect::<Vec<_>>().join("|")
}

/// A field's JSON Schema, from its [`FieldDef`].
fn field_schema(f: &FieldDef) -> Value {
    let mut m = Map::new();
    match f.kind {
        FieldKind::Any => {}
        FieldKind::Text => {
            m.insert("type".into(), json!("string"));
        }
        FieldKind::Integer => {
            m.insert("type".into(), json!("integer"));
        }
        FieldKind::Boolean => {
            m.insert("type".into(), json!("boolean"));
        }
        FieldKind::Object => {
            m.insert("type".into(), json!("object"));
        }
    }
    if let Some(n) = f.min_chars {
        m.insert("minLength".into(), json!(n));
    }
    if let Some(n) = f.max_chars {
        m.insert("maxLength".into(), json!(n));
    }
    Value::Object(m)
}

/// The open `meta` object's one described key, `cites`
/// ([`grammar_v6::CITE_KINDS`]): a list of citations, each a `kind` from the
/// set carrying that kind's keys (present, not null, a string one not
/// empty), any other key allowed. The record import refuses the same
/// records (`bad-cite`, [`grammar_v6::cite_problem`]).
fn cites_schema() -> Value {
    let kinds: Vec<&str> = grammar_v6::CITE_KINDS.iter().map(|(k, _)| *k).collect();
    let per_kind: Vec<Value> = grammar_v6::CITE_KINDS
        .iter()
        .map(|(k, keys)| {
            let props: Map<String, Value> =
                keys.iter().map(|key| (key.to_string(), json!({"not": {"type": "null"}, "minLength": 1}))).collect();
            json!({
                "if": {"properties": {"kind": {"const": k}}, "required": ["kind"]},
                "then": {"properties": props, "required": keys},
            })
        })
        .collect();
    json!({
        "type": "array",
        "description": "External citations: each a `kind` with the keys that locate it. See README.md, \"Conventions\".",
        "items": {
            "type": "object",
            "properties": {"kind": {"enum": kinds}},
            "required": ["kind"],
            "allOf": per_kind,
        },
    })
}

/// The id pattern a `to` of edge kind `kind`, leaving a node of `src`, must
/// match: local targets by their prefix; targets reached in any pool of this
/// grammar's kind by their prefix with an optional `@<pool>`; targets in a
/// pool of another kind as `<ID>@<pool>`.
fn target_pattern(g: &PoolGrammar, kind: &str, src: &str) -> String {
    let def = &g.edge_kinds[kind];
    let mut local = BTreeSet::new();
    let mut own_kind = BTreeSet::new();
    let mut foreign = false;
    let add = |set: &mut BTreeSet<String>, t: &str| {
        let (p, legacy) = prefixes(g, t);
        set.insert(p);
        if let Some(l) = legacy {
            set.insert(l.to_string());
        }
    };
    for t in &def.to {
        match t {
            TypeRef::Local { node_type } => add(&mut local, node_type),
            TypeRef::SameAsSource => add(&mut local, src),
            TypeRef::Reach { kind: k, node_type } if k.as_str() == g.kind.as_str() => add(&mut own_kind, node_type),
            TypeRef::Reach { .. } => foreign = true,
        }
    }
    let mut arms = Vec::new();
    if !local.is_empty() {
        arms.push(format!("(?:{})-{SERIAL}", alt(&local)));
    }
    if !own_kind.is_empty() {
        arms.push(format!("(?:{})-{SERIAL}(?:@{POOL_NAME})?", alt(&own_kind)));
    }
    if foreign {
        arms.push(format!("{ANY_ID}@{POOL_NAME}"));
    }
    format!("^(?:{})$", arms.join("|"))
}

/// One edge kind's targets, readable: `{type, pools}`, or `same type as the
/// source`.
fn targets_doc(g: &PoolGrammar, kind: &str) -> Value {
    let def = &g.edge_kinds[kind];
    let to: Vec<Value> = def
        .to
        .iter()
        .map(|t| match t {
            TypeRef::Local { node_type } => json!({"type": node_type, "pools": "this pool"}),
            TypeRef::SameAsSource => json!({"type": "same type as the source", "pools": "this pool"}),
            TypeRef::Reach { kind, node_type } if kind.as_str() == g.kind.as_str() => {
                json!({"type": node_type, "pools": format!("any pool of kind `{kind}` (this one included)")})
            }
            TypeRef::Reach { kind, node_type } => {
                json!({"type": node_type, "pools": format!("any pool of kind `{kind}`")})
            }
        })
        .collect();
    Value::Array(to)
}

fn edge_fields_doc(fields: &std::collections::BTreeMap<String, FieldDef>) -> Value {
    let mut props = Map::new();
    let mut req = Vec::new();
    for (name, f) in fields {
        props.insert(name.clone(), field_schema(f));
        if f.required {
            req.push(json!(name));
        }
    }
    json!({"properties": props, "required": req})
}

fn mint_doc(t: &NodeTypeDef) -> Value {
    let any: Vec<Value> = t
        .required_at_mint_any_of
        .iter()
        .map(|a| json!({"edges": a.edges, "fields": a.fields}))
        .collect();
    json!({"all_of": {"edges": t.required_edges_at_mint}, "any_of": any})
}

fn edges_doc(g: &PoolGrammar) -> Value {
    let mut kinds = Map::new();
    for (name, def) in &g.edge_kinds {
        let from: Vec<String> = def
            .from
            .iter()
            .filter_map(|t| t.node_type().map(str::to_string))
            .collect();
        kinds.insert(
            name.clone(),
            json!({
                "from": from,
                "to": targets_doc(g, name),
                "fields": edge_fields_doc(&def.fields),
                "at_most_one_per_from": def.at_most_one_per_from,
                "at_most_one_per_to": def.at_most_one_per_to,
            }),
        );
    }
    let mut mint = Map::new();
    for (name, t) in &g.node_types {
        mint.insert(name.clone(), mint_doc(t));
    }
    json!({
        "$comment": "GENERATED by libs/spine/src/schema_publish.rs from grammar_v6::workspace_v6. Do not edit; see README.md.",
        "kind": g.kind.as_str(),
        "version": g.version,
        "edge_record": {
            "required": ["kind", "to"],
            "note": "every edge may carry an optional free-text `note` string; any other attribute must be one of the kind's declared `fields`",
            "to": {
                "this pool": format!("<PREFIX>-<n>, n = {SERIAL}"),
                "another pool": format!("<PREFIX>-<n>@<pool>, pool = {POOL_NAME}; only where `to` names a pool kind"),
            },
            "inverses": "never written: an edge is stored once, on its source"
        },
        "kinds": kinds,
        "required_at_mint": mint,
    })
}

/// One node type's JSON Schema.
fn node_schema(g: &PoolGrammar, name: &str, file: &str) -> Value {
    let t = &g.node_types[name];
    let (prefix, legacy) = prefixes(g, name);
    let mut props = Map::new();
    let mut required = vec![json!("id"), json!("type")];
    props.insert(
        "id".into(),
        json!({"type": "string", "pattern": format!("^{prefix}-{SERIAL}$")}),
    );
    props.insert("type".into(), json!({"const": name}));
    if !t.states.is_empty() {
        props.insert("state".into(), json!({"enum": t.states}));
    }
    for (fname, f) in &t.fields {
        let mut fs = field_schema(f);
        if fname == META && f.kind == FieldKind::Object {
            fs["properties"] = json!({"cites": cites_schema()});
        }
        props.insert(fname.clone(), fs);
        if f.required {
            required.push(json!(fname));
        }
    }

    // The edges that may LEAVE this type, one conditional per kind.
    let leaving: Vec<&String> = g
        .edge_kinds
        .iter()
        .filter(|(_, d)| d.leaves(name))
        .map(|(k, _)| k)
        .collect();
    let mut per_kind = Vec::new();
    for k in &leaving {
        let def = &g.edge_kinds[*k];
        let mut eprops = Map::new();
        eprops.insert("kind".into(), json!({"const": k}));
        eprops.insert(
            "to".into(),
            json!({"type": "string", "pattern": target_pattern(g, k, name)}),
        );
        eprops.insert("note".into(), json!({"type": "string"}));
        let mut ereq = vec![json!("kind"), json!("to")];
        for (fname, f) in &def.fields {
            eprops.insert(fname.clone(), field_schema(f));
            if f.required {
                ereq.push(json!(fname));
            }
        }
        per_kind.push(json!({
            "if": {"properties": {"kind": {"const": k}}, "required": ["kind"]},
            "then": {"properties": eprops, "required": ereq, "additionalProperties": false},
        }));
    }
    props.insert(
        "edges".into(),
        json!({
            "type": "array",
            "items": {
                "type": "object",
                "properties": {"kind": {"enum": leaving}},
                "required": ["kind", "to"],
                "allOf": per_kind,
            },
        }),
    );

    // Required at mint: every AND-form minimum, then ONE OF the alternatives.
    let contains = |kind: &str, n: u32| json!({"contains": {"properties": {"kind": {"const": kind}}, "required": ["kind"]}, "minContains": n});
    let mut all_of = Vec::new();
    for (k, n) in &t.required_edges_at_mint {
        all_of.push(json!({"properties": {"edges": contains(k, *n)}, "required": ["edges"]}));
    }
    if !t.required_at_mint_any_of.is_empty() {
        let any: Vec<Value> = t
            .required_at_mint_any_of
            .iter()
            .map(|a| {
                let mut edge_reqs: Vec<Value> = a.edges.iter().map(|(k, n)| contains(k, *n)).collect();
                let edges = if edge_reqs.len() == 1 {
                    edge_reqs.remove(0)
                } else {
                    json!({"allOf": edge_reqs})
                };
                let mut req: Vec<Value> = vec![json!("edges")];
                req.extend(a.fields.iter().map(|f| json!(f)));
                json!({"properties": {"edges": edges}, "required": req})
            })
            .collect();
        all_of.push(json!({"anyOf": any}));
    }

    let mut x = Map::new();
    x.insert("pool_kind".into(), json!(g.kind.as_str()));
    x.insert("grammar_version".into(), json!(g.version));
    x.insert("node_type".into(), json!(name));
    x.insert("id_prefix".into(), json!(prefix));
    if let Some(l) = legacy {
        x.insert("legacy_import_id_prefix".into(), json!(l));
    }
    x.insert(
        "states".into(),
        if t.states.is_empty() {
            json!("none: this type carries no `state`")
        } else {
            json!(t.states)
        },
    );
    x.insert("required_at_mint".into(), mint_doc(t));

    let mut doc = Map::new();
    doc.insert("$schema".into(), json!(DRAFT));
    doc.insert("$id".into(), json!(id_urn(file)));
    doc.insert(
        "$comment".into(),
        json!(
            "GENERATED by libs/spine/src/schema_publish.rs from grammar_v6::workspace_v6. Do not edit; see README.md."
        ),
    );
    doc.insert("title".into(), json!(format!("{KIND} v{PIN} node: {name}")));
    doc.insert(
        "description".into(),
        json!(format!(
            "One `{name}` record at mint. Field caps and floors count characters (Unicode code points). \
             Cross-node rules (region liveness, quoting, acyclicity, one-per-role, ...) are not expressible here; see README.md."
        )),
    );
    doc.insert("type".into(), json!("object"));
    doc.insert("properties".into(), Value::Object(props));
    doc.insert("required".into(), Value::Array(required));
    doc.insert("additionalProperties".into(), json!(false));
    if !all_of.is_empty() {
        doc.insert("allOf".into(), Value::Array(all_of));
    }
    doc.insert("x-cauldron".into(), Value::Object(x));
    Value::Object(doc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(PUBLISH_DIR)
    }

    /// The drift test: the committed files ARE the generator's output, and
    /// nothing else generated-looking sits beside them. With
    /// `SPINE_SCHEMA_BLESS=1` it writes the files instead (and still fails
    /// on a stray `.json` it would not write, which a human removes).
    #[test]
    fn published_schema_matches_the_grammar() {
        let dir = dir();
        let files = render();
        let bless = std::env::var_os("SPINE_SCHEMA_BLESS").is_some_and(|v| v == "1");
        if bless {
            std::fs::create_dir_all(&dir).expect("create the publish dir");
            for (name, body) in &files {
                std::fs::write(dir.join(name), body).expect("write a published file");
            }
        }
        let mut drift = Vec::new();
        for (name, body) in &files {
            match std::fs::read_to_string(dir.join(name)) {
                Ok(on_disk) if &on_disk == body => {}
                Ok(_) => drift.push(format!("{name}: differs from the generated output")),
                Err(e) => drift.push(format!("{name}: {e}")),
            }
        }
        let expected: BTreeSet<&str> = files.iter().map(|(n, _)| n.as_str()).collect();
        for entry in std::fs::read_dir(&dir).expect("the publish dir exists") {
            let name = entry.expect("dir entry").file_name().to_string_lossy().into_owned();
            if name.ends_with(".json") && !expected.contains(name.as_str()) {
                drift.push(format!("{name}: not produced by the generator (remove it)"));
            }
        }
        assert!(
            drift.is_empty(),
            "schema/soup/workspace-v6 drifted from grammar_v6:\n  {}\nregenerate: SPINE_SCHEMA_BLESS=1 cargo test -p spine --lib schema_publish",
            drift.join("\n  ")
        );
    }

    /// The Claude Code plugin ships a COPY of the published directory
    /// (`claude-code/cauldron/schema/`, made by `scripts/sync-plugin-schema`).
    /// Every file is byte-identical and nothing else is there, or this fails.
    #[test]
    fn plugin_schema_copy_matches_the_published_schema() {
        let src = dir();
        let copy = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../claude-code/cauldron/schema");
        let names = |d: &PathBuf| -> BTreeSet<String> {
            std::fs::read_dir(d)
                .unwrap_or_else(|e| panic!("{}: {e} (run scripts/sync-plugin-schema)", d.display()))
                .map(|e| e.expect("dir entry").file_name().to_string_lossy().into_owned())
                .collect()
        };
        let (a, b) = (names(&src), names(&copy));
        let mut drift: Vec<String> = a.symmetric_difference(&b).map(|n| format!("{n}: in one directory only")).collect();
        for n in a.intersection(&b) {
            if std::fs::read(src.join(n)).unwrap() != std::fs::read(copy.join(n)).unwrap() {
                drift.push(format!("{n}: differs"));
            }
        }
        assert!(
            drift.is_empty(),
            "claude-code/cauldron/schema drifted from schema/soup/workspace-v6:\n  {}\nrun: scripts/sync-plugin-schema",
            drift.join("\n  ")
        );
    }

    /// The published `meta.cites` schema says what the import's citation
    /// check ([`grammar_v6::cite_problem`]) judges: both are built from
    /// [`grammar_v6::CITE_KINDS`], the kinds as the item's `kind` enum and
    /// each kind's keys as required, not null and (as strings) not empty,
    /// other keys allowed. The check itself, on records each side must
    /// accept and refuse alike, is below.
    #[test]
    fn the_published_cites_schema_is_the_import_check() {
        for (_, body) in render().into_iter().filter(|(n, _)| n.starts_with("node.")) {
            let schema: Value = serde_json::from_str(&body).unwrap();
            let Some(meta) = schema["properties"].get("meta") else { continue };
            let cites = &meta["properties"]["cites"];
            assert_eq!(cites["type"], "array", "{body}");
            let items = &cites["items"];
            let kinds: Vec<&str> = grammar_v6::CITE_KINDS.iter().map(|(k, _)| *k).collect();
            assert_eq!(items["properties"]["kind"]["enum"], json!(kinds));
            assert_eq!(items["required"], json!(["kind"]));
            assert!(items.get("additionalProperties").is_none(), "other keys are allowed");
            let rules = items["allOf"].as_array().unwrap();
            assert_eq!(rules.len(), grammar_v6::CITE_KINDS.len());
            for ((kind, keys), rule) in grammar_v6::CITE_KINDS.iter().zip(rules) {
                assert_eq!(rule["if"]["properties"]["kind"]["const"], json!(kind));
                assert_eq!(rule["then"]["required"], json!(keys));
                for k in *keys {
                    assert_eq!(rule["then"]["properties"][*k], json!({"not": {"type": "null"}, "minLength": 1}));
                }
            }
        }
    }

    #[test]
    fn the_import_cite_check_accepts_what_the_schema_allows_and_refuses_the_rest() {
        let good = [
            json!({}),
            json!({"kind": "case"}),
            json!({"cites": []}),
            json!({"cites": [{"kind": "session", "id": "s-1"}, {"kind": "session", "id": "s-2", "at": "turn 4"}]}),
            json!({"cites": [{"kind": "file", "path": "src/lib.rs", "line": 40, "repo": "cauldron", "commit": "main"}]}),
            json!({"cites": [{"kind": "commit", "repo": "cauldron", "sha": "main"}]}),
            json!({"cites": [{"kind": "url", "url": "https://example.com/spec"}]}),
            json!({"cites": [{"kind": "thread", "thread": "t-1", "msg": 12}, {"kind": "thread", "thread": "t-1", "msg": "12"}]}),
            json!({"cites": [{"kind": "dump", "id": "DM-3"}]}),
            json!("not an object: the grammar's to judge"),
        ];
        for meta in good {
            assert_eq!(grammar_v6::cite_problem(&meta), None, "the import refuses {meta}");
        }
        let bad = [
            (json!({"cites": [{"kind": "fiel", "path": "notes/run.log"}]}), "`fiel` is not a citation kind"),
            (json!({"cites": [{"kind": "commit", "repo": "cauldron"}]}), "(commit) lacks `sha`"),
            (json!({"cites": [{"kind": "url", "url": null}]}), "(url) lacks `url`"),
            (json!({"cites": [{"kind": "dump", "id": ""}]}), "(dump) lacks `id`"),
            (json!({"cites": [{"path": "notes/run.log"}]}), "has no `kind`"),
            (json!({"cites": [{"kind": 3, "id": "x"}]}), "`kind` is a string"),
            (json!({"cites": ["https://example.com/spec"]}), "meta.cites[0] is not an object"),
            (json!({"cites": {"kind": "url", "url": "https://example.com/spec"}}), "meta.cites is a list"),
            (json!({"cites": [{"kind": "url", "url": "u"}, {"kind": "thread", "thread": "t-1"}]}), "meta.cites[1] (thread) lacks `msg`"),
        ];
        for (meta, says) in bad {
            let why = grammar_v6::cite_problem(&meta).unwrap_or_else(|| panic!("the import accepts {meta}"));
            assert!(why.contains(says), "{meta}: {why}");
        }
    }

    /// `grammar.json` parses back to the grammar it was rendered from.
    #[test]
    fn grammar_json_round_trips() {
        let (_, body) = render().into_iter().find(|(n, _)| n == "grammar.json").unwrap();
        let back: PoolGrammar = serde_json::from_str(&body).unwrap();
        assert_eq!(back, grammar_v6::workspace_v6(PUBLISH_POOL));
    }

    /// One schema per type, twelve in all, and the numbers a reader relies on
    /// are the grammar's.
    #[test]
    fn one_schema_per_type_with_the_grammar_numbers() {
        let files = render();
        let nodes: Vec<&(String, String)> = files.iter().filter(|(n, _)| n.starts_with("node.")).collect();
        assert_eq!(nodes.len(), grammar_v6::TYPE_COUNT);
        let get = |t: &str| -> Value {
            let (_, b) = files
                .iter()
                .find(|(n, _)| n == &format!("node.{t}.schema.json"))
                .unwrap();
            serde_json::from_str(b).unwrap()
        };
        let d = get("directive");
        assert_eq!(d["properties"]["handle"]["maxLength"], json!(grammar_v6::HANDLE_CAP));
        assert_eq!(d["properties"]["body"]["maxLength"], json!(grammar_v6::BODY_CAP));
        assert_eq!(d["properties"]["id"]["pattern"], json!("^DIR-[1-9][0-9]*$"));
        assert!(d["required"].as_array().unwrap().contains(&json!("operator")));
        let r = get("region");
        assert_eq!(
            r["properties"]["boundary"]["minLength"],
            json!(grammar_v6::BOUNDARY_FLOOR)
        );
        assert_eq!(
            r["properties"]["boundary"]["maxLength"],
            json!(grammar_v6::BOUNDARY_CAP)
        );
        assert!(
            r["properties"].get("state").is_none(),
            "a region carries flags, not a state"
        );
        let m = get("milestone");
        assert_eq!(m["allOf"].as_array().unwrap().len(), 2, "serves AND the one-of");
    }
}
