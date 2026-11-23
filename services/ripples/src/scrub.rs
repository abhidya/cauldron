//! **The secret scrubber**: what a ripple sends a model, and what it writes
//! from the model's answer, with credential-shaped text replaced.
//!
//! It runs on the dump's text (and the region text sent with it) before any
//! call through `threads`, always, and before a call to a local model unless
//! the owner turned automatic processing on with `--no-scrub-local`; and on
//! every string of the model's answer before it becomes records. It never
//! changes the stored dump: it works on the copy a run reads.
//!
//! Each match is replaced by a typed placeholder, `<redacted:KIND>`, and
//! counted by kind; the journal keeps the counts (never the text). Matches
//! that overlap or touch are redacted as their union, one placeholder named
//! after the outermost of them, each kind inside it still counted: a match
//! is never dropped for overlapping another. The kinds, in the order they
//! are tried:
//!
//! | kind | what |
//! |---|---|
//! | `private_key` | a `-----BEGIN … PRIVATE KEY-----` block (RSA, EC, DSA, OPENSSH, PGP, encrypted; four or five dashes, any case), to its END line within 64 KiB or, cut short, to the end of its key-shaped lines |
//! | `jwt` | a JSON Web Token (`eyJ….eyJ….…`) |
//! | `anthropic_key` | `sk-ant-…` |
//! | `openai_key` | `sk-…`, `sk-proj-…` (with a digit in it) |
//! | `aws_access_key_id` | `AKIA`/`ASIA`/… and 16 more |
//! | `google_api_key` | `AIza` and 35 more |
//! | `google_oauth_token` | `ya29.…` |
//! | `github_token` | `ghp_`/`gho_`/`ghu_`/`ghs_`/`ghr_…`, `github_pat_…` |
//! | `gitlab_token` | `glpat-…` |
//! | `slack_token` | `xoxb-`/`xoxp-`/`xoxa-`/`xoxo-`/`xoxs-`/`xoxr-…` |
//! | `slack_webhook` | `https://hooks.slack.com/services/…` |
//! | `stripe_key` | `sk_live_`/`sk_test_`/`rk_live_`/`rk_test_…` |
//! | `npm_token` | `npm_` and 36 more |
//! | `pypi_token` | `pypi-AgE…` |
//! | `huggingface_token` | `hf_…` |
//! | `sendgrid_key` | `SG.….…` |
//! | `digitalocean_token` | `dop_v1_`/`doo_v1_`/`dor_v1_…` |
//! | `shopify_token` | `shpat_`/`shpss_`/`shpca_`/`shppa_…` |
//! | `connection_password` | the password of `scheme://user:password@host`, or a URL's userinfo that is one long token (`https://TOKEN@host`) |
//! | `authorization_header` | the value of an `Authorization:` or `Proxy-Authorization:` header |
//! | `cookie_header` | the value of a `Cookie:` or `Set-Cookie:` header |
//! | `command_line_secret` | the value of a long flag whose name holds a credential word as a whole hyphen-separated part (`--password x`, `--admin-password=x`, `--db-password "x"`, `--client-secret x`, `--access-token x`, `--storage-account-key x`; never `--api-key-id`, `--password-file`, `--token-url` and the other names of [`FLAG_NOT_SECRET`]), `-pVALUE` after `mysql` and its kin, a quoted `-p'…'` after any tool, `sshpass -p`, `redis-cli -a` |
//! | `secret_assignment` | the value assigned to a name holding `SECRET`, `TOKEN`, `PASSWORD`, `PASSWD`, `PASSPHRASE`, `PWD`, `PASS` (as a whole part of the name), `CREDENTIAL`, `API_KEY`, `ACCESS_KEY`, `PRIVATE_KEY`, `AUTH_KEY` or `ACCOUNT_KEY`, in any case: `.env` style (`NAME=value`), lower case and dotted (`password=…`, `db.password=…`, `api_key = …` with a digit or a sign in the value), quoted (`"password": "…"`, a passphrase of any number of words for a password's name), YAML-ish (`password: …`, words to the end of the line), or a URL's query (`?api_key=…`) |
//! | `high_entropy` | any other run of 32 or more base64 or URL-safe characters with upper case, lower case and digits mixed, whose character entropy is high (see [`ENTROPY_MIN`]), standing alone or as one segment of a path; never a digest in its obvious context (a `sha256-`/`sha-256=`-labelled hash of that digest's length, a go.sum `h1:` hash, the body of an image, font, audio or video `data:` URI: see `digest_spans`) |
//!
//! A value that reads as code or a stand-in is left alone: a variable
//! (`$X`, `${X}`), a template (`<…>`, `{{…}}`), a call or a type
//! (`read_password()`, `u32`, `Option<String>`), a dotted path
//! (`self.token`), a number given to a name one of whose parts counts
//! (`TOKEN_LIMIT=4096`, `MAX_TOKENS=1000`; [`COUNTER_PARTS`]).
//!
//! **It reduces the risk, and it is not a guarantee.** A secret in a format
//! it does not know, shorter than 32 characters and not given to a name or
//! a flag it knows, written in words outside an assignment, split across
//! lines, made only of hex digits (hashes look the same), glued to a word
//! before it, or written with look-alike characters (a Unicode hyphen, a
//! full-width letter) passes through. Its misses and false positives are
//! measured by the test corpus (`tests/it/scrub.rs`).
//!
//! **Linear time.** Every pattern is matched by the regex crate's automata
//! (no backtracking); a key block's END is looked for within a bounded
//! span, every END line found once; overlaps are resolved by one sort of
//! the matches. [`scrub_within`] adds a time budget in proportion to
//! the text ([`budget_for`]); a run whose scrubbing runs out of it sends
//! nothing (`scrub_timeout`).

