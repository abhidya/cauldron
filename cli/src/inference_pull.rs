//! **`cauldron inference models pull`**: fetch one model from a small
//! built-in catalogue, only when its owner runs this verb and says yes.
//!
//! Nothing else in cauldron downloads a model, and this verb downloads
//! nothing it was not asked for:
//!
//! * The catalogue is compiled in: the two models this release was measured
//!   with, each pinned to its publisher's repository at a fixed revision,
//!   with its exact size in bytes, its sha256 and its licence. A name not in
//!   it is refused; there is no URL to pass and no override of the source.
//! * Before anything is fetched it prints what would be downloaded (size,
//!   licence, where it goes: `<root>/models/<id>/`, never inside a project),
//!   the free disk there and the memory estimate against the budget, and it
//!   asks for a `yes` at a terminal; `--yes` is the same consent given up
//!   front. Not at a terminal and no `--yes`: refused.
//! * HTTPS only, through macOS's own `/usr/bin/curl` (`-q`, so no `.curlrc`
//!   is read; `--proto =https`; no redirect followed by curl). The
//!   publisher's address answers with a redirect to its file store; that
//!   redirect is followed only to a host on [`ALLOWED_REDIRECT_HOSTS`] (the
//!   one observed when this catalogue was made), over HTTPS, and the host is
//!   said. Anything else is refused and nothing is fetched from it.
//! * Resumable: the bytes go to a private temporary file beside the
//!   destination (`.<file>.part`, in a directory only you can read), and a
//!   run that stops (a lost connection, Ctrl-C) leaves it to be resumed by
//!   running the same command again (an HTTP range request from where it
//!   stopped). Nothing is registered until the whole file is there.
//! * Verified before it is registered: its size and then its sha256 (the
//!   whole file is read) must be the catalogue's. A wrong sha256 deletes
//!   nothing: the file is set aside as `.<file>.mismatch`, the run says so
//!   and refuses, and the model is not registered.
//! * The file is never executed. Once verified it is renamed into place and
//!   registered exactly as `models add` would (path, identity, sha256).
//!
//! Under test isolation the download itself is refused
//! (`env::egress::admit_external_api`); the logic is tested over a fake
//! transport ([`Transport`]) in this module's unit tests.

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use inference_registry::{memory, Config, Registry};
use serde_json::json;

/// One model this release offers to fetch.
#[derive(Debug, Clone)]
pub(crate) struct Entry {
    /// The name it is registered under (`--model <name>`, `local:<name>`).
    pub name: &'static str,
    /// The longer id, accepted too, and the name of its directory.
    pub id: &'static str,
    /// The publisher's repository (`<owner>/<repo>`), pinned at `revision`.
    pub repo: &'static str,
    pub revision: &'static str,
    pub file: &'static str,
    pub bytes: u64,
    pub sha256: &'static str,
    pub licence: &'static str,
    /// The context it is registered with.
    pub context: u32,
    /// The service's memory estimate at that context, in hundredths of a GiB
    /// (file + KV cache + the fixed allowance), as measured for this release.
    pub estimate_centi_gib: u64,
    /// What it measured, in a sentence.
    pub notes: &'static str,
}

impl Entry {
    /// The publisher's address of the file at the pinned revision.
    pub fn url(&self) -> String {
        format!("https://{PUBLISHER_HOST}/{}/resolve/{}/{}", self.repo, self.revision, self.file)
    }
    pub fn repo_url(&self) -> String {
        format!("https://{PUBLISHER_HOST}/{}/tree/{}", self.repo, self.revision)
    }
    fn estimate_bytes(&self) -> u64 {
        self.estimate_centi_gib * inference_registry::GIB / 100
    }
}

/// The publisher every catalogue entry is fetched from.
pub(crate) const PUBLISHER_HOST: &str = "huggingface.co";

/// The hosts the publisher's address may redirect a download to: its file
/// store, as observed when this catalogue was made (a 302 from
/// `huggingface.co/<repo>/resolve/<revision>/<file>` to a signed URL on this
/// host, which answers range requests). Exactly these; any other is refused.
pub(crate) const ALLOWED_REDIRECT_HOSTS: &[&str] = &["us.aws.cdn.hf.co"];

