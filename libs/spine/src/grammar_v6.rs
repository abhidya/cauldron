//! The v6 WORKSPACE grammar as DATA — the regions schema of the v5
//! workspace format, hosted at the spine's one door.
//!
//! This is the production shape the fixtures in [`crate::schema_def::fixtures`]
//! stood in for. It is written with the base schema machinery only (field kinds and
//! HARD character caps, required-at-mint edges, same-type targets, an id
//! prefix per type, the cap witness) plus edge fields; it
//! extends `schema_def.rs` with exactly one construct, the ONE-OF
//! required-at-mint (`NodeTypeDef::required_at_mint_any_of`).
//!
//! # Source of truth
//!
//! Every number and every type below is read from the v5 workspace format's
//! schema files and validator, never invented:
//!
//! - the TYPES — the v5 type table ("Ten content types plus `region`"),
//!   one `resources/schemas/node.<type>.schema.json` each, and
//!   `TYPE_PREFIX` in the v5 renderer (`v5render.py`), which
//!   `validate-pool.py` imports. ELEVEN v5 types, plus the twelfth,
//!   `viewer`, which v6 adds (below).
//! - the CAPS — `node.common.schema.json` (`handle` 1..=235, `body` <=10,000),
//!   `node.region.schema.json` (`boundary` 40..=1,000, `wake_when` <=500), and
//!   the same numbers as `HANDLE_MAX`, `BODY_MAX` and
//!   `DEFAULT_SIZING["max_boundary_chars"]` in `v5render.py`.
//! - the STATES — each type schema's `state` enum.
//! - the EDGE SET — `edge-entry.schema.json`'s closed `kind` enum, with each
//!   kind's legal source and target types from `HEAD_GRAMMAR` in
//!   `v5render.py` (the table `validate-pool.py` rule E3 enforces).
//! - the REQUIRED-AT-MINT edges — `in_region` on every content type
//!   (`validate-pool.py` rule R1 over `CONTENT_TYPES`) and
//!   `serves` on a milestone (rule E4; `node.milestone.schema.json`
//!   "serves -> directive (REQUIRED)").
//!
//! A maintainer-only probe outside the published tree re-reads those files
//! and compares them to this module, so an edit to either side fails it —
//! less the v6 deltas below, which it carries BY NAME.
//!
//! # The v6 corrections
//!
//! Four changes against v5:
//!
//! 1. `viewer` is the TWELFTH type: prefix `V-`, one per role, no state, one
//!    field, the `role` (the v5 `manifest.viewers` key, `^[a-z][a-z0-9-]*$`,
//!    so a floor of 1 and no cap). Its admitted nodes are `shows` edges
//!    (viewer -> content type) carrying a REQUIRED `cite` (object, the
//!    operator citation) and a `why` of at most [`WHY_CAP`] characters
//!    (required, floor 1: the v5 manifest's viewer entry requires `why`,
//!    `minLength` 1). The rendered caps (viewer block 5,000, viewer node
//!    1,200) stay tool-side; "one per role" reads every viewer, so it is a
//!    constraint ([`LEFT_OUT`]).
//! 2. The stored `depends-on` (task) and `superseded` (handoff) states are
//!    GONE — both are computed from edges (`depends_on`; the inbound
//!    `supersedes`), "one fact, one home".
//! 3. Required-at-mint on every content type is ONE OF: at least one
//!    `in_region`, or at least two `rejected_by` plus the field `searched`
//!    (the v5 `unplaced` object becomes `rejected_by` edges,
//!    content -> region, and ONE string field `searched`). The
//!    round-bounded half (regionless only inside an open round) is a
//!    workspace constraint, pen-enforced until constraints release.
//! 4. Pin 6, kind `workspace`, named v6 — stands ([`PIN`], [`KIND`]).
//!
//! # The v6 shape
//!
//! The stored shape differs from a v5 file where the spine's own model wins:
//! `schema_version`, `id` and `rev` are machine-owned (never declared);
//! `provenance.created` / `created_by` are computed from the minting
//! transaction, so only `provenance.operator` survives, as the object field
//! `operator` (REQUIRED on a directive: rule P1); `claims` and
//! `evidence` are arrays and go; `slug` is gone;
//! `unplaced` is an array-bearing object whose v6 form is the `rejected_by`
//! edges plus the string field `searched` (above). `meta` stays an
//! OPEN object.
//!
//! # What is NOT grammar here, and why
//!
//! Rules that read a SECOND node are constraints of the workspace kind,
//! not grammar: A1 (`admits` verbatim against the region's
//! boundary), the re-cut guard, A2, R3 (live region), E6 (ready while a
//! dependency is open), R4 (narrows acyclic), R10 (dual home), E7 (one
//! current handoff, AND the handoff's `supersedes` at mint — required only
//! when a previous handoff exists, which is a read of the pool), B1 (Outside:
//! routes), E2 (links versus refs). See [`LEFT_OUT`].
//!
//! The SEALED BODY is not on that list: it reads only the node's OWN
//! machine-owned `sealed` flag, so it is neither cross-node nor grammar data.
//! The spine enforces it for every pool — the `SEAL (b)` statement sets the
//! flag, and a SET into a sealed node's `body` refuses `sealed_body` at the
//! door and again at the landing (`schema_def::SEALED_BODY_FIELD`). The
//! ROUND that decides when to seal stays tool-side (the `U1 ...` row).
//! Single-node rules with no construct in `schema_def.rs` are listed there
//! too; only the one-of required-at-mint was added.

