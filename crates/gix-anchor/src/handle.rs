//! [`CaptureHandle`] and [`AnchorId`]: the two oids a [`crate::Binding`]
//! yields, kept as distinct types so neither is usable where the other is
//! expected.
//!
//! A [`CaptureHandle`] is the oid of a whole serialized binding tree —
//! identity and hints together. It is transient: its only job is carrying a
//! capture from `create` to `inject`, because hints must be locatable
//! before `inject` embeds them inline in a document. An [`AnchorId`] is the
//! oid of the `identity` subtree alone (`anchor.identity`) — invariant
//! under every hint change, and the value a pin cites.
//!
//! Because `identity` is a named entry inside the binding tree,
//! [`CaptureHandle::anchor_id`] reads it directly off a handle, with no
//! extra bookkeeping and without decoding `hints` at all.

use gix::ObjectId;
use gix_object::{Find, Kind};

use crate::error::{Error, Result};

/// The oid of a whole serialized [`crate::Binding`] tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CaptureHandle(ObjectId);

/// The content hash of a binding's `identity` subtree alone
/// (`anchor.identity`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AnchorId(ObjectId);

impl CaptureHandle {
    /// The [`AnchorId`] this handle's binding resolves to: `identity`'s oid,
    /// located by walking the handle's tree rather than decoding `hints`.
    ///
    /// # Errors
    ///
    /// [`Error::Object`] when `self` does not decode as a binding tree (the
    /// externally-tagged, single-entry-then-`identity`/`hints` shape every
    /// [`crate::Binding`] variant serializes to).
    pub fn anchor_id<F>(&self, store: &F) -> Result<AnchorId>
    where
        F: Find + ?Sized,
    {
        let payload = only_entry(self.0, store)?;
        let identity = named_entry(payload, "identity", store)?;
        Ok(AnchorId(identity))
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

/// `id`'s tree, asserted to hold exactly one entry — every [`crate::Binding`]
/// variant carries fields, so its externally-tagged encoding always wraps
/// the payload in a single `<variant name> → payload` entry — that entry's
/// own oid.
fn only_entry<F>(id: ObjectId, store: &F) -> Result<ObjectId>
where
    F: Find + ?Sized,
{
    match tree_entries(id, store)?.as_slice() {
        [(_, oid)] => Ok(*oid),
        _ => Err(Error::Object(format!(
            "{id} is not a single-entry binding tree"
        ))),
    }
}

/// The oid of `tree`'s entry named `name`.
fn named_entry<F>(tree: ObjectId, name: &str, store: &F) -> Result<ObjectId>
where
    F: Find + ?Sized,
{
    tree_entries(tree, store)?
        .into_iter()
        .find(|(entry_name, _)| entry_name == name)
        .map(|(_, oid)| oid)
        .ok_or_else(|| Error::Object(format!("{tree} has no {name:?} entry")))
}

/// `id`'s decoded tree entries as `(name, oid)` pairs.
fn tree_entries<F>(id: ObjectId, store: &F) -> Result<Vec<(String, ObjectId)>>
where
    F: Find + ?Sized,
{
    let mut buf = Vec::new();
    let data = store
        .try_find(&id, &mut buf)
        .map_err(|error| Error::Object(error.to_string()))?
        .ok_or_else(|| Error::Object(format!("object {id} not found")))?;
    if data.kind != Kind::Tree {
        return Err(Error::Object(format!("{id} is not a tree")));
    }
    let tree = gix_object::TreeRef::from_bytes(data.data, id.kind())
        .map_err(|error| Error::Object(error.to_string()))?;
    tree.entries
        .iter()
        .map(|entry| {
            let name = std::str::from_utf8(entry.filename)
                .map_err(|_error| Error::Object(format!("non-utf8 entry name in tree {id}")))?;
            Ok((name.to_owned(), entry.oid.to_owned()))
        })
        .collect()
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
    fn anchor_id_of_a_non_position_binding_is_its_identity_subtree() {
        let store = ObjectStore::default();
        let commit = gix::ObjectId::from_hex(b"0123456789abcdef0123456789abcdef01234567").unwrap();
        let binding = Binding::Commit {
            identity: CommitIdentity {
                commit: commit.into(),
            },
            hints: NoHints {},
        };
        let handle = binding.serialize_into(&store).unwrap();
        let expected: ObjectId = facet_git_tree::serialize_into(
            &CommitIdentity {
                commit: commit.into(),
            },
            &store,
        )
        .unwrap();
        assert_eq!(ObjectId::from(handle.anchor_id(&store).unwrap()), expected);
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
