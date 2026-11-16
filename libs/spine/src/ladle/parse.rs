//! The ladle parser — text to [`crate::ladle::ast::Statement`], against the
//! FROZEN grammar in [`crate::ladle`]'s module docs.
//!
//! Parsing is total over the frozen grammar: text the grammar admits parses;
//! text it does not refuses with a structured
//! [`crate::witness::Witness::MalformedStatement`] (message + byte offset).
//! The parser does not decide staged coverage — that is the coverage table's
//! job, by name, in [`crate::ladle::compile`] (a dialect misreads, a
//! coverage gap refuses).
//!
//! **The one exception, deliberate and recorded.** A CHAINED path
//! `(a) -[k]-> (b) -[j]-> (c)` is longer than the frozen EBNF's `hop`, so the
//! parser is the only place that can see it — and the language admits paths of
//! bounded length while only the SINGLE hop is staged, which makes a chain
//! a real-but-staged form. It therefore refuses with
//! [`crate::witness::Witness::UnimplementedForm`] naming
//! [`crate::ladle::compile::MULTI_HOP_FORM`] — parsed far enough to be NAMED,
//! never a syntax error, never a partial hop kept. The other spelling of the
//! same path (two hop clauses sharing a binding) is grammatical, parses whole,
//! and is refused by the same name from the coverage table.
//!
//! # What the parser DOES refuse
//!
//! Everything here is a shape the frozen grammar does not admit at all, so
//! refusing it is not a coverage gap:
//!
//! - a lexical error (unknown character, unterminated string, unknown string
//!   escape, a mixed-case word);
//! - a clause out of the fixed order, a second RETURN, an empty statement;
//! - **a pattern head with no pool** — refused NAMING THE RULE (pool
//!   qualification is required on every pattern head, because lock
//!   claims are pool-qualified);
//! - a binding bound twice, or a bound end `(b)` naming a binding no earlier
//!   head introduced;
//! - a name that breaks its charset rule (bindings/fields/kinds/types are
//!   `[a-z_][a-z0-9_]*`; pools are `[a-z][a-z0-9-]*`);
//! - a non-integer literal where the grammar requires a revision or a count.
//!
//! Parameter presence against an envelope's bindings is checked at compile
//! time, not here — the parser sees text only.
//!
//! # Edge direction, normalized once
//!
//! In every edge-shaped node of the AST ([`HopPattern`], `CreateEdge`,
//! `RemoveEdge`, `Condition::Exists`) the two endpoint slots hold the
//! SOURCE-TEXT left and right ends, and the direction field says which way
//! the edge runs between them. `(a) <-[k]- (b)` therefore parses to
//! left/from `a`, right/to `b`, direction right-to-left — the edge runs
//! `b -> a`. [`crate::ladle::compile`] resolves that to `src`/`dst` exactly
//! once, where the deltas are built.

use crate::delta::CountOp;
use crate::error::{Result, SpineError};
use crate::ladle::ast::*;
use crate::ladle::compile::MULTI_HOP_FORM;
use crate::schema_def::{Direction, PoolName};
use crate::witness::Witness;
use serde_json::Value;
use std::collections::BTreeSet;

/// A structured malformed-statement refusal: it names why.
pub(crate) fn malformed(message: impl Into<String>, offset: usize) -> SpineError {
    SpineError::refuse(Witness::MalformedStatement {
        message: message.into(),
        offset,
    })
}

/// Parse one ladle statement (frozen syntax in
/// [`crate::ladle`]).
///
/// Refusals (all as `SpineError::Refused`, structured):
/// - `MalformedStatement { message, offset }` — the text does not lex or
///   parse, or clauses are out of the fixed order, or a binding name is
///   reused, or a bound end names a binding never bound.
///
/// Parameter presence against an envelope's bindings is checked at compile
/// time, not here — the parser sees text only.
pub fn parse(text: &str) -> Result<Statement> {
    let toks = lex(text)?;
    let mut p = Parser {
        toks,
        pos: 0,
        end: text.len(),
        bound: BTreeSet::new(),
    };
    p.statement()
}

// ---------------------------------------------------------------------------
// Lexer
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    /// A lower-case word: binding, field, kind, node type, pool, or one of
    /// the bare literals `true` / `false` / `null`.
    Name(String),
    /// An UPPER-CASE word: a reserved keyword.
    Keyword(String),
    Param(String),
    Str(String),
    Int(i64),
    Float(f64),
    LParen,
    RParen,
    LBrace,
    RBrace,
    Colon,
    Slash,
    Comma,
    Dot,
    Op(CmpOp),
    /// `-[`
    ArrowOpenFwd,
    /// `]->`
    ArrowCloseFwd,
    /// `<-[`
    ArrowOpenBack,
    /// `]-`
    ArrowCloseBack,
}

