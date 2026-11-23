//! **The secret scrubber's corpus.** Every known format, generated at run
//! time from a fixed seed (no secret-shaped literal sits in this file:
//! each is assembled from a prefix in pieces and random characters), in
//! several surroundings: every one must be replaced, none missed. Then
//! ordinary prose and code, this repository's own docs and sources, which
//! hold no secret: every replacement there is a false positive, counted and
//! held under a ceiling. And the stored dump is never changed by a run that
//! scrubs what it sends.

use serde_json::json;

use cauldron_ripples::scrub;

use crate::support::*;

/// A small deterministic generator (xorshift).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn pick(&mut self, set: &str, n: usize) -> String {
        let b = set.as_bytes();
        (0..n).map(|_| b[(self.next() % b.len() as u64) as usize] as char).collect()
    }
}

const UPPER: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZ";
const LOWER: &str = "abcdefghijklmnopqrstuvwxyz";
const DIGIT: &str = "0123456789";

fn alnum() -> String {
    format!("{UPPER}{LOWER}{DIGIT}")
}

fn b64url() -> String {
    format!("{UPPER}{LOWER}{DIGIT}_-")
}

/// Random characters of `set`, with at least two digits, one upper and one
/// lower case letter where the set has them (as real keys do).
fn body(r: &mut Rng, set: &str, n: usize) -> String {
    loop {
        let s = r.pick(set, n);
        let ok = |pool: &str| !set.chars().any(|c| pool.contains(c)) || s.chars().any(|c| pool.contains(c));
        if ok(UPPER) && ok(LOWER) && (!set.contains('0') || s.chars().filter(|c| c.is_ascii_digit()).count() >= 2.min(n)) {
            return s;
        }
    }
}

/// One generated sample: the text it is in, the secret part that must not
/// survive, and the kind expected.
struct Sample {
    text: String,
    secret: String,
    kind: &'static str,
}

fn p(parts: &[&str]) -> String {
    parts.concat()
}

/// Every format, `n` samples each, in turning surroundings.
fn samples(n: usize) -> Vec<Sample> {
    let mut r = Rng(0x5eed_cafe_f00d_1234);
    let mut out = Vec::new();
    let wraps: [fn(&str) -> String; 5] = [
        |s: &str| s.to_string(),
        |s: &str| format!("here it is: {s} and that is all"),
        |s: &str| format!("{{\"value\": \"{s}\"}}"),
        |s: &str| format!("paste `{s}`\nnext line"),
        |s: &str| format!("(see {s}.)"),
    ];
    for i in 0..n {
        let w = wraps[i % wraps.len()];
        let mut whole = |kind: &'static str, secret: String| out.push(Sample { text: w(&secret), secret, kind });
        whole("anthropic_key", p(&["sk", "-ant-", "api03-", &body(&mut r, &b64url(), 80)]));
        whole("openai_key", p(&["sk", "-proj-", &body(&mut r, &b64url(), 48)]));
        whole("openai_key", p(&["sk", "-", &body(&mut r, &alnum(), 48)]));
        whole("aws_access_key_id", p(&["AK", "IA", &body(&mut r, &format!("{UPPER}{DIGIT}"), 16)]));
        whole("aws_access_key_id", p(&["AS", "IA", &body(&mut r, &format!("{UPPER}{DIGIT}"), 16)]));
        whole("google_api_key", p(&["AI", "za", &body(&mut r, &b64url(), 35)]));
        whole("google_oauth_token", p(&["ya", "29.", &body(&mut r, &b64url(), 60)]));
        for pre in ["ghp", "gho", "ghu", "ghs", "ghr"] {
            whole("github_token", p(&[pre, "_", &body(&mut r, &alnum(), 36)]));
        }
        whole("github_token", p(&["github", "_pat_", &body(&mut r, &format!("{}_", alnum()), 82)]));
        whole("gitlab_token", p(&["gl", "pat-", &body(&mut r, &b64url(), 20)]));
        for pre in ["xoxb", "xoxp", "xoxa"] {
            whole("slack_token", p(&[pre, "-", &body(&mut r, DIGIT, 12), "-", &body(&mut r, DIGIT, 12), "-", &body(&mut r, &alnum(), 24)]));
        }
        whole("slack_webhook", p(&["https://hooks.", "slack.com/services/T", &body(&mut r, &format!("{UPPER}{DIGIT}"), 10), "/B", &body(&mut r, &format!("{UPPER}{DIGIT}"), 10), "/", &body(&mut r, &alnum(), 24)]));
        whole("stripe_key", p(&["sk", "_live_", &body(&mut r, &alnum(), 24)]));
        whole("stripe_key", p(&["rk", "_test_", &body(&mut r, &alnum(), 24)]));
        whole("npm_token", p(&["npm", "_", &body(&mut r, &alnum(), 36)]));
        whole("pypi_token", p(&["pypi", "-AgE", "IcHlwaS5vcmc", &body(&mut r, &b64url(), 70)]));
        whole("huggingface_token", p(&["hf", "_", &body(&mut r, &format!("{UPPER}{LOWER}"), 34)]));
        whole("sendgrid_key", p(&["SG", ".", &body(&mut r, &b64url(), 22), ".", &body(&mut r, &b64url(), 43)]));
        whole("digitalocean_token", p(&["dop", "_v1_", &r.pick("0123456789abcdef", 64)]));
        whole("shopify_token", p(&["shp", "at_", &r.pick("0123456789abcdef", 32)]));
        let jwt = p(&["ey", "J", &body(&mut r, &b64url(), 30), ".ey", "J", &body(&mut r, &b64url(), 60), ".", &body(&mut r, &b64url(), 43)]);
        whole("jwt", jwt);
        whole("high_entropy", body(&mut r, &format!("{}+/", alnum()), 40));
        whole("high_entropy", body(&mut r, &b64url(), 64));
        // Private key blocks, whole and cut short.
        for label in ["RSA ", "EC ", "OPENSSH ", "", "ENCRYPTED "] {
            let lines: Vec<String> = (0..6).map(|_| body(&mut r, &format!("{}+/", alnum()), 64)).collect();
            let block = p(&["-----BEGIN ", label, "PRIVATE", " KEY-----\n", &lines.join("\n"), "\n-----END ", label, "PRIVATE", " KEY-----"]);
            out.push(Sample { text: format!("key file:\n{block}\nthe end"), secret: lines[3].clone(), kind: "private_key" });
        }
        let lines: Vec<String> = (0..4).map(|_| body(&mut r, &format!("{}+/", alnum()), 64)).collect();
        out.push(Sample { text: p(&["-----BEGIN ", "PGP PRIVATE", " KEY BLOCK-----\n\n", &lines.join("\n")]), secret: lines[2].clone(), kind: "private_key" });
        // The value only: connection strings, headers, assignments.
        let pw = body(&mut r, &alnum(), 14);
        for scheme in ["postgres", "mysql", "mongodb+srv", "redis", "amqp", "https"] {
            out.push(Sample { text: format!("url = {scheme}://app_user:{pw}@db.internal.example:5432/main"), secret: pw.clone(), kind: "connection_password" });
        }
        let tok = body(&mut r, &b64url(), 28);
        out.push(Sample { text: format!("curl -H \"Authorization: Bearer {tok}\" https://api.example.test"), secret: tok.clone(), kind: "authorization_header" });
        out.push(Sample { text: format!("Authorization: Basic {tok}"), secret: tok.clone(), kind: "authorization_header" });
        out.push(Sample { text: format!("proxy-authorization: token {tok}"), secret: tok.clone(), kind: "authorization_header" });
        let short = body(&mut r, &alnum(), 12);
        for name in ["DB_PASSWORD", "API_TOKEN", "CLIENT_SECRET", "AWS_SECRET_ACCESS_KEY", "STRIPE_API_KEY", "GITHUB_TOKEN", "PASSWORD", "SECRET"] {
            out.push(Sample { text: format!("{name}={short}"), secret: short.clone(), kind: "secret_assignment" });
            out.push(Sample { text: format!("export {name}=\"{short}\""), secret: short.clone(), kind: "secret_assignment" });
        }
        out.push(Sample { text: format!("{{\"password\": \"{short}\", \"user\": \"app\"}}"), secret: short.clone(), kind: "secret_assignment" });
        out.push(Sample { text: format!("  api_key: '{short}'"), secret: short.clone(), kind: "secret_assignment" });
        out.push(Sample { text: format!("password = \"{short}\""), secret: short.clone(), kind: "secret_assignment" });
        out.push(Sample { text: format!("password={short}"), secret: short.clone(), kind: "secret_assignment" });
        out.push(Sample { text: format!("Cookie: session={tok}; theme=dark"), secret: tok.clone(), kind: "cookie_header" });
        out.push(Sample { text: format!("deploy --password {short} --yes"), secret: short.clone(), kind: "command_line_secret" });
        out.push(Sample { text: format!("mysql -u root -p{short} main"), secret: short.clone(), kind: "command_line_secret" });
    }
    out
}