use crate::schema_def::{
    EdgeKindDef, FieldDef, FieldKind, MintAlternative, NodeTypeDef, PoolGrammar, PoolKind,
    PoolName, TypeRef,
};
use std::collections::{BTreeMap, BTreeSet};

/// The pool KIND every v6 workspace pool declares — what another pool's
/// reach names.
pub const KIND: &str = "workspace";

/// The pinned schema version: pin 6, kind `workspace`, named v6.
pub const PIN: u32 = 6;

/// `handle`: the one-line claim, 1..=235 characters, HARD
/// (`node.common.schema.json` minLength 1, maxLength 235; `HANDLE_MAX`).
pub const HANDLE_FLOOR: u64 = 1;
pub const HANDLE_CAP: u64 = 235;
/// `body`: the WRITE cap, <=10,000 characters
/// (`node.common.schema.json` maxLength 10000; `BODY_MAX`).
pub const BODY_CAP: u64 = 10_000;
/// A region's `boundary`: 40..=1,000 RAW characters
/// (`node.region.schema.json` minLength 40, maxLength 1000;
/// `DEFAULT_SIZING["max_boundary_chars"]`).
pub const BOUNDARY_FLOOR: u64 = 40;
pub const BOUNDARY_CAP: u64 = 1_000;
/// A region's `wake_when`: <=500 characters (`node.region.schema.json`).
pub const WAKE_WHEN_CAP: u64 = 500;

/// The region type's name. Regions carry FLAGS, never a state.
pub const REGION: &str = "region";
/// The region id prefix (`node.region.schema.json` `x-id-prefix`). The
/// legacy `SUB-` prefix is a migration artefact: a
/// migrated `SUB-` id keeps its number; regions MINT as `R-`.
pub const REGION_PREFIX: &str = "R";
/// The region's LEGACY prefix: a v5 pool's `SUB-<n>` region is CARRIED through the
/// `IMPORT` statement form under exactly that id — never minted, never
/// renumbered, never rejected for its prefix — so citations to it survive.
/// It is NEVER a mint prefix ([`REGION_PREFIX`] stays the only one a region
/// mints under), never admitted on a plain `CREATE`, and never on any other
/// type. A carried `SUB-n` advances the store's `(pool, "SUB")` serial
/// counter (the counter is keyed by the id's own prefix), never `R`'s.
pub const REGION_LEGACY_PREFIX: &str = "SUB";

/// The import-only LEGACY id prefixes, as data: (pool kind, node type,
/// prefix). [`crate::schema_def::PoolGrammar`]'s id-prefix rule admits one
/// of these beside the type's own `id_prefix` only where a carried id can
/// reach it — compile's `CREATE` caller-id judgment never admits it (see
/// `ladle::compile::caller_id_against_prefix`), and the store mints only
/// under `id_prefix`.
pub const LEGACY_IMPORT_PREFIXES: &[(&str, &str, &str)] = &[(KIND, REGION, REGION_LEGACY_PREFIX)];