/// **THE CATALOGUE**: the two models this release was measured with. Every
/// fact was read from the publisher's API
/// (`https://huggingface.co/api/models/<repo>/tree/main`: size and LFS
/// sha256; `/api/models/<repo>`: licence and revision) and checked against
/// a copy of each file.
pub(crate) const CATALOGUE: &[Entry] = &[
    Entry {
        name: "qwen",
        id: "qwen3.6-35b-a3b-q8",
        repo: "ggml-org/Qwen3.6-35B-A3B-GGUF",
        revision: "52f9f9b8052fb9c8b69bba4a95eda0f319ac3d2a",
        file: "Qwen3.6-35B-A3B-Q8_0.gguf",
        bytes: 36_903_139_616,
        sha256: "f189cedbe18dc524dd74014b23431081262bc9fcc57c18fec7a7a25149246570",
        licence: "Apache-2.0",
        context: 65_536,
        estimate_centi_gib: 4137,
        notes: "Qwen3.6-35B-A3B, Q8_0 (llama.cpp's own conversion). About 36 GiB resident when loaded; \
                in practice a Mac with 64 GB of memory or more. Every ripple answer was valid in this \
                release's measurements.",
    },
    Entry {
        name: "gpt-oss",
        id: "gpt-oss-120b-mxfp4",
        repo: "ggml-org/gpt-oss-120b-GGUF",
        revision: "f59990363fbbb67f01bce03e5e7bf0929e3e320b",
        file: "gpt-oss-120b-MXFP4.gguf",
        bytes: 63_387_346_208,
        sha256: "582bd40f6886200101f4c4ed9f25f3fe80cc14c86e9e2b37746cd8904a0c622d",
        licence: "Apache-2.0",
        context: 32_768,
        estimate_centi_gib: 6328,
        notes: "gpt-oss-120b, MXFP4 (llama.cpp's own conversion). About 60 GiB resident at a 32,768-token \
                context (65,536 is over the default 64 GiB budget); in practice a Mac with 96 GB or more. \
                It always writes a reasoning section, which counts toward each answer's length.",
    },
];

/// The catalogue entry `name` (its name or its longer id).
pub(crate) fn find(name: &str) -> Option<&'static Entry> {
    CATALOGUE.iter().find(|e| e.name == name || e.id == name)
}

/// Where a model is kept: `<root>/models/<id>/<file>`, shared by every
/// environment of the root (each registers it on its own).
pub(crate) fn destination(root: &Path, e: &Entry) -> PathBuf {
    root.join("models").join(e.id).join(e.file)
}

fn part_of(dest: &Path) -> PathBuf {
    sibling(dest, "part")
}

fn mismatch_of(dest: &Path) -> PathBuf {
    sibling(dest, "mismatch")
}

fn sibling(dest: &Path, ext: &str) -> PathBuf {
    let name = dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    dest.with_file_name(format!(".{name}.{ext}"))
}

fn gib(n: u64) -> String {
    memory::gib(n)
}

// ─────────────────────────────────────────────────────────── transport ──

/// What the publisher's address answered, read without following anything.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Located {
    pub status: u16,
    pub location: Option<String>,
    /// `x-linked-size`: the size of the file behind the redirect.
    pub linked_size: Option<u64>,
    /// `x-linked-etag`: for a file in large-file storage, its sha256.
    pub linked_etag: Option<String>,
}

/// How bytes are fetched. The shipped one is [`Curl`]; the tests use a fake.
pub(crate) trait Transport {
    /// Ask `url` where its bytes are (a HEAD request; no redirect followed).
    fn locate(&mut self, url: &str) -> Result<Located>;
    /// Append the bytes of `url` from `offset` to the end onto `part`, which
    /// holds exactly `offset` bytes. An error leaves whatever arrived.
    fn fetch(&mut self, url: &str, offset: u64, part: &Path) -> Result<()>;
}

/// macOS's own curl, by absolute path, with every option that matters
/// fixed here: no config file (`-q`), HTTPS only, TLS 1.2 or later, no
/// redirect followed, a stalled transfer cut off, and none of the variables
/// that would point it at another certificate store.
pub(crate) struct Curl;

const CURL: &str = "/usr/bin/curl";

impl Curl {
    fn command() -> Command {
        let mut c = Command::new(CURL);
        c.args(["-q", "--proto", "=https", "--tlsv1.2", "--max-redirs", "0", "--connect-timeout", "30"]);
        for v in ["CURL_CA_BUNDLE", "SSL_CERT_FILE", "SSL_CERT_DIR", "CURL_HOME", "SSLKEYLOGFILE"] {
            c.env_remove(v);
        }
        c.stdin(Stdio::null());
        c
    }
}

impl Transport for Curl {
    fn locate(&mut self, url: &str) -> Result<Located> {
        let o = Curl::command()
            .args(["-sS", "-I", "--max-time", "120", url])
            .output()
            .with_context(|| format!("running {CURL}"))?;
        if !o.status.success() {
            bail!("asking {PUBLISHER_HOST} where the file is failed: {}", String::from_utf8_lossy(&o.stderr).trim());
        }
        Ok(parse_headers(&String::from_utf8_lossy(&o.stdout)))
    }