/// **Zero misses on the known formats.** Every sample's secret is gone,
/// replaced by a placeholder; the format-specific ones by their own kind.
#[test]
fn sc1_every_known_format_is_replaced() {
    let all = samples(20);
    let mut misses = Vec::new();
    let mut by_kind: std::collections::BTreeMap<&str, (u32, u32)> = Default::default();
    for s in &all {
        let (out, counts) = scrub::scrub(&s.text);
        let e = by_kind.entry(s.kind).or_default();
        e.0 += 1;
        if out.contains(&s.secret) || !out.contains(&scrub::placeholder_for(s.kind)) || counts.get(s.kind).copied().unwrap_or(0) == 0 {
            misses.push(format!("{}: {:?} -> {:?}", s.kind, s.text.chars().take(24).collect::<String>(), counts));
        } else {
            e.1 += 1;
        }
    }
    eprintln!("scrub corpus: {} samples, {} kinds; caught per kind: {:?}", all.len(), by_kind.len(), by_kind);
    assert!(misses.is_empty(), "{} missed:\n{}", misses.len(), misses.join("\n"));
    // Every kind the scrubber knows has samples.
    for k in scrub::kinds() {
        assert!(by_kind.contains_key(k), "no sample of `{k}`");
    }
    // A JSON value: every string, keys left alone.
    let secret = samples(1).remove(0).secret;
    let mut v = json!({"records": [{"handle": format!("it was {secret}"), "body": "plain"}], secret.clone(): 1});
    let c = scrub::scrub_value(&mut v);
    assert_eq!(c.get("anthropic_key"), Some(&1));
    assert_eq!(v["records"][0]["handle"], "it was <redacted:anthropic_key>");
}

/// The repository's own prose and code: none of it is a secret, so every
/// replacement is a false positive.
fn corpus() -> Vec<(String, String)> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = Vec::new();
    let mut walk = vec![
        root.join("README.md"),
        root.join("CHANGELOG.md"),
        root.join("ROADMAP.md"),
        root.join("BUILDING.md"),
        root.join("docs"),
        root.join("claude-code/cauldron"),
        root.join("schema"),
        root.join("services/ripples/src"),
        root.join("services/mcp/src"),
        root.join("services/threads/src"),
        root.join("libs/soup-door/src"),
        root.join("libs/spine/src"),
        root.join("libs/env/src"),
        root.join("cli/src"),
    ];
    while let Some(p) = walk.pop() {
        if p.is_dir() {
            for e in std::fs::read_dir(&p).unwrap().flatten() {
                walk.push(e.path());
            }
            continue;
        }
        let name = p.display().to_string();
        let ok = (name.ends_with(".md") || name.ends_with(".rs")) && !name.ends_with("scrub.rs");
        if ok {
            if let Ok(t) = std::fs::read_to_string(&p) {
                files.push((name, t));
            }
        }
    }
    files.sort();
    files
}

