//! **The GGUF header**, read for the few facts the memory estimate needs.
//!
//! A `.gguf` file starts with `GGUF`, a version (2 or 3), the tensor count,
//! the metadata count, then the metadata: each a key, a type and a value
//! (`general.architecture`, `<arch>.block_count`, the attention head counts,
//! ...). Only the header is read, never a tensor; arrays (the tokenizer's
//! vocabulary) are skipped over, except short integer arrays, which some
//! models use for a per-layer head count.
//!
//! **A header is hostile until checked.** Every count and length it
//! declares is bounded before it is used (see the constants below), only the
//! entries the estimate needs are kept, at most [`HEADER_BYTES_MAX`] bytes
//! are read, and a fact outside its range ([`Facts::check`]) refuses the
//! file with the field named.

use std::collections::BTreeMap;
use std::io::{BufReader, Read};
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// The file's first four bytes.
pub const MAGIC: &[u8; 4] = b"GGUF";

/// A metadata value this reader keeps.
#[derive(Debug, Clone, PartialEq)]
pub enum Meta {
    Int(u64),
    Float(f64),
    Str(String),
    Ints(Vec<u64>),
    /// An array not kept (its element type and length).
    Skipped,
}

/// What the estimate needs from a model file's header.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Facts {
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub architecture: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_count: Option<u64>,
    /// The context the model was trained for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_length: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding_length: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_count: Option<u64>,
    /// One value for every layer, or one per layer.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub head_count_kv: Vec<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_length: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_length: Option<u64>,
}

// ───────────────────────────────────────────────────────────── the bounds ──
//
// Every number a header declares is hostile until checked: a model file is
// usually downloaded from the internet, and this reader runs at `models
// add`, `models list`, `status` and every load. Nothing is allocated from a
// declared size without one of these bounds, and a header outside them is
// refused with the field named.

/// The most bytes of a file this reader reads (the header only; a real one,
/// tokenizer included, is a few tens of MiB at most).
pub const HEADER_BYTES_MAX: u64 = 128 << 20;
/// The most metadata entries a header may declare (real files: under 100).
pub const KV_COUNT_MAX: u64 = 65_536;
/// The most tensors a header may declare (real files: a few thousand).
pub const TENSOR_COUNT_MAX: u64 = 1 << 20;
/// The most elements an array may declare (a vocabulary is under a million).
pub const ARRAY_LEN_MAX: u64 = 1 << 24;
/// How deep arrays may nest.
pub const ARRAY_DEPTH_MAX: u32 = 4;
/// The longest key (real keys: under 100 bytes).
pub const KEY_LEN_MAX: u64 = 256;
/// The longest string value kept; a longer one (a chat template) is
/// skipped over, never allocated.
pub const STR_KEEP_MAX: u64 = 4096;
/// The longest integer array kept (a per-layer value); longer is skipped.
pub const INTS_MAX: u64 = 4096;
/// The most bytes the kept entries may hold, all together.
pub const KEPT_BYTES_MAX: u64 = 1 << 20;

/// The ranges a fact must be in (inclusive), by the suffix of its key.
pub const BLOCK_COUNT: (u64, u64) = (1, 4096);
pub const CONTEXT_LENGTH: (u64, u64) = (1, 1 << 27);
pub const EMBEDDING_LENGTH: (u64, u64) = (1, 1 << 20);
pub const HEAD_COUNT: (u64, u64) = (0, 65_536);
pub const HEAD_COUNT_KV: (u64, u64) = (0, 65_536);
pub const KEY_VALUE_LENGTH: (u64, u64) = (1, 65_536);

/// The keys (by suffix after `<arch>.`) the facts are read from; only
/// these, and `general.architecture` and `general.name`, are kept.
const WANTED: &[&str] = &[
    "block_count",
    "context_length",
    "embedding_length",
    "attention.head_count",
    "attention.head_count_kv",
    "attention.key_length",
    "attention.value_length",
];

fn wanted(key: &str) -> bool {
    key == "general.architecture" || key == "general.name" || key.split_once('.').is_some_and(|(_, rest)| WANTED.contains(&rest))
}

