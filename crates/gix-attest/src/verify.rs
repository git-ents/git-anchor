//! Verification: the `Verifier` trait, its `ssh-ed25519` implementation, and
//! `Verdict`.
//!
//! **Phase 2 fills this module in.** It is declared now, empty, so the work
//! lands without touching [`crate`]'s module list. No `verify` function
//! exists anywhere in this crate yet — deliberately: a stub that answered at
//! all would answer wrongly, and the one thing worse than an unverified claim
//! is a claim reported as verified.
//!
//! What belongs here: re-serializing a claim commit without its `gpgsig`
//! header, resolving [`Envelope.key`](crate::Envelope) through
//! [`crate::key`]'s chain, and checking the stored signature block against
//! the key — dispatched on the key's format, cross-checked against the armor
//! preamble, exactly as git dispatches on `gpgsig` contents. That is the
//! entire function.
//!
//! The result type is named so it cannot be misread: `Verified` /
//! `BadSignature` / `UnknownKeyFormat`, with no `Valid` variant. Verification
//! is cryptography; *validity* is a query predicate, and it never enters this
//! repo.
