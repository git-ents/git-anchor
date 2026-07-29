//! The error type every `gix-anchor` operation returns.

use gix::ObjectId;

/// Everything that can go wrong capturing or projecting an [`crate::Anchor`].
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A revision string ([`crate::capture`]'s or [`crate::project`]'s
    /// `revision`/`target` argument) could not be resolved to a commit in
    /// the repository.
    #[error("could not resolve {0:?}")]
    Resolve(String),
    /// A git object could not be read or decoded.
    #[error("git object operation failed: {0}")]
    Object(String),
    /// The tree diff between the anchor commit and the target commit
    /// failed.
    #[error("tree diff failed: {0}")]
    Diff(String),
    /// The anchor names a path that is not a regular file in the commit it
    /// was captured against (`anchor.definition`'s path validation).
    #[error("no file at {path:?} in {commit}")]
    MissingPath {
        /// The commit the path was looked up in.
        commit: ObjectId,
        /// The path that is not a file there.
        path: String,
    },
    /// The line range does not fit the file it is anchored to
    /// (`anchor.definition`'s line-range validation).
    #[error("lines {start}..={end} do not fit {path:?} ({len} lines)")]
    LinesOutOfRange {
        /// The file the range was checked against.
        path: String,
        /// The 1-based first line of the range.
        start: u64,
        /// The 1-based last line of the range.
        end: u64,
        /// How many lines the file actually has.
        len: u64,
    },
    /// [`crate::project_exact`]'s anchor commit is no longer present in the
    /// repository (garbage collected) — [`crate::project`] catches this and
    /// retries with [`crate::project_from_context`]
    /// (`anchor.fuzzy-fallback`); a caller invoking
    /// [`crate::project_exact`] directly sees it as an ordinary error.
    #[error("the anchor commit {0} no longer exists")]
    AnchorCommitMissing(ObjectId),
    /// [`crate::capture_worktree`] or [`crate::project_worktree`] was asked
    /// to read the working tree of a repository that has none (a bare
    /// repository). Capture or project against a revision instead
    /// (`anchor.working-tree` applies only where a working tree exists).
    #[error("the repository has no working tree")]
    NoWorkingTree,
    /// Encoding a [`crate::Binding`] through the underlying `facet-git-tree`
    /// codec failed — a backend error from the `gix` object store the codec
    /// was given.
    #[error(transparent)]
    Serialize(#[from] facet_git_tree::SerializeError),
    /// Decoding a [`crate::Binding`] through the underlying `facet-git-tree`
    /// codec failed — a malformed payload tree, or a backend error from the
    /// `gix` object store the codec was given.
    #[error(transparent)]
    Deserialize(#[from] facet_git_tree::DeserializeError),
    /// [`crate::Store::create`] minted a genesis commit whose oid already
    /// names an existing note ref — an identity collision rather than a
    /// transient compare-and-swap race, so retrying would spin forever.
    #[error("genesis commit {0} already has a note ref")]
    GenesisExists(gix::ObjectId),
    /// Any underlying failure from [`crate::Store`]'s ref-store, committer,
    /// or object-database backend, collapsed to a single variant with the
    /// original error preserved as its source.
    #[error(transparent)]
    Git(Box<dyn std::error::Error + Send + Sync + 'static>),
}

impl Error {
    /// Collapse a `gix` error into [`Error::Git`], preserving it as the source.
    pub(crate) fn git<E>(err: E) -> Self
    where
        E: Into<Box<dyn std::error::Error + Send + Sync + 'static>>,
    {
        Error::Git(err.into())
    }
}

/// The `Result` alias every `gix-anchor` operation returns.
pub type Result<T> = std::result::Result<T, Error>;
