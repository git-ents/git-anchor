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
//! `verify` is deliberately absent from this module: it is cryptography, and
//! it lives in [`crate::verify`]. Nothing here reports on a signature, so
//! nothing here can be mistaken for having checked one.
//!
//! Two claim kinds *are* understood here, because they are the envelope
//! machinery itself: revocation ([`Claims::revoke`]) and key lifecycle
//! ([`Claims::add_key`](crate::Claims::add_key), in [`crate::key`]). A third
//! would be the abstraction leaking — every other payload kind is a label
//! this crate carries and never matches on.

use gix::ObjectId;
use gix::objs::{Find, Write};
use gix_store::{Committer, Layout, RefName, RefPath, RefPrefix, RefSegment, RefStore, Store};

use crate::envelope::{Envelope, Target, target_key};
use crate::error::Result;
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
    /// written. It is [`Claims::resolve`] that applies revocations
    /// structurally and fills this in; `log` has no opinion.
    ///
    /// Marked, not deleted, and with no opinion on what revocation *means*:
    /// whether a revoked claim still counts for anything is a query rule's
    /// business.
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

/// The [`Target::kind`] of a revocation's target: a claim.
pub const CLAIM_TARGET_KIND: &str = "claim";

/// The [`Target::kind`] of a key's own claim chain.
pub const KEY_TARGET_KIND: &str = "key";

