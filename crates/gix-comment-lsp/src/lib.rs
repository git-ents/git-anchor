//! `gix-comment-lsp`: an editor-facing Language Server Protocol surface over
//! `refs/comments/*`, projecting a repository's anchored comments into
//! whatever buffer the user is reading and writing new ones back through the
//! same [`gix_comment::Comments`] calls a CLI porcelain would make.
//!
//! # One responsibility
//!
//! This crate is a read-time *view* over `refs/comments/*`, owning no state
//! of its own beyond the client's open buffers: a comment left in an editor
//! or by any other frontend is one and the same entity everywhere
//! (`lens.parity`). It never shells out and never reimplements listing or
//! projection — every operation is the exact [`gix_comment::Comments`] call
//! a `git comment` CLI porcelain makes.
//!
//! - `lens.serve` — [`serve`], stdio only, no socket, no git transport.
//! - `lens.lenses` — [`Lens::code_lenses`]: one View/Reply/Resolve lens set
//!   per open, position-bound root comment projecting onto the document,
//!   derived per request.
//! - `lens.diagnostics` — [`Lens::diagnostics`]: the same comments as
//!   hint-severity diagnostics, never warnings or errors.
//! - `lens.hover` — [`Lens::hover`]: the full thread as Markdown.
//! - `lens.compose` — [`Lens::code_actions`] plus the compose flow in
//!   [`compose`]: a code action opens a git-style template file under
//!   `.git/`; saving a non-empty body creates the comment.
//! - `lens.working-tree` — projection targets the working tree, the open
//!   buffer standing in for disk, re-projected on every change.
//! - `lens.parity` — every operation is a [`gix_comment::Comments`] call.
//!
//! # The compose-on-save mechanism
//!
//! Composing works entirely through standard LSP a plain client provides —
//! `workspace/executeCommand`, `window/showDocument`, and
//! `textDocument/didSave` — with no client-specific extension. The
//! `comment.compose` command writes a git-commit-style template under
//! `.git/` and asks the client to open it; when the user saves it, the
//! `didSave` handler creates the comment (or aborts on an empty body). See
//! [`compose`] for the exact grammar and rationale.
#![forbid(unsafe_code)]

mod compose;
mod document;
mod error;
mod lens;
mod render;
mod server;

pub use error::{Error, Result};
pub use lens::{Lens, Outcome};
pub use render::{CMD_COMPOSE, CMD_REOPEN, CMD_REPLY, CMD_RESOLVE, CMD_VIEW};
pub use server::{capabilities, serve};