/// **The false-positive rate on ordinary prose and code**, measured and
/// held: under one replacement per 100,000 characters of prose, and per
/// 100,000 of code.
#[test]
fn sc2_ordinary_prose_and_code_are_left_alone() {
    let files = corpus();
    let mut stats: std::collections::BTreeMap<&str, (usize, u64, usize)> = Default::default();
    let mut hits = Vec::new();
    for (name, text) in &files {
        let class = if name.ends_with(".md") { "prose" } else { "code" };
        let (out, counts) = scrub::scrub(text);
        let n: u64 = counts.values().sum();
        // `SCRUB_SHOW_HITS=1` prints each line with a replacement.
        if n > 0 && std::env::var_os("SCRUB_SHOW_HITS").is_some() {
            for l in out.lines().filter(|l| l.contains("<redacted:")) {
                eprintln!("    {}: {}", name.rsplit("../").next().unwrap_or(name), l.trim().chars().take(160).collect::<String>());
            }
        }
        let e = stats.entry(class).or_default();
        e.0 += 1;
        e.1 += n;
        e.2 += text.chars().count();
        if n > 0 {
            hits.push(format!("{class} {}: {counts:?}", name.rsplit("../").next().unwrap_or(name)));
        }
    }
    for (class, (nfiles, n, chars)) in &stats {
        let per = *n as f64 * 100_000.0 / *chars as f64;
        eprintln!("scrub false positives, {class}: {n} in {nfiles} files, {chars} characters: {per:.3} per 100,000 characters");
    }
    for h in &hits {
        eprintln!("  {h}");
    }
    for (class, (_, n, chars)) in &stats {
        assert!(*chars > 200_000, "{class}: too little corpus");
        assert!((*n as f64) * 100_000.0 / (*chars as f64) < 1.0, "{class}: {n} false positives in {chars} characters");
    }
}

/// **The stored dump is never changed**: a run sends the scrubbed text and
/// journals the counts (never the text); the dump reads back as written.
#[test]
fn sc3_a_run_sends_scrubbed_text_and_the_dump_is_unchanged() {
    let s = Scratch::new("sc3");
    let door = Door::start(&s);
    let key = p(&["sk", "-ant-", "api03-", &Rng(7).pick(&b64url(), 60)]);
    let pw = Rng(11).pick(&alnum(), 14);
    let text = format!("the deploy failed; the key I pasted was {key}\nDB_PASSWORD={pw}");
    let id = door.dump(&text);
    let script = s.script("sc3", json!([{"output": {"records": [{"label": "n1", "type": "issue", "state": "open", "handle": format!("deploy failed with {key}"), "regions": ["R-1"]}]}}]));
    let ctx = engine(&s, &door, &script, cauldron_ripples::engine::Caps::default());
    let out = cauldron_ripples::engine::run(&ctx, &ask(&id));
    assert_eq!(out["outcome"], "processed", "{out}");
    let prompt = calls(&script)[0]["prompt"].as_str().unwrap().to_string();
    assert!(!prompt.contains(&key) && !prompt.contains(&pw), "sent unscrubbed");
    assert!(prompt.contains("<redacted:anthropic_key>") && prompt.contains("DB_PASSWORD=<redacted:secret_assignment>"), "{prompt}");
    // The answer's copy is scrubbed before it is written.
    let records: Vec<String> = out["records"].as_array().unwrap().iter().map(|x| x.as_str().unwrap().to_string()).collect();
    let got = soup_door::reads::get(&s.target(), &records, &Default::default()).unwrap();
    assert_eq!(got["records"][0]["handle"], "deploy failed with <redacted:anthropic_key>");
    // Counts in the journal, never the text.
    let red: Vec<serde_json::Value> = s.journal().into_iter().filter(|l| l["event"] == "redacted").collect();
    assert_eq!(red[0]["in"], "sent");
    assert_eq!((red[0]["counts"]["anthropic_key"].clone(), red[0]["counts"]["secret_assignment"].clone()), (json!(1), json!(1)));
    assert_eq!((red[1]["in"].clone(), red[1]["counts"]["anthropic_key"].clone()), (json!("answer"), json!(1)));
    let journal = std::fs::read_to_string(s.ripples_dir().join("runs.jsonl")).unwrap();
    assert!(!journal.contains(&key) && !journal.contains(&pw));
    // The dump itself: as written.
    assert_eq!(soup_door::dumps::text_of(&s.target().cauldron, &id).unwrap(), text);
}

/// **A local model**: scrubbed like threads by default; sent the text as it
/// is only with `--no-scrub-local` (nothing then leaves the machine). The
/// answer is scrubbed either way.
#[test]
fn sc4_local_is_scrubbed_unless_the_owner_said_not_to() {
    use cauldron_ripples::engine::{self, Caps, Opts};
    for no_scrub in [false, true] {
        let s = Scratch::new(&format!("sc4-{no_scrub}"));
        let door = Door::start(&s);
        let key = p(&["gh", "p_", &Rng(13).pick(&alnum(), 36)]);
        let id = door.dump(&format!("the CI token was {key}"));
        let script = s.script("sc4", json!([{"output": {"records": [{"label": "n1", "type": "idea", "state": "idea", "handle": format!("rotate {key}"), "regions": ["R-1"]}]}}]));
        let ctx = engine(&s, &door, &script, Caps::default());
        let mut a = ask(&id);
        a.model = Some("local:tiny".into());
        let p = engine::prepare(&ctx, &a).unwrap();
        let out = engine::execute_with(&ctx, &p, &Opts { no_scrub_local: no_scrub, ..Opts::default() });
        assert_eq!(out["outcome"], "processed", "{out}");
        let prompt = calls(&script)[0]["prompt"].as_str().unwrap().to_string();
        assert_eq!(prompt.contains(&key), no_scrub, "no_scrub_local {no_scrub}");
        let records: Vec<String> = out["records"].as_array().unwrap().iter().map(|x| x.as_str().unwrap().to_string()).collect();
        let got = soup_door::reads::get(&s.target(), &records, &Default::default()).unwrap();
        assert_eq!(got["records"][0]["handle"], "rotate <redacted:github_token>");
    }
}

