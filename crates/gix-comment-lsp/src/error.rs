//! The lens's error type: every failure a request handler can hit while
//! reading `refs/comments/*`, projecting an anchor, or writing a new
//! comment.

/// A lens operation's result.
pub type Result<T> = std::result::Result<T, Error>;

/// Everything that can go wrong deriving a lens response or composing a
/// comment through it.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A comment read, listing, projection, or mutation failed in the
    /// shared `gix-comment` library the lens calls (`lens.parity`). The
    /// caller should surface the message; it is never a protocol-level
    /// fault.
    #[error(transparent)]
    Comment(#[from] gix_comment::Error),

    /// A `gix-anchor` projection or capture failed while landing a comment
    /// onto the open document (`lens.working-tree`) or capturing a new
    /// one's anchor (`lens.compose`).
    #[error(transparent)]
    Anchor(#[from] gix_anchor::Error),

    /// A filesystem operation on the compose template under `.git/`
    /// (`lens.compose`) failed. The caller should report it; the comment
    /// was not created.
    #[error("template {path}: {source}")]
    Template {
        /// The template path the operation targeted.
        path: std::path::PathBuf,
        /// The underlying IO error.
        source: std::io::Error,
    },

    /// A comment id argument (an `executeCommand` argument, or a parsed
    /// template's reply-parent metadata) was not a valid object id.
    #[error("not a valid comment id: {0:?}")]
    InvalidId(String),

    /// An `executeCommand` request named a command the lens exposes but
    /// carried the wrong arguments (a missing or non-string comment id, for
    /// instance). The caller sent a malformed request.
    #[error("bad command arguments: {0}")]
    BadArguments(String),

    /// The JSON-RPC transport over stdio failed (a broken pipe, a malformed
    /// frame) or the initialize handshake did not complete (`lens.serve`).
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