/// The refusal of a header field outside its range.
fn out_of_range(field: &str, value: u64, (lo, hi): (u64, u64)) -> anyhow::Error {
    anyhow::anyhow!("the model file's header is refused: `{field}` is {value}, outside the range this reader accepts ({lo} to {hi})")
}

fn in_range(field: &str, value: u64, r: (u64, u64)) -> Result<u64> {
    if value < r.0 || value > r.1 {
        return Err(out_of_range(field, value, r));
    }
    Ok(value)
}

/// A reader that refuses to read past [`HEADER_BYTES_MAX`], and counts.
struct Capped<R> {
    inner: R,
    read: u64,
}

impl<R: Read> Read for Capped<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let left = HEADER_BYTES_MAX - self.read;
        if left == 0 && !buf.is_empty() {
            return Err(std::io::Error::other(format!(
                "the model file's header is refused: `header bytes` passes {HEADER_BYTES_MAX}, the most this reader reads"
            )));
        }
        let n = buf.len().min(left as usize);
        let got = self.inner.read(&mut buf[..n])?;
        self.read += got as u64;
        Ok(got)
    }
}

fn u32le(r: &mut impl Read) -> Result<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}

fn u64le(r: &mut impl Read) -> Result<u64> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b)?;
    Ok(u64::from_le_bytes(b))
}

fn skip(r: &mut impl Read, n: u64) -> Result<()> {
    let got = std::io::copy(&mut r.take(n), &mut std::io::sink())?;
    if got != n {
        bail!("the header ends early");
    }
    Ok(())
}

/// A string: kept (at most `keep` bytes, allocated only then) or skipped.
fn string(r: &mut impl Read, field: &str, keep: Option<u64>) -> Result<Option<String>> {
    let n = in_range(field, u64le(r)?, (0, HEADER_BYTES_MAX))?;
    match keep {
        Some(k) if n <= k => {
            let mut b = vec![0u8; n as usize];
            r.read_exact(&mut b)?;
            Ok(Some(String::from_utf8_lossy(&b).into_owned()))
        }
        _ => {
            skip(r, n)?;
            Ok(None)
        }
    }
}

/// The byte size of a fixed-size scalar type, or `None`.
fn fixed(t: u32) -> Option<u64> {
    match t {
        0 | 1 | 7 => Some(1),
        2 | 3 => Some(2),
        4..=6 => Some(4),
        10..=12 => Some(8),
        _ => None,
    }
}

fn scalar(r: &mut impl Read, t: u32, keep: bool) -> Result<Meta> {
    Ok(match t {
        0 | 7 => {
            let mut b = [0u8; 1];
            r.read_exact(&mut b)?;
            Meta::Int(b[0] as u64)
        }
        1 => {
            let mut b = [0u8; 1];
            r.read_exact(&mut b)?;
            Meta::Int((b[0] as i8).max(0) as u64)
        }
        2 | 3 => {
            let mut b = [0u8; 2];
            r.read_exact(&mut b)?;
            Meta::Int(if t == 2 { u16::from_le_bytes(b) as u64 } else { i16::from_le_bytes(b).max(0) as u64 })
        }
        4 => Meta::Int(u32le(r)? as u64),
        5 => Meta::Int((u32le(r)? as i32).max(0) as u64),
        6 => Meta::Float(f32::from_bits(u32le(r)?) as f64),
        10 => Meta::Int(u64le(r)?),
        11 => Meta::Int((u64le(r)? as i64).max(0) as u64),
        12 => Meta::Float(f64::from_bits(u64le(r)?)),
        8 => match string(r, "string length", keep.then_some(STR_KEEP_MAX))? {
            Some(s) => Meta::Str(s),
            None => Meta::Skipped,
        },
        other => bail!("unknown metadata type {other}"),
    })
}