/// The adversarial inputs: each `n` bytes or a little more.
fn adversarial(n: usize) -> Vec<(&'static str, String)> {
    let fill = |unit: &str| unit.repeat(n / unit.len() + 1);
    let begin = p(&["-----BEGIN ", "PRIVATE", " KEY-----"]);
    let end = p(&["-----END ", "PRIVATE", " KEY-----"]);
    let mut r = Rng(0x0dd_ba11);
    vec![
        ("unterminated key headers (the reported input)", fill(&format!("{begin} "))),
        ("key headers each with one body line, no END", fill(&format!("{begin}\nAbCdEfGh0123456789+/abcdefghij\n"))),
        ("END lines only", fill(&format!("{end}\n"))),
        ("one line of one letter", "a".repeat(n)),
        ("one line of random base64", r.pick(&format!("{}+/", alnum()), n)),
        ("one assignment with a 1 MiB value", format!("password={}", "a".repeat(n))),
        ("near-miss assignments", fill("password password: token= secret ")),
        ("near-miss key prefixes", fill(&p(&["sk", "-ant- ", "gh", "p_ ", "AK", "IA ", "eyJ. "]))),
        ("near-miss headers and URLs", fill("Authorization: postgres://u:p https://x@ ")),
    ]
}

fn time_of(text: &str) -> f64 {
    let t = std::time::Instant::now();
    let _ = scrub::scrub(text);
    t.elapsed().as_secs_f64()
}

/// **The scrubber is linear in its input.** Each adversarial input of 1 MiB
/// (the text-dump limit) is scrubbed in a small multiple of the time plain
/// prose of the same size takes, well inside the time budget a text of
/// that size is given; and twice the input takes about twice the time.
#[test]
fn sc5_adversarial_inputs_are_scrubbed_in_linear_time() {
    let mib = 1024 * 1024;
    let prose = "The cache key should hash the whole tree, not only the files that changed; a rebase moves them all. ".repeat(mib / 100 + 1);
    let base = time_of(&prose).max(0.01);
    eprintln!("scrub throughput: plain prose, 1 MiB: {:.3} s", base);
    let budget = scrub::budget_for(mib).as_secs_f64();
    for ((what, one), (_, two)) in adversarial(mib).into_iter().zip(adversarial(2 * mib)) {
        let t1 = time_of(&one);
        let t2 = time_of(&two);
        eprintln!("scrub throughput: {what}: 1 MiB {t1:.3} s ({:.1}x prose), 2 MiB {t2:.3} s", t1 / base);
        assert!(t1 < budget / 4.0, "{what}: {t1:.3} s for 1 MiB, budget {budget:.1} s");
        assert!(t1 <= 25.0 * base + 0.25, "{what}: {t1:.3} s for 1 MiB against {base:.3} s of prose");
        assert!(t2 <= 3.0 * t1 + 0.25, "{what}: not linear ({t1:.3} s for 1 MiB, {t2:.3} s for 2 MiB)");
    }
}

/// **Out of time, nothing is sent.** With a budget no text can meet, a run
/// fails `scrub_timeout` before any call: the model is never asked, the
/// dump stays `received`; automatic processing parks such a dump at once.
#[test]
fn sc6_a_scrubber_out_of_time_fails_closed() {
    use cauldron_ripples::engine::{self, Caps, Opts};
    let s = Scratch::new("sc6");
    let door = Door::start(&s);
    let id = door.dump(&"a note that takes some scrubbing. ".repeat(2000));
    let script = s.script("sc6", json!([{"output": {"records": []}}]));
    let ctx = engine(&s, &door, &script, Caps::default());
    let p = engine::prepare(&ctx, &ask(&id)).unwrap();
    let out = engine::execute_with(&ctx, &p, &Opts { scrub_budget: Some(std::time::Duration::ZERO), ..Opts::default() });
    assert_eq!((out["outcome"].clone(), out["code"].clone()), (json!("failed"), json!("scrub_timeout")), "{out}");
    assert_eq!(out["scrub"]["of"], "sent", "{out}");
    assert!(calls(&script).is_empty(), "nothing was sent");
    assert_eq!(s.dump_state(&id)["state"], "received");
    let x = scrub::scrub_within("plain", std::time::Instant::now() - std::time::Duration::from_secs(1));
    assert!(x.is_err());
}

