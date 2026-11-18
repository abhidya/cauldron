//! **Local inference, as files**: what an environment's `inference` service
//! may load, and how big it may be.
//!
//! Everything lives in `data/<env>/inference/`:
//!
//! - `models.json`: the registered model files. `cauldron inference models
//!   add <name> <path>` records a `.gguf` file the owner placed on this
//!   machine (its path, its identity as a [`FileId`], its sha256 and a few
//!   header facts); nothing is copied, moved or downloaded. `remove`
//!   forgets the entry and leaves the file alone. Before every load the
//!   file's identity is compared, and a file whose identity changed is read
//!   again and refused unless its sha256 is the one registered
//!   ([`Model::verify`]);
//! - `config.toml` (optional, written by hand): `llama_server` (the path of
//!   the `llama-server` binary; else the first one on `PATH`),
//!   `memory_budget_gib` (default 64), `idle_minutes` (default 15),
//!   `context` (the default context size, in tokens, default 65536),
//!   `verify_every_load` (default false: re-hash only when the identity
//!   changed).
//!
//! [`memory::estimate`] is the arithmetic every load is held to: the model
//! file's size, plus the KV cache for the context, plus a fixed allowance.

pub mod gguf;
pub mod memory;

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// The service's name: its directory under an environment's data root.
pub const SERVICE: &str = "inference";
/// The registry file.
pub const MODELS_FILE: &str = "models.json";
/// The settings file.
pub const CONFIG_FILE: &str = "config.toml";

/// The service's directory under an environment's data root.
pub fn dir(data_root: &Path) -> PathBuf {
    data_root.join(SERVICE)
}

// ─────────────────────────────────────────────────────────────── loopback ──

/// **A loopback address, or why not.** Every address the inference
/// service, its child, and the ripples service's backends are dialled at
/// goes through this: an IP literal and a port (no host name, so nothing is
/// looked up), in 127.0.0.0/8 or `::1`. Anything else is refused before a
/// connection is made, whichever place the address came from (an
/// environment variable, a settings file, the seam table).
pub fn loopback_addr(what: &str, addr: &str) -> std::result::Result<std::net::SocketAddr, String> {
    let sa: std::net::SocketAddr = addr
        .trim()
        .parse()
        .map_err(|e| format!("{what}: `{addr}` is not an IP address and port ({e}); only a loopback address (127.0.0.1 or ::1) is dialled, and nothing was sent"))?;
    if !sa.ip().is_loopback() {
        return Err(format!("{what}: `{addr}` is not a loopback address; it is only ever dialled on this machine (127.0.0.0/8 or ::1), and nothing was sent"));
    }
    Ok(sa)
}

// ─────────────────────────────────────────────────────────────── settings ──

/// The service's settings ([`CONFIG_FILE`]); every key optional.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// The `llama-server` to run; `None`: the first on `PATH`.
    pub llama_server: Option<PathBuf>,
    /// The most memory a loaded model may be estimated to need.
    pub memory_budget_bytes: u64,
    /// How long a loaded model may sit unused before it is unloaded.
    pub idle_seconds: u64,
    /// The context size, in tokens, a model is served with unless its
    /// registration says otherwise.
    pub context: u32,
    /// Read the whole model file and check its sha256 before EVERY load,
    /// not only when its identity changed (a 60 GB file takes a minute or
    /// more each time).
    pub verify_every_load: bool,
}

pub const GIB: u64 = 1 << 30;
/// The longest the service waits for a model to load before it gives up
/// (seconds); a caller waiting on a completion allows this on top of the
/// completion's own time limit.
pub const LOAD_TIMEOUT_S: u64 = 900;
pub const DEFAULT_BUDGET_GIB: u64 = 64;
pub const DEFAULT_IDLE_MINUTES: u64 = 15;
pub const DEFAULT_CONTEXT: u32 = 65_536;

