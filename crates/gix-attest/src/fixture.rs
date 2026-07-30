//! Test-only fixtures: the stores and envelopes this crate's unit tests are
//! written against.
//!
//! Compiled only under `cfg(test)`. Fixtures the *integration* tests need
//! live in the workspace's `test-support` crate instead.

#![allow(clippy::unwrap_used, reason = "test fixture")]

use gix::ObjectId;
use gix_store::{MemoryRefStore, Store};

use crate::envelope::{Envelope, Target};

/// A fresh in-memory store with the default layout, for schema tests.
pub(crate) fn memory_store() -> Store<MemoryRefStore, facet_git_tree::ObjectStore> {
    Store::new(
        MemoryRefStore::new(),
        facet_git_tree::ObjectStore::default(),
    )
}

/// The object id `byte` repeated twenty times.
pub(crate) fn oid(byte: u8) -> ObjectId {
    ObjectId::from_bytes_or_panic(&[byte; 20])
}

/// A target whose `kind` is an uninterpreted label — `"anchor"` here is a
/// string this crate knows nothing about, which is the point.
pub(crate) fn target(kind: &str, byte: u8) -> Target {
    Target {
        kind: kind.to_owned(),
        id: oid(byte).into(),
    }
}

/// An envelope over [`target`], with a payload hash and payload kind attest
/// never looks inside.
pub(crate) fn envelope(kind: &str, byte: u8, payload_kind: &str) -> Envelope {
    Envelope {
        target: target(kind, byte),
        payload: oid(0xaa).into(),
        payload_kind: payload_kind.to_owned(),
        key: oid(0xbb).into(),
    }
}