/// The shapes found missing after 0.1.12's first scrubber: a secret in an
/// assignment or on a command line written in other forms. Each is
/// `(shape, text, secret)`, built at run time.
fn other_forms(r: &mut Rng) -> Vec<(&'static str, String, String)> {
    let mut out = Vec::new();
    let pw = |r: &mut Rng, n: usize| body(r, &alnum(), n);
    let mut add = |shape: &'static str, text: String, secret: String| out.push((shape, text, secret));
    let v = pw(r, 12);
    add("lower-case name=value", format!("password={v}"), v);
    let v = pw(r, 12);
    add("dotted property", format!("spring.datasource.password={v}"), v);
    let v = pw(r, 12);
    add("export, lower-case", format!("export db_password={v}"), v);
    let v = pw(r, 16);
    add("INI, spaces", format!("api_key = {v}"), v);
    let v = pw(r, 12);
    add("flag --password=", format!("app --user admin --password={v} --verbose"), v);
    let v = pw(r, 12);
    add("flag --password", format!("app --password {v} --verbose"), v);
    let v = pw(r, 24);
    add("flag --token", format!("cli login --token {v}"), v);
    let v = pw(r, 20);
    add("flag --api-key=", format!("tool --api-key={v}"), v);
    let v = pw(r, 20);
    add("flag --client-secret", format!("tool --client-secret \"{v}\""), v);
    let v = pw(r, 12);
    add("mysql -p", format!("mysql -u root -p{v} main"), v);
    let v = pw(r, 12);
    add("mysql -p quoted", format!("mysqldump -h db -p'{v}' main"), v);
    let v = pw(r, 12);
    add("sshpass -p", format!("sshpass -p {v} ssh deploy@host"), v);
    let v = pw(r, 7);
    add("YAML, short value", format!("db:\n  password: {v}\n  user: app"), v);
    let v = r.pick(LOWER, 22);
    add("YAML, one long word", format!("password: {v}\n"), v);
    let words: Vec<String> = (0..4).map(|_| r.pick(LOWER, 6)).collect();
    let v = words.join(" ");
    add("quoted passphrase", format!("{{\"password\": \"{v}\"}}"), v);
    let v = words[..3].join(" ");
    add("passphrase after a key, unquoted", format!("passphrase: {v}\n"), v);
    let v = pw(r, 12);
    add("JSON key pwd", format!("{{\"pwd\": \"{v}\"}}"), v);
    let v = pw(r, 12);
    add("JSON key pass", format!("{{\"user\": \"app\", \"pass\": \"{v}\"}}"), v);
    let v = pw(r, 12);
    add("Authorization, no scheme, short", format!("Authorization: {v}"), v);
    let v = pw(r, 30);
    add("Cookie header", format!("Cookie: session={v}; theme=dark"), v);
    let v = pw(r, 24);
    add("URL userinfo, token only", format!("git clone https://{v}@git.example.test/team/repo.git"), v);
    let v = pw(r, 20);
    add("URL query api_key", format!("GET https://api.example.test/v1/items?api_key={v}&page=2"), v);
    let v = pw(r, 20);
    add("URL query token", format!("https://example.test/hook?token={v}"), v);
    let v = body(r, &b64url(), 40);
    add("deep URL path", format!("https://files.example.test/a/b/c/d/e/f/g/h/i/{v}"), v);
    // Long flags whose name holds a credential word as one of its parts.
    let v = pw(r, 14);
    add("flag --admin-password", format!("az vm create --admin-username azureuser --admin-password {v}"), v);
    let v = pw(r, 14);
    add("flag --db-password=", format!("app migrate --db-password={v}"), v);
    let v = pw(r, 14);
    add("flag --admin-password, quoted", format!("grafana-cli admin reset --admin-password \"{v}\""), v);
    let v = pw(r, 14);
    add("flag --manager-password", format!("tool --manager-password {v}"), v);
    let v = pw(r, 30);
    add("flag --access-token", format!("cli --access-token {v} --region eu"), v);
    let v = pw(r, 30);
    add("flag --storage-account-key", format!("az storage blob list --storage-account-key {v}"), v);
    let v = pw(r, 24);
    add("flag --client-secret=, single-quoted", format!("tool --client-secret='{v}'"), v);
    let v = pw(r, 24);
    add("flag --aws-secret-access-key", format!("tool --aws-secret-access-key {v}"), v);
    let v = r.pick(DIGIT, 9);
    add("flag --password, digits", format!("tool --password {v}"), v);
    let v = r.pick(DIGIT, 10);
    add("ADMIN_PASSWORD, digits", format!("ADMIN_PASSWORD={v}"), v);
    let v = r.pick(DIGIT, 5);
    add("PASSWORD, five digits", format!("PASSWORD={v}"), v);
    let v = pw(r, 12);
    add("-p quoted after another tool", format!("sometool -u root -p'{v}' main"), v);
    let v = format!("{}{}", r.pick(UPPER, 1), r.pick(LOWER, 15));
    add("YAML, one capitalised word", format!("password: {v}\n"), v);
    let lines: Vec<String> = (0..3).map(|_| body(r, &format!("{}+/", alnum()), 64)).collect();
    let last = body(r, &alnum(), 12);
    let block = p(&["----BEGIN ", "PRIVATE", " KEY----\n", &lines.join("\n"), "\n", &last, "\n----END ", "PRIVATE", " KEY----"]);
    add("PEM header with four dashes", block, last);
    let lines: Vec<String> = (0..3).map(|_| body(r, &format!("{}+/", alnum()), 64)).collect();
    let last = body(r, &alnum(), 12);
    let block = p(&["-----begin ", "rsa private", " key-----\n", &lines.join("\n"), "\n", &last, "\n"]);
    add("PEM header in lower case", block, last);
    out
}

/// Text that holds no secret, in shapes close to the ones above.
fn look_alikes() -> Vec<&'static str> {
    vec![
        "TOKEN_LIMIT=4096",
        "export MAX_TOKENS=1000",
        "PASSWORD_MIN_LENGTH=8",
        "let password = read_password()?;",
        "    token: Option<String>,",
        "fn check(password: &str) -> bool {",
        "if password == expected { return true; }",
        "password = getpass.getpass()",
        "docker login --password-stdin --username app",
        "tool --token-file /run/secrets/token",
        "pass the token to the next call, then check the password field",
        "the password: see the vault entry for this host",
        "Authorization: required",
        "mysql -p main",
        "Cookie: none",
        "git clone https://git.example.test/team/repo.git",
        "the secret: patience",
        "token=$API_TOKEN",
        "password=${DB_PASSWORD}",
        "--password <your password>",
        "export TOKEN_TTL=3600",
        "\"token\": null",
        "aws apigateway get-usage --api-key-id staging-key-two",
        "tool --password-file /run/secrets/db --yes",
        "tool --token-url https://auth.example.test/oauth/token",
        "tool --secret-name prod-database-password",
        "tool --token-ttl 3600",
        "tool --password-min-length 12",
        "llm --max-tokens 4096",
        "tool --no-password --yes",
        "ssh-tool --private-key ~/.ssh/id_ed25519",
        "tool --access-token-lifetime 3600",
        "tool --client-secret-env CLIENT_SECRET",
        "TOKEN_COUNT=500",
        "const maxTokens = 8192;",
        "tokenTimeout: 30000",
        "mkdir -p\"$dir\"",
        "password: Required",
    ]
}