/// The [`Envelope::payload_kind`] a revocation carries.
///
/// A revocation has nothing to say beyond *which* claim it revokes, which its
/// target already says, so its payload is the empty tree and this label is how
/// the chain records what the claim is.
pub const REVOCATION_KIND: &str = "revocation";

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
        self.append(&envelope.target, envelope)
    }

    /// Revoke `claim`, signing the revocation with the key `key` names, and
    /// return the revocation's own claim id.
    ///
    /// The revocation is a claim whose target is
    /// `{kind: "claim", id: <claim>}`, appended to *`claim`'s own ref* — so
    /// the chain that carries a claim carries its revocation too, and reading
    /// the chain is enough to see it. [`resolve`](Self::resolve) is what
    /// applies it.
    ///
    /// # Errors
    ///
    /// [`Error::Store`] when `claim` cannot be read or the revocation cannot
    /// be written, plus [`sign`](Self::sign)'s errors for the derived names.
    pub fn revoke(&self, claim: ObjectId, key: crate::Oid) -> Result<ObjectId> {
        let revoked = self.envelope_at(claim)?;
        let envelope = Envelope {
            target: Target {
                kind: CLAIM_TARGET_KIND.to_owned(),
                id: claim.into(),
            },
            payload: gix::ObjectId::empty_tree(gix::hash::Kind::Sha1).into(),
            payload_kind: REVOCATION_KIND.to_owned(),
            key,
        };
        self.append(&revoked.target, &envelope)
    }

    /// Append a claim carrying `envelope` to the chain of `on`, which is
    /// `envelope.target` for an ordinary claim and the *revoked* claim's
    /// target for a revocation.
    pub(crate) fn append(&self, on: &Target, envelope: &Envelope) -> Result<ObjectId> {
        let name = Self::entity_name(on)?;
        let summary = format!("claim {}", envelope.payload_kind);
        Ok(self
            .store
            .kind::<Envelope>(claim_segment())
            .update(&name, |_current| (summary.clone(), envelope.clone()))?)
    }

    /// The envelope of the claim `claim`, read out of that commit's own tree.
    ///
    /// # Errors
    ///
    /// [`Error::Store`] when the commit cannot be read or its envelope
    /// decoded — including when `claim` is not a claim commit at all.
    pub fn envelope_at(&self, claim: ObjectId) -> Result<Envelope> {
        Ok(self.store.kind::<Envelope>(claim_segment()).get_at(claim)?)
    }

    /// The store these claims live in.
    pub(crate) fn store(&self) -> &'s Store<R, O> {
        self.store
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

    /// [`log`](Self::log) with revocations applied structurally: newest
    /// first, with every revoked claim carrying the id of the revocation that
    /// revoked it in [`Claim::revoked_by`].
    ///
    /// Structural means exactly what the chain records and no more. A
    /// revocation on this chain whose target names a claim on it marks that
    /// claim; nothing is dropped, nothing is re-ordered, and what a mark
    /// *means* — whether the claim still counts, whether the revocation
    /// reaches claims admitted before it — is a query rule's, not this
    /// function's.
    ///
    /// # Errors
    ///
    /// As [`log`](Self::log).
    pub fn resolve(&self, target: &Target) -> Result<Vec<Claim>> {
        let mut claims: Vec<Claim> = self.log(target)?.collect();
        let revocations: Vec<(ObjectId, ObjectId)> = claims
            .iter()
            .filter(|claim| is_revocation(&claim.envelope))
            .map(|claim| (claim.envelope.target.id.into(), claim.id))
            .collect();
        for (revoked, revocation) in revocations {
            for claim in &mut claims {
                if claim.id == revoked {
                    // Newest first, so the first revocation seen for a claim
                    // is the newest; a second one changes nothing about the
                    // fact recorded.
                    claim.revoked_by.get_or_insert(revocation);
                }
            }
        }
        Ok(claims)
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

/// Whether `envelope` is a revocation: a claim about a claim, labeled as one.
///
/// The only payload-kind match in this crate besides the key claim's, and both
/// are envelope machinery rather than vocabulary — see the module docs.
fn is_revocation(envelope: &Envelope) -> bool {
    envelope.target.kind == CLAIM_TARGET_KIND && envelope.payload_kind == REVOCATION_KIND
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
    use crate::fixture::{envelope, oid, target};
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

    /// An unrevoked chain resolves to itself: `resolve` invents no marks.
    #[test]
    fn resolve_marks_nothing_on_a_chain_with_no_revocation() {
        let store = store();
        let claims = Claims::open(&store);
        claims
            .sign(&envelope("anchor", 0x7f, "rebind-pin"))
            .unwrap();
        claims.sign(&envelope("anchor", 0x7f, "review")).unwrap();

        let resolved = claims.resolve(&target("anchor", 0x7f)).unwrap();
        assert_eq!(resolved.len(), 2);
        assert!(resolved.iter().all(|claim| claim.revoked_by.is_none()));
    }

    /// A revocation lands on the revoked claim's own ref — the chain records
    /// it — and `resolve` marks exactly the claim it names.
    #[test]
    fn a_revocation_chains_on_the_revoked_claims_ref_and_marks_only_it() {
        let store = store();
        let claims = Claims::open(&store);
        let first = claims
            .sign(&envelope("anchor", 0x7f, "rebind-pin"))
            .unwrap();
        let second = claims.sign(&envelope("anchor", 0x7f, "review")).unwrap();
        let revocation = claims.revoke(first, oid(0xbb).into()).unwrap();

        // The revocation is a claim on the revoked claim's chain, not on the
        // chain of its own `{kind: "claim"}` target.
        let resolved = claims.resolve(&target("anchor", 0x7f)).unwrap();
        assert_eq!(
            resolved.iter().map(|claim| claim.id).collect::<Vec<_>>(),
            vec![revocation, second, first],
        );
        assert_eq!(
            claims
                .log(&Target {
                    kind: CLAIM_TARGET_KIND.to_owned(),
                    id: first.into(),
                })
                .unwrap()
                .count(),
            0,
            "a revocation starts no chain of its own"
        );

        let marks: Vec<_> = resolved
            .iter()
            .map(|claim| (claim.id, claim.revoked_by))
            .collect();
        assert_eq!(
            marks,
            vec![
                (revocation, None),
                (second, None),
                (first, Some(revocation))
            ],
            "exactly the revoked claim is marked, and it is marked rather than dropped"
        );
        assert_eq!(
            resolved[0].envelope.payload_kind, REVOCATION_KIND,
            "the revocation is itself a claim on the chain"
        );
    }
}