    fn fetch(&mut self, url: &str, offset: u64, part: &Path) -> Result<()> {
        let mut c = Curl::command();
        // A transfer slower than 10 KiB/s for two minutes is cut off; the
        // next attempt resumes where it stopped.
        c.args(["--fail", "--speed-limit", "10240", "--speed-time", "120"]);
        c.arg(if std::io::stderr().is_terminal() { "--progress-bar" } else { "-sS" });
        if offset > 0 {
            c.args(["-C", &offset.to_string()]);
        }
        c.arg("-o").arg(part).arg(url);
        let st = c.status().with_context(|| format!("running {CURL}"))?;
        if !st.success() {
            bail!("the transfer stopped ({CURL} exited with {st})");
        }
        Ok(())
    }
}

/// The last response's status and the headers this verb reads, from a
/// `curl -I` dump (case-insensitive names; CR stripped).
pub(crate) fn parse_headers(text: &str) -> Located {
    let mut l = Located::default();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.starts_with("HTTP/") {
            l = Located { status: line.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0), ..Located::default() };
            continue;
        }
        let Some((k, v)) = line.split_once(':') else { continue };
        let v = v.trim();
        match k.trim().to_ascii_lowercase().as_str() {
            "location" => l.location = Some(v.to_string()),
            "x-linked-size" => l.linked_size = v.parse().ok(),
            "x-linked-etag" => l.linked_etag = Some(v.trim_matches('"').to_string()),
            _ => {}
        }
    }
    l
}

/// The host of an `https://` URL, or why it is not one this verb follows
/// (another scheme, userinfo, a port).
pub(crate) fn https_host(url: &str) -> std::result::Result<String, String> {
    let rest = url.strip_prefix("https://").ok_or_else(|| "it is not an https:// address".to_string())?;
    let auth = rest.split(['/', '?', '#']).next().unwrap_or("");
    if auth.contains('@') || auth.contains(':') || auth.is_empty() {
        return Err("its host part is not a plain host name".into());
    }
    Ok(auth.to_ascii_lowercase())
}

/// Where the bytes of `e` are served from now: the publisher's answer,
/// checked against the catalogue, and its redirect target checked against
/// [`ALLOWED_REDIRECT_HOSTS`]. Says which host it is.
pub(crate) fn resolve(t: &mut dyn Transport, e: &Entry, say: &mut dyn FnMut(String)) -> Result<String> {
    let url = e.url();
    let l = t.locate(&url)?;
    if let Some(n) = l.linked_size {
        if n != e.bytes {
            bail!("REFUSED: {PUBLISHER_HOST} says {} is {n} bytes, not the {} this release's catalogue names; it is no longer the file that was measured. Nothing was downloaded.", e.file, e.bytes);
        }
    }
    if let Some(tag) = &l.linked_etag {
        if tag.len() == 64 && tag != e.sha256 {
            bail!("REFUSED: {PUBLISHER_HOST} says the sha256 of {} is {tag}, not the {} this release's catalogue names. Nothing was downloaded.", e.file, e.sha256);
        }
    }
    match l.status {
        200 => Ok(url),
        301 | 302 | 303 | 307 | 308 => {
            let loc = l.location.context("the publisher answered with a redirect that names no address")?;
            let host = https_host(&loc).map_err(|why| anyhow::anyhow!("REFUSED: the publisher redirected the download to an address this verb does not follow ({why}). Nothing was downloaded."))?;
            if !ALLOWED_REDIRECT_HOSTS.contains(&host.as_str()) {
                bail!(
                    "REFUSED: the publisher redirected the download to {host}, which is not a host this release fetches from ({}). Nothing was downloaded from it.",
                    ALLOWED_REDIRECT_HOSTS.join(", ")
                );
            }
            say(format!("{PUBLISHER_HOST} serves the file from {host} (its file store); fetching it from there over HTTPS"));
            Ok(loc)
        }
        s => bail!("{PUBLISHER_HOST} answered {s} for {url}"),
    }
}