impl Tok {
    /// How the token reads back in a refusal message.
    fn render(&self) -> String {
        match self {
            Tok::Name(s) => s.clone(),
            Tok::Keyword(s) => s.clone(),
            Tok::Param(s) => format!("${s}"),
            Tok::Str(s) => format!("{s:?}"),
            Tok::Int(i) => i.to_string(),
            Tok::Float(f) => f.to_string(),
            Tok::LParen => "(".into(),
            Tok::RParen => ")".into(),
            Tok::LBrace => "{".into(),
            Tok::RBrace => "}".into(),
            Tok::Colon => ":".into(),
            Tok::Slash => "/".into(),
            Tok::Comma => ",".into(),
            Tok::Dot => ".".into(),
            Tok::Op(o) => o.as_str().into(),
            Tok::ArrowOpenFwd => "-[".into(),
            Tok::ArrowCloseFwd => "]->".into(),
            Tok::ArrowOpenBack => "<-[".into(),
            Tok::ArrowCloseBack => "]-".into(),
        }
    }
}

fn lex(text: &str) -> Result<Vec<(Tok, usize)>> {
    let b = text.as_bytes();
    let mut out: Vec<(Tok, usize)> = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        let tok = match c {
            b'(' => {
                i += 1;
                Tok::LParen
            }
            b')' => {
                i += 1;
                Tok::RParen
            }
            b'{' => {
                i += 1;
                Tok::LBrace
            }
            b'}' => {
                i += 1;
                Tok::RBrace
            }
            b':' => {
                i += 1;
                Tok::Colon
            }
            b'/' => {
                i += 1;
                Tok::Slash
            }
            b',' => {
                i += 1;
                Tok::Comma
            }
            b'.' => {
                i += 1;
                Tok::Dot
            }
            b'=' => {
                i += 1;
                Tok::Op(CmpOp::Eq)
            }
            b'!' => {
                if b.get(i + 1) == Some(&b'=') {
                    i += 2;
                    Tok::Op(CmpOp::Ne)
                } else {
                    return Err(malformed("`!` is only legal as `!=`", start));
                }
            }
            b'<' => {
                if b.get(i + 1) == Some(&b'-') && b.get(i + 2) == Some(&b'[') {
                    i += 3;
                    Tok::ArrowOpenBack
                } else if b.get(i + 1) == Some(&b'=') {
                    i += 2;
                    Tok::Op(CmpOp::Le)
                } else {
                    i += 1;
                    Tok::Op(CmpOp::Lt)
                }
            }
            b'>' => {
                if b.get(i + 1) == Some(&b'=') {
                    i += 2;
                    Tok::Op(CmpOp::Ge)
                } else {
                    i += 1;
                    Tok::Op(CmpOp::Gt)
                }
            }
            b'-' => {
                if b.get(i + 1) == Some(&b'[') {
                    i += 2;
                    Tok::ArrowOpenFwd
                } else if b.get(i + 1).is_some_and(|d| d.is_ascii_digit()) {
                    lex_number(text, &mut i)?
                } else {
                    return Err(malformed(
                        "`-` is only legal opening an arrow (`-[kind]->`) or before a number",
                        start,
                    ));
                }
            }
            b']' => {
                if b.get(i + 1) == Some(&b'-') && b.get(i + 2) == Some(&b'>') {
                    i += 3;
                    Tok::ArrowCloseFwd
                } else if b.get(i + 1) == Some(&b'-') {
                    i += 2;
                    Tok::ArrowCloseBack
                } else {
                    return Err(malformed(
                        "`]` closes an arrow: write `]->` or `]-`",
                        start,
                    ));
                }
            }
            b'"' => lex_string(text, &mut i)?,
            b'$' => {
                i += 1;
                let name_start = i;
                while i < b.len() && (b[i].is_ascii_lowercase() || b[i] == b'_' || b[i].is_ascii_digit())
                {
                    i += 1;
                }
                if i == name_start {
                    return Err(malformed(
                        "a parameter is `$name` with `name` matching `[a-z_][a-z0-9_]*`",
                        start,
                    ));
                }
                let name = text[name_start..i].to_string();
                check_simple_name(&name, start, "parameter")?;
                Tok::Param(name)
            }
            b'0'..=b'9' => lex_number(text, &mut i)?,
            b'a'..=b'z' | b'_' => {
                while i < b.len() {
                    let d = b[i];
                    if d.is_ascii_lowercase() || d.is_ascii_digit() || d == b'_' {
                        i += 1;
                    } else if d == b'-'
                        && b
                            .get(i + 1)
                            .is_some_and(|n| n.is_ascii_lowercase() || n.is_ascii_digit())
                    {
                        // a pool name's interior hyphen (`fixture-alpha`);
                        // never `-[`, which cannot follow a hyphen here.
                        i += 1;
                    } else {
                        break;
                    }
                }
                if b.get(i).is_some_and(|d| d.is_ascii_uppercase()) {
                    return Err(malformed(
                        "names are lower case and keywords are UPPER case; this word is mixed",
                        start,
                    ));
                }
                Tok::Name(text[start..i].to_string())
            }
            b'A'..=b'Z' => {
                while i < b.len() && (b[i].is_ascii_uppercase() || b[i] == b'_') {
                    i += 1;
                }
                if b.get(i)
                    .is_some_and(|d| d.is_ascii_lowercase() || d.is_ascii_digit())
                {
                    return Err(malformed(
                        "keywords are UPPER case and names are lower case; this word is mixed",
                        start,
                    ));
                }
                Tok::Keyword(text[start..i].to_string())
            }
            _ => {
                return Err(malformed(
                    format!("unexpected character `{}`", c as char),
                    start,
                ))
            }
        };
        out.push((tok, start));
    }
    Ok(out)
}

