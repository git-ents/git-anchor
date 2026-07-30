//! The error type every `gix-anchor` operation returns.

use gix::ObjectId;

/// Everything that can go wrong capturing an [`crate::Anchor`] or running
/// one of the [`crate::oracle`] functions over it.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A revision string (a `revision`/`target` argument) could not be
    /// resolved to a commit in the repository.
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
    /// (`anchor.definition`'s validation, applied to the byte span it
    /// canonicalizes to).
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
    /// [`crate::capture_worktree`] was asked to read the working tree of a
    /// repository that has none (a bare repository). Capture against a
    /// revision instead (`anchor.working-tree` applies only where a working
    /// tree exists).
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
    /// Hashing an identity subtree through the identity normal form
    /// (`anchor.identity`) failed — a backend error from the object store
    /// the write was given, or a name the frozen mapping cannot express.
    #[error(transparent)]
    NormalForm(#[from] facet_git_tree::NormalFormError),
    /// [`crate::register_rebind_pin_schema`] could not register the
    /// `rebind pin` payload schema.
    #[error("registering the rebind-pin schema failed: {0}")]
    SchemaRegistration(String),
}

/// The `Result` alias every `gix-anchor` operation returns.
pub type Result<T> = std::result::Result<T, Error>;