fn value(r: &mut impl Read, t: u32, keep: bool, depth: u32) -> Result<Meta> {
    if t != 9 {
        return scalar(r, t, keep);
    }
    if depth >= ARRAY_DEPTH_MAX {
        return Err(out_of_range("array depth", depth as u64 + 1, (0, ARRAY_DEPTH_MAX as u64)));
    }
    let et = u32le(r)?;
    let n = in_range("array length", u64le(r)?, (0, ARRAY_LEN_MAX))?;
    if let Some(size) = fixed(et) {
        if keep && n <= INTS_MAX && !matches!(et, 6 | 12) {
            let mut out = Vec::with_capacity(n as usize);
            for _ in 0..n {
                if let Meta::Int(v) = scalar(r, et, false)? {
                    out.push(v);
                }
            }
            return Ok(Meta::Ints(out));
        }
        skip(r, n * size)?;
        return Ok(Meta::Skipped);
    }
    for _ in 0..n {
        value(r, et, false, depth + 1)?;
    }
    Ok(Meta::Skipped)
}

/// The bytes a kept entry holds, for [`KEPT_BYTES_MAX`].
fn weight(k: &str, v: &Meta) -> u64 {
    k.len() as u64
        + match v {
            Meta::Str(s) => s.len() as u64,
            Meta::Ints(x) => 8 * x.len() as u64,
            _ => 8,
        }
}

/// The metadata entries of the file at `path` that the facts are read from
/// (`general.architecture`, `general.name`, and the `<arch>.` keys the
/// estimate needs; arrays other than short integer ones are
/// [`Meta::Skipped`]), and the format version. Every other entry is read
/// over, never kept; every declared count and length is bounded (the
/// constants above) and at most [`HEADER_BYTES_MAX`] bytes are read.
pub fn metadata(path: &Path) -> Result<(u32, BTreeMap<String, Meta>)> {
    let f = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut r = Capped { inner: BufReader::with_capacity(1 << 20, f), read: 0 };
    let mut magic = [0u8; 4];
    r.read_exact(&mut magic).context("reading the first bytes")?;
    if &magic != MAGIC {
        bail!("{} is not a GGUF model file (it does not start with `GGUF`)", path.display());
    }
    let version = u32le(&mut r)?;
    if !(2..=3).contains(&version) {
        bail!("{} is GGUF version {version}; this reader knows versions 2 and 3", path.display());
    }
    in_range("tensor count", u64le(&mut r)?, (0, TENSOR_COUNT_MAX))?;
    let n = in_range("metadata entry count", u64le(&mut r)?, (0, KV_COUNT_MAX))?;
    let mut out = BTreeMap::new();
    let mut kept: u64 = 0;
    for i in 0..n {
        let key = {
            let n = in_range("key length", u64le(&mut r)?, (0, KEY_LEN_MAX)).with_context(|| format!("metadata entry {i}"))?;
            let mut b = vec![0u8; n as usize];
            r.read_exact(&mut b).with_context(|| format!("metadata entry {i}"))?;
            String::from_utf8_lossy(&b).into_owned()
        };
        let t = u32le(&mut r)?;
        let keep = wanted(&key);
        let v = value(&mut r, t, keep, 0).with_context(|| format!("metadata `{key}`"))?;
        if keep {
            kept += weight(&key, &v);
            if kept > KEPT_BYTES_MAX {
                return Err(out_of_range("kept metadata bytes", kept, (0, KEPT_BYTES_MAX)));
            }
            out.insert(key, v);
        }
    }
    Ok((version, out))
}

/// An architecture name: what it is joined to make keys from.
fn arch_ok(a: &str) -> bool {
    !a.is_empty() && a.len() <= 64 && a.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.'))
}

