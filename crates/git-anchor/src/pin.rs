//! `rebind pin`: the payload schema `git-attest` carries opaquely on a
//! signed claim, registered here because anchor owns the vocabulary
//! (ARCHITECTURE.md: "`rebind pin` by anchor" is "a store schema registered
//! by anchor, composed only in query"). Attest itself does not exist yet
//! (DELTA X1); this crate implements nothing beyond the schema and its
//! registration — no envelope, no signing, no chaining.

use facet::Facet;
use gix_object::{Find, Write};
use gix_store::{Committer, RefSegment, RefStore, Store, schema_of};

use crate::error::{Error, Result};
use crate::oid::Oid;

/// The kind name a [`RebindPin`] schema is published under.
pub const REBIND_PIN_KIND: &str = "rebind-pin";

/// A `rebind pin`'s payload: a revision plus a location (Worked example 1:
/// "rev C -> src/refdb/store.rs 118..568"). Opaque to `git-attest`, which
/// carries this without understanding it; meaningful only to `git-query`,
/// which composes it with anchor's own vocabulary (`pin_claim`).
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct RebindPin {
    /// The revision the pin was made against.
    pub rev: Oid,
    /// The path the anchor is pinned to at `rev`.
    pub path: String,
    /// The byte span, `(start, end)`, the anchor is pinned to at `rev`.
    pub span: (u64, u64),
}

/// Register [`RebindPin`]'s schema under [`REBIND_PIN_KIND`] in `store` — the
/// one-time act of anchor asserting ownership of the `rebind pin` claim
/// vocabulary. Re-registering the identical schema advances the ref again
/// (an ordinary commit-forward) but publishes the same schema content.
///
/// # Errors
///
/// [`Error::SchemaRegistration`] when the kind name is invalid, the schema
/// cannot be derived from [`RebindPin`], or the underlying store write
/// fails.
pub fn register_rebind_pin_schema<R, O>(store: &Store<R, O>) -> Result<gix::ObjectId>
where
    R: RefStore + Committer,
    O: Find + Write,
{
    let segment = RefSegment::new(REBIND_PIN_KIND)
        .map_err(|error| Error::SchemaRegistration(error.to_string()))?;
    let schema =
        schema_of::<RebindPin>().map_err(|error| Error::SchemaRegistration(error.to_string()))?;
    store
        .dynamic(segment)
        .schema()
        .put(&schema)
        .map_err(|error| Error::SchemaRegistration(error.to_string()))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "unit test")]

    use gix_store::{MemoryRefStore, ObjectId as StoreObjectId, RefSegment, Store};

    use super::*;
    use crate::fixture::empty_store;

    #[test]
    fn registering_the_schema_publishes_it_as_a_kind() {
        let store: Store<MemoryRefStore, facet_git_tree::ObjectStore> = empty_store();
        register_rebind_pin_schema(&store).unwrap();
        let segment = RefSegment::new(REBIND_PIN_KIND).unwrap();
        assert!(store.dynamic(segment).schema().get().unwrap().is_some());
    }

    #[test]
    fn registering_twice_publishes_the_same_schema_content() {
        let store: Store<MemoryRefStore, facet_git_tree::ObjectStore> = empty_store();
        register_rebind_pin_schema(&store).unwrap();
        let first = store
            .dynamic(RefSegment::new(REBIND_PIN_KIND).unwrap())
            .schema()
            .get()
            .unwrap();
        register_rebind_pin_schema(&store).unwrap();
        let second = store
            .dynamic(RefSegment::new(REBIND_PIN_KIND).unwrap())
            .schema()
            .get()
            .unwrap();
        assert_eq!(
            first, second,
            "re-registering the identical schema is a content no-op"
        );
    }

    #[test]
    fn rebind_pin_round_trips_through_the_registered_schema() {
        let store: Store<MemoryRefStore, facet_git_tree::ObjectStore> = empty_store();
        register_rebind_pin_schema(&store).unwrap();

        let pin = RebindPin {
            rev: gix::ObjectId::from_hex(b"cccccccccccccccccccccccccccccccccccccccc")
                .unwrap()
                .into(),
            path: "src/refdb/store.rs".to_owned(),
            span: (118, 568),
        };
        let root: StoreObjectId = facet_git_tree::serialize_into(&pin, store.objects()).unwrap();
        let back: RebindPin = facet_git_tree::deserialize(&root, store.objects()).unwrap();
        assert_eq!(back, pin);
    }
}