use std::collections::BTreeMap;
use std::sync::OnceLock;

use regex::Regex;
use serde_json::Value;

/// The least character entropy (bits per character) a `high_entropy` match
/// must have.
pub const ENTROPY_MIN: f64 = 4.2;
/// The shortest `high_entropy` match.
pub const ENTROPY_LEN: usize = 32;

/// Counts by kind.
pub type Counts = BTreeMap<String, u64>;

/// How a rule's matches are checked.
#[derive(Clone, Copy)]
enum Check {
    /// On the whole match (found by the DFA alone, no capture groups run).
    Whole(fn(&str) -> bool),
    /// On the capture groups (run only on the span the DFA found).
    Caps(fn(&regex::Captures) -> bool),
}

struct Rule {
    kind: &'static str,
    re: Regex,
    /// The group to replace (empty: the whole match).
    group: &'static str,
    check: Check,
    /// For a match that must stand alone: the characters (besides letters
    /// and digits) that may not touch it on either side.
    alone: Option<&'static str>,
}

fn whole_any(_: &str) -> bool {
    true
}

fn whole_digit(m: &str) -> bool {
    m.bytes().any(|b| b.is_ascii_digit())
}

/// A value that only names another (a variable, a template), or that is
/// plainly not a secret.
fn placeholder(v: &str) -> bool {
    let l = v.to_ascii_lowercase();
    v.is_empty()
        || v.starts_with('$')
        || v.starts_with('<')
        || v.starts_with("{{")
        || v.starts_with("%(")
        || v.starts_with('*')
        || v.starts_with('…')
        || matches!(l.as_str(), "null" | "none" | "nil" | "true" | "false" | "undefined" | "xxx" | "changeme" | "redacted" | "secret" | "password" | "token" | "...")
}

fn unquote(v: &str) -> &str {
    v.trim_matches(|c| c == '"' || c == '\'')
}

/// What a name says of the value given to it.
#[derive(PartialEq, Clone, Copy)]
enum Named {
    /// A password or a passphrase (`password`, `passwd`, `passphrase`, and
    /// `pass` or `pwd` as a whole part of the name: `db_pass`, `pwd`).
    Password,
    /// `pass` alone as a part of the name (`pass`, `db_pass`): a password,
    /// but also an English word, so only in `name=value` or quoted.
    Pass,
    /// Another secret: `secret`, `token`, `credential`, an API, access,
    /// private, auth or account key.
    Secret,
    /// Neither (`pass` inside a word: `bypass`, `passenger`).
    Neither,
}

fn named(name: &str) -> Named {
    let l = name.to_ascii_lowercase();
    let parts: Vec<&str> = l.split(|c: char| matches!(c, '_' | '.' | '-')).collect();
    if l.contains("passw") || l.contains("passphrase") || parts.iter().any(|p| matches!(*p, "pwd" | "passwd")) {
        return Named::Password;
    }
    if parts.contains(&"pass") {
        return Named::Pass;
    }
    let squeezed: String = l.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    if ["secret", "token", "credential", "apikey", "accesskey", "privatekey", "authkey", "accountkey"].iter().any(|k| squeezed.contains(k)) {
        return Named::Secret;
    }
    Named::Neither
}

/// The parts of a name: split at `_`, `.` and `-`, and where a lower-case
/// letter or a digit is followed by an upper-case one (`maxTokens`), in
/// lower case.
fn name_parts(name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut prev_lower = false;
    for ch in name.chars() {
        if matches!(ch, '_' | '.' | '-') {
            out.push(std::mem::take(&mut cur));
            prev_lower = false;
            continue;
        }
        if ch.is_ascii_uppercase() && prev_lower {
            out.push(std::mem::take(&mut cur));
        }
        prev_lower = ch.is_ascii_lowercase() || ch.is_ascii_digit();
        cur.push(ch.to_ascii_lowercase());
    }
    out.push(cur);
    out.retain(|p| !p.is_empty());
    out
}

/// The parts of a name that count or bound something (`TOKEN_LIMIT`,
/// `MAX_TOKENS`, `PASSWORD_MIN_LENGTH`, `TOKEN_TTL`, `tokenTimeout`): its
/// number is not a secret. Matched as whole parts ([`name_parts`]), so
/// `ADMIN_PASSWORD` (`min` inside `admin`) and `STORAGE_ACCOUNT_KEY` (`age`
/// inside `storage`) are not counters.
pub const COUNTER_PARTS: &[&str] = &[
    "limit", "limits", "max", "min", "ttl", "timeout", "count", "size", "length", "len", "age", "maxage", "budget", "tokens", "retries", "retry", "interval", "port", "bytes", "bits",
    "version", "index", "idx", "id", "rounds", "iterations", "cost", "window", "seconds", "secs", "ms", "days", "hours", "minutes", "lifetime",
];

fn counter_name(name: &str) -> bool {
    name_parts(name).iter().any(|p| COUNTER_PARTS.contains(&p.as_str()) || p.starts_with("expir"))
}

/// A type or a stand-in word, in lower case (`string`, `required`, `hidden`).
fn stand_in(l: &str) -> bool {
    matches!(
        l,
        "str" | "string" | "bool" | "int" | "text" | "varchar" | "bytes" | "required" | "optional" | "hidden" | "empty" | "unset" | "missing" | "masked" | "omitted" | "same" | "yes" | "no" | "self"
    )
}

