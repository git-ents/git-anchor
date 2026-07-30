//! The error type every `gix-comment` operation returns.

/// Everything that can go wrong reading or writing a [`crate::Comment`].
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The comment's storage commit could not be read or decoded while
    /// recovering a comment's author and timestamp from it.
    #[error("reading a comment's storage commit failed: {0}")]
    Commit(String),
    /// A comment id (or the target/attachment tree-ish it names) could not be
    /// resolved — [`Comments::edit`](crate::Comments::edit) on a comment that
    /// does not exist, for instance.
    #[error("could not resolve {0:?}")]
    Resolve(String),
    /// A stored parent id was not valid hex, while hydrating a
    /// [`crate::Comment`]'s [`parent`](crate::Comment::parent) — should never
    /// happen, since this crate is the only writer of that field and always
    /// writes a comment id's own hex rendering.
    #[error("stored parent id {0:?} is not a valid object id")]
    InvalidParent(String),
    /// Any underlying failure from the store's ref-store, committer, object
    /// database, or codec, collapsed to a single variant with the original
    /// error preserved as its source.
    #[error(transparent)]
    Git(Box<dyn std::error::Error + Send + Sync + 'static>),
}

impl Error {
    /// Collapse a `gix-store` error into [`Error::Git`], preserving it as
    /// the source.
    pub(crate) fn git<E>(err: E) -> Self
    where
        E: Into<Box<dyn std::error::Error + Send + Sync + 'static>>,
    {
        Error::Git(err.into())
    }
}

impl From<gix_store::Error> for Error {
    fn from(err: gix_store::Error) -> Self {
        Error::git(err)
    }
}

/// The `Result` alias every `gix-comment` operation returns.
pub type Result<T> = std::result::Result<T, Error>;
