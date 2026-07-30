//! The error type every `gix-attest` operation returns.

/// Everything that can go wrong writing or reading a claim.
///
/// Every variant is a failure of machinery this crate *calls* — hashing,
/// schema registration, a store write, a ref name — because attest owns no
/// policy of its own to fail on.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A `gix-store` read or write failed.
    #[error(transparent)]
    Store(#[from] gix_store::Error),
    /// Hashing a [`Target`](crate::Target) through the identity normal form
    /// failed — a name the frozen mapping cannot express, or a backend error
    /// from the object store the hash was written into.
    #[error(transparent)]
    NormalForm(#[from] facet_git_tree::NormalFormError),
    /// A claim ref name could not be assembled — a target key that is not a
    /// usable ref segment.
    #[error(transparent)]
    RefName(#[from] gix_store::InvalidRefName),
    /// A schema this crate owns could not be registered: an invalid kind
    /// name, a schema that cannot be derived, or a store write failure.
    #[error("registering the {kind} schema failed: {source}")]
    SchemaRegistration {
        /// The kind whose schema registration failed.
        kind: &'static str,
        /// Why it failed.
        source: Box<gix_store::Error>,
    },
    /// A schema could not be derived from a Rust type.
    #[error(transparent)]
    Schema(#[from] facet_git_tree::SchemaError),
    /// An operation this phase deliberately does not answer. It is an error
    /// rather than a default answer so no caller can mistake "not built yet"
    /// for "nothing to report".
    #[error("{0} is not implemented yet")]
    Unimplemented(&'static str),
}

/// The `Result` alias every `gix-attest` operation returns.
pub type Result<T> = std::result::Result<T, Error>;