/// The legacy import prefix of `node_type` in a pool of kind `kind`, if any.
pub fn legacy_import_prefix(kind: &str, node_type: &str) -> Option<&'static str> {
    LEGACY_IMPORT_PREFIXES
        .iter()
        .find(|(k, t, _)| *k == kind && *t == node_type)
        .map(|(_, _, p)| *p)
}

/// The twelfth type: one per role, no state, one field.
pub const VIEWER: &str = "viewer";
/// The viewer id prefix (`V-`).
pub const VIEWER_PREFIX: &str = "V";
/// The viewer's one field: the role it renders for.
pub const VIEWER_ROLE: &str = "role";
/// `shows.why`: at most 235 characters, HARD (the v5 manifest's
/// viewer entry `why` maxLength 235). The first edge-field cap in v6.
pub const WHY_CAP: u64 = 235;
/// The content-node field the regionless alternative requires (the v5
/// format's `unplaced.searched[]` as one string). v5 gives each search term
/// `minLength` 1 and no `maxLength`, and v6 names no cap: floor 1,
/// uncapped.
pub const SEARCHED: &str = "searched";
/// The regionless alternative's minimum of `rejected_by` edges ("at least
/// two candidates", as in the v5 format).
pub const REJECTED_BY_MIN: u32 = 2;

/// The ten CONTENT types: (name, id prefix, closed state vocabulary), from
/// the v5 type table and each `node.<type>.schema.json` — LESS the two
/// stored computed states ([`DROPPED_STATES`]).
pub const CONTENT_TYPES: [(&str, &str, &[&str]); 10] = [
    ("directive", "DIR", &["active", "done", "dropped"]),
    ("constraint", "CON", &["standing", "retired"]),
    ("danger", "D", &["lurking", "being-slain", "slain"]),
    ("issue", "IS", &["open", "resolved"]),
    ("decision", "DEC", &["decided", "revisited"]),
    ("question", "Q", &["open", "closed"]),
    ("task", "T", &["ready", "active", "done"]),
    ("milestone", "M", &["pending", "active", "done", "dropped"]),
    ("idea", "C", &["idea", "exploring", "stub", "active", "done", "dropped"]),
    ("handoff", "HO", &["current"]),
];

/// The v5 states v6 does NOT store: (type, state, the
/// edge it is computed from). Data, so the source probe carries exactly
/// these deltas against the v5 enums.
pub const DROPPED_STATES: [(&str, &str, &str); 2] = [
    ("task", "depends-on", "depends_on (an outbound edge to a non-terminal node)"),
    ("handoff", "superseded", "supersedes (an inbound edge)"),
];

/// The number of node types: the source's eleven ("Ten content
/// types plus `region`") plus `viewer`.
pub const TYPE_COUNT: usize = CONTENT_TYPES.len() + 2;

/// Who an edge kind may leave, or arrive at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ends {
    /// Every v5 type, region included (`HEAD_GRAMMAR`'s `ANY`) — NOT the
    /// viewer: v6 gives a viewer exactly one edge kind, `shows`,
    /// so it neither leaves nor receives `supersedes`, `derived_from` or
    /// `ref`.
    All,
    /// The ten content types (`HEAD_GRAMMAR`'s `CONTENT_TYPES`).
    Content,
    /// Exactly these local types.
    Only(&'static [&'static str]),
    /// A local node of the SOURCE's own type (`HEAD_GRAMMAR`'s `"SAME"`).
    SameAsSource,
}

/// One edge kind of the CLOSED set: its sources, its local targets, and
/// the reaches toward other pools' kinds it adds (a reach toward this
/// pool's own kind covers this pool too).
#[derive(Debug, Clone, Copy)]
pub struct EdgeSpec {
    pub kind: &'static str,
    pub from: Ends,
    pub to: Ends,
    /// Extra far endpoints `(pool kind, node type)`.
    pub reaches: &'static [(&'static str, &'static str)],
}

