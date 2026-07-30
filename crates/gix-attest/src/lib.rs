//! Signed claim envelopes and chains (ARCHITECTURE.md, "git-attest").
//!
//! A claim is a chained commit on a claim ref. The [`Envelope`] — target
//! descriptor, payload tree hash, signing-key id — is a `gix-store` typed
//! document; the signature bytes ride the claim commit's standard `gpgsig`
//! header, so `git log --show-signature` verifies a claim with stock git and
//! no tooling of ours installed.
//!
//! This crate owns the envelope and the chain. It owns no payload, no policy,
//! no validity, no hash function, and no ref-advance rules:
//!
//! | concern | owner |
//! |---|---|
//! | the hash function, the identity normal form | `facet-git-tree` — called here |
//! | signing mechanics, signature transport | `gix-store`'s `Signer` seam and `gpgsig` header — written through here |
//! | ref writes, compare-and-swap, layout | `gix-store` — called here |
//! | payload schemas (rebind pin, action, review) | their vocabulary owners — hashes carried, never fetched |
//! | claim validity, revocation *semantics*, thresholds | query rules — absent here |
//! | enforcement (which namespaces require signatures) | boundary hooks and policy — absent here |
//!
//! Sibling crates in this workspace are not siblings in the dependency graph:
//! `gix-anchor` and `gix-attest` do not depend on each other in either
//! direction, and CI proves it over `cargo metadata`. `"anchor"` occurs in
//! this crate only as an uninterpreted [`Target::kind`] label in tests.
//!
//! # Phase coverage
//!
//! What exists: [`Envelope`], [`Target`], [`target_key`],
//! [`register_claim_schema`], and [`Claims`] — `open`, `sign`, `log`.
//! [`Claims::resolve`] refuses with [`Error::Unimplemented`] rather than
//! reporting an unrevoked chain it has not checked, and no `verify` exists at
//! all; [`key`] and [`verify`] say what fills them.
//!
//! # Examples
//!
//! Sign two claims about one target and walk the chain:
//!
//! ```
//! use gix_attest::{Claims, Envelope, Target, register_claim_schema};
//! use gix_store::{MemoryRefStore, Store};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! // Any `gix-store` store will do; a real repository's is the usual one.
//! // `gix_attest::layout()` is what puts claim refs at `refs/claims/<target-key>`.
//! let store = Store::with_layout(
//!     MemoryRefStore::new(),
//!     facet_git_tree::ObjectStore::default(),
//!     gix_attest::layout(),
//! );
//! register_claim_schema(&store)?;
//!
//! // `"anchor"` is a label this crate never interprets, and the payload is a
//! // tree hash it never fetches.
//! let target = Target {
//!     kind: "anchor".to_owned(),
//!     id: gix::ObjectId::from_hex(b"7f3e000000000000000000000000000000000000")?.into(),
//! };
//! let envelope = Envelope {
//!     target: target.clone(),
//!     payload: gix::ObjectId::empty_tree(gix::hash::Kind::Sha1).into(),
//!     payload_kind: "rebind-pin".to_owned(),
//!     key: gix::ObjectId::null(gix::hash::Kind::Sha1).into(),
//! };
//!
//! let claims = Claims::open(&store);
//! let first = claims.sign(&envelope)?;
//! let second = claims.sign(&envelope)?;
//!
//! // The chain walks newest first, and the claim id is the commit's own oid.
//! let ids: Vec<_> = claims.log(&target)?.map(|claim| claim.id).collect();
//! assert_eq!(ids, vec![second, first]);
//! assert_eq!(
//!     claims.reference(&target)?.as_str(),
//!     format!("refs/claims/{}", gix_attest::target_key(&target)?),
//! );
//! # Ok(())
//! # }
//! ```
#![forbid(unsafe_code)]

mod chain;
mod envelope;
mod error;
#[cfg(test)]
mod fixture;
pub mod key;
mod oid;
mod schema;
pub mod verify;

pub use chain::{Claim, Claims, layout};
pub use envelope::{Envelope, Target, target_key};
pub use error::{Error, Result};
pub use oid::Oid;
pub use schema::{CLAIM_KIND, register_claim_schema};
