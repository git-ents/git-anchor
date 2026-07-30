//! [`Fingerprint`]: a fuzzy, versioned, non-identity-bearing content
//! signature — one of `hints.fingerprints` (`anchor.hints`).
//!
//! `minhash-shingle-v1` is the one algorithm this crate implements: a
//! bottom-`k`-free, fixed-width MinHash over overlapping byte shingles, cheap
//! to compute and to compare for near-duplicate detection. The name and its
//! parameters travel with every value, so a future algorithm lands
//! side-by-side rather than displacing this one, and nothing here is ever
//! read by [`crate::Anchor::id`].

use facet::Facet;

/// One `key = value` fingerprint parameter, named rather than positional so
/// [`Fingerprint::params`] self-describes without an external spec lookup.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct Param {
    /// The parameter's name (`"k"`, `"hashes"`, ...).
    pub key: String,
    /// The parameter's value, decimal for every parameter this crate emits.
    pub value: String,
}

/// A fuzzy content signature: which algorithm, under which parameters,
/// produced which bytes. Never identity-bearing (`anchor.hints`) — recomputed
/// on demand from `identity` and the repository, never trusted blindly.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct Fingerprint {
    /// The algorithm's name, e.g. [`MINHASH_SHINGLE_V1`].
    pub algo: String,
    /// The algorithm's parameters, order-independent.
    pub params: Vec<Param>,
    /// The algorithm's output bytes.
    pub value: Vec<u8>,
}

/// [`Fingerprint::algo`] for [`minhash_shingle`]: a fixed-width MinHash over
/// `k`-byte shingles, `hashes` independent hash functions.
pub const MINHASH_SHINGLE_V1: &str = "minhash-shingle-v1";

/// The shingle length [`capture`](crate::capture) fingerprints with.
const SHINGLE_LEN: usize = 8;

/// How many independent hash functions [`capture`](crate::capture)
/// fingerprints with.
const NUM_HASHES: usize = 4;

/// Fixed odd multipliers seeding [`NUM_HASHES`] independent hash functions —
/// arbitrary but fixed, since the whole point of naming the algorithm and its
/// parameters is that the mapping from bytes to fingerprint never moves
/// under an unmarked code change.
const SEEDS: [u64; NUM_HASHES] = [
    0x9E37_79B9_7F4A_7C15,
    0xC2B2_AE3D_27D4_EB4F,
    0x1656_67B1_9E37_79F9,
    0xFF51_AFD7_ED55_8CCD,
];

/// `minhash_shingle(bytes, SHINGLE_LEN, NUM_HASHES)`, tagged
/// [`MINHASH_SHINGLE_V1`] — the one fingerprint [`crate::capture`] and
/// [`crate::capture_worktree`] compute.
#[must_use]
pub fn capture_fingerprint(bytes: &[u8]) -> Fingerprint {
    Fingerprint {
        algo: MINHASH_SHINGLE_V1.to_owned(),
        params: vec![
            Param {
                key: "k".to_owned(),
                value: SHINGLE_LEN.to_string(),
            },
            Param {
                key: "hashes".to_owned(),
                value: NUM_HASHES.to_string(),
            },
        ],
        value: minhash_shingle(bytes, SHINGLE_LEN, NUM_HASHES),
    }
}

/// A fixed-width MinHash over `bytes`' overlapping `k`-byte shingles, one
/// `u64` minimum per hash function, big-endian concatenated. A `bytes`
/// shorter than `k` is treated as its own single shingle.
///
/// # Examples
///
/// ```
/// use gix_anchor::minhash_shingle;
///
/// let a = minhash_shingle(b"the quick brown fox", 8, 4);
/// let b = minhash_shingle(b"the quick brown fox", 8, 4);
/// assert_eq!(a, b, "deterministic for identical input");
/// assert_eq!(a.len(), 4 * 8, "4 hashes, 8 bytes each");
/// ```
#[must_use]
pub fn minhash_shingle(bytes: &[u8], k: usize, hashes: usize) -> Vec<u8> {
    let mut mins = vec![u64::MAX; hashes];
    let mut fold = |shingle: &[u8]| {
        let base = fnv1a(shingle);
        for (slot, seed) in mins.iter_mut().zip(SEEDS.iter().cycle()).take(hashes) {
            let h = splitmix64(base ^ seed);
            if h < *slot {
                *slot = h;
            }
        }
    };
    if bytes.len() <= k {
        fold(bytes);
    } else {
        for window in bytes.windows(k) {
            fold(window);
        }
    }
    mins.iter().flat_map(|v| v.to_be_bytes()).collect()
}

/// The 64-bit FNV-1a hash of `bytes` — a fast, well-distributed non-cryptographic
/// hash, adequate for shingle fingerprinting (fuzzy matching, not a security
/// boundary).
fn fnv1a(bytes: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01B3;
    let mut hash = OFFSET;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// `SplitMix64`'s output mixing step — cheap avalanche so [`fnv1a`]'s output
/// XORed against a fixed seed still spreads across the `u64` range per hash
/// function.
fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minhash_is_deterministic_and_sensitive_to_content() {
        let a = minhash_shingle(b"the quick brown fox jumps", 8, 4);
        let b = minhash_shingle(b"the quick brown fox jumps", 8, 4);
        assert_eq!(a, b);
        let c = minhash_shingle(b"the quick brown fox leaps", 8, 4);
        assert_ne!(a, c);
    }

    #[test]
    fn minhash_handles_input_shorter_than_the_shingle() {
        let out = minhash_shingle(b"hi", 8, 4);
        assert_eq!(out.len(), 4 * 8);
    }

    #[test]
    fn capture_fingerprint_names_its_algorithm_and_parameters() {
        let fp = capture_fingerprint(b"some content");
        assert_eq!(fp.algo, MINHASH_SHINGLE_V1);
        let keys: Vec<&str> = fp.params.iter().map(|p| p.key.as_str()).collect();
        assert_eq!(keys, vec!["k", "hashes"]);
    }
}
