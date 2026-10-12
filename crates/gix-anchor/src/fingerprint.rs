//! [`Fingerprint`]: a fuzzy, versioned, non-identity-bearing content
//! signature — one of `hints.fingerprints` (`anchor.hints`).
//!
//! `minhash-shingle-v2` is the current fixed-width MinHash over overlapping
//! byte shingles. Version 1 remains readable for persisted hints. Algorithm
//! names and parameters travel with each value, and hints never affect
//! [`crate::Anchor::id`].

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

/// The current fingerprint format with sixteen independent MinHash slots.
pub const MINHASH_SHINGLE_V2: &str = "minhash-shingle-v2";

/// The shingle length used by fingerprint version 1.
const V1_SHINGLE_LEN: usize = 8;

/// The shingle length used by the current fingerprint version.
const SHINGLE_LEN: usize = 4;

/// How many independent hash functions [`capture`](crate::capture)
/// fingerprints with.
const NUM_HASHES: usize = 16;

/// Fixed odd multipliers seeding [`NUM_HASHES`] independent hash functions —
/// arbitrary but fixed, since the whole point of naming the algorithm and its
/// parameters is that the mapping from bytes to fingerprint never moves
/// under an unmarked code change.
const SEEDS: [u64; NUM_HASHES] = [
    0x9E37_79B9_7F4A_7C15,
    0xC2B2_AE3D_27D4_EB4F,
    0x1656_67B1_9E37_79F9,
    0xFF51_AFD7_ED55_8CCD,
    0xD6E8_FEB8_6659_FD93,
    0xA5A3_58F1_9D2C_13B7,
    0x8CB9_2BA7_2F3D_8DD7,
    0xDB4F_0B91_75AE_2165,
    0xBBE0_5A4D_9C2F_137B,
    0x6C8E_9CF5_7093_2BD1,
    0xF135_7AEA_2E62_A9C5,
    0x91E1_0DA5_C79E_7B3F,
    0xB5C0_FBCF_EC4D_3A29,
    0xE8D3_2A74_6F91_C5B7,
    0x47A1_9D3C_B682_F05E,
    0xC6BC_2796_92B5_C323,
];

/// Capture a v2 fingerprint with sixteen independent MinHash slots.
#[must_use]
pub fn capture_fingerprint(bytes: &[u8]) -> Fingerprint {
    fingerprint(bytes, MINHASH_SHINGLE_V2, SHINGLE_LEN, NUM_HASHES)
}

fn fingerprint(bytes: &[u8], algo: &str, shingle_len: usize, hashes: usize) -> Fingerprint {
    Fingerprint {
        algo: algo.to_owned(),
        params: vec![
            Param {
                key: "k".to_owned(),
                value: shingle_len.to_string(),
            },
            Param {
                key: "hashes".to_owned(),
                value: hashes.to_string(),
            },
        ],
        value: minhash_shingle(bytes, shingle_len, hashes),
    }
}

pub(crate) fn fingerprint_for(algo: &str, bytes: &[u8]) -> Option<Vec<u8>> {
    match algo {
        MINHASH_SHINGLE_V1 => Some(minhash_shingle(bytes, V1_SHINGLE_LEN, 4)),
        MINHASH_SHINGLE_V2 => Some(minhash_shingle(bytes, SHINGLE_LEN, NUM_HASHES)),
        _ => None,
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
/// let a = minhash_shingle(b"the quick brown fox", 8, 16);
/// let b = minhash_shingle(b"the quick brown fox", 8, 16);
/// assert_eq!(a, b, "deterministic for identical input");
/// assert_eq!(a.len(), 16 * 8, "16 hashes, 8 bytes each");
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
        let a = minhash_shingle(b"the quick brown fox jumps", 8, 16);
        let b = minhash_shingle(b"the quick brown fox jumps", 8, 16);
        assert_eq!(a, b);
        let c = minhash_shingle(b"the quick brown fox leaps", 8, 16);
        assert_ne!(a, c);
    }

    #[test]
    fn minhash_handles_input_shorter_than_the_shingle() {
        let out = minhash_shingle(b"hi", 8, 16);
        assert_eq!(out.len(), 16 * 8);
    }

    #[test]
    fn capture_fingerprint_names_its_algorithm_and_parameters() {
        let fp = capture_fingerprint(b"some content");
        assert_eq!(fp.algo, MINHASH_SHINGLE_V2);
        let keys: Vec<&str> = fp.params.iter().map(|p| p.key.as_str()).collect();
        assert_eq!(keys, vec!["k", "hashes"]);
        assert_eq!(fp.value.len(), 16 * 8);
    }
}
