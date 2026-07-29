//! [`Oid`]: this crate's single `Facet`-derived representation of a 20-byte
//! object id.

use facet::Facet;
use gix::ObjectId;

/// A 20-byte object id, wrapped so it can derive [`Facet`] — [`ObjectId`]
/// itself is defined outside this crate and cannot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Facet)]
#[facet(transparent)]
pub struct Oid([u8; 20]);

impl From<ObjectId> for Oid {
    fn from(id: ObjectId) -> Self {
        let bytes: [u8; 20] = id
            .as_slice()
            .try_into()
            .expect("Oid assumes a 20-byte (SHA-1) object id");
        Self(bytes)
    }
}

impl From<Oid> for ObjectId {
    fn from(oid: Oid) -> Self {
        ObjectId::from_bytes_or_panic(&oid.0)
    }
}