/// THE CLOSED EDGE SET — `edge-entry.schema.json`'s `kind` enum, each row's
/// endpoints from `HEAD_GRAMMAR` (v5render.py). An edge kind outside this
/// set refuses `unknown_edge_kind`; an inverse spelling (`blocked_by`,
/// `superseded_by`, ...) is simply not in it.
///
/// Cross-pool targets (`ID@board`), where v5 allows them: `derived_from`
/// and `ref` may point at any workspace node elsewhere (the target grammar
/// `ID@board`; `HEAD_GRAMMAR` `ANY`), `resolved_by` at a blueprint
/// migration or release (`edge-entry.schema.json`: "issue|question ->
/// decision|task|`ID@board` (a blueprint migration or release)"). Every
/// other kind is local only (validate-pool E3: "may not target a foreign
/// node"). `thread#msg` targets derive no row.
pub const EDGE_KINDS: [EdgeSpec; 12] = [
    EdgeSpec { kind: "in_region", from: Ends::Content, to: Ends::Only(&[REGION]), reaches: &[] },
    EdgeSpec {
        kind: "depends_on",
        from: Ends::Only(&["task", "milestone"]),
        to: Ends::Only(&["task", "milestone", "question"]),
        reaches: &[],
    },
    EdgeSpec {
        kind: "blocks",
        from: Ends::Only(&["issue", "question", "danger"]),
        to: Ends::Only(&["task", "milestone", "directive", "question"]),
        reaches: &[],
    },
    EdgeSpec { kind: "supersedes", from: Ends::All, to: Ends::SameAsSource, reaches: &[] },
    EdgeSpec { kind: "derived_from", from: Ends::All, to: Ends::All, reaches: &[] },
    EdgeSpec {
        kind: "resolved_by",
        from: Ends::Only(&["issue", "question"]),
        to: Ends::Only(&["decision", "task"]),
        reaches: &[("blueprint", "migration"), ("blueprint", "release")],
    },
    EdgeSpec {
        kind: "serves",
        from: Ends::Only(&["milestone"]),
        to: Ends::Only(&["directive"]),
        reaches: &[],
    },
    EdgeSpec { kind: "narrows", from: Ends::Only(&[REGION]), to: Ends::Only(&[REGION]), reaches: &[] },
    EdgeSpec { kind: "relates", from: Ends::Only(&[REGION]), to: Ends::Only(&[REGION]), reaches: &[] },
    EdgeSpec { kind: "ref", from: Ends::All, to: Ends::All, reaches: &[] },
    // ---- v6 additions ----
    // The regionless node's rejection evidence: one edge per candidate
    // region. Its v5 attributes (`rejects` 20..=1,000, `rev`) are edge
    // fields v6 does not declare; see LEFT_OUT.
    EdgeSpec { kind: "rejected_by", from: Ends::Content, to: Ends::Only(&[REGION]), reaches: &[] },
    // A viewer's admitted node: viewer -> any content type, fields
    // `cite` (required) and `why` (<= WHY_CAP) — see `edge_fields`.
    EdgeSpec { kind: "shows", from: Ends::Only(&[VIEWER]), to: Ends::Content, reaches: &[] },
];

/// The v5 edge kinds' count (`edge-entry.schema.json`'s `kind` enum), and
/// the kinds v6 adds to it — the source probe's edge-set delta.
pub const V6_ADDED_EDGE_KINDS: [&str; 2] = ["rejected_by", "shows"];

/// The declared FIELDS of an edge kind. Only `shows`
/// declares any: its `cite` (required) and its `why`
/// (<= 235). Every other kind's attributes stay undeclared (LEFT_OUT), so
/// an edge carrying one refuses `undeclared_edge_field`.
pub fn edge_fields(kind: &str) -> BTreeMap<String, FieldDef> {
    match kind {
        "shows" => BTreeMap::from([
            (
                "cite".to_string(),
                FieldDef { required: true, kind: FieldKind::Object, max_chars: None, min_chars: None },
            ),
            (
                "why".to_string(),
                FieldDef { required: true, kind: FieldKind::Text, max_chars: Some(WHY_CAP), min_chars: Some(1) },
            ),
        ]),
        _ => BTreeMap::new(),
    }
}

/// Kinds whose `All` target also reaches every workspace pool (`ID@board`).
const REACHES_EVERY_WORKSPACE: [&str; 2] = ["derived_from", "ref"];

/// Required-at-mint edges, the AND form: (node type, edge kind, minimum).
/// `in_region` is NOT here: on a content type it is one arm of
/// [`mint_alternatives`].
pub const REQUIRED_EXTRA: [(&str, &str, u32); 1] = [("milestone", "serves", 1)];

