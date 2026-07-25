//! The error type every `gix-comment` operation returns.

/// Everything that can go wrong reading or writing a [`crate::Comment`].
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A `gix-anchor` [`Store`](gix_anchor::Store) operation failed — the
    /// note persistence, binding codec, or projection layer a comment is
    /// built on. The original error is preserved as the source.
    #[error(transparent)]
    Anchor(#[from] gix_anchor::Error),
    /// The note's storage commit could not be read or decoded while
    /// recovering a comment's author and timestamp from it.
    #[error("reading a comment's storage commit failed: {0}")]
    Commit(String),
    /// A comment id (or the target/attachment tree-ish it names) could not be
    /// resolved — [`Comments::edit`](crate::Comments::edit) on a comment that
    /// does not exist, for instance.
    #[error("could not resolve {0:?}")]
    Resolve(String),
    /// A stored [`StoredNote::parent`](gix_anchor::StoredNote::parent) was
    /// not valid hex, while hydrating a [`crate::Comment`]'s
    /// [`parent`](crate::Comment::parent) — should never happen, since this
    /// crate is the only writer of that field and always writes a comment
    /// id's own hex rendering.
    #[error("stored parent id {0:?} is not a valid object id")]
    InvalidParent(String),
}

/// The `Result` alias every `gix-comment` operation returns.
pub type Result<T> = std::result::Result<T, Error>;