impl Default for Config {
    fn default() -> Config {
        Config { llama_server: None, memory_budget_bytes: DEFAULT_BUDGET_GIB * GIB, idle_seconds: DEFAULT_IDLE_MINUTES * 60, context: DEFAULT_CONTEXT, verify_every_load: false }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    llama_server: Option<PathBuf>,
    memory_budget_gib: Option<f64>,
    idle_minutes: Option<f64>,
    /// For tests and the impatient: wins over `idle_minutes`.
    idle_seconds: Option<u64>,
    context: Option<u32>,
    verify_every_load: Option<bool>,
}

impl Config {
    /// The settings of the environment whose data root is `data_root` (no
    /// file: the defaults).
    pub fn load(data_root: &Path) -> Result<Config> {
        let p = dir(data_root).join(CONFIG_FILE);
        let text = match std::fs::read_to_string(&p) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
            Err(e) => return Err(e).with_context(|| format!("reading {}", p.display())),
        };
        let f: ConfigFile = toml::from_str(&text).with_context(|| format!("{} (keys: llama_server, memory_budget_gib, idle_minutes, context, verify_every_load)", p.display()))?;
        let mut c = Config::default();
        if let Some(l) = f.llama_server {
            if !l.is_absolute() {
                bail!("{}: `llama_server` must be an absolute path, not `{}`", p.display(), l.display());
            }
            c.llama_server = Some(l);
        }
        if let Some(g) = f.memory_budget_gib {
            if !(g > 0.0 && g <= 4096.0) {
                bail!("{}: `memory_budget_gib` must be above 0 and at most 4096, not {g}", p.display());
            }
            c.memory_budget_bytes = (g * GIB as f64) as u64;
        }
        if let Some(m) = f.idle_minutes {
            if !(m > 0.0 && m <= 7.0 * 24.0 * 60.0) {
                bail!("{}: `idle_minutes` must be above 0 and at most a week, not {m}", p.display());
            }
            c.idle_seconds = (m * 60.0).round().max(1.0) as u64;
        }
        if let Some(s) = f.idle_seconds {
            if s == 0 {
                bail!("{}: `idle_seconds` must be above 0", p.display());
            }
            c.idle_seconds = s;
        }
        if let Some(x) = f.context {
            check_context(x).with_context(|| p.display().to_string())?;
            c.context = x;
        }
        if let Some(v) = f.verify_every_load {
            c.verify_every_load = v;
        }
        Ok(c)
    }
}

/// A context size this service will serve: 512 to 1,048,576 tokens.
pub fn check_context(x: u32) -> Result<()> {
    if !(512..=1_048_576).contains(&x) {
        bail!("a context size is 512 to 1048576 tokens, not {x}");
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────── registry ──

/// One registered model file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Model {
    pub name: String,
    /// Absolute, as given (never copied).
    pub path: PathBuf,
    pub size_bytes: u64,
    /// Seconds since the epoch, when it was registered.
    pub mtime: i64,
    /// The sha256 of the file's whole content, read at registration and
    /// again whenever its [`FileId`] is no longer the one recorded.
    pub sha256: String,
    /// The file as the filesystem knows it, at registration: compared
    /// before every load ([`Model::verify`]). Absent (an entry written
    /// before this was recorded): the file is re-hashed at its next load.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<FileId>,
    pub added_at: String,
    /// The context size to serve it with; `None`: the service's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<u32>,
    #[serde(default)]
    pub facts: gguf::Facts,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Registry {
    #[serde(default)]
    pub models: Vec<Model>,
}

/// A model name: a lower-case letter or digit, then up to 63 of those,
/// `.`, `_` or `-`.
pub fn name_ok(n: &str) -> bool {
    let b = n.as_bytes();
    !b.is_empty() && b.len() <= 64 && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit()) && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'.' | b'_' | b'-'))
}

