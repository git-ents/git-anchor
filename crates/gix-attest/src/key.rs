//! Key lifecycle: `AttestKey`, its schema registration, and key-chain
//! resolution.
//!
//! **Phase 2 fills this module in.** It is declared now, empty, so the work
//! lands without touching [`crate`]'s module list.
//!
//! What belongs here, and nothing else: `AttestKey { format, public_key, .. }`
//! — the one payload schema attest registers, because key material *is*
//! envelope machinery — plus resolution of [`Envelope.key`](crate::Envelope)
//! through the key claim's own chain, so a rotation is a chained claim rather
//! than a new identity.
//!
//! What does not belong here: whether a key was *valid* at the moment a claim
//! was admitted. That is `key_valid_at`, a query predicate over op-log order,
//! and the retroactivity of key revocation is rule policy. Neither enters
//! this crate.
