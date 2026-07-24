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
    /// A stored tree's entry names matched none of [`crate::Binding`]'s five
    /// variant shapes ([`crate::Binding::deserialize`]'s sniffing rule):
    /// neither `blob`+`content` (`Position`), `base_tree` (`Delta`),
    /// `witness`+`tree` (`Tree`), exactly `{commit, tree}` (`Hybrid`), nor
    /// exactly `{commit}` (`Commit`).
    #[error("tree {id} does not match any known binding shape (entries: {entries:?})")]
    UnknownBindingShape {
        /// The tree that could not be recognized as any binding variant.
        id: ObjectId,
        /// The entry names actually present in that tree.
        entries: Vec<String>,
    },
    /// A [`crate::Store`] ref-path component was not usable as a Git
    /// ref-name segment. In practice this never fires — both components are
    /// hex [`ObjectId`] text — but it is checked anyway rather than trusted.
    #[error("invalid {what} {value:?}: {reason}")]
    InvalidRefComponent {
        /// Which component was rejected (`"target"` or `"id"`).
        what: &'static str,
        /// The offending value.
        value: String,
        /// Why it was rejected.
        reason: &'static str,
    },
    /// A [`crate::Store::attach`] write lost its compare-and-swap race too
    /// many times in a row.
    #[error("gave up updating {refname} after {attempts} contended attempts")]
    CasExhausted {
        /// The ref that stayed contended.
        refname: String,
        /// How many attempts were made before giving up.
        attempts: u32,
    },
    /// Any underlying `gix` failure from [`crate::Store`]'s ref, commit, or
    /// lock-file operations, collapsed to a single variant with the
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
