//! The claim envelope and its target descriptor.
//!
//! An [`Envelope`] is what attest owns of a claim: *what* is claimed about
//! (a [`Target`]), *what the claim says* (a payload tree hash plus the name
//! of the schema kind that payload was written under), and *who signed it*
//! (the claim id of the signing key's key-add claim). Nothing here reads the
//! payload, and nothing here interprets [`Target::kind`].
//!
//! The two types are deliberately unlike each other:
//!
//! - [`Envelope`] is a *document*. It goes through the general
//!   `facet-git-tree` codec, so it may evolve like any other store document.
//! - [`Target`] is *frozen*. It carries
//!   `#[facet(facet_git_tree::identity_key)]`, so its subtree is hashed
//!   through the identity normal form ([`target_key`]) and schema
//!   registration refuses any shape that leaves the frozen universe.
//!
//! Phase-1 targets are single-hash only: a commit range or a `(base, tip)`
//! pair is not a new field but a new `kind` whose `id` is the normal-form
//! hash of a range descriptor, so no key computed here ever moves.

use std::collections::BTreeMap;

use facet::Facet;
use facet_git_tree::normal_form::{self, NormalForm};
use gix::ObjectId;

use crate::error::Result;
use crate::oid::Oid;

/// What a claim is about: an opaque `{kind, id}` descriptor.
///
/// `kind` is a label — `"blob"`, `"commit"`, `"tree"`, `"anchor"`, whatever
/// a vocabulary owner publishes — and this crate never matches on it.
/// Targets are typed *per check*: the consumer (a query rule, a forge check)
/// decides what a `"blob"` target means. A kind string is not a dependency,
/// so no vocabulary crate appears in attest's dependency graph because of it.
///
/// # Examples
///
/// ```
/// use gix_attest::{Target, target_key};
///
/// let id = gix::ObjectId::from_hex(b"7f3e000000000000000000000000000000000000").unwrap();
/// let target = Target { kind: "anchor".to_owned(), id: id.into() };
///
/// // The key is a pure function of the descriptor, through the frozen
/// // identity normal form — the same mapping anchor ids and action keys use.
/// assert_eq!(target_key(&target).unwrap(), target_key(&target).unwrap());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
#[facet(facet_git_tree::identity_key)]
pub struct Target {
    /// The label qualifying `id`, never interpreted here.
    pub kind: String,
    /// The object or identity hash the label qualifies.
    pub id: Oid,
}

/// A claim's envelope: the part of a claim attest understands.
///
/// The claim commit's tree is this struct serialized through the general
/// codec. Signature bytes are *not* here — they ride the claim commit's
/// standard `gpgsig` header, because a signature inside the tree would make
/// the signed content contain its own signature.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct Envelope {
    /// The normal-form descriptor of what is claimed about; its hash
    /// ([`target_key`]) names the claim ref.
    pub target: Target,
    /// The payload's store tree hash. Opaque: attest carries it and never
    /// fetches it.
    pub payload: Oid,
    /// The name of the store schema kind the payload was written under — a
    /// label for consumers, which join it against `refs/schema/<kind>`.
    /// Carried without being understood, exactly like [`payload`](Self::payload).
    pub payload_kind: String,
    /// The claim id of the key-add (or rotate) claim for the signing key.
    pub key: Oid,
}

/// The target key of `target`: the hash of its descriptor through the
/// identity normal form.
///
/// This is the same move as an anchor id or an action key — "no separate key
/// crates": the frozen mapping lives in `facet-git-tree`, and attest calls
/// it. The key names the claim ref, `refs/claims/<target-key>`.
///
/// # Errors
///
/// [`Error::NormalForm`](crate::Error::NormalForm) when the frozen mapping
/// cannot be written (a backend failure of the in-memory object store it
/// hashes into).
pub fn target_key(target: &Target) -> Result<ObjectId> {
    let (oid, _objects) = normal_form::hash(&to_normal_form(target))?;
    Ok(oid)
}

/// `target` in the identity normal form's closed universe.
///
/// Written by hand, not derived: the normal form has no reflection path from
/// an arbitrary `Facet` type, so the frozen mapping names its own fields —
/// and the field names here must match [`Target`]'s, which the
/// universe-checked schema registration in [`crate::schema`] keeps honest.
fn to_normal_form(target: &Target) -> NormalForm {
    NormalForm::Struct(BTreeMap::from([
        ("kind".to_owned(), NormalForm::Str(target.kind.clone())),
        ("id".to_owned(), NormalForm::Hash(ObjectId::from(target.id))),
    ]))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "unit test")]

    use super::*;

    fn oid(byte: u8) -> ObjectId {
        ObjectId::from_bytes_or_panic(&[byte; 20])
    }

    fn target(kind: &str, byte: u8) -> Target {
        Target {
            kind: kind.to_owned(),
            id: oid(byte).into(),
        }
    }

    #[test]
    fn the_target_key_is_a_function_of_both_fields() {
        let base = target_key(&target("blob", 0x11)).unwrap();
        assert_eq!(base, target_key(&target("blob", 0x11)).unwrap());
        assert_ne!(
            base,
            target_key(&target("commit", 0x11)).unwrap(),
            "the kind label participates in the key"
        );
        assert_ne!(
            base,
            target_key(&target("blob", 0x22)).unwrap(),
            "the id participates in the key"
        );
    }

    /// The frozen mapping is a change detector on purpose: a target key is a
    /// published claim-ref name, so a change to [`to_normal_form`] must break
    /// loudly here rather than silently re-home every claim in existence.
    #[test]
    fn the_target_key_is_frozen() {
        let key = target_key(&Target {
            kind: "anchor".to_owned(),
            id: ObjectId::from_hex(b"7f3e000000000000000000000000000000000000")
                .unwrap()
                .into(),
        })
        .unwrap();
        assert_eq!(key.to_string(), "3ae941b8060a8c2e5650d58a61fb8b0d8e017525");
    }

    /// An envelope round-trips through the general codec: it is a document,
    /// not an identity, and nothing about it is hashed through the normal
    /// form.
    #[test]
    fn an_envelope_round_trips_through_the_general_codec() {
        let envelope = Envelope {
            target: target("anchor", 0x7f),
            payload: oid(0xaa).into(),
            payload_kind: "rebind-pin".to_owned(),
            key: oid(0xbb).into(),
        };
        let objects = facet_git_tree::ObjectStore::default();
        let root = facet_git_tree::serialize_into(&envelope, &objects).unwrap();
        let back: Envelope = facet_git_tree::deserialize(&root, &objects).unwrap();
        assert_eq!(back, envelope);
    }
}
