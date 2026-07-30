//! Claims as chained commits on claim refs.
//!
//! A claim is a commit on `refs/claims/<target-key>`; the claim id *is* that
//! commit's oid, so no side mapping exists. Claims about one target chain as
//! parent and child on the one ref, and the ref only ever advances by a
//! commit whose parent is the tip it was read from — the compare-and-swap
//! `gix-store` already performs *is* chain integrity, so a lost race retries
//! on the new tip and concurrent claims serialize instead of forking.
//!
//! Everything mechanical here is store's:
//!
//! - the write is [`gix_store::Kind::update`], so the tree is the ordinary
//!   `{value/, schema/}` document tree and the commit is an ordinary commit;
//! - the signature is whatever [`Signer`](gix_refstore::Signer) the caller
//!   configured with [`Store::with_signer`], landing in the standard `gpgsig`
//!   header — **attest contributes zero signing code**;
//! - the chain walk is store's first-parent history.
//!
//! `verify` is deliberately absent from this module and from this phase: it
//! is cryptography, it lives in [`crate::verify`], and Phase 2 writes it.
//! Nothing here reports on a signature, so nothing here can be mistaken for
//! having checked one.

use gix::ObjectId;
use gix::objs::{Find, Write};
use gix_store::{Committer, Layout, RefName, RefPath, RefPrefix, RefSegment, RefStore, Store};

use crate::envelope::{Envelope, Target, target_key};
use crate::error::{Error, Result};
use crate::schema::claim_segment;

/// One claim: its id, its envelope, and whether the chain structurally
/// revokes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    /// The claim id: the claim commit's own oid.
    pub id: ObjectId,
    /// What the claim says, as attest understands it.
    pub envelope: Envelope,
    /// The revocation claim that revokes this one, if any.
    ///
    /// Always `None` from [`Claims::log`], which reports the chain as
    /// written. It is [`Claims::resolve`] — Phase 2 — that applies
    /// revocations structurally and fills this in; `log` has no opinion.
    pub revoked_by: Option<ObjectId>,
}

/// The claims in one store: the whole consumer contract for the chain.
///
/// # The layout
///
/// Claim refs are `<store data prefix>/claims/<target-key>`, so a store
/// opened over [`layout`] — whose data prefix is `refs` — puts them
/// at exactly `refs/claims/<target-key>`. The schema stays at
/// `refs/schema/claims`.
pub struct Claims<'s, R, O> {
    store: &'s Store<R, O>,
}

impl<'s, R, O> Claims<'s, R, O>
where
    R: RefStore + Committer,
    O: Find + Write,
{
    /// Open the claims of `store`.
    ///
    /// Signing is a property of `store` (see [`Store::with_signer`]), not of
    /// this handle: a store with a signer writes signed claims, one without
    /// writes unsigned ones, and attest neither configures nor inspects the
    /// difference.
    pub fn open(store: &'s Store<R, O>) -> Self {
        Claims { store }
    }

    /// Append a claim carrying `envelope` to its target's chain, and return
    /// the claim id.
    ///
    /// The ref is derived from `envelope.target` alone, and the write is
    /// store's compare-and-swap commit-forward: a claim lands as a child of
    /// whatever tip it was read against, retrying on the new tip when a
    /// concurrent writer wins the race.
    ///
    /// # Errors
    ///
    /// [`Error::NormalForm`] when the target key cannot be hashed,
    /// [`Error::RefName`] when it is not a usable ref segment, and
    /// [`Error::Store`] when the write fails — including
    /// [`gix_store::Error::NoSchema`] when
    /// [`register_claim_schema`](crate::register_claim_schema) has not run.
    pub fn sign(&self, envelope: &Envelope) -> Result<ObjectId> {
        let name = Self::entity_name(&envelope.target)?;
        let summary = format!("claim {}", envelope.payload_kind);
        Ok(self
            .store
            .kind::<Envelope>(claim_segment())
            .update(&name, |_current| (summary.clone(), envelope.clone()))?)
    }

    /// The chain of claims on `target`, newest first.
    ///
    /// The iterator is materialized before it is returned, because a decode
    /// failure part-way down a chain is a real error and an
    /// `Iterator<Item = Claim>` has nowhere to put one; the walk itself is
    /// store's first-parent history, so the order is the chain's order.
    ///
    /// [`Claim::revoked_by`] is `None` throughout: this is the chain as
    /// written, with no revocation applied. See [`resolve`](Self::resolve).
    ///
    /// # Errors
    ///
    /// [`Error::NormalForm`]/[`Error::RefName`] for an unusable target, and
    /// [`Error::Store`] when a claim commit cannot be read or its envelope
    /// decoded.
    pub fn log(&self, target: &Target) -> Result<impl Iterator<Item = Claim>> {
        let kind = self.store.kind::<Envelope>(claim_segment());
        let name = Self::entity_name(target)?;
        let mut claims = Vec::new();
        for id in kind.history(&name)? {
            claims.push(Claim {
                id,
                envelope: kind.get_at(id)?,
                revoked_by: None,
            });
        }
        Ok(claims.into_iter())
    }

    /// [`log`](Self::log) with revocations applied structurally.
    ///
    /// # Errors
    ///
    /// Always [`Error::Unimplemented`]: revocation is Phase 2's native
    /// vocabulary, and answering "nothing is revoked" before it exists would
    /// be a false negative dressed as an answer.
    pub fn resolve(&self, _target: &Target) -> Result<Vec<Claim>> {
        Err(Error::Unimplemented("resolve"))
    }

    /// The ref the claims about `target` live on.
    ///
    /// # Errors
    ///
    /// [`Error::NormalForm`] when the target key cannot be hashed, and
    /// [`Error::RefName`] when it is not a usable ref segment.
    pub fn reference(&self, target: &Target) -> Result<RefName> {
        Ok(self
            .store
            .kind::<Envelope>(claim_segment())
            .reference(&Self::entity_name(target)?))
    }

    /// The entity name a target's claims are stored under: its target key,
    /// hex.
    fn entity_name(target: &Target) -> Result<RefPath> {
        Ok(RefSegment::new(target_key(target)?.to_string())?.into())
    }
}