/// **Secrets in other forms, and look-alikes.** Every shape of
/// [`other_forms`] is replaced; none of [`look_alikes`] is touched. The
/// numbers are printed for the record.
#[test]
fn sc7_secrets_in_other_forms_are_replaced_and_look_alikes_left() {
    let mut r = Rng(0x0f0f_5eed_2026_0001);
    let mut misses = Vec::new();
    let mut total = 0;
    for _ in 0..10 {
        for (shape, text, secret) in other_forms(&mut r) {
            total += 1;
            let (out, _) = scrub::scrub(&text);
            if out.contains(&secret) {
                misses.push(format!("{shape}: {out:?}"));
            }
        }
    }
    let mut fps = Vec::new();
    for t in look_alikes() {
        let (out, c) = scrub::scrub(t);
        if !c.is_empty() {
            fps.push(format!("{t:?} -> {out:?}"));
        }
    }
    let shapes: std::collections::BTreeSet<&str> = misses.iter().map(|m| m.split(':').next().unwrap_or("")).collect();
    let n_shapes = other_forms(&mut Rng(1)).len();
    eprintln!("scrub other forms: {total} samples of {n_shapes} shapes: {} samples missed, {} shapes missed; look-alikes: {} of {} touched", misses.len(), shapes.len(), fps.len(), look_alikes().len());
    misses.sort();
    misses.dedup_by(|a, b| a.split(':').next() == b.split(':').next());
    for m in &misses {
        eprintln!("  missed {m}");
    }
    for f in &fps {
        eprintln!("  touched {f}");
    }
    assert!(misses.is_empty() && fps.is_empty());
}

/// Base64 of `n` random bytes' length, standard alphabet, padded.
fn b64_digest(r: &mut Rng, bytes: usize) -> String {
    let chars = bytes.div_ceil(3) * 4;
    let pad = (3 - bytes % 3) % 3;
    format!("{}{}", body(r, &format!("{}+/", alnum()), chars - pad), "=".repeat(pad))
}

fn hex(r: &mut Rng, n: usize) -> String {
    r.pick("0123456789abcdef", n)
}

fn uuid(r: &mut Rng) -> String {
    format!("{}-{}-4{}-a{}-{}", hex(r, 8), hex(r, 4), hex(r, 3), hex(r, 3), hex(r, 12))
}

/// Digests and ids in their obvious contexts, which hold no secret:
/// `(shape, text)`, built at run time.
fn digests(r: &mut Rng) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    let v = b64_digest(r, 48);
    out.push(("SRI integrity, sha384", format!("<script src=\"/app.js\" integrity=\"sha384-{v}\" crossorigin=\"anonymous\"></script>")));
    let v = b64_digest(r, 64);
    out.push(("lockfile integrity, sha512", format!("      \"integrity\": \"sha512-{v}\",")));
    let v = b64_digest(r, 32);
    out.push(("Nix hash, sha256", format!("  hash = \"sha256-{v}\";")));
    let v = b64_digest(r, 20);
    out.push(("SRI integrity, sha1", format!("integrity sha1-{v}")));
    let (a, b) = (b64_digest(r, 32), b64_digest(r, 32));
    out.push(("go.sum line", format!("example.test/mod v1.4.2 h1:{a}\nexample.test/mod v1.4.2/go.mod h1:{b}")));
    let v = b64_digest(r, 32);
    out.push(("HTTP Digest header", format!("Digest: sha-256={v}")));
    let v = b64_digest(r, 64);
    out.push(("Repr-Digest header", format!("Repr-Digest: sha-512=:{v}:")));
    let v = b64_digest(r, 300);
    out.push(("data URI, PNG", format!("![logo](data:image/png;base64,{v})")));
    let v = b64_digest(r, 240);
    out.push(("data URI, font in CSS", format!("@font-face {{ src: url(data:font/woff2;base64,{v}) format(\"woff2\"); }}")));
    let v = b64_digest(r, 120);
    out.push(("data URI, SVG", format!("<img src=\"data:image/svg+xml;base64,{v}\">")));
    out.push(("git commit line", format!("commit {}\nAuthor: A Person <a@example.test>", hex(r, 40))));
    out.push(("git short shas", format!("Merge: {} {}\n{} fix the cache key", hex(r, 7), hex(r, 7), hex(r, 12))));
    out.push(("git range", format!("git log {}..{}", hex(r, 40), hex(r, 40))));
    out.push(("sha256sum line", format!("{}  cauldron-0.1.12-aarch64.tar.gz", hex(r, 64))));
    out.push(("image digest", format!("image: registry.example.test/app@sha256:{}", hex(r, 64))));
    out.push(("UUID in JSON", format!("{{\"id\": \"{}\", \"kind\": \"run\"}}", uuid(r))));
    out.push(("UUID request id header", format!("X-Request-Id: {}", uuid(r))));
    out.push(("UUID in prose", format!("the run {} ended at noon", uuid(r))));
    out
}