fn code_like(s: &str) -> bool {
    s.contains(['(', ')', '<', '>', '[', ']', '&', '{', '}', '`']) || s.contains("::") || s.contains("->") || s.contains("=>")
}

/// An unquoted value that reads as code or a word, not a credential: a
/// dotted path (`self.token`), a snake_case or CamelCase identifier with no
/// digit (`read_token`, `String`), or a common type or stand-in word.
fn word_or_identifier(v: &str) -> bool {
    if stand_in(&v.to_ascii_lowercase()) {
        return true;
    }
    let ident = |p: &str| !p.is_empty() && p.bytes().next().is_some_and(|b| b.is_ascii_alphabetic() || b == b'_') && p.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
    if v.contains('.') && v.split('.').all(ident) {
        return true;
    }
    let digitless = !v.bytes().any(|b| b.is_ascii_digit());
    // snake_case (an underscore, no digit), or CamelCase (an upper-case
    // initial, then lower case, no digit).
    (digitless && ident(v) && v.contains('_')) || (digitless && v.len() > 1 && v.as_bytes()[0].is_ascii_uppercase() && v[1..].bytes().all(|b| b.is_ascii_lowercase()))
}

/// A type in a declaration (`u32`, `usize`, `f64`, `bool`, `Duration`).
fn type_name(v: &str) -> bool {
    matches!(
        v,
        "u8" | "u16" | "u32" | "u64" | "u128" | "usize" | "i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "f32" | "f64" | "bool" | "char" | "str" | "String" | "Duration" | "Instant" | "number" | "string" | "boolean" | "int" | "float"
    )
}

/// The value has a digit or a sign in it (not only letters).
fn mixed(v: &str) -> bool {
    v.bytes().any(|b| b.is_ascii_digit() || !(b.is_ascii_alphanumeric() || b == b'_'))
}

fn assignment(c: &regex::Captures) -> bool {
    let raw = &c["v"];
    let v = unquote(raw);
    if placeholder(v) || v.len() < 3 {
        return false;
    }
    let quoted = raw.starts_with('"') || raw.starts_with('\'');
    let name = &c["name"];
    let kind = named(name);
    if kind == Named::Neither {
        return false;
    }
    // A number given to a name that counts something.
    if v.bytes().all(|b| b.is_ascii_digit()) && counter_name(name) {
        return false;
    }
    let upper = name.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_' || b == b'.');
    if quoted {
        // A quoted value: any but an empty one or a placeholder. Given to a
        // password's name, any number of words (a passphrase); given to
        // another, one that reads as a sentence is prose.
        return matches!(kind, Named::Password | Named::Pass) || v.split_whitespace().count() <= 2;
    }
    if code_like(v) || v.starts_with(['/', ':']) || v.starts_with("./") {
        return false;
    }
    let colon = &c["sep"] == ":";
    if colon && type_name(v) {
        // `const MAX_TOKENS: u32 = …`: a declaration's type.
        return false;
    }
    if upper {
        // `.env` style: an upper-case name. After a colon, not a short
        // lower-case word (`TOKEN: the …` in prose).
        return !(colon && v.len() < 12 && v.bytes().all(|b| b.is_ascii_lowercase()));
    }
    if word_or_identifier(v) {
        return false;
    }
    let spaced = c.name("sp").is_some_and(|m| !m.as_str().is_empty());
    match (&c["sep"], kind) {
        (":", Named::Pass) => false,
        ("=", Named::Pass) => !spaced && v.len() >= 4,
        // `password: hunter`, `password=hunter`: a password's name takes any
        // value that is not a word of code.
        (_, Named::Password) => v.len() >= 4,
        // `name=value` with no space around it (a shell, a `.env`, a
        // properties file): a value with a digit or a sign in it.
        ("=", _) if !spaced => v.len() >= 6 && mixed(v),
        // `name = value`, `name: value`: long, with a digit or a sign, and a
        // letter (not a number or an expression).
        _ => v.len() >= 8 && mixed(v) && v.bytes().any(|b| b.is_ascii_alphabetic()),
    }
}

/// `password: correct horse battery staple` on a line of its own (a YAML
/// key): a password's name, then the words to the end of the line; or one
/// capitalised word of 8 or more letters there (`password: Riverboats`,
/// which an assignment takes for a type's name).
fn passphrase_line(c: &regex::Captures) -> bool {
    let v = c["v"].trim();
    let words = v.split_whitespace().count();
    if named(&c["name"]) != Named::Password || placeholder(v) || code_like(v) || !v.split_whitespace().all(|w| w.len() <= 256) {
        return false;
    }
    let capitalised = v.len() >= 8 && v.as_bytes()[0].is_ascii_uppercase() && v[1..].bytes().all(|b| b.is_ascii_lowercase()) && !stand_in(&v.to_ascii_lowercase());
    (2..=8).contains(&words) || (words == 1 && capitalised)
}

/// **The credential words of a flag's name**, as whole hyphen-separated
/// parts: `password`, `passwd`, `pwd`, `passphrase`, `secret`, `token`,
/// `credential(s)`, `apikey`, and the pairs `api-key`, `private-key`,
/// `access-key`, `auth-key`, `account-key`.
pub const FLAG_WORDS: &[&str] = &["password", "passwd", "pwd", "passphrase", "secret", "token", "credential", "credentials", "apikey"];
/// The pairs of parts that name a credential in a flag (`--private-key`).
pub const FLAG_PAIRS: &[(&str, &str)] = &[("api", "key"), ("private", "key"), ("access", "key"), ("auth", "key"), ("account", "key")];
/// **A part after a flag's credential word that makes it name something
/// else** (`--api-key-id`, `--password-file`, `--token-url`, `--secret-name`,
/// `--token-ttl`): the flag is not a secret's.
pub const FLAG_NOT_SECRET: &[&str] = &[
    "id", "ids", "file", "files", "path", "stdin", "env", "cmd", "command", "dir", "url", "uri", "endpoint", "name", "names", "type", "helper", "prompt", "arn", "ref", "header", "field", "label",
    "length", "len", "min", "max", "ttl", "timeout", "lifetime", "expiry", "expires", "expiration", "limit", "count", "policy", "store", "source", "provider", "format", "mode", "version",
    "rotation", "location",
];