/// The AND-form required-at-mint edges of one type, from the data above.
pub fn required_edges(node_type: &str) -> BTreeMap<String, u32> {
    let mut m = BTreeMap::new();
    for (t, k, n) in REQUIRED_EXTRA {
        if t == node_type {
            m.insert(k.to_string(), n);
        }
    }
    m
}

/// The ONE-OF required-at-mint of one type: a
/// content type mints with at least one `in_region`, OR with at least
/// [`REJECTED_BY_MIN`] `rejected_by` plus the field [`SEARCHED`]. A region
/// and a viewer have none.
pub fn mint_alternatives(node_type: &str) -> Vec<MintAlternative> {
    if !CONTENT_TYPES.iter().any(|(n, _, _)| *n == node_type) {
        return vec![];
    }
    vec![
        MintAlternative {
            edges: BTreeMap::from([("in_region".to_string(), 1)]),
            fields: BTreeSet::new(),
        },
        MintAlternative {
            edges: BTreeMap::from([("rejected_by".to_string(), REJECTED_BY_MIN)]),
            fields: BTreeSet::from([SEARCHED.to_string()]),
        },
    ]
}

/// **External citations** (`meta.cites`): each a `{kind, ...}` object, its
/// `kind` one of these, carrying the keys listed for it. `meta` is an OPEN
/// object and its keys are not grammar ([`LEFT_OUT`], M1), so this is not
/// judged by the door's grammar: the record import judges it
/// ([`cite_problem`], refusing `bad-cite`), and the published schema
/// describes it on `meta.cites` (`schema_publish`), so the two say the same
/// thing. Any other key is allowed (`file`'s optional `line`, `repo`,
/// `commit`; `session`'s optional `at`). A required key must be present and
/// not null, and a string one must not be empty.
pub const CITE_KINDS: &[(&str, &[&str])] = &[
    ("session", &["id"]),
    ("file", &["path"]),
    ("commit", &["repo", "sha"]),
    ("url", &["url"]),
    ("thread", &["thread", "msg"]),
    // A dump (`DM-<n>`) in the same cauldron's dumps store.
    ("dump", &["id"]),
];

/// What is wrong with `meta`'s `cites`, if anything ([`CITE_KINDS`]): the
/// first problem, as a sentence starting `meta.cites`. `None` when `meta`
/// has no `cites` (or is not an object: the grammar judges that).
pub fn cite_problem(meta: &serde_json::Value) -> Option<String> {
    use serde_json::Value;
    let cites = meta.as_object()?.get("cites")?;
    let Value::Array(list) = cites else {
        return Some("meta.cites is a list of citation objects, each {\"kind\": ..., <its keys>}".to_string());
    };
    let kinds = || CITE_KINDS.iter().map(|(k, _)| *k).collect::<Vec<_>>().join(", ");
    for (i, c) in list.iter().enumerate() {
        let Value::Object(m) = c else {
            return Some(format!("meta.cites[{i}] is not an object; a citation is {{\"kind\": ..., <its keys>}}"));
        };
        let kind = match m.get("kind") {
            Some(Value::String(k)) => k.as_str(),
            Some(_) => return Some(format!("meta.cites[{i}]: `kind` is a string, one of {}", kinds())),
            None => return Some(format!("meta.cites[{i}] has no `kind` (one of {})", kinds())),
        };
        let Some((_, keys)) = CITE_KINDS.iter().find(|(k, _)| *k == kind) else {
            return Some(format!("meta.cites[{i}]: `{kind}` is not a citation kind (one of {})", kinds()));
        };
        let missing: Vec<&str> = keys
            .iter()
            .copied()
            .filter(|k| match m.get(*k) {
                None | Some(Value::Null) => true,
                Some(Value::String(s)) => s.is_empty(),
                Some(_) => false,
            })
            .collect();
        if !missing.is_empty() {
            let all = keys.iter().map(|k| format!("`{k}`")).collect::<Vec<_>>().join(" and ");
            let lacks = missing.iter().map(|k| format!("`{k}`")).collect::<Vec<_>>().join(" and ");
            return Some(format!("meta.cites[{i}] ({kind}) lacks {lacks}; a `{kind}` citation carries {all}"));
        }
    }
    None
}