/// Secrets in or beside the same contexts, which must still be replaced:
/// `(shape, text, secret)`.
fn digest_neighbours(r: &mut Rng) -> Vec<(&'static str, String, String)> {
    let mut out = Vec::new();
    let v = body(r, &format!("{}+/", alnum()), 44);
    out.push(("token beside a digest label", format!("integrity ok; token {v}"), v));
    let v = body(r, &format!("{}+/", alnum()), 50);
    out.push(("not a digest's length after sha256-", format!("sha256-{v}"), v));
    let v = body(r, &format!("{}+/", alnum()), 40);
    out.push(("text data URI", format!("data:text/plain;base64,{v}"), v));
    let v = body(r, &format!("{}+/", alnum()), 43);
    out.push(("h1: inside a word", format!("ch1:{v}"), v));
    let v = b64_digest(r, 32);
    out.push(("digest label glued to a word", format!("key mysha256-{v}"), v.trim_end_matches('=').to_string()));
    let v = b64_digest(r, 32);
    out.push(("digest-length body run on past its length", format!("sha256-{}{}", v.trim_end_matches('='), "Zq9"), v.trim_end_matches('=').to_string()));
    let v = body(r, &alnum(), 40);
    out.push(("secret assignment with a digest label", format!("API_TOKEN=sha256-{v}"), v));
    let v = body(r, &b64url(), 40);
    out.push(("bearer token, UUID-length", format!("Authorization: Bearer {v}"), v));
    out
}

/// **Digests and ids are left, their neighbours still caught.** Every shape
/// of [`digests`] is counted (touched or not); every one of
/// [`digest_neighbours`] must be replaced. Printed for the record.
#[test]
fn sc8_digests_and_ids_in_obvious_contexts_are_left_alone() {
    let mut r = Rng(0x0d16_e575_2026_0008);
    let (mut touched, mut total) = (Vec::new(), 0);
    let mut misses = Vec::new();
    for _ in 0..10 {
        for (shape, text) in digests(&mut r) {
            total += 1;
            let (out, c) = scrub::scrub(&text);
            if !c.is_empty() {
                touched.push(format!("{shape}: {c:?} {}", out.chars().take(100).collect::<String>()));
            }
        }
        for (shape, text, secret) in digest_neighbours(&mut r) {
            if scrub::scrub(&text).0.contains(&secret) {
                misses.push(format!("{shape}: {text}"));
            }
        }
    }
    let n_shapes = digests(&mut Rng(1)).len();
    let shapes: std::collections::BTreeSet<&str> = touched.iter().map(|m| m.split(':').next().unwrap_or("")).collect();
    eprintln!("scrub digests and ids: {total} samples of {n_shapes} shapes: {} touched ({} shapes); neighbours missed: {}", touched.len(), shapes.len(), misses.len());
    touched.sort();
    touched.dedup_by(|a, b| a.split(':').next() == b.split(':').next());
    for t in &touched {
        eprintln!("  touched {t}");
    }
    for m in &misses {
        eprintln!("  missed {m}");
    }
    assert!(misses.is_empty(), "{misses:?}");
    assert!(touched.is_empty(), "{touched:?}");
}

/// Every shape of the corpus as one planted secret: `(shape, text, secret)`,
/// the known formats ([`samples`]) and the other forms ([`other_forms`]).
fn atoms(r: &mut Rng) -> Vec<(String, String, String)> {
    let mut out: Vec<(String, String, String)> = samples(1).into_iter().map(|s| (s.kind.to_string(), s.text, s.secret)).collect();
    out.extend(other_forms(r).into_iter().map(|(shape, t, s)| (shape.to_string(), t, s)));
    out
}

/// Words of ordinary prose around the planted secrets.
fn filler(r: &mut Rng, n: usize) -> String {
    let words = ["the", "build", "failed", "again", "after", "we", "moved", "cache", "to", "a", "new", "disk", "and", "then", "it", "worked", "note", "for", "later"];
    (0..n).map(|_| words[(r.next() % words.len() as u64) as usize]).collect::<Vec<_>>().join(" ")
}

/// **Does any part of a planted secret survive?** Every run of 8 characters
/// of each secret (a shorter secret whole) that is in `out`, unless the text
/// around the secrets holds it too (a scheme, a prefix of the context).
fn survivors(text: &str, out: &str, secrets: &[String]) -> Vec<String> {
    let mut ctx = text.to_string();
    for s in secrets {
        ctx = ctx.replace(s.as_str(), "\u{1}");
    }
    let mut hits = Vec::new();
    for s in secrets {
        let cs: Vec<char> = s.chars().collect();
        let windows: Vec<String> = if cs.len() < 8 { vec![s.clone()] } else { cs.windows(8).map(|w| w.iter().collect()).collect() };
        if let Some(w) = windows.into_iter().find(|w| out.contains(w.as_str()) && !ctx.contains(w.as_str())) {
            hits.push(w);
        }
    }
    hits
}

