//! Ladle — the query language's write statement and bulk-read form. The
//! grammar is the door's contract. Ladle is the expression language.
//!
//! # THE CONCRETE TEXT SYNTAX — FROZEN
//!
//! This doc comment is the one normative definition of ladle's surface
//! syntax. Implementations follow it exactly; changing it is a contract
//! change.
//!
//! ## A worked example (a supersede, spelled concretely)
//!
//! ```text
//! MATCH (old: fixture-alpha/doc { id: $old_id })
//! EXPECT REV(old) = 12
//! CREATE (new: fixture-alpha/doc { title: $title, state: "draft" })
//! CREATE (new) -[supersedes]-> (old)
//! SET old.state = "superseded"
//! RETURN new.id AS new_id, REV(old)
//! ```
//!
//! One statement, one commit, one spine row — or one whole refusal with
//! witnesses.
//!
//! ## Lexical rules
//!
//! - Clause keywords are UPPERCASE and reserved: `MATCH WHERE EXPECT CREATE
//!   SET REMOVE SEAL IMPORT RETURN`, plus `AND OR NOT EXISTS REV COUNT NOTE AS`.
//!   (`IMPORT` is a later addition — see the frozen-semantics paragraph.)
//! - Whitespace (spaces, tabs, newlines) separates tokens and is otherwise
//!   insignificant. There are no comments.
//! - **binding / field / kind / node-type names**: `[a-z_][a-z0-9_]*`.
//! - **pool names**: `[a-z][a-z0-9-]*` (they are file names; `spine` is
//!   reserved for the projected namespace).
//! - **parameters**: `$name` (`[a-z_][a-z0-9_]*`), resolved from the
//!   envelope's bindings — values are NEVER spliced into the text. A referenced-but-unsupplied or supplied-but-unreferenced
//!   parameter refuses.
//! - **values**: `"strings"` (escapes: `\" \\ \n \t`), integers, decimal
//!   floats, `true`, `false`, `null`, or `$param`.
//! - Two field names are RESERVED in field maps and field refs: `id` (the
//!   store-minted identifier — match-by-identifier is an ordinary field-map
//!   entry `{ id: $x }`) and `state` (the node's state).
//!
//! ## Grammar (EBNF; `*` zero+, `?` optional, `|` alternatives)
//!
//! ```text
//! statement   := match* where* expect* mutation* return?
//! mutation    := create | set | remove | seal | import
//!
//! match       := "MATCH" ( node_head | hop )
//! node_head   := "(" binding ":" pool "/" node_type field_map? ")"
//! field_map   := "{" [ field ":" value { "," field ":" value } ] "}"
//! hop         := end arrow end
//! end         := node_head | "(" binding ")"          // bound end
//! arrow       := "-[" kind "]->" | "<-[" kind "]-"
//!
//! where       := "WHERE" predicate
//! predicate   := and_pred { "OR" and_pred }
//! and_pred    := unary { "AND" unary }
//! unary       := "NOT" unary | "(" predicate ")" | comparison | existence
//! comparison  := field_ref cmp value
//! cmp         := "=" | "!=" | "<" | "<=" | ">" | ">="
//! existence   := "EXISTS" "(" binding ")" arrow ( "(" binding ")" | "()" )
//!                                        // "()" = any far endpoint
//! field_ref   := binding "." field { "." field }     // dots address meta
//!
//! expect      := "EXPECT" ( rev_expect | count_expect | value_expect )
//! rev_expect  := "REV" "(" binding ( "." field )? ")" "=" value
//! count_expect:= "COUNT" "(" binding ")" ( "=" | "<=" ) value
//! value_expect:= field_ref "=" value
//!
//! create      := "CREATE" ( node_head | edge_create )
//! edge_create := "(" binding ")" arrow "(" binding ")" ( "NOTE" value )?
//! set         := "SET" field_ref "=" value
//! remove      := "REMOVE" "(" binding ")" arrow "(" binding ")"
//! seal        := "SEAL" "(" binding ")"
//! import      := "IMPORT" node_head   // node_head's field_map MUST name `id`
//! return      := "RETURN" ret_item { "," ret_item }
//! ret_item    := ret_expr ( "AS" name )?
//! ret_expr    := "REV" "(" binding ( "." field )? ")" | field_ref
//! ```
//!
//! ## Frozen semantics riding the syntax
//!
//! - **Clause order is fixed** as the EBNF's sequence; at most one RETURN.
//!   Out-of-order clauses are a malformed statement, not a reordering.
//! - **Pool qualification is required on every pattern head** (a pattern
//!   that names its pool claims in the right pool for free).
//!   Bound ends `(b)` inherit the pool their binding matched or created in.
//! - A `node_head` in MATCH matches a SET of nodes (possibly empty — the
//!   empty match SUCCEEDS touching nothing; pin emptiness with
//!   `EXPECT COUNT`). In CREATE it mints exactly
//!   one node per statement execution.
//! - A hop `(a: p/t) -[k]-> (b)` narrows BOTH ends to pairs connected by a
//!   `k` edge. Each side is a fresh head or a previously bound binding;
//!   binding names are unique per statement.
//! - `ret_expr` `binding.id` returns the identifier (minted ones included);
//!   the default column name of an un-aliased item is its source text,
//!   e.g. `"new.id"` or `"REV(old)"`.
//! - `REMOVE` takes bound endpoints; removing a matched SET of edges is
//!   expressed by matching the endpoints first (set bindings dissolve a
//!   cluster in one line). Edges only; nodes are never removed.
//! - `SEAL (b)` (an ADDITION to the frozen grammar, a contract change)
//!   sets the machine-owned `sealed` flag on the node `b`
//!   bound. `b` must be a MATCH head naming `id`; one SEAL per node, any
//!   number per statement, all-or-nothing like every clause. A SEAL whose
//!   head bound nothing refuses `unknown_node` and the statement seals
//!   nothing; re-sealing a sealed node lands nothing for it. Once sealed, a
//!   SET into the node's `body` refuses `sealed_body`; its state, other
//!   fields and edges stay writable, and correction is by superseding. No
//!   statement clears the flag. Spelled, for two nodes:
//!   `MATCH (a: ws/task { id: $a }) MATCH (b: ws/decision { id: $b })
//!   SEAL (a) SEAL (b)`.
//! - `IMPORT (b: p/t { id: $id, ... })` (an ADDITION to the frozen
//!   grammar, a contract change, on the SEAL precedent: carried ids enter
//!   ONLY through an import-only statement form on the one door). It lands ONE node under the SERIAL id
//!   its source already holds, where `CREATE` would mint one. Its head's
//!   field map MUST name `id`; every other field, and `state`, mean exactly
//!   what they mean in a CREATE head. The id is judged in this order: the
//!   type's grammar sets `id_prefix` (else `machine_owned_field` — a type
//!   that mints no serial has a store-minted id); the value carries that
//!   prefix — or, for a `region` ONLY, the legacy import prefix `SUB`
//!   (a carried v5 `SUB-<n>` keeps its id and number; `grammar_v6::LEGACY_IMPORT_PREFIXES`; never a mint
//!   prefix, never on a CREATE, never on another type) — (else
//!   `id_prefix_mismatch`); the rest after `<prefix>-` is a
//!   serial `[1-9][0-9]*` (else `carried_id_not_serial` — the provisional
//!   `-~` form, `Q-0`, `Q-007`, `Q-abc` all fall here). An id already held
//!   by the pool, or named twice in one statement, refuses
//!   `duplicate_node_id`. ONLY an `Agent` initiator may submit it (anything
//!   else refuses `import_not_permitted`, before any pool is opened, on both
//!   modes). The decided flip advances the `(pool, prefix)` serial counter
//!   to `max(last, n)` for every carried `n` BEFORE it numbers the same
//!   row's CREATE mints, so a statement mixing the two numbers its mints
//!   above every carried id; the counter is the id's OWN prefix's, so a
//!   carried `SUB-9` moves `SUB` and never `R`. A v6 required-at-mint edge
//!   the v5 source NEVER carried is not a refusal on this form: the node
//!   lands with the gap declared in `meta.imported_from.issues`
//!   (`schema_def::IMPORT_ISSUES_KEY`); a CREATE is never waived. The distinction from CREATE is LEXICAL: an
//!   ordinary CREATE naming `id` still refuses `machine_owned_field`
//!   byte-for-byte. `RETURN b.id` projects the carried id. Spelled:
//!   `IMPORT (r: ws/region { id: $r, handle: $h, boundary: $b })
//!   IMPORT (q: ws/question { id: $q, handle: $qh })
//!   CREATE (q) -[in_region]-> (r) NOTE $admits RETURN q.id`.
//!
//! ## Staged coverage
//!
//! The GRAMMAR above is the whole contract now. The first build implements:
//! match by identifier, match by pool+type+state (field-map entries on
//! `id`/`state` ONLY — general field-value entries are staged OUT; they
//! refuse by name), match across a
//! SINGLE edge hop;
//! EXPECT whole; CREATE, SET, REMOVE whole; WHERE over bound fields;
//! RETURN projections. Every grammatically valid but unimplemented form
//! refuses BY NAME (a coverage gap refuses; a dialect misreads) — see
//! [`compile`]'s coverage table for the frozen form names.

pub mod ast;
pub mod compile;
pub mod parse;
