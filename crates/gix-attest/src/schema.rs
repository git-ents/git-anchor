//! Registration of the kinds attest owns at `refs/schema/*`.
//!
//! Attest registers exactly one kind this phase: the [`Envelope`], the claim
//! machinery itself. Payload schemas belong to their vocabulary owners —
//! `rebind pin` to anchor, action records to the action schema, review
//! assertions to forge — and attest registers none of them, because it
//! carries payload hashes without understanding them. (Phase 2 adds one
//! more, `AttestKey`, for the same reason the envelope qualifies: key
//! material *is* envelope machinery.)
//!
//! Registration is also the enforcement point for the frozen target
//! descriptor. [`gix_store::KindSchema::write`] runs `check_identity_subtrees`
//! over the compiled schema, so a [`Target`](crate::Target) whose shape left
//! the identity normal form's universe would be refused here — before
//! anything is published — rather than producing target keys the frozen
//! mapping cannot reproduce.

use gix::objs::{Find, Write};
use gix_store::{Committer, RefSegment, RefStore, Store, schema_of};

use crate::envelope::Envelope;
use crate::error::{Error, Result};

/// The kind name an [`Envelope`] schema is published under, and the segment
/// claim refs are grouped by (`refs/claims/<target-key>`).
pub const CLAIM_KIND: &str = "claims";

/// The ref segment [`CLAIM_KIND`] names.
///
/// # Panics
///
/// Never: [`CLAIM_KIND`] is a valid ref segment, checked by
/// [`tests::the_claim_kind_is_a_valid_ref_segment`].
pub(crate) fn claim_segment() -> RefSegment {
    RefSegment::new(CLAIM_KIND).expect("the claim kind is a valid ref segment")
}

/// Register [`Envelope`]'s schema under [`CLAIM_KIND`] in `store`.
///
/// Re-registering the identical schema advances the schema ref again (an
/// ordinary commit-forward) but publishes the same schema content.
///
/// # Errors
///
/// [`Error::Schema`] when the schema cannot be derived from [`Envelope`], and
/// [`Error::SchemaRegistration`] when the store write fails — including the
/// identity-universe refusal that keeps [`Target`](crate::Target) inside the
/// frozen mapping.
pub fn register_claim_schema<R, O>(store: &Store<R, O>) -> Result<gix::ObjectId>
where
    R: RefStore + Committer,
    O: Find + Write,
{
    let schema = schema_of::<Envelope>()?;
    store
        .dynamic(claim_segment())
        .schema()
        .put(&schema)
        .map_err(|source| Error::SchemaRegistration {
            kind: CLAIM_KIND,
            source: Box::new(source),
        })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, reason = "unit test")]

    use facet::Facet;
    use facet_git_tree::UniverseError;
    use gix_store::{MemoryRefStore, Store};

    use super::*;
    use crate::fixture::memory_store;

    #[test]
    fn the_claim_kind_is_a_valid_ref_segment() {
        assert_eq!(claim_segment().as_str(), CLAIM_KIND);
    }

    #[test]
    fn registering_the_schema_publishes_it_as_a_kind() {
        let store: Store<MemoryRefStore, facet_git_tree::ObjectStore> = memory_store();
        register_claim_schema(&store).unwrap();
        assert!(
            store
                .dynamic(claim_segment())
                .schema()
                .get()
                .unwrap()
                .is_some()
        );
        assert_eq!(store.kinds().unwrap(), vec![claim_segment()]);
    }

    #[test]
    fn registering_twice_publishes_the_same_schema_content() {
        let store: Store<MemoryRefStore, facet_git_tree::ObjectStore> = memory_store();
        register_claim_schema(&store).unwrap();
        let first = store.dynamic(claim_segment()).schema().get().unwrap();
        register_claim_schema(&store).unwrap();
        let second = store.dynamic(claim_segment()).schema().get().unwrap();
        assert_eq!(
            first, second,
            "re-registering the identical schema is a content no-op"
        );
    }

    /// The universe check is what registration buys: the marked `target`
    /// subtree is walked, and an [`Envelope`]'s is inside the frozen
    /// universe, so nothing about registering it is accidental.
    #[test]
    fn the_envelopes_target_subtree_is_a_checked_identity_subtree() {
        let schema = schema_of::<Envelope>().unwrap();
        let subtrees: Vec<_> = facet_git_tree::identity_subtrees(&schema)
            .map(|(path, _node)| path.to_owned())
            .collect();
        assert_eq!(
            subtrees.len(),
            1,
            "exactly one marked subtree, the target: {subtrees:?}"
        );
        facet_git_tree::check_identity_subtrees(&schema).unwrap();
    }

    /// The same registration path, exercised against a target that *left* the
    /// universe: an envelope whose descriptor grew a field the frozen mapping
    /// cannot express is refused, and publishes nothing.
    ///
    /// The out-of-universe shape has to be written out here because
    /// [`Target`] itself is inside the universe — which is the property under
    /// test. This is the counterfactual: had `Target` looked like this, it
    /// could not have been registered, and no target key would exist to
    /// disagree about.
    #[test]
    fn a_target_outside_the_universe_is_refused_at_registration() {
        #[derive(Facet)]
        #[facet(facet_git_tree::identity_key)]
        struct WideTarget {
            kind: String,
            id: crate::Oid,
            /// Not in the frozen universe: an enum carries a tag the normal
            /// form has no variant for.
            scope: Scope,
        }

        #[derive(Facet)]
        #[repr(u8)]
        enum Scope {
            Whole,
            Range,
        }

        #[derive(Facet)]
        struct WideEnvelope {
            target: WideTarget,
            payload: crate::Oid,
            payload_kind: String,
            key: crate::Oid,
        }

        let store: Store<MemoryRefStore, facet_git_tree::ObjectStore> = memory_store();
        let error = store
            .dynamic(claim_segment())
            .schema()
            .put(&schema_of::<WideEnvelope>().unwrap())
            .unwrap_err();

        let gix_store::Error::IdentityUniverse { kind, source, .. } = error else {
            panic!("expected a universe refusal, got {error}");
        };
        assert_eq!(kind, claim_segment());
        let UniverseError::Excluded { path, found } = source else {
            panic!("expected an exclusion, got {source}");
        };
        assert_eq!(found, "Enum");
        assert!(path.ends_with(".scope"), "{path}");
        assert!(
            store
                .dynamic(claim_segment())
                .schema()
                .get()
                .unwrap()
                .is_none(),
            "a refused registration publishes nothing"
        );
    }
}