/// Does a flag's name (without its `--`) name a secret? A credential word
/// as a whole part of it ([`FLAG_WORDS`], [`FLAG_PAIRS`]), with no part
/// after the last one that names something else ([`FLAG_NOT_SECRET`]), and
/// not `--no-…`.
pub fn flag_names_secret(name: &str) -> bool {
    let parts: Vec<String> = name.to_ascii_lowercase().split('-').filter(|p| !p.is_empty()).map(str::to_string).collect();
    if parts.first().is_some_and(|p| p == "no") {
        return false;
    }
    let mut last = None;
    for i in 0..parts.len() {
        if FLAG_WORDS.contains(&parts[i].as_str()) {
            last = Some(i);
        }
        if i + 1 < parts.len() && FLAG_PAIRS.contains(&(parts[i].as_str(), parts[i + 1].as_str())) {
            last = Some(i + 1);
        }
    }
    match last {
        None => false,
        Some(i) => !parts[i + 1..].iter().any(|p| FLAG_NOT_SECRET.contains(&p.as_str())),
    }
}

/// `--password x`, `--admin-password=x`, `--client-secret "x"`: a flag whose
/// name names a secret ([`flag_names_secret`]), and its value. A number is
/// a secret too, unless a part of the name counts something.
fn flag(c: &regex::Captures) -> bool {
    let name = &c["name"];
    let raw = &c["v"];
    let v = unquote(raw);
    if !flag_names_secret(name) || placeholder(v) || v.starts_with('-') || v.len() < 4 {
        return false;
    }
    // A path (`--private-key ~/.ssh/id_ed25519`): where the secret is kept.
    if v.starts_with(['/', '~']) || v.starts_with("./") || v.starts_with("../") {
        return false;
    }
    if v.bytes().all(|b| b.is_ascii_digit()) && counter_name(name) {
        return false;
    }
    raw.starts_with(['"', '\'']) || !code_like(v)
}

/// `mysql … -pVALUE`, `sshpass -p VALUE`: a password on a tool's command
/// line.
fn tool_password(c: &regex::Captures) -> bool {
    let v = unquote(&c["v"]);
    !placeholder(v) && v.len() >= 3
}

/// A query parameter naming a secret (`?api_key=…`, `&token=…`).
fn query_secret(c: &regex::Captures) -> bool {
    let v = &c["v"];
    named(&c["name"]) != Named::Neither && !placeholder(v) && v.len() >= 8 && (mixed(v) || (v.bytes().any(|b| b.is_ascii_uppercase()) && v.bytes().any(|b| b.is_ascii_lowercase())))
}

/// A `Cookie:` or `Set-Cookie:` header with a value (`name=value`).
fn cookie(c: &regex::Captures) -> bool {
    let v = c["v"].trim();
    v.contains('=') && !placeholder(v)
}

/// A URL whose userinfo is one long token (`https://TOKEN@host`).
fn userinfo_token(c: &regex::Captures) -> bool {
    let v = &c["v"];
    !placeholder(v) && v.bytes().any(|b| b.is_ascii_digit()) && v.bytes().any(|b| b.is_ascii_alphabetic())
}

fn authorization(c: &regex::Captures) -> bool {
    let v = &c["v"];
    if placeholder(v) || code_like(v) || v.starts_with(['`', '…']) || v.ends_with('`') {
        return false;
    }
    // With a scheme; or long; or a short value that is not a word.
    c.name("scheme").is_some() || v.len() >= 16 || (v.len() >= 8 && (mixed(v) || (v.bytes().any(|b| b.is_ascii_uppercase()) && v[1..].bytes().any(|b| b.is_ascii_uppercase()))))
}

fn connection(c: &regex::Captures) -> bool {
    !placeholder(&c["v"])
}

/// Shannon entropy of `s`, bits per character.
pub fn entropy(s: &str) -> f64 {
    let mut n: BTreeMap<char, f64> = BTreeMap::new();
    let mut total = 0.0;
    for ch in s.chars() {
        *n.entry(ch).or_insert(0.0) += 1.0;
        total += 1.0;
    }
    if total == 0.0 {
        return 0.0;
    }
    n.values().map(|c| c / total).map(|p| -p * p.log2()).sum()
}

fn high_entropy(m: &str) -> bool {
    let s = m.trim_end_matches('=');
    if s.len() < ENTROPY_LEN {
        return false;
    }
    let (lower, upper, digit) = (s.bytes().any(|b| b.is_ascii_lowercase()), s.bytes().any(|b| b.is_ascii_uppercase()), s.bytes().filter(|b| b.is_ascii_digit()).count());
    // A path or a dotted, dashed or underscored name is words, not a key.
    let seps = s.bytes().filter(|b| matches!(b, b'/' | b'-' | b'_')).count();
    lower && upper && digit >= 2 && seps * 8 <= s.len() && entropy(s) >= ENTROPY_MIN
}