/// **Nested and overlapped, nothing survives.** Every corpus shape planted
/// in random prose beside every other (overlapping lines and neighbours,
/// in either order), and inside each shape that holds a value of its own
/// (a `Cookie:` header, a quoted passphrase, a quoted flag value, a header
/// token, an unquoted assignment, a URL's query and password), around
/// secret words of that outer shape: no run of 8 characters of any planted
/// secret (the outer's words included) is left. A shape is nested where the
/// outer's own syntax can hold it as written (a line in a quoted value only
/// when it has no such quote; in a YAML passphrase only words that do not
/// read as code; a bare token only where a token goes).
#[test]
fn sc9_nested_and_overlapping_secrets_leave_nothing() {
    let mut r = Rng(0x9e57_ed00_2026_0009);
    let all = atoms(&mut r);
    let mut fails: Vec<String> = Vec::new();
    let mut n = 0usize;
    let mut check = |shape: String, text: String, secrets: Vec<String>, fails: &mut Vec<String>| {
        n += 1;
        let (out, _) = scrub::scrub(&text);
        let s = survivors(&text, &out, &secrets);
        if !s.is_empty() {
            fails.push(format!("{shape}: {:?} -> {:?}", text.chars().take(160).collect::<String>(), out.chars().take(160).collect::<String>()));
        }
    };
    // A YAML-style key (`password: …` to the end of the line) is a line of
    // its own: it starts a line wherever it is planted.
    let line_shaped = |shape: &str| matches!(shape, "YAML, one long word" | "YAML, one capitalised word" | "passphrase after a key, unquoted");
    // Side by side, every pairing, three separators, in random prose.
    for (sa, ta, xa) in &all {
        for (sb, tb, xb) in &all {
            for sep in [" ", "; ", "\n"] {
                let sep = if line_shaped(sb) { "\n" } else { sep };
                let text = format!("{}\n{ta}{sep}{tb} {}", filler(&mut r, 3), filler(&mut r, 3));
                check(format!("{sa} + {sb}"), text, vec![xa.clone(), xb.clone()], &mut fails);
            }
        }
    }
    // Inside a value of its own: each outer shape around each corpus shape.
    type Outer = (&'static str, fn(&str, &str, &str) -> String, fn(&str) -> bool);
    let lines: [Outer; 4] = [
        ("Cookie header", |a, inner, b| format!("Cookie: a={a}; t={inner}; sid={b}"), |t| !t.contains('\n')),
        ("quoted passphrase", |a, inner, b| format!("{{\"password\": \"{a} {inner} {b}\"}}"), |t| !t.contains(['\n', '"'])),
        ("quoted flag value", |a, inner, b| format!("tool --client-secret '{a} {inner} {b}' --yes"), |t| !t.contains(['\n', '\''])),
        ("YAML passphrase line", |a, inner, b| format!("db:\n  password: {a} {inner} {b}\n"), |t| !t.contains(['\n', '#', '"', '\'', '(', ')', '<', '>', '[', ']', '&', '{', '}', '`']) && t.split_whitespace().count() <= 4),
    ];
    let tokens: [Outer; 4] = [
        ("bearer token", |a, inner, _| format!("Authorization: Bearer {a}{inner}"), |t| !t.contains(['"', '\'', ',', ';'])),
        ("unquoted assignment", |a, inner, _| format!("API_TOKEN={a}{inner}"), |t| !t.contains(['"', '\'', ',', ';'])),
        ("URL query", |a, inner, _| format!("https://api.example.test/v1?token={a}{inner}&page=2"), |t| !t.contains(['&', '#', '"', '\'', '<', '>'])),
        ("URL password", |a, inner, _| format!("postgres://app:{a}{inner}@db.example.test/main"), |t| !t.contains(['@', '/'])),
    ];
    for (shape, text, secret) in &all {
        for (outer, make, holds) in lines {
            if holds(text) {
                let (a, b) = (body(&mut r, &alnum(), 12), body(&mut r, &alnum(), 12));
                check(format!("{shape} in {outer}"), make(&a, text, &b), vec![secret.clone(), a, b], &mut fails);
            }
        }
        if secret.contains(char::is_whitespace) {
            continue;
        }
        for (outer, make, holds) in tokens {
            if holds(secret) {
                let a = body(&mut r, &alnum(), 12);
                check(format!("{shape} in {outer}"), make(&a, secret, ""), vec![secret.clone(), a], &mut fails);
            }
        }
    }
    let shapes: std::collections::BTreeSet<String> = fails.iter().map(|f| f.split(':').next().unwrap_or("").to_string()).collect();
    eprintln!("scrub nested and overlapping: {n} texts from {} shapes; {} left part of a secret ({} pairings)", all.len(), fails.len(), shapes.len());
    let mut seen = std::collections::BTreeSet::new();
    for f in &fails {
        if seen.insert(f.split(':').next().unwrap_or("").to_string()) {
            eprintln!("  left {f}");
        }
    }
    assert!(fails.is_empty());
}

/// **Which long flags name a secret**: a credential word as a whole part of
/// the name, anywhere in it; and the explicit look-alikes that do not.
#[test]
fn sc10_flags_naming_a_secret_and_their_look_alikes() {
    let secret = [
        "password", "admin-password", "db-password", "manager-password", "root-passwd", "pwd", "passphrase", "key-passphrase", "secret", "client-secret", "token", "access-token",
        "refresh-token", "auth-token", "api-key", "apikey", "x-api-key", "private-key", "access-key", "aws-secret-access-key", "storage-account-key", "auth-key", "credential",
        "credentials", "registry-password", "password-for-root",
    ];
    let not_secret = [
        "api-key-id", "password-file", "password-stdin", "token-file", "secret-name", "secret-arn", "token-url", "token-endpoint", "max-tokens", "token-ttl", "password-min-length",
        "no-password", "access-token-lifetime", "client-secret-env", "secret-id", "private-key-path", "credential-helper", "password-prompt", "token-type", "monkey", "keyring", "passenger",
        "bypass-cache", "tokens", "password-policy", "key-id", "secret-store",
    ];
    let wrong: Vec<&str> = secret.iter().filter(|n| !scrub::flag_names_secret(n)).chain(not_secret.iter().filter(|n| scrub::flag_names_secret(n))).copied().collect();
    assert!(wrong.is_empty(), "misjudged: {wrong:?}");
    // Every form of the value: `--flag value`, `--flag=value`, quoted both ways.
    let v = body(&mut Rng(0x0f1a_9000_2026_0010), &alnum(), 16);
    for name in secret {
        for text in [format!("tool --{name} {v} --yes"), format!("tool --{name}={v}"), format!("tool --{name} \"{v}\""), format!("tool --{name}='{v}'")] {
            assert!(!scrub::scrub(&text).0.contains(&v), "{text}");
        }
    }
    for name in not_secret {
        let text = format!("tool --{name} {v}");
        assert_eq!(scrub::scrub(&text).1.get("command_line_secret"), None, "{text}");
    }
}