/// Bring `part` to the whole of `e`'s bytes, resuming from what it holds.
/// Each attempt asks the publisher afresh (its file-store addresses
/// expire); five attempts in a row that add nothing give up, leaving the
/// partial file to resume later.
pub(crate) fn download(t: &mut dyn Transport, e: &Entry, part: &Path, say: &mut dyn FnMut(String)) -> Result<()> {
    let mut idle = 0;
    loop {
        let have = std::fs::metadata(part).map(|m| m.len()).unwrap_or(0);
        if have == e.bytes {
            return Ok(());
        }
        if have > e.bytes {
            bail!("{} holds {have} bytes, more than the {} the file has; it is not a part of this file. Move it away and run this again.", part.display(), e.bytes);
        }
        if idle >= 5 {
            bail!(
                "the download stopped at {} of {} ({have} bytes) after 5 attempts that added nothing. The partial file is kept at {}: run the same command again to resume.",
                gib(have),
                gib(e.bytes),
                part.display()
            );
        }
        if have > 0 {
            say(format!("resuming at {} of {}", gib(have), gib(e.bytes)));
        }
        let from = resolve(t, e, say)?;
        let r = t.fetch(&from, have, part);
        let now = std::fs::metadata(part).map(|m| m.len()).unwrap_or(0);
        if let Err(err) = r {
            say(format!("{err:#}"));
        }
        if now > have {
            idle = 0;
        } else {
            idle += 1;
        }
    }
}

/// The partial file, whole: its size and then its sha256 must be the
/// catalogue's. Then it is renamed into place and described for the
/// registry, without reading it again. A wrong sha256 sets the file aside
/// (nothing deleted) and refuses.
pub(crate) fn verify_and_place(e: &Entry, part: &Path, dest: &Path, say: &mut dyn FnMut(String)) -> Result<inference_registry::Model> {
    let size = std::fs::metadata(part).with_context(|| format!("{} cannot be read", part.display()))?.len();
    if size != e.bytes {
        bail!("{} holds {size} bytes, not {}; nothing was registered. Run the same command again to resume.", part.display(), e.bytes);
    }
    say(format!("checking the sha256 of all {} (the whole file is read once)…", gib(e.bytes)));
    let (id, sha) = inference_registry::hash_identified(part)?;
    if sha != e.sha256 {
        let aside = mismatch_of(dest);
        std::fs::rename(part, &aside).with_context(|| format!("setting {} aside as {}", part.display(), aside.display()))?;
        bail!(
            "REFUSED: the downloaded file's sha256 is {sha}, not {}. It is NOT registered, and nothing was deleted: it is kept at {} for you to look at. Remove it to pull again.",
            e.sha256,
            aside.display()
        );
    }
    std::fs::rename(part, dest).with_context(|| format!("renaming {} to {}", part.display(), dest.display()))?;
    Registry::describe_verified(e.name, dest, Some(e.context), &sha, &id)
}

/// A file already at the destination (an earlier pull, or one placed
/// there): read whole and checked like a download, never trusted by name.
fn verify_existing(e: &Entry, dest: &Path) -> Result<inference_registry::Model> {
    let m = Registry::describe(e.name, dest, Some(e.context))?;
    if m.sha256 != e.sha256 || m.size_bytes != e.bytes {
        bail!(
            "REFUSED: {} is already there, but it is not the catalogue's file (sha256 {}, {} bytes; the catalogue's is {}, {} bytes). Nothing was changed or registered; move it away to pull again.",
            dest.display(),
            m.sha256,
            m.size_bytes,
            e.sha256,
            e.bytes
        );
    }
    Ok(m)
}

// ───────────────────────────────────────────────────────────────── verb ──

#[derive(Default)]
struct Args {
    env: Option<String>,
    yes: bool,
    list: bool,
    pos: Vec<String>,
}

fn parse(args: &[String]) -> Result<Args> {
    let mut a = Args::default();
    let mut it = args.iter();
    while let Some(s) = it.next() {
        match s.as_str() {
            "--yes" | "-y" => a.yes = true,
            "--list" => a.list = true,
            "--env" => a.env = Some(it.next().cloned().context("`--env` needs a name")?),
            x if x.starts_with("--env=") => a.env = Some(x["--env=".len()..].to_string()),
            x if x.starts_with('-') => bail!("unknown option `{x}` for `cauldron inference models pull` (`cauldron inference help`)"),
            x => a.pos.push(x.to_string()),
        }
    }
    Ok(a)
}

fn list() -> Result<i32> {
    let v: Vec<_> = CATALOGUE
        .iter()
        .map(|e| {
            json!({"name": e.name, "id": e.id, "repository": e.repo_url(), "file": e.file, "bytes": e.bytes, "size": gib(e.bytes),
                   "sha256": e.sha256, "licence": e.licence, "context": e.context,
                   "memory_estimate": format!("{}.{:02} GiB at a {}-token context", e.estimate_centi_gib / 100, e.estimate_centi_gib % 100, e.context),
                   "notes": e.notes})
        })
        .collect();
    println!("{}", serde_json::to_string_pretty(&json!({"catalogue": v, "fetched_from": PUBLISHER_HOST, "redirects_followed_to": ALLOWED_REDIRECT_HOSTS}))?);
    Ok(0)
}