/// **Digests, not secrets**: the spans of `text` the `high_entropy` rule
/// leaves alone, found in one pass. Each is base64 in a context that says
/// what it is, of exactly the length that context gives it:
///
/// - after a digest label (`sha256-`, `sha384-`, `sha512-`, `sha1-`,
///   `md5-`, as Subresource Integrity, lockfiles and Nix write them; `sha-256=`
///   or `sha-256=:` as the HTTP `Digest` and `Repr-Digest` headers do), when
///   the body is that digest's length in base64 and the label starts a word;
/// - a go.sum `h1:` hash (SHA-256, 44 characters), starting a word;
/// - the body of an image, font, audio or video `data:` URI
///   (`data:image/png;base64,…`); other `data:` URIs (text, JSON) are not
///   left alone.
///
/// Every other rule runs as before inside these spans; only the generic
/// high-entropy rule skips them.
fn digest_spans(text: &str) -> Vec<(usize, usize)> {
    static RE: OnceLock<[Regex; 3]> = OnceLock::new();
    let [labelled, gosum, data] = RE.get_or_init(|| {
        [
            Regex::new(r"(?i)(?:^|[^A-Za-z0-9_-])(?P<l>sha-?(?:1|224|256|384|512)|md5)(?:-|=:?)(?P<b>[A-Za-z0-9+/]{20,88}={0,2})").expect("compiles"),
            Regex::new(r"(?:^|[\s(\[{,;])h1:(?P<b>[A-Za-z0-9+/]{43}=)").expect("compiles"),
            Regex::new(r"(?i)data:(?:image|font|audio|video)/[A-Za-z0-9.+-]+(?:;[A-Za-z0-9=._-]+)*;base64,(?P<b>[A-Za-z0-9+/]+={0,2})").expect("compiles"),
        ]
    });
    let bytes_of = |label: &str| match label.to_ascii_lowercase().replace('-', "").as_str() {
        "md5" => 16,
        "sha1" => 20,
        "sha224" => 28,
        "sha256" => 32,
        "sha384" => 48,
        _ => 64,
    };
    // Base64 of `n` bytes: unpadded length, and padded only to its own block.
    let fits = |b: &str, n: usize| {
        let core = b.trim_end_matches('=');
        core.len() == (4 * n).div_ceil(3) && (b.len() == core.len() || b.len() == n.div_ceil(3) * 4)
    };
    let mut out = Vec::new();
    for c in labelled.captures_iter(text) {
        let b = &c["b"];
        if fits(b, bytes_of(&c["l"])) {
            let m = c.name("b").expect("matched");
            // The body ends the run: no base64 character after it.
            if !text[m.end()..].chars().next().is_some_and(|ch| ch.is_ascii_alphanumeric() || "+/".contains(ch)) {
                // From the label: a `sha384-…` run is one high-entropy match.
                out.push((c.name("l").expect("matched").start(), m.end()));
            }
        }
    }
    for re in [gosum, data] {
        for c in re.captures_iter(text) {
            let m = c.name("b").expect("matched");
            out.push((m.start(), m.end()));
        }
    }
    out.sort();
    out
}

/// Is `[s, e)` inside one of `spans` (sorted by start)?
fn within(spans: &[(usize, usize)], s: usize, e: usize) -> bool {
    let i = spans.partition_point(|&(a, _)| a <= s);
    i > 0 && spans[i - 1].1 >= e
}