/// The v5 types (`HEAD_GRAMMAR`'s `ANY`): content types first, region last.
pub fn v5_type_names() -> Vec<&'static str> {
    CONTENT_TYPES.iter().map(|(n, _, _)| *n).chain([REGION]).collect()
}

/// Every node type name: the v5 types, then `viewer`.
pub fn type_names() -> Vec<&'static str> {
    v5_type_names().into_iter().chain([VIEWER]).collect()
}

/// The rules this grammar deliberately does NOT express, each with its
/// validate-pool id (or a short name) and why. Data, so the report and a
/// reader see one list.
pub const LEFT_OUT: &[(&str, &str)] = &[
    // ---- cross-node: constraints of the workspace kind ----
    ("A1", "in_region `admits` verbatim against the target region's CURRENT boundary — reads the region"),
    ("re-cut guard", "a boundary edit deleting a span a member quotes — reads every inbound in_region"),
    ("A2", "in_region without admits once the pool flag is set — reads a pool-level flag"),
    ("R3", "in_region target must be a LIVE (not retired) region — reads the region's flag"),
    ("E6", "task `ready` while a depends_on target is non-terminal — reads the target's state"),
    ("R4", "narrows acyclic — reads the narrows graph"),
    ("R10", "ancestor+descendant dual home — reads the narrows closure"),
    ("E7", "exactly one current handoff, and `supersedes` at a handoff mint unless it is the first — reads every handoff"),
    ("viewer one per role", "at most one viewer per `role` value — reads every viewer"),
    ("regionless round-bound", "a regionless mint (rejected_by + searched) only inside an open round, and placed by its close — reads round state; pen-enforced until constraints release"),
    ("B1", "boundary Outside: routes to a live region — reads the named regions"),
    ("E2", "[[links]] in a body agree with the stored ref edges — derivation, tool-side until triggers"),
    // ---- single-node, but no construct in schema_def.rs (none added) ----
    ("L1 (links)", "no [[link]] inside a handle — a syntax rule over text; text syntax is the schema author's constraint, not the door's"),
    ("L3 (clauses)", "boundary carries `Inside:` and `Outside:` — same: a syntax rule over text"),
    ("E5", "resolved_by on an OPEN issue/question — a state-conditioned edge; needs a construct judging the part's state beside its edges"),
    ("supersedes note", "`note` REQUIRED on supersedes — an edge field"),
    ("admits >= 20", "`admits` REQUIRED on in_region, 20..=1,000 — an edge field"),
    ("rejected_by fields", "`rejects` (20..=1,000, verbatim boundary span) and `rev` on rejected_by — edge fields v6 does not name; declared alongside admits/note, not here (v6 caps only the fields it names)"),
    ("M1", "retired meta keys — `meta` is an OPEN object, so its keys are not grammar"),
    ("reserved meta", "typed reserved keys (meta.stop boolean, meta.blocking, ...) — inside the open `meta` object"),
    ("P2", "amnesty shape only on pre-stamp nodes — compares against a pool-level migration stamp"),
    ("landed_by", "computed attribution edge toward {spine, txn} — no v5 source; the spine row already holds who and through what"),
    // ---- tool-side permanently ----
    ("R5 R6 R7 V1 V2 V3 R11", "rendered and aggregate caps (viewer block 5,000, viewer node 1,200 among them) — the meter is the renderer"),
    ("U1 U1w U2 U3 X1 X2 I1 I2 P3 E1f E1t E1p", "round state, reverse index, migration identity, citation verification, foreign/pending targets"),
];