impl Facts {
    /// **Is every fact in its range?** The refusal names the field. Facts
    /// read by [`facts`] always are; this is also checked before an
    /// estimate is made from a registry written by hand or by another
    /// version.
    pub fn check(&self) -> Result<()> {
        let one = |field: &str, v: Option<u64>, r: (u64, u64)| -> Result<()> {
            if let Some(v) = v {
                in_range(field, v, r)?;
            }
            Ok(())
        };
        one("block_count", self.block_count, BLOCK_COUNT)?;
        one("context_length", self.context_length, CONTEXT_LENGTH)?;
        one("embedding_length", self.embedding_length, EMBEDDING_LENGTH)?;
        one("attention.head_count", self.head_count, HEAD_COUNT)?;
        one("attention.key_length", self.key_length, KEY_VALUE_LENGTH)?;
        one("attention.value_length", self.value_length, KEY_VALUE_LENGTH)?;
        in_range("attention.head_count_kv (entries)", self.head_count_kv.len() as u64, (0, BLOCK_COUNT.1))?;
        for v in &self.head_count_kv {
            in_range("attention.head_count_kv", *v, HEAD_COUNT_KV)?;
        }
        if let Some(a) = &self.architecture {
            if !arch_ok(a) {
                bail!("the model file's header is refused: `general.architecture` is not an architecture name (letters, digits, `_`, `-`, `.`, at most 64)");
            }
        }
        Ok(())
    }
}

/// The [`Facts`] of the file at `path`; a fact outside its range is
/// refused, naming the field ([`Facts::check`]).
pub fn facts(path: &Path) -> Result<Facts> {
    let (version, m) = metadata(path)?;
    let arch = match m.get("general.architecture") {
        Some(Meta::Str(s)) => Some(s.clone()),
        _ => None,
    };
    if let Some(a) = &arch {
        if !arch_ok(a) {
            bail!("the model file's header is refused: `general.architecture` is not an architecture name (letters, digits, `_`, `-`, `.`, at most 64)");
        }
    }
    let int = |k: &str| -> Option<u64> {
        let a = arch.as_deref()?;
        match m.get(&format!("{a}.{k}")) {
            Some(Meta::Int(v)) => Some(*v),
            Some(Meta::Ints(v)) => v.iter().copied().max(),
            _ => None,
        }
    };
    let kv = arch.as_deref().and_then(|a| m.get(&format!("{a}.attention.head_count_kv"))).map(|v| match v {
        Meta::Int(x) => vec![*x],
        Meta::Ints(x) => x.clone(),
        _ => vec![],
    });
    let f = Facts {
        version,
        name: match m.get("general.name") {
            Some(Meta::Str(s)) => Some(s.chars().take(200).collect()),
            _ => None,
        },
        block_count: int("block_count"),
        context_length: int("context_length"),
        embedding_length: int("embedding_length"),
        head_count: int("attention.head_count"),
        head_count_kv: kv.unwrap_or_default(),
        key_length: int("attention.key_length"),
        value_length: int("attention.value_length"),
        architecture: arch,
    };
    f.check()?;
    Ok(f)
}