fn rules() -> &'static [Rule] {
    static RULES: OnceLock<Vec<Rule>> = OnceLock::new();
    RULES.get_or_init(|| {
        let w = |kind: &'static str, pat: &str, check: fn(&str) -> bool| Rule { kind, re: Regex::new(pat).expect("a scrub rule compiles"), group: "", check: Check::Whole(check), alone: None };
        let c = |kind: &'static str, pat: &str, group: &'static str, check: fn(&regex::Captures) -> bool| Rule { kind, re: Regex::new(pat).expect("a scrub rule compiles"), group, check: Check::Caps(check), alone: None };
        let alone = |r: Rule, chars: &'static str| Rule { alone: Some(chars), ..r };
        vec![
            // `private_key` is found by [`private_keys`], not by a rule.
            w("jwt", r"\beyJ[A-Za-z0-9_-]{8,}\.eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}", whole_any),
            w("anthropic_key", r"\bsk-ant-[A-Za-z0-9_-]{20,}", whole_any),
            w("openai_key", r"\bsk-(?:proj-|svcacct-|admin-)?[A-Za-z0-9_-]{20,}", whole_digit),
            w("aws_access_key_id", r"\b(?:AKIA|ASIA|ABIA|ACCA|AGPA|AIDA|AROA|AIPA|ANPA|ANVA|APKA)[A-Z0-9]{16}\b", whole_any),
            w("google_api_key", r"\bAIza[0-9A-Za-z_-]{35}", whole_any),
            w("google_oauth_token", r"\bya29\.[0-9A-Za-z_-]{20,}", whole_any),
            w("github_token", r"\b(?:(?:ghp|gho|ghu|ghs|ghr)_[A-Za-z0-9]{36,}|github_pat_[A-Za-z0-9_]{22,})", whole_any),
            w("gitlab_token", r"\bglpat-[A-Za-z0-9_-]{20,}", whole_any),
            w("slack_token", r"\bxox[abposr]-[A-Za-z0-9-]{10,}", whole_any),
            w("slack_webhook", r"https://hooks\.slack\.com/services/[A-Za-z0-9/_-]+", whole_any),
            w("stripe_key", r"\b(?:sk|rk)_(?:live|test)_[A-Za-z0-9]{16,}", whole_any),
            w("npm_token", r"\bnpm_[A-Za-z0-9]{36}\b", whole_any),
            w("pypi_token", r"\bpypi-AgE[A-Za-z0-9_-]{50,}", whole_any),
            w("huggingface_token", r"\bhf_[A-Za-z0-9]{30,}\b", whole_any),
            w("sendgrid_key", r"\bSG\.[A-Za-z0-9_-]{16,}\.[A-Za-z0-9_-]{16,}", whole_any),
            w("digitalocean_token", r"\bdo[opr]_v1_[a-f0-9]{64}\b", whole_any),
            w("shopify_token", r"\bshp(?:at|ss|ca|pa)_[a-fA-F0-9]{32}\b", whole_any),
            c("connection_password", r"\b[A-Za-z][A-Za-z0-9+.-]*://[^\s:/@]+:(?P<v>[^\s@/]+)@", "v", connection),
            c("connection_password", r"\b[A-Za-z][A-Za-z0-9+.-]*://(?P<v>[^\s:/@]{16,})@", "v", userinfo_token),
            c(
                "authorization_header",
                r"(?i)\b(?:proxy-)?authorization\s*:\s*(?:(?P<scheme>bearer|basic|token|digest|negotiate|apikey|bot)\s+)?(?P<v>[^\s\x22',;]+)",
                "v",
                authorization,
            ),
            c("cookie_header", r"(?im)\b(?:set-)?cookie[ \t]*:[ \t]*(?P<v>[^\n]+)", "v", cookie),
            c(
                "command_line_secret",
                r#"(?:^|[\s(])--(?P<name>[A-Za-z0-9-]*(?i:passw(?:or)?d|passphrase|pwd|token|secret|key|credential)[A-Za-z0-9-]*)(?:=|[ \t]+)(?P<v>"[^"\n]*"|'[^'\n]*'|[^\s"']+)"#,
                "v",
                flag,
            ),
            c("command_line_secret", r#"\b(?:mysql|mysqldump|mysqladmin|mysqlimport|mysqlshow|mariadb|mariadb-dump)\b[^\n]{0,200}?[ \t]-p(?P<v>'[^'\n]*'|"[^"\n]*"|[^\s'"-][^\s]*)"#, "v", tool_password),
            // `-p'…'` or `-p"…"`, glued: a password on any tool's command
            // line (`psql`-likes, `mongo`, `7z`), quoted so it cannot be a
            // port or a path.
            c("command_line_secret", r#"(?:^|[\s(])-p(?P<v>'[^'\n]+'|"[^"\n]+")"#, "v", tool_password),
            c("command_line_secret", r#"\bsshpass[ \t]+-p[ \t]*(?P<v>'[^'\n]*'|"[^"\n]*"|[^\s'"-][^\s]*)"#, "v", tool_password),
            c("command_line_secret", r#"\bredis-cli\b[^\n]{0,200}?[ \t]-a[ \t]+(?P<v>'[^'\n]*'|"[^"\n]*"|[^\s'"-][^\s]*)"#, "v", tool_password),
            c("secret_assignment", r"[?&](?P<name>[A-Za-z0-9_.-]*(?i:token|secret|passw(?:or)?d|pwd|pass|api_?key|access_?key|signature|sig|session)[A-Za-z0-9_.-]*)=(?P<v>[^&#\s\x22'<>]+)", "v", query_secret),
            c(
                "secret_assignment",
                r#"(?im)^[ \t]*["']?(?P<name>[A-Za-z0-9_.-]*(?:passw(?:or)?d|passphrase|pwd|pass)[A-Za-z0-9_.-]*)["']?[ \t]*:[ \t]*(?P<v>[^\s"'#][^\n#]{0,200})$"#,
                "v",
                passphrase_line,
            ),
            c(
                "secret_assignment",
                r#"(?i)(?:^|[\s"'{,;(])(?:export\s+)?["']?(?P<name>[A-Za-z0-9_.-]*(?:secret|token|passw(?:or)?d|passphrase|pwd|pass|credential|api[_-]?key|access[_-]?key|private[_-]?key|auth[_-]?key|account[_-]?key)[A-Za-z0-9_.-]*)["']?(?P<sp>[ \t]*)(?P<sep>=|:)\s*(?P<v>"[^"\n]*"|'[^'\n]*'|[^\s"',;]+)"#,
                "v",
                assignment,
            ),
            alone(w("high_entropy", r"[A-Za-z0-9+/_-]{32,}={0,2}", high_entropy), "+/_-"),
            // A token as one segment of a path (`…/a/b/c/TOKEN`): the run
            // without `/`, which may touch it.
            alone(w("high_entropy", r"[A-Za-z0-9+_-]{32,}={0,2}", high_entropy), "+_-"),
        ]
    })
}

/// The `-----BEGIN … PRIVATE KEY-----` line.
fn key_header() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)-{4,5} ?BEGIN (?:[A-Z0-9]+ ){0,4}PRIVATE KEY(?: BLOCK)? ?-{4,5}").expect("compiles"))
}

/// The `-----END … PRIVATE KEY-----` line.
fn key_footer() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)-{4,5} ?END (?:[A-Z0-9]+ ){0,4}PRIVATE KEY(?: BLOCK)? ?-{4,5}").expect("compiles"))
}

/// How far after a key's BEGIN line its END line is looked for, and how
/// much of a key cut short (no END) is taken: bounded, so no input makes
/// the search scan to the end of the text once per BEGIN.
pub const KEY_SPAN: usize = 64 * 1024;
/// The most lines of a key cut short that are taken.
pub const KEY_LINES: usize = 400;

fn key_body_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=' | b':' | b',' | b'-' | b' ')
}

