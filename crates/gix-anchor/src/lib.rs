//! Anchor identity and hints (ARCHITECTURE.md, "git-anchor"): a `Binding`'s
//! non-derivable coordinates — a genesis commit, a path, a byte span — and
//! the additive, versioned, never-identity-bearing hints riding alongside
//! them, plus the three oracles that map an anchor onto another revision.
//!
//! This crate owns the `Anchor` abstraction. Anchors resolve independently of
//! any consumer: a downstream consumer's `Comment` is merely the first
//! client (its `anchor: RawTree` field embeds the tree an [`Anchor`]
//! serializes to), and reviews, TODO trackers, and blame overlays can reuse
//! the same mechanism. (A consuming crate depends on this crate, not the
//! other way around, so this crate's own examples and tests stand a
//! `Comment`-shaped struct in for it rather than importing it.)
//!
//! # Spec coverage
//!
//! This crate implements, from the crate specification:
//!
//! - `anchor.definition` — [`Anchor`] and [`capture`]'s validation.
//! - `anchor.identity` — [`AnchorIdentity`] and [`Anchor::id`], hashed
//!   through the identity normal form, independent of [`AnchorHints`].
//! - `anchor.immutable` — no mutating API exists; the commit id is plain
//!   data.
//! - `anchor.retention` — [`AnchorHints::fingerprints`] and
//!   [`AnchorHints::descriptors`], additive and never identity-bearing.
//! - `anchor.oracles` — [`diff_trace`], [`fingerprint_oracle`], and
//!   [`op_log`], each reporting `(oracle, confidence)` candidates and
//!   applying no threshold.
//! - `anchor.working-tree` — [`capture_worktree`], the on-disk bytes as a
//!   capture source, `HEAD` recorded as the best-effort commit field.
//! - `anchor.tree-pair-diff` — [`diff_trees`], a structural, pruning tree
//!   diff over any [`gix_object::Find`] source, with no `gix::Repository`
//!   required.
//!
//! # Examples
//!
//! Capture an anchor, store it inside a `Comment`, read it back, and map it
//! onto a later commit with [`diff_trace`]:
//!
//! ```
//! use gix_anchor::{Anchor, LineRange};
//! use facet_git_tree::RawTree;
//!
//! // Stands in for a downstream consumer's `Comment` (this crate cannot
//! // depend on such a consumer, which itself depends on this crate): any
//! // struct embedding an anchor's tree by `RawTree` behaves identically.
//! # #[derive(facet::Facet)]
//! # struct Comment { body: String, anchor: RawTree }
//! #
//! # fn git(dir: &std::path::Path, args: &[&str]) {
//! #     let status = std::process::Command::new("git").arg("-C").arg(dir)
//! #         .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
//! #         .args(args).status().unwrap();
//! #     assert!(status.success());
//! # }
//! # let dir = tempfile::tempdir().expect("tempdir");
//! # std::process::Command::new("git").arg("init").arg("-q").arg(dir.path()).status().unwrap();
//! # std::fs::write(dir.path().join("file.txt"), (1..=10).map(|n| format!("line {n}\n")).collect::<String>()).unwrap();
//! # git(dir.path(), &["add", "-A"]);
//! # git(dir.path(), &["commit", "-q", "-m", "one"]);
//! let repo = gix::open(dir.path()).expect("open");
//!
//! // Capture against HEAD: the line range is a capture-time input,
//! // canonicalized to a byte span before it ever reaches `identity`.
//! let anchor = gix_anchor::capture(&repo, "HEAD", "file.txt", Some(LineRange { start: 3, end: 4 }))
//!     .expect("capture");
//!
//! // The anchor serializes into the same store the comment does; the
//! // comment embeds it by tree id (`RawTree`), so the anchored content is
//! // reachable from the comment's own ref.
//! let store = facet_git_tree::ObjectStore::default();
//! let anchor_tree = facet_git_tree::serialize_into(&anchor, &store).expect("serialize anchor");
//! let comment = Comment {
//!     body: "these two lines look off by one".to_owned(),
//!     anchor: RawTree::new(anchor_tree),
//! };
//! let root = facet_git_tree::serialize_into(&comment, &store).expect("serialize comment");
//!
//! // Read the comment back and recover the identical anchor.
//! let back: Comment = facet_git_tree::deserialize(&root, &store).expect("deserialize comment");
//! let anchor_back: Anchor =
//!     facet_git_tree::deserialize(&back.anchor.oid(), &store).expect("deserialize anchor");
//! assert_eq!(anchor_back, anchor);
//!
//! // Edit above the range and trace: the anchor relocates, unmutated.
//! # std::fs::write(dir.path().join("file.txt"), format!("added\n{}", (1..=10).map(|n| format!("line {n}\n")).collect::<String>())).unwrap();
//! # git(dir.path(), &["add", "-A"]);
//! # git(dir.path(), &["commit", "-q", "-m", "two"]);
//! let repo = gix::open(dir.path()).expect("reopen");
//! let candidates = gix_anchor::diff_trace(&repo, &anchor_back, "HEAD").expect("diff_trace");
//! assert_eq!(candidates.len(), 1);
//! assert_eq!(candidates[0].path, "file.txt");
//! assert_eq!(candidates[0].confidence, 1.0);
//! ```
#![forbid(unsafe_code)]

mod anchor;
mod binding;
mod diff;
mod error;
mod fingerprint;
#[cfg(test)]
mod fixture;
mod handle;
mod oid;
mod oracle;
mod pin;
mod util;

pub use anchor::{
    Anchor, AnchorHints, AnchorIdentity, Descriptor, LineRange, Span, capture, capture_worktree,
};
pub use binding::{
    Binding, CommitIdentity, DeltaHints, DeltaIdentity, EvalState, HybridIdentity, NoHints,
    TreeHints, TreeIdentity, Validity, revalidate,
};
pub use diff::{TreeChange, diff_trees};
pub use error::{Error, Result};
pub use fingerprint::{Fingerprint, MINHASH_SHINGLE_V1, Param, minhash_shingle};
pub use handle::{AnchorId, CaptureHandle};
pub use oid::Oid;
pub use oracle::{
    Candidate, DIFF_TRACE_ALGORITHM, DIFF_TRACE_ORACLE_VERSION, OpLogSource, Oracle, diff_trace,
    fingerprint as fingerprint_oracle, minhash_similarity, op_log,
};
pub use pin::{REBIND_PIN_KIND, RebindPin, register_rebind_pin_schema};
