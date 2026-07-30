//! The error type every `gix-attest` operation returns.

/// Everything that can go wrong writing or reading a claim.
///
/// Every variant is either a failure of machinery this crate *calls* —
/// hashing, schema registration, a store write, a ref name — or a structural
/// impossibility in what it read back: a claim that is not a commit, a key
/// reference that names no key. None is a judgment, because attest owns no
/// policy of its own to fail on, and a cryptographic outcome is a
/// [`Verdict`](crate::Verdict) rather than an error.
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
    /// Key material could not be parsed in the format it claimed.
    #[error("the public key is not an OpenSSH public key: {0}")]
    KeyMaterial(String),
    /// A claim commit could not be read out of the object database.
    #[error("reading claim {claim} failed: {source}")]
    Object {
        /// The claim whose commit could not be read.
        claim: gix::ObjectId,
        /// Why the read failed.
        source: gix::objs::find::Error,
    },
    /// A claim id names no object in this repository — a chain referring out
    /// of its own store, which no read can repair.
    #[error("claim {claim} is missing from the object database")]
    MissingClaim {
        /// The claim id that resolved to nothing.
        claim: gix::ObjectId,
    },
    /// A claim id names an object that is not a commit, so it is not a claim.
    #[error("{claim} is not a claim commit")]
    NotAClaim {
        /// The id that named a non-commit.
        claim: gix::ObjectId,
    },
    /// An [`Envelope.key`](crate::Envelope::key) named a claim that does not
    /// publish a key. Attest understands two claim kinds and this is one of
    /// them, so a mislabeled one is a structural error rather than a verdict.
    #[error("claim {claim} is not a key claim: its payload kind is {payload_kind}")]
    NotAKeyClaim {
        /// The claim that was expected to publish a key.
        claim: gix::ObjectId,
        /// The payload kind it carries instead.
        payload_kind: String,
    },
    /// A key claim is not on the chain of the key it names as its target — a
    /// claim id reached from outside the chain it belongs to, which makes its
    /// predecessors unknowable.
    #[error("claim {claim} is not on its target's chain")]
    ClaimOffChain {
        /// The claim that was not found on the chain it names.
        claim: gix::ObjectId,
    },
}

/// The `Result` alias every `gix-attest` operation returns.
pub type Result<T> = std::result::Result<T, Error>;