/// **A GGUF header, written**: for tests and fixtures, a file with these
/// metadata entries and no tensors (strings, unsigned integers, integer
/// arrays).
pub fn write_header(path: &Path, entries: &[(&str, Meta)], padding: u64) -> Result<()> {
    use std::io::Write;
    let mut out: Vec<u8> = Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&3u32.to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes());
    out.extend_from_slice(&(entries.len() as u64).to_le_bytes());
    let s = |out: &mut Vec<u8>, v: &str| {
        out.extend_from_slice(&(v.len() as u64).to_le_bytes());
        out.extend_from_slice(v.as_bytes());
    };
    for (k, v) in entries {
        s(&mut out, k);
        match v {
            Meta::Int(x) if *x > u32::MAX as u64 => {
                out.extend_from_slice(&10u32.to_le_bytes());
                out.extend_from_slice(&x.to_le_bytes());
            }
            Meta::Int(x) => {
                out.extend_from_slice(&4u32.to_le_bytes());
                out.extend_from_slice(&(*x as u32).to_le_bytes());
            }
            Meta::Float(x) => {
                out.extend_from_slice(&6u32.to_le_bytes());
                out.extend_from_slice(&(*x as f32).to_bits().to_le_bytes());
            }
            Meta::Str(x) => {
                out.extend_from_slice(&8u32.to_le_bytes());
                s(&mut out, x);
            }
            Meta::Ints(xs) => {
                out.extend_from_slice(&9u32.to_le_bytes());
                out.extend_from_slice(&4u32.to_le_bytes());
                out.extend_from_slice(&(xs.len() as u64).to_le_bytes());
                for x in xs {
                    out.extend_from_slice(&(*x as u32).to_le_bytes());
                }
            }
            Meta::Skipped => {
                // An array of strings, to be skipped over by the reader.
                out.extend_from_slice(&9u32.to_le_bytes());
                out.extend_from_slice(&8u32.to_le_bytes());
                out.extend_from_slice(&2u64.to_le_bytes());
                s(&mut out, "a");
                s(&mut out, "bc");
            }
        }
    }
    let mut f = std::fs::File::create(path)?;
    f.write_all(&out)?;
    // Stand-in weights: zeros after the header, so the file has a size.
    let zeros = vec![0u8; 1 << 16];
    let mut left = padding;
    while left > 0 {
        let n = left.min(zeros.len() as u64) as usize;
        f.write_all(&zeros[..n])?;
        left -= n as u64;
    }
    f.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_written_header_reads_back_and_arrays_are_skipped_or_kept() {
        let d = std::env::temp_dir().join(format!("gguf-t-{}-{}", std::process::id(), chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("m.gguf");
        write_header(
            &p,
            &[
                ("general.architecture", Meta::Str("qwen3moe".into())),
                ("general.name", Meta::Str("Fixture".into())),
                ("tokenizer.ggml.tokens", Meta::Skipped),
                ("qwen3moe.block_count", Meta::Int(4)),
                ("qwen3moe.attention.head_count", Meta::Int(8)),
                ("qwen3moe.attention.head_count_kv", Meta::Ints(vec![2, 0, 2, 0])),
                ("qwen3moe.attention.key_length", Meta::Int(64)),
                ("qwen3moe.embedding_length", Meta::Int(512)),
            ],
            100,
        )
        .unwrap();
        let f = facts(&p).unwrap();
        assert_eq!(f.architecture.as_deref(), Some("qwen3moe"));
        assert_eq!(f.block_count, Some(4));
        assert_eq!(f.head_count_kv, vec![2, 0, 2, 0]);
        assert_eq!(f.key_length, Some(64));
        assert_eq!(f.value_length, None);
        std::fs::write(d.join("x.gguf"), b"GGML1234").unwrap();
        assert!(facts(&d.join("x.gguf")).unwrap_err().to_string().contains("not a GGUF"));
        let _ = std::fs::remove_dir_all(&d);
    }

    fn scratch(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("gguf-{tag}-{}-{}", std::process::id(), chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// This process's resident size, in KiB (`ps`).
    fn rss_kib() -> u64 {
        let o = std::process::Command::new("/bin/ps").args(["-o", "rss=", "-p", &std::process::id().to_string()]).output().unwrap();
        String::from_utf8_lossy(&o.stdout).trim().parse().unwrap_or(0)
    }

    /// Run `f` while a sampler watches this process's resident size; answer
    /// the most it grew, in KiB.
    fn peak_growth(f: impl FnOnce()) -> u64 {
        use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
        use std::sync::Arc;
        let base = rss_kib();
        let (stop, peak) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicU64::new(base)));
        let t = {
            let (stop, peak) = (stop.clone(), peak.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    peak.fetch_max(rss_kib(), Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            })
        };
        f();
        peak.fetch_max(rss_kib(), Ordering::SeqCst);
        stop.store(true, Ordering::SeqCst);
        t.join().unwrap();
        peak.load(Ordering::SeqCst).saturating_sub(base)
    }

    /// The memory a hostile header may cost, at most (KiB): the reader's own
    /// buffers and kept bytes are a few MiB; this is a ceiling with room.
    const CEILING_KIB: u64 = 256 * 1024;

    #[test]
    fn the_reported_header_a_block_count_of_two_to_the_forty_is_refused_by_name() {
        let d = scratch("h40");
        let p = d.join("hostile.gguf");
        write_header(&p, &[("general.architecture", Meta::Str("llama".into())), ("llama.block_count", Meta::Int(1 << 40)), ("llama.attention.head_count_kv", Meta::Int(1))], 0).unwrap();
        let t0 = std::time::Instant::now();
        let mut err = String::new();
        let grew = peak_growth(|| err = format!("{:#}", facts(&p).unwrap_err()));
        assert!(err.contains("`block_count` is 1099511627776") && err.contains("1 to 4096"), "{err}");
        assert!(t0.elapsed() < std::time::Duration::from_secs(5));
        assert!(grew < CEILING_KIB, "grew {grew} KiB");
        // The estimate never allocates per layer, whatever a registry says.
        for layers in [1u64 << 40, u64::MAX] {
            let f = Facts { block_count: Some(layers), head_count_kv: vec![1], key_length: Some(128), value_length: Some(128), ..Facts::default() };
            assert!(crate::memory::kv_per_token(&f).is_none());
            let e = crate::memory::estimate(1 << 30, &f, 4096, 64 << 30);
            assert!(e.how.contains("a quarter"), "{}", e.how);
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn every_declared_size_is_bounded_and_the_field_named() {
        let d = scratch("bounds");
        let p = d.join("b.gguf");
        let head = |tensors: u64, n: u64| {
            let mut b = Vec::new();
            b.extend_from_slice(MAGIC);
            b.extend_from_slice(&3u32.to_le_bytes());
            b.extend_from_slice(&tensors.to_le_bytes());
            b.extend_from_slice(&n.to_le_bytes());
            b
        };
        let entry = |b: &mut Vec<u8>, k: &str, t: u32| {
            b.extend_from_slice(&(k.len() as u64).to_le_bytes());
            b.extend_from_slice(k.as_bytes());
            b.extend_from_slice(&t.to_le_bytes());
        };
        let refused = |b: &[u8], field: &str| {
            std::fs::write(&p, b).unwrap();
            let e = format!("{:#}", facts(&p).unwrap_err());
            assert!(e.contains(&format!("`{field}`")), "{field}: {e}");
        };
        refused(&head(u64::MAX, 0), "tensor count");
        refused(&head(0, 1 << 40), "metadata entry count");
        let mut b = head(0, 1);
        b.extend_from_slice(&(1u64 << 40).to_le_bytes());
        refused(&b, "key length");
        let mut b = head(0, 1);
        entry(&mut b, "tokenizer.ggml.tokens", 9);
        b.extend_from_slice(&8u32.to_le_bytes());
        b.extend_from_slice(&(1u64 << 40).to_le_bytes());
        refused(&b, "array length");
        let mut b = head(0, 1);
        entry(&mut b, "general.name", 8);
        b.extend_from_slice(&u64::MAX.to_le_bytes());
        refused(&b, "string length");
        let mut b = head(0, 1);
        entry(&mut b, "x.nested", 9);
        for _ in 0..8 {
            b.extend_from_slice(&9u32.to_le_bytes());
            b.extend_from_slice(&1u64.to_le_bytes());
        }
        refused(&b, "array depth");
        for (k, v, field) in [
            ("llama.context_length", 1u64 << 40, "context_length"),
            ("llama.embedding_length", 1 << 40, "embedding_length"),
            ("llama.attention.head_count", 1 << 40, "attention.head_count"),
            ("llama.attention.head_count_kv", 1 << 40, "attention.head_count_kv"),
            ("llama.attention.key_length", 1 << 40, "attention.key_length"),
            ("llama.attention.value_length", 0, "attention.value_length"),
            ("llama.block_count", 0, "block_count"),
        ] {
            write_header(&p, &[("general.architecture", Meta::Str("llama".into())), (k, Meta::Int(v))], 0).unwrap();
            let e = format!("{:#}", facts(&p).unwrap_err());
            assert!(e.contains(&format!("`{field}` is {v}")), "{field}: {e}");
        }
        // A string declared longer than the most this reader reads, in a
        // sparse file that really is that long: read up to the cap, refused.
        let mut b = head(0, 1);
        entry(&mut b, "tokenizer.chat_template", 8);
        b.extend_from_slice(&(HEADER_BYTES_MAX + 1).to_le_bytes());
        std::fs::write(&p, &b).unwrap();
        std::fs::OpenOptions::new().write(true).open(&p).unwrap().set_len(HEADER_BYTES_MAX + (1 << 20)).unwrap();
        let e = format!("{:#}", facts(&p).unwrap_err());
        assert!(e.contains("`string length`"), "{e}");
        let mut b = head(0, 1);
        entry(&mut b, "tokenizer.ggml.merges", 9);
        // 2^24 eight-byte elements: exactly the cap, after the header's own bytes.
        b.extend_from_slice(&10u32.to_le_bytes());
        b.extend_from_slice(&(ARRAY_LEN_MAX).to_le_bytes());
        std::fs::write(&p, &b).unwrap();
        std::fs::OpenOptions::new().write(true).open(&p).unwrap().set_len(HEADER_BYTES_MAX + (1 << 20)).unwrap();
        let e = format!("{:#}", facts(&p).unwrap_err());
        assert!(e.contains("`header bytes`"), "{e}");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// xorshift64*, for mutations that are the same at every run.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(2_685_821_657_736_338_717)
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n.max(1)
        }
    }

    #[test]
    fn many_mutated_headers_never_cost_more_than_the_ceiling() {
        let d = scratch("fuzz");
        let p = d.join("m.gguf");
        write_header(
            &p,
            &[
                ("general.architecture", Meta::Str("llama".into())),
                ("general.name", Meta::Str("Fixture".into())),
                ("tokenizer.ggml.tokens", Meta::Skipped),
                ("llama.block_count", Meta::Int(4)),
                ("llama.context_length", Meta::Int(8192)),
                ("llama.embedding_length", Meta::Int(512)),
                ("llama.attention.head_count", Meta::Int(8)),
                ("llama.attention.head_count_kv", Meta::Ints(vec![2, 2, 2, 2])),
                ("llama.attention.key_length", Meta::Int(64)),
                ("tokenizer.chat_template", Meta::Str("{{ x }}".repeat(40))),
            ],
            0,
        )
        .unwrap();
        let base = std::fs::read(&p).unwrap();
        let hostile = [u64::MAX, 1 << 63, 1 << 40, 1 << 32, u32::MAX as u64, 1 << 24, 4097, 0];
        let mut rng = Rng(20_261_117);
        let (mut ok, mut refused) = (0u32, 0u32);
        let grew = peak_growth(|| {
            for _ in 0..4000 {
                let mut b = base.clone();
                for _ in 0..=rng.below(3) {
                    match rng.below(5) {
                        0 if !b.is_empty() => {
                            let i = rng.below(b.len() as u64) as usize;
                            b[i] = rng.next() as u8;
                        }
                        1 | 2 if b.len() >= 8 => {
                            let i = rng.below(b.len().saturating_sub(8) as u64) as usize;
                            let v = if rng.below(3) == 0 { rng.next() } else { hostile[rng.below(hostile.len() as u64) as usize] };
                            let w = if rng.below(2) == 0 { v.to_le_bytes().to_vec() } else { (v as u32).to_le_bytes().to_vec() };
                            b[i..i + w.len()].copy_from_slice(&w);
                        }
                        3 if !b.is_empty() => b.truncate(rng.below(b.len() as u64) as usize),
                        _ => b.extend((0..rng.below(64)).map(|_| rng.next() as u8)),
                    }
                }
                std::fs::write(&p, &b).unwrap();
                match facts(&p) {
                    Ok(f) => {
                        ok += 1;
                        f.check().unwrap();
                        let _ = crate::memory::estimate(b.len() as u64, &f, 65_536, 64 << 30);
                        let (_, m) = metadata(&p).unwrap();
                        assert!(m.iter().map(|(k, v)| weight(k, v)).sum::<u64>() <= KEPT_BYTES_MAX);
                    }
                    Err(_) => refused += 1,
                }
            }
        });
        assert!(ok > 0 && refused > 0, "the mutations reach both sides: {ok} read, {refused} refused");
        assert!(grew < CEILING_KIB, "4000 mutated headers grew this process by {grew} KiB");
        let _ = std::fs::remove_dir_all(&d);
    }
}