fn lex_number(text: &str, i: &mut usize) -> Result<Tok> {
    let b = text.as_bytes();
    let start = *i;
    if b[*i] == b'-' {
        *i += 1;
    }
    while *i < b.len() && b[*i].is_ascii_digit() {
        *i += 1;
    }
    let mut is_float = false;
    if b.get(*i) == Some(&b'.') && b.get(*i + 1).is_some_and(|d| d.is_ascii_digit()) {
        is_float = true;
        *i += 1;
        while *i < b.len() && b[*i].is_ascii_digit() {
            *i += 1;
        }
    }
    let raw = &text[start..*i];
    if is_float {
        raw.parse::<f64>()
            .map(Tok::Float)
            .map_err(|_| malformed(format!("`{raw}` is not a decimal float"), start))
    } else {
        raw.parse::<i64>()
            .map(Tok::Int)
            .map_err(|_| malformed(format!("`{raw}` is not an integer this build can hold"), start))
    }
}

fn lex_string(text: &str, i: &mut usize) -> Result<Tok> {
    let b = text.as_bytes();
    let start = *i;
    *i += 1; // opening quote
    let mut s = String::new();
    loop {
        let c = *b
            .get(*i)
            .ok_or_else(|| malformed("unterminated string literal", start))?;
        match c {
            b'"' => {
                *i += 1;
                return Ok(Tok::Str(s));
            }
            b'\\' => {
                let e = *b
                    .get(*i + 1)
                    .ok_or_else(|| malformed("unterminated string escape", *i))?;
                let decoded = match e {
                    b'"' => '"',
                    b'\\' => '\\',
                    b'n' => '\n',
                    b't' => '\t',
                    other => {
                        return Err(malformed(
                            format!(
                                "unknown string escape `\\{}` (the frozen escapes are \\\" \\\\ \\n \\t)",
                                other as char
                            ),
                            *i,
                        ))
                    }
                };
                s.push(decoded);
                *i += 2;
            }
            _ => {
                // Multi-byte UTF-8 rides through unchanged.
                let ch = text[*i..]
                    .chars()
                    .next()
                    .ok_or_else(|| malformed("unterminated string literal", start))?;
                s.push(ch);
                *i += ch.len_utf8();
            }
        }
    }
}

fn check_simple_name(name: &str, offset: usize, what: &str) -> Result<()> {
    let ok = {
        let mut cs = name.chars();
        match cs.next() {
            Some(c) if c.is_ascii_lowercase() || c == '_' => {
                cs.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
            }
            _ => false,
        }
    };
    if ok {
        Ok(())
    } else {
        Err(malformed(
            format!("`{name}` is not a legal {what} name: names match `[a-z_][a-z0-9_]*`"),
            offset,
        ))
    }
}

fn check_pool_name(name: &str, offset: usize) -> Result<()> {
    let ok = {
        let mut cs = name.chars();
        match cs.next() {
            Some(c) if c.is_ascii_lowercase() => {
                cs.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            }
            _ => false,
        }
    };
    if ok {
        Ok(())
    } else {
        Err(malformed(
            format!("`{name}` is not a legal pool name: pool names match `[a-z][a-z0-9-]*`"),
            offset,
        ))
    }
}

// ---------------------------------------------------------------------------
// Recursive-descent parser
// ---------------------------------------------------------------------------