/// **Private key blocks**, in one forward pass: each BEGIN line to the first
/// END line within [`KEY_SPAN`] after it; with none there (cut short), to
/// the end of the key-shaped lines that follow it (at most [`KEY_LINES`]
/// lines and [`KEY_SPAN`] bytes). Every END line is found once, up front,
/// so the work is linear in the text whatever it holds.
fn private_keys(text: &str, deadline: Option<std::time::Instant>) -> Result<Vec<(usize, usize)>, ()> {
    let ends: Vec<usize> = key_footer().find_iter(text).map(|m| m.end()).collect();
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut at = 0;
    let mut n = 0u32;
    while let Some(h) = key_header().find_at(text, at) {
        n += 1;
        if n % 64 == 0 && deadline.is_some_and(|d| std::time::Instant::now() > d) {
            return Err(());
        }
        let limit = h.end().saturating_add(KEY_SPAN).min(text.len());
        let i = ends.partition_point(|&e| e <= h.end());
        let end = match ends.get(i) {
            Some(&e) if e <= limit => e,
            _ => {
                // Cut short: the key-shaped lines after the header.
                let mut e = h.end();
                let mut lines = 0;
                while lines < KEY_LINES && e < limit {
                    let mut p = e;
                    while p < limit && matches!(bytes[p], b' ' | b'\t') {
                        p += 1;
                    }
                    if p < limit && bytes[p] == b'\r' {
                        p += 1;
                    }
                    if p >= limit || bytes[p] != b'\n' {
                        break;
                    }
                    p += 1;
                    while p < limit && matches!(bytes[p], b' ' | b'\t') {
                        p += 1;
                    }
                    let q = p;
                    while p < limit && key_body_char(bytes[p]) {
                        p += 1;
                    }
                    e = p;
                    lines += 1;
                    if q == p || (p < limit && bytes[p] != b'\n' && bytes[p] != b'\r') {
                        break;
                    }
                }
                e
            }
        };
        out.push((h.start(), end));
        at = end.max(h.end());
        if at >= text.len() {
            break;
        }
    }
    Ok(out)
}

/// The placeholder for a kind.
pub fn placeholder_for(kind: &str) -> String {
    format!("<redacted:{kind}>")
}

/// The time [`scrub_within`] is given for `len` bytes of text: a floor,
/// and more in proportion to the text, far above what any text needs
/// (the work is linear: about 0.1 s per MiB, optimised).
pub fn budget_for(len: usize) -> std::time::Duration {
    std::time::Duration::from_millis(BUDGET_FLOOR_MS + (len as u64).saturating_mul(BUDGET_MS_PER_MIB) / (1024 * 1024))
}

/// [`budget_for`]'s floor.
pub const BUDGET_FLOOR_MS: u64 = 2_000;
/// [`budget_for`]'s allowance per MiB of text.
pub const BUDGET_MS_PER_MIB: u64 = 20_000;

/// The scrubber ran out of its time (`scrub_timeout`): what it was given.
#[derive(Debug, Clone, PartialEq)]
pub struct Exhausted {
    pub budget_ms: u64,
    pub bytes: usize,
}

/// **Scrub one text**: the text with every match replaced, and the count by
/// kind. A `high_entropy` match must stand alone (no base64 character just
/// before or after it). Linear in the text; no time limit (see
/// [`scrub_within`]).
pub fn scrub(text: &str) -> (String, Counts) {
    match scrub_by(text, None) {
        Ok(x) => x,
        Err(_) => unreachable!("no deadline, no exhaustion"),
    }
}

/// [`scrub`], given until `deadline`: `Err` when it was not done by then,
/// and the text must then not be sent.
pub fn scrub_within(text: &str, deadline: std::time::Instant) -> Result<(String, Counts), Exhausted> {
    let started = std::time::Instant::now();
    scrub_by(text, Some(deadline)).map_err(|()| Exhausted { budget_ms: deadline.saturating_duration_since(started).as_millis() as u64, bytes: text.len() })
}