/// Free bytes on the volume holding `dir` (or its nearest existing
/// ancestor), from `/bin/df`.
fn free_bytes(dir: &Path) -> Option<u64> {
    let mut d = dir;
    while !d.exists() {
        d = d.parent()?;
    }
    let o = Command::new("/bin/df").arg("-Pk").arg(d).stdin(Stdio::null()).output().ok()?;
    let text = String::from_utf8_lossy(&o.stdout);
    let kib: u64 = text.lines().nth(1)?.split_whitespace().nth(3)?.parse().ok()?;
    Some(kib * 1024)
}

/// This Mac's memory, from `sysctl hw.memsize`.
fn machine_memory() -> Option<u64> {
    let o = Command::new("/usr/sbin/sysctl").args(["-n", "hw.memsize"]).stdin(Stdio::null()).output().ok()?;
    String::from_utf8_lossy(&o.stdout).trim().parse().ok()
}

/// A private directory (0700) for the model and its partial file.
fn private_dir(d: &Path) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(d).with_context(|| format!("creating {}", d.display()))?;
    std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700)).with_context(|| format!("making {} private", d.display()))?;
    Ok(())
}

pub(crate) fn pull(args: &[String]) -> Result<i32> {
    let a = parse(args)?;
    if a.list {
        return list();
    }
    let [name] = a.pos.as_slice() else {
        bail!("`cauldron inference models pull` takes one catalogue name (`cauldron inference models pull --list`)");
    };
    let Some(e) = find(name) else {
        bail!(
            "REFUSED: `{name}` is not in this release's catalogue ({}). Nothing was downloaded. To use a model file you already have, register it with `cauldron inference models add <name> <path>`.",
            CATALOGUE.iter().map(|e| format!("{} = {}", e.name, e.id)).collect::<Vec<_>>().join(", ")
        );
    };
    let env = crate::mcp::environment(a.env.as_deref())?;
    let reg = Registry::load(&env.data_root)?;
    if let Some(m) = reg.get(e.name) {
        if m.sha256 == e.sha256 {
            eprintln!("cauldron inference: `{}` is already registered in `{}` (the catalogue's file, at {}); nothing to download.", e.name, env.name, m.path.display());
            return Ok(0);
        }
        bail!(
            "REFUSED: a model named `{}` is already registered in `{}` with another file ({}); `cauldron inference models remove {} --env {}` first. Nothing was downloaded.",
            e.name,
            env.name,
            m.path.display(),
            e.name,
            env.name
        );
    }
    let cfg = Config::load(&env.data_root)?;
    let dest = destination(&env.root, e);
    let part = part_of(&dest);

    // A complete file already there: checked, then registered; no download.
    if dest.exists() {
        eprintln!("cauldron inference: {} is already there; reading it whole to check its sha256 against the catalogue…", dest.display());
        crate::inference::progress_lines();
        let m = verify_existing(e, &dest)?;
        return register(&env, e, m, &cfg);
    }
    if mismatch_of(&dest).exists() {
        bail!("REFUSED: an earlier pull's file whose sha256 was wrong is kept at {}; look at it, remove it, and run this again. Nothing was downloaded.", mismatch_of(&dest).display());
    }

    let have = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    let need = e.bytes.saturating_sub(have);
    let free = free_bytes(dest.parent().unwrap_or(&env.root));
    let est = e.estimate_bytes();
    let fits = est <= cfg.memory_budget_bytes;
    let mem = machine_memory();
    let plan = json!({
        "model": e.name, "id": e.id, "file": e.file, "repository": e.repo_url(),
        "size": gib(e.bytes), "bytes": e.bytes, "sha256": e.sha256, "licence": e.licence,
        "destination": dest, "already_downloaded": if have > 0 { json!(gib(have)) } else { json!(null) },
        "to_download": gib(need), "free_disk": free.map(gib),
        "registered_as": e.name, "context": e.context,
        "memory_estimate": gib(est), "memory_budget": gib(cfg.memory_budget_bytes), "fits_the_budget": fits,
        "this_mac_has": mem.map(gib), "notes": e.notes,
    });
    println!("{}", serde_json::to_string_pretty(&json!({"would_download": plan}))?);
    eprintln!(
        "cauldron inference: `{}` is {} ({} bytes), licence {}, from {} at revision {}.",
        e.name,
        gib(e.bytes),
        e.bytes,
        e.licence,
        e.repo,
        &e.revision[..12]
    );
    eprintln!("cauldron inference: it goes to {} (resumable; nothing is registered until its sha256 is checked).", dest.display());
    if !fits {
        eprintln!(
            "cauldron inference: NOTE: its memory estimate, {}, is over this environment's budget ({}): loading it would be refused until `memory_budget_gib` is raised in {}.",
            gib(est),
            gib(cfg.memory_budget_bytes),
            inference_registry::dir(&env.data_root).join(inference_registry::CONFIG_FILE).display()
        );
    }
    if let Some(m) = mem {
        if est > m {
            eprintln!("cauldron inference: NOTE: this Mac has {} of memory, less than the {} the model needs loaded.", gib(m), gib(est));
        }
    }
    match free {
        Some(f) if f < need + inference_registry::GIB => {
            bail!(
                "REFUSED: {} more must be downloaded and only {} is free on the disk that holds {}. Nothing was downloaded.",
                gib(need),
                gib(f),
                dest.parent().unwrap_or(&env.root).display()
            );
        }
        _ => {}
    }
    if !a.yes {
        if !std::io::stdin().is_terminal() {
            bail!("REFUSED: not at a terminal, so nobody can say yes. Nothing was downloaded. Run it at a terminal, or pass --yes to consent to this download.");
        }
        eprint!("cauldron inference: download {} now? Type yes to go on: ", gib(need));
        std::io::stderr().flush().ok();
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line)?;
        if !matches!(line.trim().to_ascii_lowercase().as_str(), "yes" | "y") {
            eprintln!("cauldron inference: not downloaded.");
            return Ok(1);
        }
    }
    env::egress::admit_external_api("`cauldron inference models pull`", env::egress::process_isolation().as_deref())
        .map_err(|why| anyhow::anyhow!("REFUSED: {why}. Nothing was downloaded."))?;

    private_dir(dest.parent().context("the destination has no directory")?)?;
    if !part.exists() {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&part).with_context(|| format!("creating {}", part.display()))?;
    }
    let mut say = |s: String| eprintln!("cauldron inference: {s}");
    if let Err(err) = download(&mut Curl, e, &part, &mut say) {
        let now = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
        bail!("{err:#}\nNothing was registered; {} of {} is kept at {} and the same command resumes it.", gib(now), gib(e.bytes), part.display());
    }
    crate::inference::progress_lines();
    let m = verify_and_place(e, &part, &dest, &mut say)?;
    register(&env, e, m, &cfg)
}