/// The v6 workspace grammar for a pool named `pool`.
pub fn workspace_v6(pool: &str) -> PoolGrammar {
    let text = |required: bool, floor: Option<u64>, cap: u64| FieldDef {
        required,
        kind: FieldKind::Text,
        max_chars: Some(cap),
        min_chars: floor,
    };
    let of = |kind: FieldKind| FieldDef {
        kind,
        ..FieldDef::optional()
    };

    let mut node_types = BTreeMap::new();
    for (name, prefix, states) in CONTENT_TYPES {
        let fields = BTreeMap::from([
            ("handle".to_string(), text(true, Some(HANDLE_FLOOR), HANDLE_CAP)),
            ("body".to_string(), text(false, None, BODY_CAP)),
            ("meta".to_string(), of(FieldKind::Object)),
            (
                SEARCHED.to_string(),
                FieldDef { required: false, kind: FieldKind::Text, max_chars: None, min_chars: Some(1) },
            ),
            (
                "operator".to_string(),
                FieldDef {
                    // P1: a directive carries an `operator` citation.
                    required: name == "directive",
                    kind: FieldKind::Object,
                    max_chars: None,
                    min_chars: None,
                },
            ),
        ]);
        node_types.insert(
            name.to_string(),
            NodeTypeDef {
                fields,
                states: states.iter().map(|s| s.to_string()).collect(),
                count_cells: BTreeMap::new(),
                id_prefix: Some(prefix.to_string()),
                required_edges_at_mint: required_edges(name),
                required_at_mint_any_of: mint_alternatives(name),
            },
        );
    }
    node_types.insert(
        REGION.to_string(),
        NodeTypeDef {
            fields: BTreeMap::from([
                ("handle".to_string(), text(true, Some(HANDLE_FLOOR), HANDLE_CAP)),
                ("boundary".to_string(), text(true, Some(BOUNDARY_FLOOR), BOUNDARY_CAP)),
                ("thresholds".to_string(), of(FieldKind::Object)),
                ("retired".to_string(), of(FieldKind::Boolean)),
                ("deferred_by".to_string(), of(FieldKind::Object)),
                ("provisional".to_string(), of(FieldKind::Boolean)),
                ("wake_when".to_string(), text(false, None, WAKE_WHEN_CAP)),
                ("meta".to_string(), of(FieldKind::Object)),
                ("operator".to_string(), of(FieldKind::Object)),
            ]),
            // Flags, never a state enum — an empty vocabulary refuses
            // any `state` (validate-pool R2).
            states: vec![],
            count_cells: BTreeMap::new(),
            id_prefix: Some(REGION_PREFIX.to_string()),
            required_edges_at_mint: required_edges(REGION),
            required_at_mint_any_of: mint_alternatives(REGION),
        },
    );
    node_types.insert(
        VIEWER.to_string(),
        NodeTypeDef {
            // ONE field, the role; floor 1 from the v5
            // manifest's role-key pattern, no cap (none named).
            fields: BTreeMap::from([(
                VIEWER_ROLE.to_string(),
                FieldDef { required: true, kind: FieldKind::Text, max_chars: None, min_chars: Some(1) },
            )]),
            // No state: an empty vocabulary refuses any `state`.
            states: vec![],
            count_cells: BTreeMap::new(),
            id_prefix: Some(VIEWER_PREFIX.to_string()),
            required_edges_at_mint: required_edges(VIEWER),
            required_at_mint_any_of: mint_alternatives(VIEWER),
        },
    );

    let all = v5_type_names();
    let content: Vec<&str> = CONTENT_TYPES.iter().map(|(n, _, _)| *n).collect();
    let expand = |e: Ends| -> Vec<&'static str> {
        match e {
            Ends::All => all.clone(),
            Ends::Content => content.clone(),
            Ends::Only(ts) => ts.to_vec(),
            Ends::SameAsSource => vec![],
        }
    };
    let mut edge_kinds = BTreeMap::new();
    for spec in EDGE_KINDS {
        let from: BTreeSet<TypeRef> = expand(spec.from).into_iter().map(TypeRef::local).collect();
        let mut to: BTreeSet<TypeRef> = if spec.to == Ends::SameAsSource {
            BTreeSet::from([TypeRef::SameAsSource])
        } else if REACHES_EVERY_WORKSPACE.contains(&spec.kind) {
            // A reach toward this pool's own kind covers this pool too
            // ("the declaring pool included"), so it is the whole target set.
            expand(spec.to).into_iter().map(|t| TypeRef::reach(KIND, t)).collect()
        } else {
            expand(spec.to).into_iter().map(TypeRef::local).collect()
        };
        for (k, t) in spec.reaches {
            to.insert(TypeRef::reach(*k, *t));
        }
        edge_kinds.insert(
            spec.kind.to_string(),
            EdgeKindDef {
                from,
                to,
                at_most_one_per_from: false,
                at_most_one_per_to: false,
                fields: edge_fields(spec.kind),
            },
        );
    }

    PoolGrammar {
        pool: PoolName::new(pool),
        kind: PoolKind::new(KIND),
        version: PIN,
        node_types,
        edge_kinds,
    }
}