/// **A file as the filesystem knows it**: its canonical path (every
/// symlink resolved), device, inode, size, and modification and change
/// times to the nanosecond. A `touch -r` can set the modification time
/// back; nobody but the kernel sets the change time, and a symlink pointed
/// elsewhere, a file renamed over this one or one rewritten in place each
/// changes at least one of these.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileId {
    pub real_path: PathBuf,
    pub dev: u64,
    pub ino: u64,
    pub size: u64,
    pub mtime: i64,
    pub mtime_nsec: i64,
    pub ctime: i64,
    pub ctime_nsec: i64,
}

impl FileId {
    fn of(real_path: &Path, m: &std::fs::Metadata) -> FileId {
        use std::os::unix::fs::MetadataExt;
        FileId {
            real_path: real_path.to_path_buf(),
            dev: m.dev(),
            ino: m.ino(),
            size: m.size(),
            mtime: m.mtime(),
            mtime_nsec: m.mtime_nsec(),
            ctime: m.ctime(),
            ctime_nsec: m.ctime_nsec(),
        }
    }

    /// The file at `path` now (its symlinks resolved).
    pub fn now(path: &Path) -> Result<FileId> {
        let real = std::fs::canonicalize(path).with_context(|| format!("{} cannot be read (moved or deleted?)", path.display()))?;
        let m = std::fs::metadata(&real).with_context(|| format!("{} cannot be read", real.display()))?;
        Ok(FileId::of(&real, &m))
    }

    /// What differs from `then`, in words; empty: nothing.
    pub fn differences(&self, then: &FileId) -> Vec<String> {
        let mut d = vec![];
        if self.real_path != then.real_path {
            d.push(format!("it now resolves to {} (was {})", self.real_path.display(), then.real_path.display()));
        }
        if (self.dev, self.ino) != (then.dev, then.ino) {
            d.push("it is another file (device and inode)".into());
        }
        if self.size != then.size {
            d.push(format!("size {} → {}", then.size, self.size));
        }
        if (self.mtime, self.mtime_nsec) != (then.mtime, then.mtime_nsec) {
            d.push("modification time".into());
        }
        if (self.ctime, self.ctime_nsec) != (then.ctime, then.ctime_nsec) {
            d.push("change time".into());
        }
        d
    }
}

/// The sha256 of what `r` reads, to its end, as lower-case hex: streamed
/// through a 1 MiB buffer, so a 60 GB file costs 1 MiB of memory.
pub fn sha256_reader(r: impl std::io::Read) -> Result<String> {
    sha256_reader_with(r, PROGRESS_EVERY, &mut |_| {})
}