fn register(env: &env::Environment, e: &Entry, m: inference_registry::Model, cfg: &Config) -> Result<i32> {
    let est = crate::inference::estimate_json(&m, cfg);
    Registry::update(&env.data_root, |r| r.add(m.clone())).map_err(|err| anyhow::anyhow!("REFUSED: {err:#}. Nothing was registered."))?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({"registered": m, "memory_estimate": est, "registry": inference_registry::dir(&env.data_root).join(inference_registry::MODELS_FILE)}))?
    );
    eprintln!(
        "cauldron inference: registered `{}` in `{}`; it loads when a request first names it (`cauldron ripples run <DM-n> --backend local --model {}`).",
        e.name, env.name, e.name
    );
    Ok(0)
}

// ─────────────────────────────────────────────────────────────── tests ──

#[cfg(test)]
mod tests {
    use super::*;
    use inference_registry::gguf::Meta;

    fn scratch(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "cauldron-pull-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p.canonicalize().unwrap()
    }

    /// A small real GGUF file's bytes, and a catalogue entry for them.
    fn model(dir: &Path) -> (Vec<u8>, Entry) {
        let f = dir.join("src.gguf");
        inference_registry::gguf::write_header(
            &f,
            &[
                ("general.architecture", Meta::Str("llama".into())),
                ("llama.block_count", Meta::Int(2)),
                ("llama.attention.head_count", Meta::Int(4)),
                ("llama.attention.head_count_kv", Meta::Int(1)),
                ("llama.embedding_length", Meta::Int(256)),
            ],
            300_000,
        )
        .unwrap();
        let bytes = std::fs::read(&f).unwrap();
        let sha = inference_registry::sha256_file(&f).unwrap();
        let e = Entry {
            name: "tiny",
            id: "tiny-test",
            repo: "example/tiny",
            revision: "0123456789abcdef",
            file: "tiny.gguf",
            bytes: bytes.len() as u64,
            sha256: Box::leak(sha.into_boxed_str()),
            licence: "Apache-2.0",
            context: 4096,
            estimate_centi_gib: 1,
            notes: "",
        };
        (bytes, e)
    }

    /// Serves `data` from a file store behind a redirect. Each fetch stops
    /// after `chunk` bytes (an interrupted transfer) unless that reaches the
    /// end; `cut_all` makes every fetch fail with nothing written.
    struct Fake {
        data: Vec<u8>,
        chunk: usize,
        redirect_to: String,
        linked_etag: Option<String>,
        cut_all: bool,
        locates: usize,
        offsets: Vec<u64>,
    }

