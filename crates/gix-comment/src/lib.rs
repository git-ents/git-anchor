//! Comments attached to Git objects, built on [`gix_anchor`]'s [`Binding`]
//! and this crate's own `gix-store` persistence under `refs/comments`.
//!
//! A comment is the simplest possible client of an anchor: a message pinned
//! to whatever a [`Binding`] names — a commit, a tree, or a durable
//! line-range position in a blob — that *follows the content* across history
//! exactly as the anchor it rides on does.
//!
//! - The comment **message** is the document's body.
//! - The comment **author** and **timestamp** are the storage commit's — a
//!   comment is a git commit, so git already records who wrote it and when;
//!   this crate reads those back rather than storing them a second time.
//! - An optional **attachment** is an arbitrary tree, embedded in the
//!   document so it stays reachable through the comment's own ref
//!   (`anchor.retention`).
//!
//! Everything a caller needs to say what a comment is *about* — [`Binding`],
//! [`capture`], [`capture_worktree`], line ranges, and projection — is
//! re-exported here, so a consumer depends on `gix-comment` alone.
//!
//! # Examples
//!
//! Comment on a commit, then read the message and author back:
//!
//! ```
//! use gix_comment::{Binding, Comments};
//!
//! # let dir = tempfile::tempdir().expect("tempdir");
//! # std::process::Command::new("git").arg("init").arg("-q").arg(dir.path()).status().unwrap();
//! # std::fs::write(dir.path().join("file.txt"), "a\nb\nc\n").unwrap();
//! # let run = |args: &[&str]| {
//! #     let ok = std::process::Command::new("git").arg("-C").arg(dir.path())
//! #         .args(args).status().unwrap().success();
//! #     assert!(ok);
//! # };
//! # run(&["config", "user.name", "Ada"]);
//! # run(&["config", "user.email", "ada@example.com"]);
//! # run(&["add", "-A"]);
//! # run(&["commit", "-q", "-m", "one"]);
//! let repo = gix::open(dir.path()).expect("open");
//! let comments = Comments::open(&repo);
//!
//! let head = repo.head_id().expect("head").detach();
//! let id = comments.add(&Binding::Commit { commit: head.into() }, "ship it", None).expect("add");
//!
//! let comment = comments.get(id).expect("get").expect("exists");
//! assert_eq!(comment.message, "ship it");
//! assert_eq!(comment.author.name, "Ada"); // from the storage commit, not stored twice
//! ```
#![forbid(unsafe_code)]

mod comment;
mod error;
mod store;

pub use comment::{Author, Comment, Comments, State, Thread};
pub use error::{Error, Result};

// The `gix-anchor` vocabulary a caller needs to describe and project what a
// comment is about — re-exported so consumers depend on `gix-comment` alone.
pub use gix_anchor::{
    Anchor, Binding, LineRange, Projection, capture, capture_worktree, project, project_worktree,
    snippet,
};