fn scrub_by(text: &str, deadline: Option<std::time::Instant>) -> Result<(String, Counts), ()> {
    let late = || deadline.is_some_and(|d| std::time::Instant::now() > d);
    // Every match of every rule is kept, `(start, end, rank, kind)`: the rank
    // is its rule's place in the order tried (private keys first). None is
    // ever dropped for overlapping another; overlaps are resolved after,
    // by redacting their union ([`merge`]).
    let mut spans: Vec<(usize, usize, usize, &'static str)> = Vec::new();
    for (s, e) in private_keys(text, deadline)? {
        spans.push((s, e, 0, "private_key"));
    }
    let digests = digest_spans(text);
    for (rank, rule) in rules().iter().enumerate() {
        if late() {
            return Err(());
        }
        let mut n = 0u32;
        match rule.check {
            Check::Whole(check) => {
                for m in rule.re.find_iter(text) {
                    n += 1;
                    if n % 256 == 0 && late() {
                        return Err(());
                    }
                    if !check(m.as_str()) {
                        continue;
                    }
                    if rule.kind == "high_entropy" && within(&digests, m.start(), m.end()) {
                        continue;
                    }
                    if let Some(extra) = rule.alone {
                        let joined = |ch: Option<char>| ch.is_some_and(|ch| ch.is_ascii_alphanumeric() || extra.contains(ch));
                        if joined(text[..m.start()].chars().next_back()) || joined(text[m.end()..].chars().next()) {
                            continue;
                        }
                    }
                    spans.push((m.start(), m.end(), rank + 1, rule.kind));
                }
            }
            Check::Caps(check) => {
                for c in rule.re.captures_iter(text) {
                    n += 1;
                    if n % 64 == 0 && late() {
                        return Err(());
                    }
                    if !check(&c) {
                        continue;
                    }
                    let m = if rule.group.is_empty() { c.get(0) } else { c.name(rule.group) };
                    if let Some(m) = m.filter(|m| m.start() < m.end()) {
                        spans.push((m.start(), m.end(), rank + 1, rule.kind));
                    }
                }
            }
        }
    }
    let mut out = String::with_capacity(text.len());
    let mut counts = Counts::new();
    let mut at = 0;
    for (s, e, kind, inner) in merge(spans) {
        out.push_str(&text[at..s]);
        out.push_str(&placeholder_for(kind));
        for (k, n) in inner {
            *counts.entry(k.to_string()).or_insert(0) += n;
        }
        at = e;
    }
    out.push_str(&text[at..]);
    Ok((out, counts))
}

/// **Overlapping matches, as their union.** `spans` (any order) grouped into
/// runs of matches that overlap or touch; each run is redacted whole, as one
/// placeholder named after its outermost match (the longest; of two as long,
/// the earlier rule's), so a secret inside another (a token in a `Cookie:`
/// value, a key in a quoted passphrase) never lets the rest of the outer one
/// through. Matches of the very same text by two rules are one match, the
/// earlier rule's. Each run's counts: one per match of each kind (matches
/// of one kind that overlap count once), except that the generic `high_entropy` is
/// not counted inside another kind's run (it only names what no other rule
/// does). Sorting makes it `n log n` in the matches.
fn merge(mut spans: Vec<(usize, usize, usize, &'static str)>) -> Vec<(usize, usize, &'static str, Vec<(&'static str, u64)>)> {
    spans.sort_by_key(|&(s, e, rank, _)| (s, std::cmp::Reverse(e), rank));
    // Two rules matching the very same text found one secret: the earlier
    // rule names it (an `sk-ant-…` key is also `sk-…`-shaped).
    spans.dedup_by(|b, a| (a.0, a.1) == (b.0, b.1));
    let mut runs: Vec<Vec<(usize, usize, usize, &'static str)>> = Vec::new();
    let mut end = 0;
    for sp in spans {
        match runs.last_mut() {
            Some(run) if sp.0 <= end => {
                end = end.max(sp.1);
                run.push(sp);
            }
            _ => {
                end = sp.1;
                runs.push(vec![sp]);
            }
        }
    }
    runs.into_iter()
        .map(|run| {
            let start = run.iter().map(|x| x.0).min().unwrap_or(0);
            let stop = run.iter().map(|x| x.1).max().unwrap_or(0);
            let outer = run.iter().min_by_key(|x| (std::cmp::Reverse(x.1 - x.0), x.2)).map(|x| x.3).unwrap_or("high_entropy");
            // Per kind, the matches of that kind merged among themselves.
            let mut per: BTreeMap<&'static str, (u64, usize)> = BTreeMap::new();
            for &(s, e, _, k) in &run {
                if k == "high_entropy" && outer != "high_entropy" {
                    continue;
                }
                let x = per.entry(k).or_insert((0, 0));
                if x.0 == 0 || s >= x.1 {
                    x.0 += 1;
                }
                x.1 = x.1.max(e);
            }
            let inner: Vec<(&'static str, u64)> = per.into_iter().map(|(k, (n, _))| (k, n)).collect();
            (start, stop, outer, inner)
        })
        .collect()
}

/// Add `b`'s counts to `a`.
pub fn add(a: &mut Counts, b: &Counts) {
    for (k, n) in b {
        *a.entry(k.clone()).or_insert(0) += n;
    }
}

/// **Scrub every string in a JSON value** (keys are left as they are), in
/// place; the counts.
pub fn scrub_value(v: &mut Value) -> Counts {
    let mut counts = Counts::new();
    match v {
        Value::String(s) => {
            let (t, c) = scrub(s);
            if !c.is_empty() {
                *s = t;
                add(&mut counts, &c);
            }
        }
        Value::Array(a) => {
            for x in a {
                add(&mut counts, &scrub_value(x));
            }
        }
        Value::Object(m) => {
            for x in m.values_mut() {
                add(&mut counts, &scrub_value(x));
            }
        }
        _ => {}
    }
    counts
}

/// [`scrub_value`], given until `deadline`.
pub fn scrub_value_within(v: &mut Value, deadline: std::time::Instant) -> Result<Counts, Exhausted> {
    let mut counts = Counts::new();
    match v {
        Value::String(s) => {
            let (t, c) = scrub_within(s, deadline)?;
            if !c.is_empty() {
                *s = t;
                add(&mut counts, &c);
            }
        }
        Value::Array(a) => {
            for x in a {
                add(&mut counts, &scrub_value_within(x, deadline)?);
            }
        }
        Value::Object(m) => {
            for x in m.values_mut() {
                add(&mut counts, &scrub_value_within(x, deadline)?);
            }
        }
        _ => {}
    }
    Ok(counts)
}

/// The total bytes of the strings in a JSON value.
pub fn value_bytes(v: &Value) -> usize {
    match v {
        Value::String(s) => s.len(),
        Value::Array(a) => a.iter().map(value_bytes).sum(),
        Value::Object(m) => m.values().map(value_bytes).sum(),
        _ => 0,
    }
}

/// Every kind, in the order tried.
pub fn kinds() -> Vec<&'static str> {
    let mut out = vec!["private_key"];
    for r in rules() {
        if !out.contains(&r.kind) {
            out.push(r.kind);
        }
    }
    out
}