/// The [`Layout`] that puts claim refs at `refs/claims/<target-key>`, the ref
/// layout claims are specified to live at, with schemas left at
/// `refs/schema/*`.
///
/// A free function rather than a [`Claims`] associated one so a caller can
/// name it before it has a store to open [`Claims`] over.
///
/// # Panics
///
/// Never: both prefixes are valid, checked by
/// [`tests::the_layout_is_the_documented_one`].
#[must_use]
pub fn layout() -> Layout {
    Layout {
        data: RefPrefix::new("refs").expect("`refs` is a valid ref prefix"),
        ..Layout::default()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "unit test")]

    use gix_store::MemoryRefStore;

    use super::*;
    use crate::fixture::{envelope, target};
    use crate::schema::register_claim_schema;

    fn store() -> Store<MemoryRefStore, facet_git_tree::ObjectStore> {
        let store = Store::with_layout(
            MemoryRefStore::new(),
            facet_git_tree::ObjectStore::default(),
            layout(),
        );
        register_claim_schema(&store).unwrap();
        store
    }

    #[test]
    fn the_layout_is_the_documented_one() {
        let layout = layout();
        assert_eq!(layout.data.as_str(), "refs");
        assert_eq!(layout.schema.as_str(), "refs/schema");
    }

    #[test]
    fn a_claim_ref_is_refs_claims_target_key() {
        let store = store();
        let claims = Claims::open(&store);
        let target = target("anchor", 0x7f);
        assert_eq!(
            claims.reference(&target).unwrap().as_str(),
            format!("refs/claims/{}", target_key(&target).unwrap())
        );
    }

    #[test]
    fn a_signed_claim_is_readable_back_as_its_envelope() {
        let store = store();
        let claims = Claims::open(&store);
        let envelope = envelope("anchor", 0x7f, "rebind-pin");
        let id = claims.sign(&envelope).unwrap();

        let log: Vec<_> = claims.log(&envelope.target).unwrap().collect();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].id, id);
        assert_eq!(log[0].envelope, envelope);
        assert_eq!(log[0].revoked_by, None, "log applies no revocation");
    }

    #[test]
    fn two_claims_on_one_target_chain_newest_first() {
        let store = store();
        let claims = Claims::open(&store);
        let first = claims
            .sign(&envelope("anchor", 0x7f, "rebind-pin"))
            .unwrap();
        let second = claims.sign(&envelope("anchor", 0x7f, "review")).unwrap();

        let log: Vec<_> = claims.log(&target("anchor", 0x7f)).unwrap().collect();
        assert_eq!(
            log.iter().map(|claim| claim.id).collect::<Vec<_>>(),
            vec![second, first],
            "newest first"
        );
        assert_eq!(log[0].envelope.payload_kind, "review");
    }

    #[test]
    fn claims_on_different_targets_are_different_chains() {
        let store = store();
        let claims = Claims::open(&store);
        claims
            .sign(&envelope("anchor", 0x7f, "rebind-pin"))
            .unwrap();
        claims.sign(&envelope("blob", 0x7f, "review")).unwrap();

        assert_eq!(claims.log(&target("anchor", 0x7f)).unwrap().count(), 1);
        assert_eq!(claims.log(&target("blob", 0x7f)).unwrap().count(), 1);
    }

    #[test]
    fn an_unclaimed_target_has_an_empty_log() {
        let store = store();
        assert_eq!(
            Claims::open(&store)
                .log(&target("commit", 0x01))
                .unwrap()
                .count(),
            0
        );
    }

    #[test]
    fn resolve_refuses_rather_than_reporting_nothing_revoked() {
        let store = store();
        let error = Claims::open(&store)
            .resolve(&target("anchor", 0x7f))
            .unwrap_err();
        assert!(matches!(error, Error::Unimplemented("resolve")), "{error}");
    }
}