/// [`sha256_reader`], telling `progress` the bytes read so far after
/// every `every` bytes ([`PROGRESS_EVERY`] for a file being registered).
pub fn sha256_reader_with(mut r: impl std::io::Read, every: u64, progress: &mut dyn FnMut(u64)) -> Result<String> {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let every = every.max(1);
    let (mut total, mut next) = (0u64, every);
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        total += n as u64;
        if total >= next {
            progress(total);
            next += every;
        }
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// How often a long hash reports its progress: every 4 GiB.
pub const PROGRESS_EVERY: u64 = 4 * GIB;

thread_local! {
    /// What a hash in this thread reports its progress to (the CLI sets
    /// it; the service does not).
    static PROGRESS: std::cell::RefCell<Option<Box<dyn FnMut(u64)>>> = const { std::cell::RefCell::new(None) };
}

/// Report the progress of every file hashed in this thread to `f`.
pub fn report_hash_progress(f: impl FnMut(u64) + 'static) {
    PROGRESS.with(|p| *p.borrow_mut() = Some(Box::new(f)));
}

fn hash_with_progress(r: impl std::io::Read) -> Result<String> {
    PROGRESS.with(|p| match p.borrow_mut().as_mut() {
        Some(f) => sha256_reader_with(r, PROGRESS_EVERY, &mut |n| f(n)),
        None => sha256_reader(r),
    })
}

/// The sha256 of the file at `path`, read whole, as lower-case hex.
pub fn sha256_file(path: &Path) -> Result<String> {
    let f = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    sha256_reader(f)
}

/// **Open the file at `path` and hash it, as one file**: its canonical
/// path, then the open file's identity before and after the whole read
/// (one that changed while it was read is refused), and its sha256.
pub fn hash_identified(path: &Path) -> Result<(FileId, String)> {
    let real = std::fs::canonicalize(path).with_context(|| format!("{} cannot be read (moved or deleted?)", path.display()))?;
    let f = std::fs::File::open(&real).with_context(|| format!("opening {}", real.display()))?;
    let before = FileId::of(&real, &f.metadata()?);
    let sha = hash_with_progress(&f)?;
    let after = FileId::of(&real, &f.metadata()?);
    let d = after.differences(&before);
    if !d.is_empty() {
        bail!("{} changed while it was read ({}); nothing was recorded", real.display(), d.join(", "));
    }
    Ok((before, sha))
}

impl Registry {
    fn path(data_root: &Path) -> PathBuf {
        dir(data_root).join(MODELS_FILE)
    }

    /// The registry (none yet: empty).
    pub fn load(data_root: &Path) -> Result<Registry> {
        let p = Registry::path(data_root);
        match std::fs::read_to_string(&p) {
            Ok(t) => serde_json::from_str(&t).with_context(|| format!("{} is not a model registry", p.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Registry::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", p.display())),
        }
    }

    /// Written whole, owner-only, by rename.
    pub fn save(&self, data_root: &Path) -> Result<()> {
        use std::io::Write;
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        let d = dir(data_root);
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&d).with_context(|| format!("creating {}", d.display()))?;
        let tmp = d.join(format!(".{MODELS_FILE}.{}", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        {
            let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
            f.write_all(serde_json::to_string_pretty(self)?.as_bytes())?;
            f.write_all(b"\n")?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, Registry::path(data_root))?;
        Ok(())
    }

    /// **Re-record the identity of `name`** after its file was hashed again
    /// and found to be the one registered (so the next load need not read
    /// it again). Only when the entry is still the one verified.
    pub fn record_identity(data_root: &Path, name: &str, sha256: &str, file: &FileId) -> Result<()> {
        Registry::update(data_root, |r| {
            if let Some(m) = r.models.iter_mut().find(|m| m.name == name && m.sha256 == sha256) {
                m.file = Some(file.clone());
                m.size_bytes = file.size;
                m.mtime = file.mtime;
            }
            Ok(())
        })
    }

    /// **Change the registry as one step**: under an exclusive lock on
    /// `models.json.lock`, read it as it is now, let `f` change it, and
    /// write it. Two writers (two `models add`, an add and a remove, the
    /// service re-recording a file) never overwrite each other's change.
    /// Slow work, hashing a file above all, is done before, outside the
    /// lock.
    pub fn update<T>(data_root: &Path, f: impl FnOnce(&mut Registry) -> Result<T>) -> Result<T> {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        let d = dir(data_root);
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&d).with_context(|| format!("creating {}", d.display()))?;
        let lock_path = d.join(format!("{MODELS_FILE}.lock"));
        let lock = std::fs::OpenOptions::new().create(true).truncate(false).write(true).mode(0o600).open(&lock_path).with_context(|| format!("opening {}", lock_path.display()))?;
        lock.lock().with_context(|| format!("locking {}", lock_path.display()))?;
        let mut r = Registry::load(data_root)?;
        let out = f(&mut r)?;
        r.save(data_root)?;
        drop(lock);
        Ok(out)
    }

    pub fn get(&self, name: &str) -> Option<&Model> {
        self.models.iter().find(|m| m.name == name)
    }

    /// **The model a request that names none means**: the only one
    /// registered, or a refusal that lists them.
    pub fn default_model(&self) -> Result<&Model> {
        match self.models.as_slice() {
            [one] => Ok(one),
            [] => bail!("no model is registered; place a .gguf file on this machine and register it with `cauldron inference models add <name> <path>`"),
            many => bail!("{} models are registered ({}); name one with `--model <name>`", many.len(), many.iter().map(|m| m.name.as_str()).collect::<Vec<_>>().join(", ")),
        }
    }

    /// **Register** the file at `path` as `name`: its size, modification
    /// time, sha256 and header facts. The file is read, never copied or
    /// changed.
    pub fn describe(name: &str, path: &Path, context: Option<u32>) -> Result<Model> {
        if !name_ok(name) {
            bail!("`{name}` is not a model name: a lower-case letter or digit, then up to 63 of those, `.`, `_` or `-`");
        }
        if !path.is_absolute() {
            bail!("give the model file's absolute path, not `{}`", path.display());
        }
        if let Some(c) = context {
            check_context(c)?;
        }
        let md = std::fs::metadata(path).with_context(|| format!("{} cannot be read", path.display()))?;
        if !md.is_file() {
            bail!("{} is not a file", path.display());
        }
        let real = std::fs::canonicalize(path).with_context(|| format!("{} cannot be read", path.display()))?;
        for p in [path, real.as_path()] {
            if p.extension().and_then(|e| e.to_str()) != Some("gguf") {
                bail!("{} is not a `.gguf` file; llama-server loads GGUF model files", p.display());
            }
        }
        let facts = gguf::facts(&real)?;
        let (file, sha256) = hash_identified(&real)?;
        Ok(Model {
            name: name.to_string(),
            path: path.to_path_buf(),
            size_bytes: file.size,
            mtime: file.mtime,
            sha256,
            file: Some(file),
            added_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            context,
            facts,
        })
    }

    /// Add `m`; a name already registered is refused.
    pub fn add(&mut self, m: Model) -> Result<()> {
        if self.get(&m.name).is_some() {
            bail!("a model named `{}` is already registered; remove it first (`cauldron inference models remove {}`)", m.name, m.name);
        }
        self.models.push(m);
        self.models.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(())
    }

    /// Forget `name` (the file is left alone).
    pub fn remove(&mut self, name: &str) -> Result<Model> {
        match self.models.iter().position(|m| m.name == name) {
            Some(i) => Ok(self.models.remove(i)),
            None => bail!("no model named `{name}` is registered"),
        }
    }
}

/// What [`Model::verify`] found.
#[derive(Debug, Clone, PartialEq)]
pub struct Verified {
    /// The file as it is now: the one to load (by its `real_path`).
    pub file: FileId,
    /// Why it was hashed again (empty: its identity was the one recorded,
    /// and it was not).
    pub rehashed_because: Vec<String>,
}

impl Model {
    /// **What differs between the file now and the one registered**, by
    /// identity alone ([`FileId`]; nothing is read): empty when nothing
    /// does. An entry with no recorded identity differs.
    pub fn differences(&self) -> Result<Vec<String>> {
        let now = FileId::now(&self.path).with_context(|| format!("model `{}`", self.name))?;
        Ok(match &self.file {
            Some(then) => now.differences(then),
            None => vec!["no identity was recorded at registration".into()],
        })
    }

    /// **Is the file still the one registered?** Its identity ([`FileId`])
    /// is compared first; when anything differs, or when `always_hash`,
    /// the whole file is read again (streamed) and its sha256 must be the
    /// one registered, or it is refused, saying how to register it again.
    pub fn verify(&self, always_hash: bool) -> Result<Verified> {
        let mut why = self.differences().with_context(|| format!("{}: register it again", self.path.display()))?;
        if why.is_empty() && !always_hash {
            let file = FileId::now(&self.path)?;
            return Ok(Verified { file, rehashed_because: vec![] });
        }
        if why.is_empty() {
            why.push("every load is verified (`verify_every_load`)".into());
        }
        let (file, sha) = hash_identified(&self.path).with_context(|| format!("model `{}`", self.name))?;
        if sha != self.sha256 {
            bail!(
                "model `{}`: {} is not the file registered ({}): its sha256 is now {sha}, and {} was registered. A load refuses it. If this file is the one you mean, register it again: `cauldron inference models remove {}` then `cauldron inference models add {} {}`",
                self.name,
                self.path.display(),
                why.join(", "),
                self.sha256,
                self.name,
                self.name,
                self.path.display()
            );
        }
        Ok(Verified { file, rehashed_because: why })
    }

    /// Kept for callers that only ask "is it unchanged?": [`Model::verify`]
    /// without forcing a read.
    pub fn check_unchanged(&self) -> Result<()> {
        self.verify(false).map(|_| ())
    }

    /// The context size it is served with under `c`.
    pub fn context_under(&self, c: &Config) -> u32 {
        self.context.unwrap_or(c.context)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("inference-registry-{tag}-{}-{}", std::process::id(), chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn only_a_loopback_address_is_accepted() {
        for ok in ["127.0.0.1:9", "127.8.9.10:1", "[::1]:80", " 127.0.0.1:3 "] {
            assert!(loopback_addr("x", ok).is_ok(), "{ok}");
        }
        for (bad, why) in [
            ("192.0.2.7:443", "not a loopback"),
            ("0.0.0.0:9", "not a loopback"),
            ("[::]:9", "not a loopback"),
            ("[::ffff:192.0.2.7]:9", "not a loopback"),
            ("localhost:9", "not an IP address"),
            ("example.invalid:9", "not an IP address"),
            ("127.0.0.1", "not an IP address and port"),
        ] {
            let e = loopback_addr("the seam", bad).unwrap_err();
            assert!(e.contains(why) && e.contains("nothing was sent") && e.starts_with("the seam"), "{bad}: {e}");
        }
    }

    #[test]
    fn names_and_settings() {
        for ok in ["qwen", "q3.6-35b_q8", "0x"] {
            assert!(name_ok(ok), "{ok}");
        }
        for bad in ["", "Qwen", "-a", "a b", "a/b", &"a".repeat(65)] {
            assert!(!name_ok(bad), "{bad}");
        }
        let d = scratch("cfg");
        assert_eq!(Config::load(&d).unwrap(), Config::default());
        assert_eq!(Config::default().memory_budget_bytes, 64 * GIB);
        assert_eq!(Config::default().idle_seconds, 900);
        std::fs::create_dir_all(dir(&d)).unwrap();
        std::fs::write(dir(&d).join(CONFIG_FILE), "memory_budget_gib = 48\nidle_minutes = 2\ncontext = 8192\nllama_server = \"/opt/x/llama-server\"\n").unwrap();
        let c = Config::load(&d).unwrap();
        assert_eq!((c.memory_budget_bytes, c.idle_seconds, c.context), (48 * GIB, 120, 8192));
        std::fs::write(dir(&d).join(CONFIG_FILE), "budget = 1\n").unwrap();
        assert!(Config::load(&d).is_err(), "an unknown key is refused");
        std::fs::write(dir(&d).join(CONFIG_FILE), "llama_server = \"llama-server\"\n").unwrap();
        assert!(Config::load(&d).unwrap_err().to_string().contains("absolute"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_model_is_recorded_not_copied_and_a_changed_file_is_refused() {
        let d = scratch("reg");
        let p = d.join("tiny.gguf");
        gguf::write_header(&p, &[("general.architecture", gguf::Meta::Str("llama".into()))], 1000).unwrap();
        let m = Registry::describe("tiny", &p, None).unwrap();
        assert_eq!(m.sha256, sha256_file(&p).unwrap());
        assert_eq!(m.sha256.len(), 64);
        assert_eq!(m.size_bytes, std::fs::metadata(&p).unwrap().len());
        let mut r = Registry::load(&d).unwrap();
        r.add(m.clone()).unwrap();
        assert!(r.add(m.clone()).is_err(), "a name is registered once");
        r.save(&d).unwrap();
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(dir(&d).join(MODELS_FILE)).unwrap().permissions().mode() & 0o777, 0o600);
        let r2 = Registry::load(&d).unwrap();
        assert_eq!(r2.get("tiny"), Some(&m));
        assert_eq!(r2.default_model().unwrap().name, "tiny");
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 2, "the model file and the service dir; nothing copied");
        m.check_unchanged().unwrap();
        std::fs::write(&p, b"GGUF").unwrap();
        assert!(format!("{:#}", m.check_unchanged().unwrap_err()).contains("is not the file registered"));
        assert!(Registry::describe("x", Path::new("rel.gguf"), None).is_err());
        assert!(Registry::describe("x", &d.join("missing.gguf"), None).is_err());
        std::fs::write(d.join("w.bin"), b"GGUF").unwrap();
        assert!(Registry::describe("x", &d.join("w.bin"), None).unwrap_err().to_string().contains(".gguf"));
        let mut r3 = r2.clone();
        r3.remove("tiny").unwrap();
        assert!(r3.default_model().unwrap_err().to_string().contains("no model is registered"));
        assert!(p.exists(), "remove leaves the file");
        let _ = std::fs::remove_dir_all(&d);
    }
    fn touch_r(reference: &Path, target: &Path) {
        let st = std::process::Command::new("/usr/bin/touch").arg("-r").arg(reference).arg(target).status().unwrap();
        assert!(st.success());
    }

    fn fixture(p: &Path, name: &str) {
        gguf::write_header(p, &[("general.architecture", gguf::Meta::Str("llama".into())), ("general.name", gguf::Meta::Str(name.into()))], 1000).unwrap();
    }

    #[test]
    fn a_file_swapped_under_the_same_size_and_mtime_is_refused_by_its_sha256() {
        let d = scratch("swap").canonicalize().unwrap();
        // 1. Renamed over: another inode, same size, `touch -r`.
        let p = d.join("tiny.gguf");
        fixture(&p, "aaaa");
        let m = Registry::describe("tiny", &p, None).unwrap();
        let f = m.file.clone().unwrap();
        assert_eq!(f.real_path, p);
        assert!(m.differences().unwrap().is_empty());
        let other = d.join("other.gguf");
        fixture(&other, "bbbb");
        assert_eq!(std::fs::metadata(&other).unwrap().len(), f.size, "the same size");
        touch_r(&p, &other);
        std::fs::rename(&other, &p).unwrap();
        let e = format!("{:#}", m.verify(false).unwrap_err());
        assert!(e.contains("is not the file registered") && e.contains("its sha256 is now") && e.contains("cauldron inference models add tiny"), "{e}");
        assert!(e.contains("device and inode"), "{e}");

        // 2. Rewritten in place, same size, `touch -r` from a copy of the
        // original times: the change time still moves.
        let q = d.join("inplace.gguf");
        fixture(&q, "aaaa");
        let keep = d.join("times.ref");
        std::fs::write(&keep, b"").unwrap();
        touch_r(&q, &keep);
        let m2 = Registry::describe("inplace", &q, None).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        {
            use std::io::{Seek, Write};
            let mut f = std::fs::OpenOptions::new().write(true).open(&q).unwrap();
            f.seek(std::io::SeekFrom::End(-1)).unwrap();
            f.write_all(b"x").unwrap();
        }
        touch_r(&keep, &q);
        let now = FileId::now(&q).unwrap();
        let then = m2.file.clone().unwrap();
        assert_eq!((now.size, now.mtime, now.mtime_nsec, now.ino), (then.size, then.mtime, then.mtime_nsec, then.ino), "size, mtime and inode as registered");
        assert!(m2.differences().unwrap().iter().any(|x| x == "change time"));
        assert!(format!("{:#}", m2.verify(false).unwrap_err()).contains("is not the file registered"));

        // 3. A symlink pointed at another file of the same size and times.
        let target = d.join("t1.gguf");
        fixture(&target, "aaaa");
        let link = d.join("link.gguf");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let m3 = Registry::describe("link", &link, None).unwrap();
        assert_eq!(m3.path, link, "the path as given");
        assert_eq!(m3.file.as_ref().unwrap().real_path, target, "and the real path");
        let t2 = d.join("t2.gguf");
        fixture(&t2, "bbbb");
        touch_r(&target, &t2);
        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(&t2, &link).unwrap();
        let e = format!("{:#}", m3.verify(false).unwrap_err());
        assert!(e.contains("now resolves to") && e.contains("is not the file registered"), "{e}");

        // 4. Only touched (same content): read again, accepted, and its new
        // identity is what the next check compares with.
        let r = d.join("touched.gguf");
        fixture(&r, "cccc");
        let m4 = Registry::describe("touched", &r, None).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::process::Command::new("/usr/bin/touch").arg(&r).status().unwrap();
        let v = m4.verify(false).unwrap();
        assert!(!v.rehashed_because.is_empty());
        let mut reg = Registry::default();
        reg.add(m4.clone()).unwrap();
        reg.save(&d).unwrap();
        Registry::record_identity(&d, "touched", &m4.sha256, &v.file).unwrap();
        let again = Registry::load(&d).unwrap().get("touched").unwrap().verify(false).unwrap();
        assert!(again.rehashed_because.is_empty(), "nothing is read again once recorded");
        // Forced: read every time, and still accepted.
        assert!(!Registry::load(&d).unwrap().get("touched").unwrap().verify(true).unwrap().rehashed_because.is_empty());

        // 5. An entry with no recorded identity is read again, not trusted.
        let mut old = m4.clone();
        old.file = None;
        assert!(old.verify(false).unwrap().rehashed_because[0].contains("no identity"));
        // A non-gguf file behind a `.gguf` link is refused at registration.
        std::fs::write(d.join("w.bin"), b"GGUF").unwrap();
        std::os::unix::fs::symlink(d.join("w.bin"), d.join("w.gguf")).unwrap();
        assert!(Registry::describe("w", &d.join("w.gguf"), None).unwrap_err().to_string().contains("w.bin is not a `.gguf` file"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_long_hash_reports_its_progress() {
        let d = scratch("progress");
        let p = d.join("big.gguf");
        let every = 4 << 20;
        std::fs::File::create(&p).unwrap().set_len(2 * every + 7).unwrap();
        let seen = std::rc::Rc::new(std::cell::RefCell::new(vec![]));
        let s2 = seen.clone();
        let sha = sha256_reader_with(std::fs::File::open(&p).unwrap(), every, &mut |n| s2.borrow_mut().push(n)).unwrap();
        assert_eq!(*seen.borrow(), [every, 2 * every]);
        assert_eq!(sha, sha256_file(&p).unwrap());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn two_writers_never_lose_each_others_entry() {
        let d = scratch("race");
        let p = d.join("a.gguf");
        fixture(&p, "aaaa");
        let a = Registry::describe("a", &p, None).unwrap();
        // The slow writer read the registry and is hashing; meanwhile
        // another adds `b`, and another holds the lock a while.
        let stale = Registry::load(&d).unwrap();
        let mut b = a.clone();
        b.name = "b".into();
        Registry::update(&d, |r| r.add(b.clone())).unwrap();
        let held = {
            let d = d.clone();
            let mut c = a.clone();
            c.name = "c".into();
            std::thread::spawn(move || {
                Registry::update(&d, |r| {
                    std::thread::sleep(std::time::Duration::from_millis(300));
                    r.add(c)
                })
                .unwrap()
            })
        };
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(stale.models.is_empty());
        Registry::update(&d, |r| r.add(a.clone())).unwrap();
        held.join().unwrap();
        let names: Vec<String> = Registry::load(&d).unwrap().models.into_iter().map(|m| m.name).collect();
        assert_eq!(names, ["a", "b", "c"]);
        // A name taken meanwhile is refused at the write, not overwritten.
        assert!(Registry::update(&d, |r| r.add(a.clone())).unwrap_err().to_string().contains("already registered"));
        let _ = std::fs::remove_dir_all(&d);
    }
}