    impl Fake {
        fn new(data: Vec<u8>, chunk: usize) -> Fake {
            Fake { data, chunk, redirect_to: format!("https://{}/store/abc?sig=x", ALLOWED_REDIRECT_HOSTS[0]), linked_etag: None, cut_all: false, locates: 0, offsets: vec![] }
        }
    }

    impl Transport for Fake {
        fn locate(&mut self, url: &str) -> Result<Located> {
            assert!(url.starts_with("https://huggingface.co/"), "{url}");
            self.locates += 1;
            Ok(Located { status: 302, location: Some(self.redirect_to.clone()), linked_size: Some(self.data.len() as u64), linked_etag: self.linked_etag.clone() })
        }
        fn fetch(&mut self, url: &str, offset: u64, part: &Path) -> Result<()> {
            assert_eq!(url, self.redirect_to, "fetched only from the address the publisher named");
            assert_eq!(std::fs::metadata(part).unwrap().len(), offset, "a resume starts where the file ends");
            self.offsets.push(offset);
            if self.cut_all {
                bail!("connection reset");
            }
            let start = offset as usize;
            let end = (start + self.chunk).min(self.data.len());
            let mut f = std::fs::OpenOptions::new().append(true).open(part).unwrap();
            f.write_all(&self.data[start..end]).unwrap();
            if end < self.data.len() {
                bail!("connection reset at {end}");
            }
            Ok(())
        }
    }

    fn quiet() -> impl FnMut(String) {
        |_| {}
    }

    #[test]
    fn an_interrupted_download_resumes_with_range_requests_and_registers_only_when_verified() {
        let d = scratch("resume");
        let (bytes, e) = model(&d);
        let dest = d.join("models/tiny-test/tiny.gguf");
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        let part = part_of(&dest);
        std::fs::write(&part, b"").unwrap();
        let mut t = Fake::new(bytes.clone(), 100_000);
        download(&mut t, &e, &part, &mut quiet()).unwrap();
        assert_eq!(t.offsets, vec![0, 100_000, 200_000, 300_000], "each attempt resumed from where the last stopped");
        assert_eq!(t.locates, 4, "each attempt asked the publisher afresh");
        assert!(!dest.exists(), "nothing at the final name before it is verified");
        let m = verify_and_place(&e, &part, &dest, &mut quiet()).unwrap();
        assert!(dest.exists() && !part.exists());
        assert_eq!(std::fs::read(&dest).unwrap(), bytes);
        assert_eq!(m.sha256, e.sha256);
        assert_eq!(m.context, Some(4096));
        assert_eq!(m.file.as_ref().unwrap().real_path, dest, "the identity recorded is the file at its final name");
        assert!(m.differences().unwrap().is_empty(), "the registered identity matches the file now");
    }

    #[test]
    fn a_stalled_download_gives_up_and_keeps_the_partial_file_for_a_later_resume() {
        let d = scratch("stall");
        let (bytes, e) = model(&d);
        let dest = d.join("tiny.gguf");
        let part = part_of(&dest);
        std::fs::write(&part, &bytes[..1000]).unwrap();
        let mut t = Fake::new(bytes.clone(), 10);
        t.cut_all = true;
        let err = download(&mut t, &e, &part, &mut quiet()).unwrap_err().to_string();
        assert!(err.contains("5 attempts") && err.contains("run the same command again to resume"), "{err}");
        assert_eq!(std::fs::read(&part).unwrap(), &bytes[..1000], "the partial file is untouched");
        assert!(!dest.exists());
        // The same file, later: resumes from 1000 and completes.
        let mut t = Fake::new(bytes.clone(), 1 << 20);
        download(&mut t, &e, &part, &mut quiet()).unwrap();
        assert_eq!(t.offsets, vec![1000]);
        assert_eq!(std::fs::read(&part).unwrap(), bytes);
    }

    #[test]
    fn a_wrong_checksum_deletes_nothing_reports_and_registers_nothing() {
        let d = scratch("badsum");
        let (mut bytes, e) = model(&d);
        *bytes.last_mut().unwrap() ^= 1;
        let dest = d.join("tiny.gguf");
        let part = part_of(&dest);
        std::fs::write(&part, b"").unwrap();
        let mut t = Fake::new(bytes.clone(), 1 << 20);
        download(&mut t, &e, &part, &mut quiet()).unwrap();
        let err = verify_and_place(&e, &part, &dest, &mut quiet()).unwrap_err().to_string();
        assert!(err.contains("REFUSED") && err.contains("NOT registered") && err.contains("nothing was deleted"), "{err}");
        assert!(!dest.exists() && !part.exists());
        assert_eq!(std::fs::read(mismatch_of(&dest)).unwrap(), bytes, "the bad file is kept, set aside, byte for byte");
        // Opposed: a file of the wrong size is not hashed or moved at all.
        std::fs::write(&part, &bytes[..10]).unwrap();
        let err = verify_and_place(&e, &part, &dest, &mut quiet()).unwrap_err().to_string();
        assert!(err.contains("resume"), "{err}");
        assert!(part.exists());
    }