struct Parser {
    toks: Vec<(Tok, usize)>,
    pos: usize,
    end: usize,
    /// Bindings introduced so far, in source order — the parser enforces
    /// bind-before-use and one binding, one name.
    bound: BTreeSet<String>,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos).map(|(t, _)| t)
    }

    fn peek_at(&self, n: usize) -> Option<&Tok> {
        self.toks.get(self.pos + n).map(|(t, _)| t)
    }

    /// The byte offset of the current token, or end-of-text.
    fn off(&self) -> usize {
        self.toks.get(self.pos).map(|(_, o)| *o).unwrap_or(self.end)
    }

    fn bump(&mut self) -> Option<(Tok, usize)> {
        let t = self.toks.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    fn eat(&mut self, want: &Tok) -> bool {
        if self.peek() == Some(want) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, want: Tok) -> Result<usize> {
        let off = self.off();
        match self.peek() {
            Some(t) if *t == want => {
                self.pos += 1;
                Ok(off)
            }
            Some(t) => Err(malformed(
                format!("expected `{}`, found `{}`", want.render(), t.render()),
                off,
            )),
            None => Err(malformed(
                format!("expected `{}`, found the end of the statement", want.render()),
                off,
            )),
        }
    }

    /// A lower-case word in a position that requires a simple name.
    fn name(&mut self, what: &str) -> Result<(String, usize)> {
        let off = self.off();
        match self.bump() {
            Some((Tok::Name(n), _)) => {
                check_simple_name(&n, off, what)?;
                Ok((n, off))
            }
            Some((t, _)) => Err(malformed(
                format!("expected a {what} name, found `{}`", t.render()),
                off,
            )),
            None => Err(malformed(
                format!("expected a {what} name, found the end of the statement"),
                off,
            )),
        }
    }

    /// A lower-case word without the simple-name charset check (pool names
    /// carry hyphens and are checked separately).
    fn raw_name(&mut self, what: &str) -> Result<(String, usize)> {
        let off = self.off();
        match self.bump() {
            Some((Tok::Name(n), _)) => Ok((n, off)),
            Some((t, _)) => Err(malformed(
                format!("expected a {what} name, found `{}`", t.render()),
                off,
            )),
            None => Err(malformed(
                format!("expected a {what} name, found the end of the statement"),
                off,
            )),
        }
    }

    fn bind(&mut self, name: &str, off: usize) -> Result<()> {
        if !self.bound.insert(name.to_string()) {
            return Err(malformed(
                format!("binding `{name}` is introduced twice: binding names are unique per statement"),
                off,
            ));
        }
        Ok(())
    }

    fn require_bound(&self, name: &str, off: usize) -> Result<()> {
        if self.bound.contains(name) {
            Ok(())
        } else {
            Err(malformed(
                format!(
                    "`{name}` is used before it is bound: a bound end names a binding an earlier pattern head introduced"
                ),
                off,
            ))
        }
    }

    // ---- statement ----

    fn statement(&mut self) -> Result<Statement> {
        if self.toks.is_empty() {
            return Err(malformed(
                "empty statement: a ladle statement is at least one clause",
                0,
            ));
        }
        let mut stmt = Statement {
            matches: Vec::new(),
            wheres: Vec::new(),
            expects: Vec::new(),
            mutations: Vec::new(),
            returns: None,
        };
        let mut phase = 0usize;
        while self.pos < self.toks.len() {
            let off = self.off();
            let kw = match self.peek() {
                Some(Tok::Keyword(k)) => k.clone(),
                Some(t) => {
                    return Err(malformed(
                        format!(
                            "expected a clause keyword (MATCH, WHERE, EXPECT, CREATE, SET, REMOVE, SEAL, IMPORT, RETURN), found `{}`",
                            t.render()
                        ),
                        off,
                    ))
                }
                None => break,
            };
            let ph = match kw.as_str() {
                "MATCH" => 1,
                "WHERE" => 2,
                "EXPECT" => 3,
                "CREATE" | "SET" | "REMOVE" | "SEAL" | "IMPORT" => 4,
                "RETURN" => 5,
                _ => {
                    return Err(malformed(
                        format!(
                            "`{kw}` is not a clause keyword; the clauses are MATCH, WHERE, EXPECT, CREATE, SET, REMOVE, SEAL, IMPORT, RETURN"
                        ),
                        off,
                    ))
                }
            };
            if ph < phase {
                return Err(malformed(
                    format!(
                        "clause `{kw}` is out of order: the clause order is fixed — MATCH, WHERE, EXPECT, CREATE/SET/REMOVE/SEAL/IMPORT, RETURN (frozen grammar); out-of-order clauses are malformed, never reordered"
                    ),
                    off,
                ));
            }
            phase = ph;
            self.pos += 1;
            match kw.as_str() {
                "MATCH" => {
                    let c = self.match_clause()?;
                    stmt.matches.push(c);
                }
                "WHERE" => {
                    let c = self.predicate()?;
                    stmt.wheres.push(c);
                }
                "EXPECT" => {
                    let e = self.expectation()?;
                    stmt.expects.push(e);
                }
                "CREATE" => {
                    let m = self.create_clause()?;
                    stmt.mutations.push(m);
                }
                "SET" => {
                    let m = self.set_clause()?;
                    stmt.mutations.push(m);
                }
                "REMOVE" => {
                    let m = self.remove_clause()?;
                    stmt.mutations.push(m);
                }
                "SEAL" => {
                    // `SEAL (b)` — one bound end, nothing else.
                    if self.peek() == Some(&Tok::LParen) && self.peek_at(2) == Some(&Tok::Colon) {
                        return Err(malformed(
                            "SEAL takes a bound end `(b)`: MATCH the node by identifier first, then `SEAL (b)`",
                            self.off(),
                        ));
                    }
                    let target = self.bound_end()?;
                    stmt.mutations.push(Mutation::Seal { target });
                }
                "IMPORT" => {
                    let m = self.import_clause()?;
                    stmt.mutations.push(m);
                }
                "RETURN" => {
                    if stmt.returns.is_some() {
                        return Err(malformed(
                            "a statement carries at most one RETURN (frozen grammar)",
                            off,
                        ));
                    }
                    stmt.returns = Some(self.return_clause()?);
                }
                _ => unreachable!("clause keyword already classified"),
            }
        }
        Ok(stmt)
    }

    // ---- patterns ----

    fn at_arrow(&self) -> bool {
        matches!(
            self.peek(),
            Some(Tok::ArrowOpenFwd) | Some(Tok::ArrowOpenBack)
        )
    }

    fn arrow(&mut self) -> Result<(String, ArrowDir)> {
        let off = self.off();
        match self.bump() {
            Some((Tok::ArrowOpenFwd, _)) => {
                let (kind, k_off) = self.name("edge kind")?;
                let _ = k_off;
                match self.peek() {
                    Some(Tok::ArrowCloseFwd) => {
                        self.pos += 1;
                        Ok((kind, ArrowDir::LeftToRight))
                    }
                    _ => Err(malformed(
                        "a forward arrow is written `-[kind]->`",
                        self.off(),
                    )),
                }
            }
            Some((Tok::ArrowOpenBack, _)) => {
                let (kind, _) = self.name("edge kind")?;
                match self.peek() {
                    Some(Tok::ArrowCloseBack) => {
                        self.pos += 1;
                        Ok((kind, ArrowDir::RightToLeft))
                    }
                    _ => Err(malformed(
                        "a backward arrow is written `<-[kind]-`",
                        self.off(),
                    )),
                }
            }
            _ => Err(malformed("expected an arrow (`-[kind]->` or `<-[kind]-`)", off)),
        }
    }

    /// `( binding : pool / type field_map? )` — a fresh head — or `( binding )`
    /// — a bound end.
    fn pattern_end(&mut self) -> Result<PatternEnd> {
        self.expect(Tok::LParen)?;
        let (binding, b_off) = self.name("binding")?;
        if self.eat(&Tok::Colon) {
            let (first, first_off) = self.raw_name("pool")?;
            if !self.eat(&Tok::Slash) {
                return Err(malformed(
                    format!(
                        "pool qualification is required on every pattern head (lock claims are pool-qualified, and a pattern that names its pool claims in the right pool for free): write `({binding}: <pool>/{first} ...)`"
                    ),
                    self.off(),
                ));
            }
            check_pool_name(&first, first_off)?;
            let (node_type, _) = self.name("node type")?;
            let fields = if self.eat(&Tok::LBrace) {
                self.field_map()?
            } else {
                Vec::new()
            };
            self.expect(Tok::RParen)?;
            self.bind(&binding, b_off)?;
            Ok(PatternEnd::Head(NodePattern {
                binding: Binding(binding),
                pool: PoolName::new(first),
                node_type,
                fields,
            }))
        } else {
            self.expect(Tok::RParen)?;
            self.require_bound(&binding, b_off)?;
            Ok(PatternEnd::Bound(Binding(binding)))
        }
    }

    /// A `( binding )` end only — CREATE/REMOVE edge endpoints.
    fn bound_end(&mut self) -> Result<Binding> {
        self.expect(Tok::LParen)?;
        let (binding, off) = self.name("binding")?;
        if self.peek() == Some(&Tok::Colon) {
            return Err(malformed(
                "an edge clause takes bound endpoints `(a) -[kind]-> (b)`; mint or match the nodes first",
                self.off(),
            ));
        }
        self.expect(Tok::RParen)?;
        self.require_bound(&binding, off)?;
        Ok(Binding(binding))
    }

    fn field_map(&mut self) -> Result<Vec<(String, ValueExpr)>> {
        let mut out: Vec<(String, ValueExpr)> = Vec::new();
        if self.eat(&Tok::RBrace) {
            return Ok(out);
        }
        loop {
            let (field, f_off) = self.name("field")?;
            if out.iter().any(|(f, _)| *f == field) {
                return Err(malformed(
                    format!("field `{field}` appears twice in one field map"),
                    f_off,
                ));
            }
            self.expect(Tok::Colon)?;
            let v = self.value()?;
            out.push((field, v));
            if self.eat(&Tok::Comma) {
                continue;
            }
            self.expect(Tok::RBrace)?;
            return Ok(out);
        }
    }

    fn match_clause(&mut self) -> Result<MatchClause> {
        let clause_off = self.off();
        let left = self.pattern_end()?;
        if self.at_arrow() {
            let (kind, arrow) = self.arrow()?;
            let right = self.pattern_end()?;
            if self.at_arrow() {
                // A CHAINED path `(a) -[k]-> (b) -[j]-> (c)`. The frozen
                // EBNF's `hop` is one arrow, but the language admits paths of
                // bounded length and only the SINGLE hop is staged — so
                // this is a coverage gap, not a syntax error, and a coverage
                // gap refuses BY NAME. Parsed far enough to be
                // named; no partial hop is kept.
                return Err(SpineError::refuse(Witness::UnimplementedForm {
                    form: MULTI_HOP_FORM.to_string(),
                }));
            }
            Ok(MatchClause::Hop(HopPattern {
                left,
                kind,
                right,
                arrow,
            }))
        } else {
            match left {
                PatternEnd::Head(h) => Ok(MatchClause::Node(h)),
                PatternEnd::Bound(b) => Err(malformed(
                    format!(
                        "MATCH ({b}) is not a pattern: a MATCH clause is a pattern head `({b}: pool/type {{ ... }})` or a single hop, and pool qualification is required on every pattern head"
                    ),
                    clause_off,
                )),
            }
        }
    }

    // ---- values ----

    fn value(&mut self) -> Result<ValueExpr> {
        let off = self.off();
        match self.bump() {
            Some((Tok::Str(s), _)) => Ok(ValueExpr::Literal(Value::String(s))),
            Some((Tok::Int(i), _)) => Ok(ValueExpr::Literal(Value::from(i))),
            Some((Tok::Float(f), _)) => Ok(ValueExpr::Literal(Value::from(f))),
            Some((Tok::Param(p), _)) => Ok(ValueExpr::Param(p)),
            Some((Tok::Name(n), _)) => match n.as_str() {
                "true" => Ok(ValueExpr::Literal(Value::Bool(true))),
                "false" => Ok(ValueExpr::Literal(Value::Bool(false))),
                "null" => Ok(ValueExpr::Literal(Value::Null)),
                other => {
                    // A name followed by `(` in a value position is the same
                    // staged-out scalar-function form as in a condition head;
                    // refuse it by its name, not as a bare word.
                    if matches!(self.peek(), Some(Tok::LParen)) {
                        return Err(SpineError::refuse(Witness::UnimplementedForm {
                            form: crate::ladle::compile::SCALAR_FUNCTION_FORM.to_string(),
                        }));
                    }
                    Err(malformed(
                        format!(
                            "`{other}` is a bare word, not a value: values are quoted strings, numbers, true, false, null, or `$param` (values ride the bindings, never the text)"
                        ),
                        off,
                    ))
                }
            },
            Some((t, _)) => Err(malformed(
                format!("expected a value, found `{}`", t.render()),
                off,
            )),
            None => Err(malformed(
                "expected a value, found the end of the statement",
                off,
            )),
        }
    }

    /// A value in a position the grammar types as an integer (a revision or
    /// a count). A literal is checked now; a `$param` is checked at compile
    /// time, when the bindings are in hand.
    fn integer_value(&mut self, what: &str) -> Result<ValueExpr> {
        let off = self.off();
        let v = self.value()?;
        match &v {
            ValueExpr::Param(_) => Ok(v),
            ValueExpr::Literal(Value::Number(n)) if n.is_i64() || n.is_u64() => Ok(v),
            other => Err(malformed(
                format!("{what} takes an integer or a `$param`, found `{other}`"),
                off,
            )),
        }
    }

    fn field_ref(&mut self) -> Result<FieldRef> {
        let (binding, off) = self.name("binding")?;
        self.require_bound(&binding, off)?;
        self.expect(Tok::Dot)?;
        let mut path = Vec::new();
        loop {
            let (f, _) = self.name("field")?;
            path.push(f);
            if !self.eat(&Tok::Dot) {
                break;
            }
        }
        Ok(FieldRef {
            binding: Binding(binding),
            path,
        })
    }

    // ---- WHERE ----

    fn predicate(&mut self) -> Result<Condition> {
        let mut parts = vec![self.and_pred()?];
        while matches!(self.peek(), Some(Tok::Keyword(k)) if k == "OR") {
            self.pos += 1;
            parts.push(self.and_pred()?);
        }
        Ok(if parts.len() == 1 {
            parts.pop().expect("one part")
        } else {
            Condition::Or(parts)
        })
    }

    fn and_pred(&mut self) -> Result<Condition> {
        let mut parts = vec![self.unary()?];
        while matches!(self.peek(), Some(Tok::Keyword(k)) if k == "AND") {
            self.pos += 1;
            parts.push(self.unary()?);
        }
        Ok(if parts.len() == 1 {
            parts.pop().expect("one part")
        } else {
            Condition::And(parts)
        })
    }

    fn unary(&mut self) -> Result<Condition> {
        match self.peek() {
            Some(Tok::Keyword(k)) if k == "NOT" => {
                self.pos += 1;
                Ok(Condition::Not(Box::new(self.unary()?)))
            }
            Some(Tok::Keyword(k)) if k == "EXISTS" => {
                self.pos += 1;
                self.existence()
            }
            Some(Tok::LParen) => {
                self.pos += 1;
                let inner = self.predicate()?;
                self.expect(Tok::RParen)?;
                Ok(inner)
            }
            Some(Tok::Name(_)) => {
                // A name followed by `(` is a scalar-function call — a form
                // the language names but does not stage, so it refuses BY
                // NAME here (there is no AST shape for it to be detected in
                // later; a coverage gap refuses, never misreads).
                if matches!(self.peek_at(1), Some(Tok::LParen)) {
                    return Err(SpineError::refuse(Witness::UnimplementedForm {
                        form: crate::ladle::compile::SCALAR_FUNCTION_FORM.to_string(),
                    }));
                }
                let left = self.field_ref()?;
                let op_off = self.off();
                let op = match self.bump() {
                    Some((Tok::Op(o), _)) => o,
                    Some((t, _)) => {
                        return Err(malformed(
                            format!(
                                "expected a comparison operator (= != < <= > >=), found `{}`",
                                t.render()
                            ),
                            op_off,
                        ))
                    }
                    None => {
                        return Err(malformed(
                            "expected a comparison operator (= != < <= > >=)",
                            op_off,
                        ))
                    }
                };
                let right = self.value()?;
                Ok(Condition::Compare { left, op, right })
            }
            Some(t) => Err(malformed(
                format!("expected a condition, found `{}`", t.render()),
                self.off(),
            )),
            None => Err(malformed(
                "expected a condition, found the end of the statement",
                self.off(),
            )),
        }
    }

    /// `EXISTS (a) -[k]-> (b)` / `EXISTS (a) -[k]-> ()`.
    fn existence(&mut self) -> Result<Condition> {
        let from = self.bound_end()?;
        let (kind, arrow) = self.arrow()?;
        let to = if self.peek() == Some(&Tok::LParen) && self.peek_at(1) == Some(&Tok::RParen) {
            self.pos += 2;
            None
        } else {
            Some(self.bound_end()?)
        };
        let direction = match arrow {
            ArrowDir::LeftToRight => Direction::Out,
            ArrowDir::RightToLeft => Direction::In,
        };
        Ok(Condition::Exists {
            from,
            kind,
            to,
            direction,
        })
    }

    // ---- EXPECT ----

    fn expectation(&mut self) -> Result<Expectation> {
        match self.peek() {
            Some(Tok::Keyword(k)) if k == "REV" => {
                self.pos += 1;
                self.expect(Tok::LParen)?;
                let (binding, off) = self.name("binding")?;
                self.require_bound(&binding, off)?;
                let field = if self.eat(&Tok::Dot) {
                    Some(self.name("field")?.0)
                } else {
                    None
                };
                self.expect(Tok::RParen)?;
                self.expect(Tok::Op(CmpOp::Eq))?;
                let expected = self.integer_value("EXPECT REV")?;
                Ok(Expectation::Revision {
                    binding: Binding(binding),
                    field,
                    expected,
                })
            }
            Some(Tok::Keyword(k)) if k == "COUNT" => {
                self.pos += 1;
                self.expect(Tok::LParen)?;
                let (binding, off) = self.name("binding")?;
                self.require_bound(&binding, off)?;
                self.expect(Tok::RParen)?;
                let op_off = self.off();
                let op = match self.bump() {
                    Some((Tok::Op(CmpOp::Eq), _)) => CountOp::Exactly,
                    Some((Tok::Op(CmpOp::Le), _)) => CountOp::AtMost,
                    Some((t, _)) => {
                        return Err(malformed(
                            format!(
                                "EXPECT COUNT takes `=` (exactly) or `<=` (at most), found `{}`",
                                t.render()
                            ),
                            op_off,
                        ))
                    }
                    None => {
                        return Err(malformed("EXPECT COUNT takes `=` or `<=`", op_off));
                    }
                };
                let expected = self.integer_value("EXPECT COUNT")?;
                Ok(Expectation::Count {
                    binding: Binding(binding),
                    op,
                    expected,
                })
            }
            Some(Tok::Name(_)) => {
                let field = self.field_ref()?;
                self.expect(Tok::Op(CmpOp::Eq))?;
                let expected = self.value()?;
                Ok(Expectation::Value { field, expected })
            }
            Some(t) => Err(malformed(
                format!(
                    "expected `REV(...)`, `COUNT(...)`, or `binding.field = value` after EXPECT, found `{}`",
                    t.render()
                ),
                self.off(),
            )),
            None => Err(malformed(
                "expected an expectation after EXPECT",
                self.off(),
            )),
        }
    }

    // ---- mutations ----

    fn create_clause(&mut self) -> Result<Mutation> {
        let is_head = matches!(self.peek(), Some(Tok::LParen))
            && matches!(self.peek_at(1), Some(Tok::Name(_)))
            && matches!(self.peek_at(2), Some(Tok::Colon));
        if is_head {
            match self.pattern_end()? {
                PatternEnd::Head(h) => Ok(Mutation::CreateNode(h)),
                PatternEnd::Bound(_) => unreachable!("lookahead said head"),
            }
        } else {
            let from = self.bound_end()?;
            let (kind, arrow) = self.arrow()?;
            let to = self.bound_end()?;
            let note = if matches!(self.peek(), Some(Tok::Keyword(k)) if k == "NOTE") {
                self.pos += 1;
                Some(self.value()?)
            } else {
                None
            };
            Ok(Mutation::CreateEdge {
                from,
                kind,
                to,
                arrow,
                note,
            })
        }
    }

    /// `IMPORT (b: pool/type { id: $id, ... })` — exactly one fresh node
    /// head whose field map names `id`. A bound end
    /// or an edge shape is malformed: edges are CREATEd.
    fn import_clause(&mut self) -> Result<Mutation> {
        let off = self.off();
        let is_head = matches!(self.peek(), Some(Tok::LParen))
            && matches!(self.peek_at(1), Some(Tok::Name(_)))
            && matches!(self.peek_at(2), Some(Tok::Colon));
        if !is_head {
            return Err(malformed(
                "IMPORT takes a node head `(b: pool/type { id: $id, ... })`; an edge is CREATEd",
                off,
            ));
        }
        let head = match self.pattern_end()? {
            PatternEnd::Head(h) => h,
            PatternEnd::Bound(_) => unreachable!("lookahead said head"),
        };
        if self.at_arrow() {
            return Err(malformed(
                "IMPORT takes a node head `(b: pool/type { id: $id, ... })`; an edge is CREATEd",
                self.off(),
            ));
        }
        if head.field(FIELD_ID).is_none() {
            return Err(malformed(
                "IMPORT carries a serial id the source already holds: name `id`; to mint one, write CREATE",
                off,
            ));
        }
        Ok(Mutation::ImportNode(head))
    }

    fn set_clause(&mut self) -> Result<Mutation> {
        let target = self.field_ref()?;
        self.expect(Tok::Op(CmpOp::Eq))?;
        let value = self.value()?;
        Ok(Mutation::Set { target, value })
    }

    fn remove_clause(&mut self) -> Result<Mutation> {
        let from = self.bound_end()?;
        let (kind, arrow) = self.arrow()?;
        let to = self.bound_end()?;
        Ok(Mutation::RemoveEdge {
            from,
            kind,
            to,
            arrow,
        })
    }

    // ---- RETURN ----

    fn return_clause(&mut self) -> Result<ReturnClause> {
        let mut items = Vec::new();
        loop {
            let expr = self.return_expr()?;
            let alias = if matches!(self.peek(), Some(Tok::Keyword(k)) if k == "AS") {
                self.pos += 1;
                Some(self.name("column alias")?.0)
            } else {
                None
            };
            items.push(ReturnItem { expr, alias });
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        Ok(ReturnClause { items })
    }

    fn return_expr(&mut self) -> Result<ReturnExpr> {
        match self.peek() {
            Some(Tok::Keyword(k)) if k == "REV" => {
                self.pos += 1;
                self.expect(Tok::LParen)?;
                let (binding, off) = self.name("binding")?;
                self.require_bound(&binding, off)?;
                let field = if self.eat(&Tok::Dot) {
                    Some(self.name("field")?.0)
                } else {
                    None
                };
                self.expect(Tok::RParen)?;
                Ok(ReturnExpr::Rev {
                    binding: Binding(binding),
                    field,
                })
            }
            Some(Tok::Name(_)) => Ok(ReturnExpr::Field(self.field_ref()?)),
            Some(t) => Err(malformed(
                format!(
                    "expected a projection item (`binding.field` or `REV(binding)`), found `{}`",
                    t.render()
                ),
                self.off(),
            )),
            None => Err(malformed(
                "expected a projection item after RETURN",
                self.off(),
            )),
        }
    }
}
