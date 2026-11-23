//! **The memory estimate a model is held to before it is loaded.**
//!
//! `total = weights + kv + allowance`, where
//!
//! - `weights` is the model file's size (llama-server maps the whole file);
//! - `kv` is the KV cache for the context, at llama-server's default cache
//!   type (f16, 2 bytes a value): `context × Σ over layers (KV heads of the
//!   layer × (key length + value length)) × 2`. The head counts and lengths
//!   come from the file's header ([`crate::gguf::Facts`]); a key or value
//!   length the header does not give is `embedding_length / head_count`. A
//!   header that lacks what this needs is estimated at a quarter of the
//!   file's size, and the estimate says so. Models whose layers do not all
//!   keep a full cache (sliding windows, recurrent layers that are not
//!   marked with zero KV heads) are over-estimated, never under;
//! - `allowance` is a fixed 2 GiB for llama-server's compute buffers and
//!   the runtime itself.
//!
//! A model whose total is over the budget is refused, with this arithmetic
//! in the message.

use serde::Serialize;

use crate::gguf::Facts;
use crate::GIB;

/// The fixed allowance for compute buffers and the runtime.
pub const ALLOWANCE: u64 = 2 * GIB;
/// Bytes per cached value (f16).
pub const KV_BYTES: u64 = 2;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Estimate {
    pub weights_bytes: u64,
    pub kv_bytes: u64,
    pub allowance_bytes: u64,
    pub total_bytes: u64,
    pub context: u32,
    pub budget_bytes: u64,
    pub fits: bool,
    /// The arithmetic, in one line.
    pub how: String,
}

/// Bytes as GiB, two decimals.
pub fn gib(b: u64) -> String {
    format!("{:.2} GiB", b as f64 / GIB as f64)
}

/// The KV bytes per token of context, and how they were found; `None` when
/// the header does not give enough, when a fact is outside its range
/// ([`Facts::check`]), or when it gives no KV head at all (a recurrent
/// model, or a header that says so to be estimated at nothing): those are
/// estimated at a quarter of the file, never at zero. Nothing is allocated
/// per layer: the layer count is a header's word.
pub fn kv_per_token(f: &Facts) -> Option<(u64, String)> {
    f.check().ok()?;
    let layers = f.block_count.filter(|n| *n > 0)?;
    let (heads, first) = match f.head_count_kv.as_slice() {
        [] => (f.head_count?.checked_mul(layers)?, f.head_count?),
        [one] => (one.checked_mul(layers)?, *one),
        many if many.len() as u64 == layers => (many.iter().try_fold(0u64, |a, b| a.checked_add(*b))?, many[0]),
        _ => return None,
    };
    if heads == 0 {
        return None;
    }
    let derived = || -> Option<u64> { Some(f.embedding_length? / f.head_count.filter(|h| *h > 0)?) };
    let k = f.key_length.or_else(derived).filter(|k| *k > 0)?;
    let v = f.value_length.or_else(derived).filter(|v| *v > 0)?;
    let bytes = heads.checked_mul(k.checked_add(v)?)?.checked_mul(KV_BYTES)?;
    let layers_text = match f.head_count_kv.as_slice() {
        many if many.len() > 1 => format!("{heads} KV heads over {layers} layers"),
        _ => format!("{layers} layers × {first} KV heads"),
    };
    Some((bytes, format!("{layers_text} × ({k} + {v}) × {KV_BYTES} bytes = {bytes} bytes a token")))
}

/// **The estimate** for a model file of `size` bytes with header `f`,
/// served with `context` tokens, against `budget` bytes.
pub fn estimate(size: u64, f: &Facts, context: u32, budget: u64) -> Estimate {
    let (kv, kv_how) = match kv_per_token(f) {
        Some((per, how)) => {
            let kv = per.saturating_mul(context as u64);
            (kv, format!("KV cache {} ({how}, × {context} tokens)", gib(kv)))
        }
        None => {
            let kv = size / 4;
            (kv, format!("KV cache {} (the file's header does not give usable layer and head counts, so a quarter of the file's size is assumed)", gib(kv)))
        }
    };
    let total = size.saturating_add(kv).saturating_add(ALLOWANCE);
    let fits = total <= budget;
    let how = format!(
        "weights {} (the file's size) + {kv_how} + {} for compute buffers and the runtime = {} ({total} bytes), {} the memory budget of {}",
        gib(size),
        gib(ALLOWANCE),
        gib(total),
        if fits { "within" } else { "OVER" },
        gib(budget)
    );
    Estimate { weights_bytes: size, kv_bytes: kv, allowance_bytes: ALLOWANCE, total_bytes: total, context, budget_bytes: budget, fits, how }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> Facts {
        Facts {
            version: 3,
            architecture: Some("qwen3moe".into()),
            block_count: Some(48),
            head_count: Some(32),
            head_count_kv: vec![4],
            key_length: Some(128),
            value_length: Some(128),
            embedding_length: Some(2048),
            ..Facts::default()
        }
    }

    #[test]
    fn the_arithmetic_is_the_documented_one() {
        // 48 layers × 4 heads × (128 + 128) × 2 = 98,304 bytes a token.
        let (per, _) = kv_per_token(&facts()).unwrap();
        assert_eq!(per, 98_304);
        let e = estimate(38 * GIB, &facts(), 65_536, 64 * GIB);
        assert_eq!(e.kv_bytes, 98_304 * 65_536);
        assert_eq!(e.total_bytes, 38 * GIB + 6 * GIB + 2 * GIB);
        assert!(e.fits);
        assert!(e.how.contains("within"), "{}", e.how);
        let over = estimate(63 * GIB, &facts(), 131_072, 64 * GIB);
        assert!(!over.fits);
        assert!(over.how.contains("OVER") && over.how.contains("64.00 GiB") && over.how.contains("× 131072 tokens"), "{}", over.how);
    }

    #[test]
    fn per_layer_counts_derived_lengths_and_a_bare_header() {
        let mut f = facts();
        f.block_count = Some(4);
        f.head_count_kv = vec![4, 0, 4, 0];
        f.key_length = None;
        f.value_length = None;
        // 8 heads × (64 + 64) × 2 = 2048 (lengths from 2048 / 32).
        assert_eq!(kv_per_token(&f).unwrap().0, 2048);
        f.head_count_kv = vec![1, 2, 3];
        assert!(kv_per_token(&f).is_none(), "a per-layer list of the wrong length is not guessed at");
        let e = estimate(40 * GIB, &Facts::default(), 4096, 64 * GIB);
        assert_eq!(e.kv_bytes, 10 * GIB);
        assert!(e.how.contains("a quarter"), "{}", e.how);
    }
}
