//! [`CaptureHandle`] and [`AnchorId`]: the two oids a [`crate::Binding`]
//! yields, kept as distinct types so neither is usable where the other is
//! expected.
//!
//! A [`CaptureHandle`] is the oid of a whole serialized binding tree —
//! identity and hints together, through the general `facet-git-tree` codec
//! (so the binding can be embedded and read back as an ordinary document
//! field). It is transient: its only job is carrying a capture from
//! `create` to `inject`. An [`AnchorId`] is the hash of the `identity`
//! subtree *alone*, through the identity normal form (`anchor.identity`) —
//! a different mapping than the general codec, so [`CaptureHandle::anchor_id`]
//! decodes the binding and recomputes it rather than reading any oid off
//! the handle's own tree.

use facet_git_tree::normal_form::{self, NormalForm};
use gix::ObjectId;
use gix_object::Find;

use crate::error::{Error, Result};

/// The oid of a whole serialized [`crate::Binding`] tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CaptureHandle(ObjectId);

/// The content hash of a binding's `identity` subtree alone, through the
/// identity normal form (`anchor.identity`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AnchorId(ObjectId);

impl CaptureHandle {
    /// The [`AnchorId`] this handle's binding resolves to: decodes the
    /// binding the handle names and recomputes its `identity` subtree's
    /// normal-form hash — never the general codec's oid for that subtree,
    /// which the handle's own tree cannot be read off directly (`anchor.identity`).
    ///
    /// # Errors
    ///
    /// [`Error::Deserialize`] when `self` does not decode as a
    /// [`crate::Binding`].
    pub fn anchor_id<F>(&self, store: &F) -> Result<AnchorId>
    where
        F: Find + ?Sized,
    {
        crate::binding::Binding::deserialize(self, store)?.anchor_id()
    }
}

impl From<ObjectId> for CaptureHandle {
    fn from(oid: ObjectId) -> Self {
        Self(oid)
    }
}

impl From<CaptureHandle> for ObjectId {
    fn from(handle: CaptureHandle) -> Self {
        handle.0
    }
}

impl From<ObjectId> for AnchorId {
    fn from(oid: ObjectId) -> Self {
        Self(oid)
    }
}

impl From<AnchorId> for ObjectId {
    fn from(id: AnchorId) -> Self {
        id.0
    }
}

impl std::fmt::Display for CaptureHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

impl std::fmt::Display for AnchorId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

impl std::str::FromStr for CaptureHandle {
    type Err = gix::hash::decode::Error;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Ok(Self(ObjectId::from_hex(s.as_bytes())?))
    }
}

/// Any `Binding` variant's `identity` subtree, expressed in the identity
/// normal form's closed universe (`facet_git_tree::normal_form`) — the
/// mapping [`hash_identity`] hashes through, frozen independent of the
/// general codec (ARCHITECTURE.md, "Identity normal form"). Implemented by
/// hand, not derived, because the normal form has no reflection path from an
/// arbitrary `Facet` type: each identity struct names its own fields.
pub(crate) trait IdentityNormalForm {
    /// `self`, as a [`NormalForm`] value.
    fn to_normal_form(&self) -> NormalForm;
}

/// The content hash of an identity subtree, through the identity normal
/// form — the one place that knows an anchor id is `identity`'s hash, never
/// `hints`'s. [`Anchor::id`](crate::Anchor::id) and [`crate::Binding::anchor_id`]
/// both delegate here.
pub(crate) fn hash_identity<T: IdentityNormalForm>(identity: &T) -> Result<AnchorId> {
    let (oid, _store) = normal_form::hash(&identity.to_normal_form()).map_err(Error::NormalForm)?;
    Ok(AnchorId::from(oid))
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "unit test"
    )]

    use facet_git_tree::ObjectStore;

    use super::*;
    use crate::LineRange;
    use crate::anchor::capture;
    use crate::binding::{Binding, CommitIdentity, NoHints};
    use crate::fixture::{commit_all, numbered, repo};

    #[test]
    fn anchor_id_matches_anchors_own_id_and_ignores_hints() {
        let dir = repo();
        std::fs::write(dir.path().join("file.txt"), numbered(1..=10)).unwrap();
        commit_all(dir.path(), "one");
        let git_repo = gix::open(dir.path()).unwrap();
        let anchor = capture(
            &git_repo,
            "HEAD",
            "file.txt",
            Some(LineRange { start: 3, end: 4 }),
        )
        .unwrap();
        let expected = anchor.id().unwrap();

        let store = ObjectStore::default();
        let handle = Binding::Position(anchor).serialize_into(&store).unwrap();
        assert_eq!(handle.anchor_id(&store).unwrap(), expected);
    }

    #[test]
    fn anchor_id_of_a_non_position_binding_is_its_identity_subtree_through_the_normal_form() {
        let store = ObjectStore::default();
        let commit = gix::ObjectId::from_hex(b"0123456789abcdef0123456789abcdef01234567").unwrap();
        let identity = CommitIdentity {
            commit: commit.into(),
        };
        let binding = Binding::Commit {
            identity,
            hints: NoHints {},
        };
        let handle = binding.serialize_into(&store).unwrap();
        let expected = hash_identity(&identity).unwrap();
        assert_eq!(handle.anchor_id(&store).unwrap(), expected);
    }

    #[test]
    fn two_handles_from_identical_captures_share_an_anchor_id() {
        let dir = repo();
        std::fs::write(dir.path().join("file.txt"), numbered(1..=10)).unwrap();
        commit_all(dir.path(), "one");
        let git_repo = gix::open(dir.path()).unwrap();
        let store = ObjectStore::default();

        let first = capture(&git_repo, "HEAD", "file.txt", None).unwrap();
        let second = capture(&git_repo, "HEAD", "file.txt", None).unwrap();
        let first_handle = Binding::Position(first).serialize_into(&store).unwrap();
        let second_handle = Binding::Position(second).serialize_into(&store).unwrap();
        assert_eq!(first_handle, second_handle);
        assert_eq!(
            first_handle.anchor_id(&store).unwrap(),
            second_handle.anchor_id(&store).unwrap()
        );
    }
}