    #[test]
    fn a_redirect_off_the_publishers_file_store_is_refused_before_any_byte() {
        let d = scratch("redir");
        let (bytes, e) = model(&d);
        let part = d.join(".tiny.gguf.part");
        std::fs::write(&part, b"").unwrap();
        for to in ["https://elsewhere.example/x", "http://us.aws.cdn.hf.co/x", "https://user@us.aws.cdn.hf.co/x", "https://us.aws.cdn.hf.co:8443/x"] {
            let mut t = Fake::new(bytes.clone(), 1 << 20);
            t.redirect_to = to.to_string();
            let err = download(&mut t, &e, &part, &mut quiet()).unwrap_err().to_string();
            assert!(err.contains("REFUSED") && err.contains("Nothing was downloaded"), "{to}: {err}");
            assert!(t.offsets.is_empty(), "{to}: nothing fetched");
        }
        // The publisher naming another sha256 for the file: refused too.
        let mut t = Fake::new(bytes.clone(), 1 << 20);
        t.linked_etag = Some("ab".repeat(32));
        let err = download(&mut t, &e, &part, &mut quiet()).unwrap_err().to_string();
        assert!(err.contains("not the") && t.offsets.is_empty(), "{err}");
        // Opposed: the observed host is followed.
        let mut t = Fake::new(bytes.clone(), 1 << 20);
        let mut said = vec![];
        download(&mut t, &e, &part, &mut |s| said.push(s)).unwrap();
        assert!(said.iter().any(|s| s.contains("us.aws.cdn.hf.co")), "the host is said: {said:?}");
    }

    #[test]
    fn headers_hosts_and_the_catalogue() {
        let l = parse_headers("HTTP/2 302 \r\nLocation: https://us.aws.cdn.hf.co/a?b=c\r\nX-Linked-Size: 12\r\nx-linked-etag: \"abc\"\r\n\r\n");
        assert_eq!(l, Located { status: 302, location: Some("https://us.aws.cdn.hf.co/a?b=c".into()), linked_size: Some(12), linked_etag: Some("abc".into()) });
        assert_eq!(https_host("https://Us.Aws.Cdn.Hf.Co/x?y").unwrap(), "us.aws.cdn.hf.co");
        assert!(https_host("ftp://x/y").is_err() && https_host("https://a:1/").is_err() && https_host("https://u@a/").is_err());
        assert_eq!(CATALOGUE.len(), 2, "exactly the two measured models");
        for e in CATALOGUE {
            assert!(e.sha256.len() == 64 && e.sha256.bytes().all(|b| b.is_ascii_hexdigit()));
            assert!(e.url().starts_with("https://huggingface.co/ggml-org/") && e.url().contains(e.revision));
            assert!(inference_registry::name_ok(e.name) && inference_registry::name_ok(e.id));
            assert_eq!(find(e.name).unwrap().id, e.id);
            assert_eq!(find(e.id).unwrap().name, e.name);
            assert_eq!(e.licence, "Apache-2.0");
        }
        assert!(find("llama").is_none());
        assert_eq!(destination(Path::new("/r"), &CATALOGUE[0]), Path::new("/r/models/qwen3.6-35b-a3b-q8/Qwen3.6-35B-A3B-Q8_0.gguf"));
    }

    /// The shipped transport's command: macOS's curl by absolute path, no
    /// config file, HTTPS only, no redirect followed by curl.
    #[test]
    fn the_shipped_transport_is_https_only_and_follows_no_redirect() {
        let c = Curl::command();
        assert_eq!(c.get_program(), CURL);
        let a: Vec<_> = c.get_args().map(|s| s.to_string_lossy().into_owned()).collect();
        assert_eq!(a[0], "-q", "no .curlrc: -q must be the first argument");
        assert!(a.windows(2).any(|w| w == ["--proto", "=https"]), "{a:?}");
        assert!(a.windows(2).any(|w| w == ["--max-redirs", "0"]), "{a:?}");
        assert!(a.contains(&"--tlsv1.2".to_string()));
        assert!(!a.iter().any(|x| x == "-k" || x == "--insecure" || x == "-L" || x == "--location"));
    }
}
